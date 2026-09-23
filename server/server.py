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
import hmac
import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

import requests
from flask import Flask, Response, abort, jsonify, request, send_file, url_for

import config
from config import (
    CACHE_DIR,
    CACHE_MAX_BYTES,
    CATALOG_TTL_SECONDS,
    METADATA_MAX_SCREENSHOTS,
    HOST,
    LEGACY_CACHE_DIR,
    LEGACY_SAVE_ROOT,
    PORT,
    SAVE_ROOT,
    RAWG_API_KEY,
    SAVE_TOKEN,
    SAVE_VERSIONS_KEPT,
)
from library import scan_library_located
from metadata import MetadataError, RawgClient, fill_game_folder

app = Flask(__name__)

_catalog: dict = {}  # game id -> Game
_game_dirs: dict = {}  # game id -> the directory its files are in
_media_dirs: dict = {}  # game id -> the directory its cover and README are in
_catalog_signature: Optional[tuple] = None  # what the tree looked like when it was scanned
_catalog_scanned_at: float = 0.0

STATIC_MEDIA_EXTENSIONS = {".png", ".jpg", ".jpeg", ".webp", ".mp4", ".mkv", ".webm"}

# A generous ceiling rather than a tuned one: console-era saves are
# kilobytes and a PC prefix's user directory is usually megabytes, so
# anything approaching this is a sign the wrong directory got archived.
# Its real job is to stop one bad client filling the disk.
MAX_SAVE_BYTES = 512 * 1024 * 1024


def _migrate_legacy_dir(legacy: Path, current: Path) -> bool:
    """
    Moves a directory left behind by the old name into its new home,
    once, and only when there is nothing at the new path to overwrite.

    The saves are the reason this exists: they are the one thing here
    that cannot be regenerated, and a checkout that was merely updated
    would otherwise come up pointing at an empty directory and look for
    all the world like it had lost them. Returns whether anything moved,
    so the caller can say so rather than doing it silently.
    """
    if current.exists() or not legacy.is_dir():
        return False
    try:
        current.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(legacy), str(current))
    except OSError:
        # Better to carry on with an empty directory than to refuse to
        # start; the old one is still there to be moved by hand.
        return False
    return True


def migrate_legacy_state() -> None:
    for legacy, current, what in (
        (LEGACY_SAVE_ROOT, SAVE_ROOT, "cloud saves"),
        (LEGACY_CACHE_DIR, CACHE_DIR, "conversion cache"),
    ):
        if _migrate_legacy_dir(legacy, current):
            print(f"moved {what} from {legacy} to {current}")


def library_roots() -> list:
    """
    The drives the library lives on, in the order they are searched.

    Read from the config module on every call rather than imported
    once, so that a config.py written before there could be several —
    one with only LIBRARY_ROOT in it — still works, and so the tests
    can point the server at a temporary tree.
    """
    roots = getattr(config, "LIBRARY_ROOTS", None)
    if not roots:
        roots = [config.LIBRARY_ROOT]
    return [Path(root) for root in roots]


def _reload_catalog() -> None:
    global _catalog, _game_dirs, _media_dirs, _catalog_signature, _catalog_scanned_at

    catalog: dict = {}
    dirs: dict = {}
    media: dict = {}
    missing = []
    for root in library_roots():
        try:
            found = scan_library_located(root)
        except FileNotFoundError:
            # A drive that isn't mounted right now shouldn't empty the
            # catalog of the ones that are. It is still worth saying
            # out loud, since a library that is quietly half its usual
            # size is the kind of thing nobody notices.
            missing.append(root)
            print(f"library root is not there, skipping it: {root}")
            continue
        for located in found:
            game = located.game
            if game.id in catalog:
                # First drive listed wins. A title being copied from
                # one drive to another exists on both for as long as
                # the copy takes; serving the established copy until
                # the old one is deleted is the safe half of that.
                continue
            catalog[game.id] = game
            # Taken from the scan rather than rebuilt from the id: a
            # game's files and its catalog furniture are the same
            # directory for a title that lives in one, and two
            # different ones for a disc sitting loose in a platform
            # folder.
            dirs[game.id] = located.content_dir
            media[game.id] = located.media_dir

    if missing and len(missing) == len(library_roots()):
        # Every drive gone is a configuration problem rather than a
        # library that happens to be empty, and it reads far better as
        # the error it always was than as a server with no games.
        raise FileNotFoundError(f"Library root does not exist: {missing[0]}")

    _catalog = catalog
    _game_dirs = dirs
    _media_dirs = media
    _catalog_signature = _library_signature()
    _catalog_scanned_at = time.time()


def _library_signature() -> tuple:
    """
    A cheap fingerprint of the library's shape: every platform and game
    directory, with its modification time.

    Two levels deep and no further, which is the whole point. A full
    scan walks every PC game's entire tree to size it, and on a large
    library that is seconds; this stats a few hundred directories and
    is imperceptible. A directory's mtime moves when anything is added
    to or removed from it, so a new game, a deleted one and a renamed
    one are all caught.

    What it does not catch is a file *edited* in place several levels
    down — an .exe replaced by a patch, say — which would change a
    game's size without changing any directory it is counted under.
    The TTL below is the backstop for that, and /rescan is the answer
    for someone who knows they have just changed something.
    """
    entries = []
    for root in library_roots():
        try:
            platforms = sorted(root.iterdir())
        except OSError:
            # Counted as part of the shape: a drive appearing or going
            # away is exactly the kind of change this exists to catch.
            entries.append((str(root), None))
            continue
        for platform_dir in platforms:
            if not platform_dir.is_dir():
                continue
            try:
                entries.append((str(platform_dir), platform_dir.stat().st_mtime))
                for game_dir in sorted(platform_dir.iterdir()):
                    if game_dir.is_dir():
                        entries.append((str(game_dir), game_dir.stat().st_mtime))
            except OSError:
                continue
    return tuple(entries)


def _ensure_catalog() -> None:
    """
    Rescans only when something looks different, or when the cached
    scan is old enough to be worth distrusting.

    /library used to rescan the whole tree on every single call, which
    on a large library made opening the app a multi-second wait — the
    client now paints its installed titles from its own cache to hide
    that, and this is the other half: making the wait short rather than
    hiding it.
    """
    if not _catalog:
        _reload_catalog()
        return
    if time.time() - _catalog_scanned_at > CATALOG_TTL_SECONDS:
        _reload_catalog()
        return
    if _library_signature() != _catalog_signature:
        _reload_catalog()


@app.route("/library")
def library_route():
    _ensure_catalog()

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
    path = _safe_join(_resolve_media_dir(f"{platform}/{title}"), filename)
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


@app.route("/archive/<platform>/<title>")
def archive_route(platform: str, title: str):
    """
    Every file of one game, as a single uncompressed tar, streamed.

    A PC game is a tree of thousands of files, and installing one meant
    thousands of HTTP requests — correct, and fine on a LAN, but each
    one pays for a connection, a route lookup and a catalog hit, and
    the sum of that dwarfed the transfer itself for small files. This
    is the same bytes in one response.

    Uncompressed on purpose: game files are already compressed, and
    gzip would spend the server's CPU to make the transfer slower.

    Conversion still happens per file, before the entry is written, so
    a Switch title's .nsz arrives as the .nsp the client expects —
    exactly as it does through /download. That is also why this can't
    advertise a Content-Length: the converted sizes aren't known until
    each one is produced, and guessing would be worse than streaming
    without one.
    """
    game_id = f"{platform}/{title}"
    game = _catalog.get(game_id)
    if game is None:
        abort(404, "unknown game id")

    game_dir = _resolve_game_dir(game_id)

    # Resolved before streaming starts. Once the first byte is out the
    # status line is already sent, and a failure after that can only
    # truncate the response — so anything that can abort cleanly is
    # done here, while abort() still produces an error the client can
    # read.
    members = []
    for entry in game.files:
        source = _safe_join(game_dir, entry.filename)
        if entry.needs_conversion:
            members.append((entry.filename.replace(".nsz", ".nsp"),
                            _get_or_convert(game_id, source)))
        else:
            members.append((entry.filename, source))

    def stream():
        # A tar written to a pipe-like object: each member is handed to
        # tarfile, which writes it straight through to the buffer, and
        # the buffer is drained after each one rather than accumulating
        # the whole title in memory.
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w|") as archive:
            for name, path in members:
                archive.add(path, arcname=name, recursive=False)
                chunk = buffer.getvalue()
                if chunk:
                    yield chunk
                    buffer.seek(0)
                    buffer.truncate()
        # The closing blocks tarfile writes on exit.
        tail = buffer.getvalue()
        if tail:
            yield tail

    return Response(
        stream(),
        mimetype="application/x-tar",
        headers={"Content-Disposition": f'attachment; filename="{title}.tar"'},
    )


KEY_NAMES = ("prod.keys", "keys.txt")


def keys_file() -> Optional[Path]:
    """
    The Switch keys nsz should use, or None if there are none to find.

    nsz looks for keys relative to the HOME of whoever runs it, which
    is why decompressing by hand works while the same title fails
    through this server: the systemd unit runs as its own system user,
    whose home is not the one holding the prod.keys that works. So the
    path is resolved here, by this process, and handed to nsz
    explicitly rather than left to be discovered.

    KEYS_FILE in config.py (or NSZ_KEYS in the environment) wins, as a
    file or as a directory holding one. After that, the places someone
    would have put them anyway.
    """
    configured = str(getattr(config, "KEYS_FILE", "") or "").strip()
    candidates = []
    if configured:
        path = Path(configured).expanduser()
        candidates.extend([path / name for name in KEY_NAMES] if path.is_dir() else [path])

    home = Path.home()
    xdg = os.environ.get("XDG_CONFIG_HOME")
    for directory in (
        home / ".switch",
        Path(xdg) / "nsz" if xdg else home / ".config" / "nsz",
        Path("/etc/games-galore"),
    ):
        candidates.extend(directory / name for name in KEY_NAMES)

    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def _searched_for_keys() -> str:
    """Where to look, for an error message that ends the search."""
    configured = str(getattr(config, "KEYS_FILE", "") or "").strip()
    if configured:
        return configured
    return "~/.switch/, ~/.config/nsz/ or /etc/games-galore/"


def _conversion_error(stderr: str, keys: Optional[Path]) -> str:
    """
    nsz's own words, unless they are the ones that need translating.

    "Could not load keys file" is the failure someone will meet, and
    on its own it says nothing about why nsz can see the keys from a
    shell and not from here.
    """
    message = (stderr or "").strip()
    if "keys file" not in message.lower():
        return f"nsz conversion failed: {message}"

    if keys is None:
        return (
            "nsz could not find the Switch keys. Looked in "
            f"{_searched_for_keys()}. Set KEYS_FILE in the server's config.py "
            "(or NSZ_KEYS in its environment) to a prod.keys this server's "
            "user can read — the one in your own home directory is not "
            "readable by the account the service runs as."
        )
    return (
        f"nsz could not use the keys at {keys}: {message}. Check that the "
        "account running this server can read that file and that it is a "
        "current prod.keys."
    )


def _legacy_keys_home(keys: Path):
    """
    A HOME for an nsz too old to understand --keys.

    That version reads $HOME/.switch/prod.keys and nowhere else useful,
    so it gets a directory shaped like that, with a link to the real
    keys in it. Deleted with the context manager; nothing is copied.
    """
    directory = tempfile.TemporaryDirectory(prefix="games-galore-keys-")
    switch = Path(directory.name) / ".switch"
    switch.mkdir()
    try:
        (switch / "prod.keys").symlink_to(keys)
    except OSError:
        shutil.copy2(keys, switch / "prod.keys")
    return directory


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

    keys = keys_file()
    command = ["nsz", "-D", "--output", str(cache_dir)]
    if keys is not None:
        command += ["--keys", str(keys)]
    command.append(str(nsz_path))

    result = subprocess.run(command, capture_output=True, text=True)

    # An nsz old enough not to know --keys reads $HOME/.switch instead,
    # so it gets a HOME shaped that way rather than a hard failure over
    # an argument.
    if (
        result.returncode != 0
        and keys is not None
        and "unrecognized arguments" in (result.stderr or "")
    ):
        with _legacy_keys_home(keys) as home:
            result = subprocess.run(
                ["nsz", "-D", "--output", str(cache_dir), str(nsz_path)],
                capture_output=True,
                text=True,
                env={**os.environ, "HOME": home},
            )

    if result.returncode != 0:
        abort(500, _conversion_error(result.stderr, keys))
    if not cached_nsp.exists():
        abort(500, "nsz reported success but no .nsp file was produced")

    # Swept after the new file is in place, never before: the cache is
    # only over its cap because of what was just added, and evicting to
    # make room for a file that then fails to convert would throw away
    # work for nothing.
    _prune_cache()

    return cached_nsp


def _prune_cache(limit: int = CACHE_MAX_BYTES) -> None:
    """
    Evicts converted .nsp files, least recently used first, until the
    cache is back under `limit`.

    Nothing here is precious — every byte can be regenerated by
    decompressing the source again — but nothing ever removed it
    either, and a decompressed .nsp is roughly twice the .nsz it came
    from, so a library browsed for long enough would eventually fill
    whatever disk this server runs on.

    "Least recently used" means atime where the filesystem keeps one
    (a file served to a client was read, which is exactly the signal
    wanted) and mtime otherwise, since a mount with noatime reports a
    stale atime rather than no atime at all. Best-effort: a file that
    vanishes underneath the sweep, or refuses to be deleted, is skipped
    rather than raised — this runs on the path that is about to serve
    somebody a download.
    """
    if not CACHE_DIR.exists():
        return

    files = []
    total = 0
    for path in CACHE_DIR.rglob("*.nsp"):
        try:
            stat = path.stat()
        except OSError:
            continue
        files.append((max(stat.st_atime, stat.st_mtime), stat.st_size, path))
        total += stat.st_size

    if total <= limit:
        return

    files.sort(key=lambda entry: entry[0])  # oldest touch first
    for _, size, path in files:
        if total <= limit:
            break
        try:
            path.unlink()
        except OSError:
            continue
        total -= size

    # A game directory left holding nothing is just clutter; the next
    # conversion recreates it.
    for directory in sorted(CACHE_DIR.rglob("*"), reverse=True):
        if directory.is_dir():
            try:
                directory.rmdir()
            except OSError:
                pass


@app.route("/rescan", methods=["POST"])
def rescan_route():
    """
    Forces a rescan, for the case the signature check cannot see: a
    file edited in place deep inside a game's tree. Cheap to call and
    safe to call often — it reads the library and writes nothing.
    """
    _reload_catalog()
    return jsonify({"games": len(_catalog)})


@app.route("/metadata/<platform>/<title>", methods=["POST"])
def metadata_route(platform: str, title: str):
    """
    Fills in one game's folder from RAWG: description, cover art,
    screenshots, a trailer and a game.json of genre and tags.

    `?overwrite=1` replaces what is already there. Without it, every
    file that exists is left alone — the point being that a library
    curated by hand is not something to overwrite on someone's behalf.
    """
    _check_write_auth()

    game_id = f"{platform}/{title}"
    _ensure_catalog()
    if game_id not in _catalog:
        abort(404, "unknown game id")

    overwrite = request.args.get("overwrite", "").lower() in {"1", "true", "yes"}
    try:
        result = _fetch_metadata_for(game_id, overwrite=overwrite)
    except MetadataError as e:
        abort(502, str(e))

    # The folder changed, so the cached catalog is behind — unless the
    # caller says it is working through a list, in which case rescanning
    # the whole library once per game would cost far more than the
    # fetches do. The client's next /library call picks the changes up
    # anyway: writing into a game's folder moves its mtime, which is
    # exactly what the catalog's signature check watches.
    if request.args.get("rescan", "").lower() not in {"0", "false", "no"}:
        _reload_catalog()
    return jsonify(result.to_dict())


@app.route("/metadata", methods=["POST"])
def metadata_all_route():
    """
    The same for every game that is missing something, in one pass.

    Games that already have a description and a cover are not looked up
    at all, so a second run over a mostly-complete library costs almost
    nothing. A failure on one game is recorded and the run continues; a
    failure that will affect every game — a bad key, a rate limit —
    stops it, since there is no sense in making four hundred more
    requests that cannot work.
    """
    _check_write_auth()
    overwrite = request.args.get("overwrite", "").lower() in {"1", "true", "yes"}

    _ensure_catalog()
    results = []
    for game_id, game in sorted(_catalog.items()):
        if not overwrite and game.description and game.cover:
            continue
        try:
            results.append(_fetch_metadata_for(game_id, overwrite=overwrite).to_dict())
        except MetadataError as e:
            _reload_catalog()
            return jsonify({"results": results, "stopped": str(e)}), 502
        except Exception as e:  # noqa: BLE001 - one bad game is not the run
            results.append({"title": game.title, "error": str(e)})

    _reload_catalog()
    return jsonify({"results": results, "stopped": None})


def _fetch_metadata_for(game_id: str, *, overwrite: bool):
    game = _catalog[game_id]
    # A key sent with the request wins over the configured one: it is
    # the more recent statement of intent, and it means the whole thing
    # can be set up from the app without editing a file on this
    # machine. Taken from a header rather than the query string so it
    # stays out of access logs and browser history.
    key = request.headers.get("X-RAWG-Key", "").strip() or RAWG_API_KEY
    client = RawgClient(key, requests.Session())
    # Written where the catalog reads it from, which for a loose disc
    # is its own folder under .metadata/ rather than the platform
    # folder its file happens to sit in.
    directory = _resolve_media_dir(game_id)
    directory.mkdir(parents=True, exist_ok=True)
    return fill_game_folder(
        directory,
        game.title,
        client,
        overwrite=overwrite,
        max_screenshots=METADATA_MAX_SCREENSHOTS,
    )


@app.route("/status")
def status_route():
    nsz_path = shutil.which("nsz")
    version = None
    if nsz_path:
        result = subprocess.run(["nsz", "--version"], capture_output=True, text=True)
        version = (result.stdout or result.stderr).strip()
    keys = keys_file()
    return jsonify({
        "nsz_found": nsz_path is not None,
        "nsz_version": version,
        # Reported alongside nsz itself because they fail as a pair: an
        # nsz with no keys converts nothing, and says so only once a
        # download has already started.
        "keys_found": keys is not None,
        "keys_path": str(keys) if keys else None,
        "keys_searched": _searched_for_keys(),
        "library_roots": _library_root_status(),
    })


def _library_root_status() -> list:
    """
    Each library drive, whether it is there, and how much room is left
    on it.

    Reported by the machine that actually has the disks: the client
    can then say "the library server is nearly full" in the one place
    someone would look, rather than that being something you find out
    by sshing in.
    """
    _ensure_catalog()
    status = []
    for root in library_roots():
        exists = root.is_dir()
        free = None
        if exists:
            try:
                free = shutil.disk_usage(root).free
            except OSError:
                free = None
        status.append({
            "path": str(root),
            "exists": exists,
            "free_bytes": free,
            "free_text": _human_bytes(free) if free is not None else None,
            "games": sum(1 for directory in _game_dirs.values()
                         if directory.parent.parent == root),
        })
    return status


def _human_bytes(count: int) -> str:
    """Two significant figures and the unit a person would use."""
    units = ["KB", "MB", "GB", "TB"]
    if count < 1024:
        return f"{count} bytes"
    value = float(count)
    unit = units[0]
    for unit in units:
        value /= 1024
        if value < 1024:
            break
    return f"{value:.0f} {unit}" if value >= 10 else f"{value:.1f} {unit}"


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


def _check_write_auth() -> None:
    """
    Guards every endpoint that writes something, when a token is
    configured.

    That is the saves — the only data here that is personal rather than
    a copy of what is already on the drive, which is why the guard
    covers reading them as well — and the metadata fetcher, which
    writes into the library itself. The catalog, the media and the game
    files stay open: they are the browsing surface this whole tool
    exists to expose on a LAN.

    Compared in constant time, which costs nothing and removes the
    question of whether a timing difference could be measured across a
    network.
    """
    if not SAVE_TOKEN:
        return
    presented = request.headers.get("Authorization", "")
    if not hmac.compare_digest(presented, f"Bearer {SAVE_TOKEN}"):
        abort(401, "a valid save token is required")


@app.route("/saves/<platform>/<title>")
def save_list_route(platform: str, title: str):
    """
    Every stored version for this game, newest first. The client
    compares the newest entry against what's on its own disk to decide
    whether it is behind, ahead, or in conflict.
    """
    _check_write_auth()
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
    _check_write_auth()
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
    _check_write_auth()
    directory = _save_dir(platform, title)
    path = _safe_join(directory, f"{_safe_segment(version)}.tar.gz")
    return send_file(path, conditional=True, as_attachment=True)


def _prune_versions(directory: Path) -> None:
    for stale in _read_versions(directory)[SAVE_VERSIONS_KEPT:]:
        (directory / f"{stale['version']}.tar.gz").unlink(missing_ok=True)
        (directory / f"{stale['version']}.json").unlink(missing_ok=True)


def _resolve_game_dir(game_id: str) -> Path:
    # Recorded when the catalog was built rather than recomputed here:
    # with several drives, which one a title came from is a fact from
    # the scan, and re-deriving it would have to guess the same order
    # again.
    directory = _game_dirs.get(game_id)
    if directory is None:
        abort(404, "unknown game id")
    return directory

def _resolve_media_dir(game_id: str) -> Path:
    """
    Where a game's cover, screenshots, README and game.json live.

    The game's own directory for a title that has one; a folder
    under `.metadata/` for a disc sitting loose in a platform
    folder, since that folder belongs to every loose title in it and
    writing one game's cover into it would be writing into all of
    them.
    """
    directory = _media_dirs.get(game_id)
    if directory is None:
        abort(404, "unknown game id")
    return directory


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
    migrate_legacy_state()
    _reload_catalog()
    app.run(host=HOST, port=PORT)
