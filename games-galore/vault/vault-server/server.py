"""
Vault library server.

Runs on the machine with the mounted drive. Exposes the catalog and
game files over HTTP so the desktop client never needs direct
filesystem access to the library, and never needs Python or `nsz`
installed at all — both stay here.

Endpoints:
  GET /library                                  -> full catalog, JSON
  GET /media/<platform>/<title>/<filename>      -> a screenshot or trailer
  GET /download/<platform>/<title>/<filename>   -> a game file (converts nsz -> nsp first if needed)
  GET /status                                   -> whether `nsz` is available on this machine
  GET /saves/<platform>/<title>                 -> stored save versions, newest first
  POST /saves/<platform>/<title>                -> store the body as a new save version
  GET /saves/<platform>/<title>/<version>       -> one save archive

A game's id is "<platform>/<title>", which is why those two make up the
first half of the media and download paths. `filename` is relative to
the game's folder and may itself contain subdirectories, since a PC
game's executable often sits inside its tree rather than beside it.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from flask import Flask, abort, jsonify, request, send_file, url_for

from config import CACHE_DIR, HOST, LIBRARY_ROOT, PORT, SAVE_ROOT, SAVE_VERSIONS_KEPT
from library import scan_library

app = Flask(__name__)

_catalog: dict = {}  # game id -> Game

STATIC_MEDIA_EXTENSIONS = {".png", ".jpg", ".jpeg", ".webp", ".mp4", ".mkv", ".webm"}

# A generous ceiling rather than a tuned one: console-era saves are
# kilobytes and a PC prefix's user directory is usually megabytes, so
# anything approaching this is a sign the wrong directory got archived.
# Its real job is to stop one bad client filling the disk.
MAX_SAVE_BYTES = 512 * 1024 * 1024


def _reload_catalog() -> None:
    global _catalog
    _catalog = {g.id: g for g in scan_library(LIBRARY_ROOT)}


@app.route("/library")
def library_route():
    # Rescans every call — simple and correct at ~500 titles. Worth
    # swapping for a cached catalog plus a manual /rescan trigger (or a
    # filesystem watcher) if this ever gets slow.
    _reload_catalog()

    payload = []
    for game in _catalog.values():
        d = game.to_dict()
        d["screenshots"] = [
            _media_url(game, s) for s in game.screenshots
        ]
        d["cover"] = _media_url(game, game.cover) if game.cover else None
        d["trailer"] = _media_url(game, game.trailer) if game.trailer else None
        payload.append(d)
    return jsonify(payload)


def _media_url(game, filename: str) -> str:
    return url_for(
        "media_route",
        platform=game.platform,
        title=game.title,
        filename=filename,
        _external=True,
    )


# Platform and title are matched as single segments and the filename
# takes everything after them, so a file inside a game's subdirectory
# ("PC/Some Game/bin/game.exe") splits correctly. Matching the game id
# itself with a greedy <path:> converter instead would swallow those
# leading subdirectories into the id and leave only the basename as the
# filename, which no lookup would then resolve.
@app.route("/media/<platform>/<title>/<path:filename>")
def media_route(platform: str, title: str, filename: str):
    game_dir = _resolve_game_dir(f"{platform}/{title}")
    path = _safe_join(game_dir, filename)
    if path.suffix.lower() not in STATIC_MEDIA_EXTENSIONS:
        abort(403)
    return send_file(path, conditional=True)


@app.route("/download/<platform>/<title>/<path:filename>")
def download_route(platform: str, title: str, filename: str):
    game_id = f"{platform}/{title}"
    game = _catalog.get(game_id)
    if game is None:
        abort(404, "unknown game id")

    matching = next((f for f in game.files if f.filename == filename), None)
    if matching is None:
        abort(404, "unknown file for this game")

    game_dir = _resolve_game_dir(game_id)
    source_path = _safe_join(game_dir, filename)

    if not matching.needs_conversion:
        # conditional=True gets Range-request support for free from
        # Werkzeug, so the client can show real progress and resume.
        return send_file(source_path, conditional=True, as_attachment=True)

    return send_file(_get_or_convert(game_id, source_path), conditional=True, as_attachment=True)


def _get_or_convert(game_id: str, nsz_path: Path) -> Path:
    """
    Converts nsz_path once and caches the .nsp under CACHE_DIR, keyed by
    game id, so repeat downloads of the same title never re-run
    decompression. The source library is never modified.
    """
    cache_dir = CACHE_DIR / game_id
    cache_dir.mkdir(parents=True, exist_ok=True)
    cached_nsp = cache_dir / (nsz_path.stem + ".nsp")

    if cached_nsp.exists():
        return cached_nsp

    if shutil.which("nsz") is None:
        abort(503, "nsz is not installed on this server")

    result = subprocess.run(
        ["nsz", "-D", "--output", str(cache_dir), str(nsz_path)],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        abort(500, f"nsz conversion failed: {result.stderr.strip()}")
    if not cached_nsp.exists():
        abort(500, "nsz reported success but no .nsp file was produced")

    return cached_nsp


@app.route("/status")
def status_route():
    nsz_path = shutil.which("nsz")
    version = None
    if nsz_path:
        result = subprocess.run(["nsz", "--version"], capture_output=True, text=True)
        version = (result.stdout or result.stderr).strip()
    return jsonify({"nsz_found": nsz_path is not None, "nsz_version": version})


def _safe_segment(value: str) -> str:
    """
    One path component straight off the URL. Flask's default converter
    already rules out slashes, so this is about the rest: empty names,
    `.`/`..`, and anything with a separator smuggled in. Saves are
    written, not just read, so a bad segment here would create
    directories rather than merely read the wrong file.
    """
    if not value or value in {".", ".."} or "/" in value or "\\" in value or "\0" in value:
        abort(400, "invalid path segment")
    return value


def _save_dir(platform: str, title: str, create: bool = False) -> Path:
    path = SAVE_ROOT / _safe_segment(platform) / _safe_segment(title)
    # Belt and braces: confirm the composed path really is under
    # SAVE_ROOT before anything is created or read.
    if SAVE_ROOT.resolve() not in path.resolve().parents:
        abort(400, "invalid path")
    if create:
        path.mkdir(parents=True, exist_ok=True)
    return path


def _read_versions(directory: Path) -> list:
    """Every stored version's metadata, newest first."""
    if not directory.is_dir():
        return []
    versions = []
    for meta_path in directory.glob("*.json"):
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue  # a half-written or hand-edited sidecar isn't fatal
        if (directory / f"{meta.get('version')}.tar.gz").is_file():
            versions.append(meta)
    versions.sort(key=lambda m: m.get("uploaded_at", ""), reverse=True)
    return versions


@app.route("/saves/<platform>/<title>")
def save_list_route(platform: str, title: str):
    """
    Every stored version for this game, newest first. The client
    compares the newest entry against what's on its own disk to decide
    whether it is behind, ahead, or in conflict.
    """
    return jsonify({"versions": _read_versions(_save_dir(platform, title))})


@app.route("/saves/<platform>/<title>", methods=["POST"])
def save_upload_route(platform: str, title: str):
    """
    Stores the request body as a new version. The body is the raw
    .tar.gz the client built from its save directory — no multipart
    wrapper, since there's exactly one file and the client is not a
    browser form.

    `device` and `saved_at` are the client's claims about where the
    save came from and when it was last touched locally. They're
    recorded for display and conflict detection, never trusted for
    anything that touches the filesystem.
    """
    blob = request.get_data()
    if not blob:
        abort(400, "empty upload")
    if len(blob) > MAX_SAVE_BYTES:
        abort(413, f"save exceeds {MAX_SAVE_BYTES} bytes")

    directory = _save_dir(platform, title, create=True)
    now = datetime.now(timezone.utc)
    digest = hashlib.sha256(blob).hexdigest()

    # Re-uploading an identical save is a no-op rather than a new
    # version — otherwise merely launching and quitting a game would
    # push out the history that makes versions worth keeping.
    existing = _read_versions(directory)
    if existing and existing[0].get("sha256") == digest:
        return jsonify({"version": existing[0]["version"], "unchanged": True})

    # Second-resolution timestamp plus a digest prefix: sortable,
    # filesystem-safe, and collision-free within the same second.
    version = f"{now.strftime('%Y%m%dT%H%M%SZ')}-{digest[:8]}"
    meta = {
        "version": version,
        "uploaded_at": now.isoformat(),
        "size_bytes": len(blob),
        "sha256": digest,
        "device": str(request.args.get("device", ""))[:64],
        "saved_at": str(request.args.get("saved_at", ""))[:64],
    }

    # Archive first, sidecar second: _read_versions only reports a
    # version once both exist, so a crash between the two leaves an
    # ignored orphan rather than a metadata entry pointing at nothing.
    (directory / f"{version}.tar.gz").write_bytes(blob)
    (directory / f"{version}.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")

    _prune_versions(directory)
    return jsonify(meta)


@app.route("/saves/<platform>/<title>/<version>")
def save_download_route(platform: str, title: str, version: str):
    directory = _save_dir(platform, title)
    path = _safe_join(directory, f"{_safe_segment(version)}.tar.gz")
    return send_file(path, conditional=True, as_attachment=True)


def _prune_versions(directory: Path) -> None:
    for stale in _read_versions(directory)[SAVE_VERSIONS_KEPT:]:
        (directory / f"{stale['version']}.tar.gz").unlink(missing_ok=True)
        (directory / f"{stale['version']}.json").unlink(missing_ok=True)


def _resolve_game_dir(game_id: str) -> Path:
    game = _catalog.get(game_id)
    if game is None:
        abort(404, "unknown game id")
    platform, title = game_id.split("/", 1)
    return LIBRARY_ROOT / platform / title


def _safe_join(base: Path, filename: str) -> Path:
    """
    Resolves filename against base and refuses anything that escapes
    it. filename comes straight from the URL, so this is the one place
    a crafted request (an absolute path, or `../../etc/passwd`) gets
    stopped before it reaches the filesystem.
    """
    if Path(filename).is_absolute() or ".." in Path(filename).parts:
        abort(400, "invalid path")

    candidate = (base / filename).resolve()
    base_resolved = base.resolve()
    if candidate != base_resolved and base_resolved not in candidate.parents:
        abort(400, "invalid path")
    if not candidate.is_file():
        abort(404)
    return candidate


if __name__ == "__main__":
    _reload_catalog()
    app.run(host=HOST, port=PORT)
