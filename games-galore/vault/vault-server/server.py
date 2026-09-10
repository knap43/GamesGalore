"""
Vault library server.

Runs on the machine with the mounted drive. Exposes the catalog and
game files over HTTP so the desktop client never needs direct
filesystem access to the library, and never needs Python or `nsz`
installed at all — both stay here.

Endpoints:
  GET /library                          -> full catalog, JSON
  GET /media/<game_id>/<filename>       -> a screenshot or trailer
  GET /download/<game_id>/<filename>    -> a game file (converts nsz -> nsp first if needed)
  GET /status                           -> whether `nsz` is available on this machine
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

from flask import Flask, abort, jsonify, send_file, url_for

from config import CACHE_DIR, HOST, LIBRARY_ROOT, PORT
from library import scan_library

app = Flask(__name__)

_catalog: dict = {}  # game id -> Game

STATIC_MEDIA_EXTENSIONS = {".png", ".jpg", ".jpeg", ".webp", ".mp4", ".mkv", ".webm"}


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
            url_for("media_route", game_id=game.id, filename=s, _external=True)
            for s in game.screenshots
        ]
        d["cover"] = (
            url_for("media_route", game_id=game.id, filename=game.cover, _external=True)
            if game.cover
            else None
        )
        d["trailer"] = (
            url_for("media_route", game_id=game.id, filename=game.trailer, _external=True)
            if game.trailer
            else None
        )
        payload.append(d)
    return jsonify(payload)


@app.route("/media/<path:game_id>/<filename>")
def media_route(game_id: str, filename: str):
    game_dir = _resolve_game_dir(game_id)
    path = _safe_join(game_dir, filename)
    if path.suffix.lower() not in STATIC_MEDIA_EXTENSIONS:
        abort(403)
    return send_file(path, conditional=True)


@app.route("/download/<path:game_id>/<filename>")
def download_route(game_id: str, filename: str):
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
