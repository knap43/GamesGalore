"""
Fixture tests for the library scanner and the HTTP routes.

Builds real game folders in a temporary directory and scans them, then
runs the Flask app against that library — including a simulated install
that fetches every file a title's catalog entry lists and compares the
reconstructed tree byte-for-byte with the source.

Run from this directory, with flask installed:

    python3 test_server.py
"""

from __future__ import annotations

import json
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path
from urllib.parse import quote

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

failures = 0


def check(label: str, got, want) -> None:
    global failures
    if got == want:
        print(f"PASS  {label}")
        return
    failures += 1
    print(f"FAIL  {label}\n        got={got!r}\n       want={want!r}")


def build_library(root: Path) -> None:
    def write(rel: str, size: int = 0, text: str | None = None) -> None:
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if text is not None:
            path.write_text(text)
        else:
            # Content derived from the path, so a file landing in the
            # wrong place is detectable rather than passing because
            # every fixture file is identical zeros.
            path.write_bytes((rel.encode() * size)[:size] if size else b"")

    # A PC game as they actually come: executable in a subdirectory,
    # data in sibling trees, an uninstaller and a bundled runtime that
    # must not be mistaken for the game.
    write("PC/Hollow Meridian/README.md", text="Hollow Meridian (2021)\n\nA game.")
    write("PC/Hollow Meridian/cover.png", 100)
    write("PC/Hollow Meridian/hm_trailer.mp4", 500)
    write("PC/Hollow Meridian/unins000.exe", 900_000)
    write("PC/Hollow Meridian/bin/HollowMeridian.exe", 40_000)
    write("PC/Hollow Meridian/bin/steam_api64.dll", 2_000)
    write("PC/Hollow Meridian/data/pak01.vpk", 9_000_000)
    write("PC/Hollow Meridian/data/art/logo.png", 3_000)
    write("PC/Hollow Meridian/redist/vcredist_x64.exe", 8_000_000)

    # Spaces in both the title and a subdirectory name.
    write("PC/Moth & Ember/Launch.exe", 1_000)
    write("PC/Moth & Ember/game data/assets.pak", 5_000)

    write("PS1/Static Choir/Static Choir.cue", 300)
    write("PS1/Static Choir/Static Choir.bin", 600_000)

    write("Switch/198X/base.nsp", 1_000)
    write("Switch/198X/update.nsz", 2_000)


def encode_segments(value: str) -> str:
    """Mirrors encode_path_segments in the client's install_state.rs."""
    return "/".join(quote(part, safe="") for part in value.split("/"))


def main() -> int:
    from library import scan_library

    tmp = Path(tempfile.mkdtemp())
    root = tmp / "library"
    build_library(root)

    games = {g.id: g for g in scan_library(root)}

    print("--- catalog ---")
    hm = games["PC/Hollow Meridian"]
    names = [f.filename for f in hm.files]
    expected = [
        "bin/HollowMeridian.exe",
        "bin/steam_api64.dll",
        "data/art/logo.png",
        "data/pak01.vpk",
        "redist/vcredist_x64.exe",
        "unins000.exe",
    ]
    check("PC: lists every game file, not just the entry point", sorted(names), expected)
    check("PC: entry point listed first", names[0], "bin/HollowMeridian.exe")
    check("PC: picks the game over the uninstaller and the redist",
          names[0], "bin/HollowMeridian.exe")
    check("PC: per-file sizes sum to the real footprint",
          sum(f.size_bytes for f in hm.files), 17_945_000)
    check("PC: README, cover and trailer are not game files",
          [n for n in names if n in ("README.md", "cover.png", "hm_trailer.mp4")], [])
    check("PC: a nested image is a game asset and counts",
          next(f.size_bytes for f in hm.files if f.filename == "data/art/logo.png"), 3_000)

    ps1 = games["PS1/Static Choir"]
    check("PS1: lists the .bin as well as the .cue",
          sorted(f.filename for f in ps1.files),
          ["Static Choir.bin", "Static Choir.cue"])
    check("PS1: .cue listed first", ps1.files[0].filename, "Static Choir.cue")

    switch = games["Switch/198X"]
    check("Switch: both files, conversion flagged per file",
          sorted((f.filename, f.size_bytes, f.needs_conversion) for f in switch.files),
          [("base.nsp", 1000, False), ("update.nsz", 2000, True)])

    print("\n--- routes and install ---")
    import config

    config.LIBRARY_ROOTS = [root]
    config.LIBRARY_ROOT = root
    config.CACHE_DIR = tmp / "cache"
    import server as srv

    srv.CACHE_DIR = config.CACHE_DIR
    srv._reload_catalog()
    srv.app.config["SERVER_NAME"] = "testhost"
    client = srv.app.test_client()

    install_root = tmp / "installs"

    def simulate_install(game):
        """Mirrors install_game: fetch every listed file, recreating dirs."""
        dest = install_root / game.platform / game.title
        dest.mkdir(parents=True, exist_ok=True)
        total = sum(f.size_bytes for f in game.files)
        done = 0
        pcts = []
        for entry in game.files:
            url = (
                f"/download/{encode_segments(game.platform)}"
                f"/{encode_segments(game.title)}"
                f"/{encode_segments(entry.filename)}"
            )
            response = client.get(url)
            if response.status_code != 200:
                return dest, f"{entry.filename} -> HTTP {response.status_code}", pcts
            out = dest / entry.filename
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(response.data)
            done += len(response.data)
            pcts.append(min(100, done * 100 // total) if total else 0)
        return dest, None, pcts

    dest, err, pcts = simulate_install(hm)
    check("PC: every listed file downloads", err, None)
    installed = sorted(p.relative_to(dest).as_posix() for p in dest.rglob("*") if p.is_file())
    check("PC: installed tree matches the catalog exactly", installed, expected)
    check("PC: nested directories recreated", (dest / "data/art/logo.png").is_file(), True)
    source = root / "PC/Hollow Meridian"
    check("PC: every file byte-identical to the source",
          all((dest / n).read_bytes() == (source / n).read_bytes() for n in names), True)
    check("PC: progress rises monotonically to 100",
          pcts == sorted(pcts) and pcts[-1] == 100, True)
    check("PC: progress spans the title, so the first file isn't 100%",
          pcts[0] < 100, True)

    dest, err, _ = simulate_install(games["PC/Moth & Ember"])
    check("PC: spaces in title and subdirectory round-trip", err, None)
    check("PC: nested file with a space in its directory present",
          (dest / "game data/assets.pak").is_file(), True)

    dest, err, _ = simulate_install(ps1)
    check("PS1: the .bin actually downloads now", err, None)
    check("PS1: both files on disk",
          sorted(p.name for p in dest.iterdir()),
          ["Static Choir.bin", "Static Choir.cue"])

    pc_dest = install_root / "PC/Hollow Meridian"
    check("PC: all three executables land, so the launcher must discriminate",
          sorted(p.relative_to(pc_dest).as_posix() for p in pc_dest.rglob("*.exe")),
          ["bin/HollowMeridian.exe", "redist/vcredist_x64.exe", "unins000.exe"])

    print("\n--- cloud saves ---")
    srv.SAVE_ROOT = tmp / "saves"
    srv.SAVE_VERSIONS_KEPT = 3

    def blob(n):
        return b"SAVEDATA" * n

    r = client.post("/saves/Switch/198X?device=laptop&saved_at=2026-09-17T10:00:00Z",
                    data=blob(10), content_type="application/octet-stream")
    check("upload accepted", r.status_code, 200)
    first = r.get_json()
    check("upload records size", first["size_bytes"], len(blob(10)))
    check("upload records the device", first["device"], "laptop")
    check("upload records the client's saved_at", first["saved_at"], "2026-09-17T10:00:00Z")

    listing = client.get("/saves/Switch/198X").get_json()["versions"]
    check("the version is listed", [v["version"] for v in listing], [first["version"]])

    r = client.get(f"/saves/Switch/198X/{first['version']}")
    check("download returns the exact bytes", r.data, blob(10))

    r = client.post("/saves/Switch/198X", data=blob(10),
                    content_type="application/octet-stream")
    check("an identical re-upload is not a new version", r.get_json().get("unchanged"), True)
    check("...and does not grow the history",
          len(client.get("/saves/Switch/198X").get_json()["versions"]), 1)

    import time
    for n in (11, 12, 13):
        time.sleep(1.05)  # versions are second-resolution; keep them distinct
        client.post("/saves/Switch/198X", data=blob(n), content_type="application/octet-stream")
    listing = client.get("/saves/Switch/198X").get_json()["versions"]
    check("retention caps the stored versions", len(listing), 3)
    check("newest is first", listing[0]["size_bytes"], len(blob(13)))
    check("the pruned archive is gone from disk",
          len(list((srv.SAVE_ROOT / "Switch" / "198X").glob("*.tar.gz"))), 3)

    check("a game with no saves lists nothing",
          client.get("/saves/PC/Nothing Saved").get_json()["versions"], [])
    check("an empty upload is refused",
          client.post("/saves/Switch/198X", data=b"").status_code, 400)
    check("an unknown version 404s",
          client.get("/saves/Switch/198X/nope").status_code, 404)
    # Percent-encoded, because Werkzeug normalises a literal "/../" out
    # of the path before routing ever sees it — the encoded form is what
    # actually arrives at _safe_segment as a segment to validate.
    check("an encoded dot-dot segment is refused",
          [client.post("/saves/%2e%2e/198X", data=b"x").status_code,
           client.get("/saves/%2e%2e/198X").status_code], [400, 400])
    check("an encoded dot segment is refused",
          client.get("/saves/%2e/198X").status_code, 400)
    check("a dot segment is refused", client.get("/saves/./198X").status_code, 400)
    check("saves for two platforms do not collide",
          (client.post("/saves/PC/198X", data=blob(1),
                       content_type="application/octet-stream").status_code,
           len(client.get("/saves/PC/198X").get_json()["versions"])), (200, 1))
    # Only the two real platform directories exist — no refused request
    # managed to create anything of its own along the way.
    check("nothing escaped SAVE_ROOT",
          sorted(d.name for d in srv.SAVE_ROOT.iterdir()), ["PC", "Switch"])

    print("\n--- metadata fetcher ---")
    # Driven against recorded responses: the tests stay offline, fast,
    # and out of anybody's rate limit.
    import metadata as md

    class FakeResponse:
        def __init__(self, payload=None, blob=None, status=200):
            self._payload = payload
            self._blob = blob or b""
            self.status_code = status

        def json(self):
            return self._payload

        def iter_content(self, chunk_size=8192):
            yield self._blob

        def raise_for_status(self):
            if self.status_code >= 400:
                raise RuntimeError(f"HTTP {self.status_code}")

    class FakeSession:
        """Answers the four RAWG endpoints and any image URL."""

        def __init__(self, status=200):
            self.calls = []
            self.status = status

        def get(self, url, params=None, timeout=None, stream=False):
            self.calls.append(url)
            if self.status != 200:
                return FakeResponse(status=self.status)
            if url.endswith("/games") and (params or {}).get("search"):
                return FakeResponse({"results": [
                    {"id": 7, "name": "Hollow Meridian II", "released": "2024-01-01",
                     "background_image": "https://img/other.jpg"},
                    {"id": 1, "name": "Hollow Meridian", "released": "2021-06-04",
                     "background_image": "https://img/cover.jpg"},
                ]})
            if re.search(r"/games/\d+$", url):
                return FakeResponse({
                    "description_raw": "A long dark corridor of a game.",
                    "released": "2021-06-04",
                    "genres": [{"name": "RPG"}, {"name": "Indie"}],
                    "tags": [{"name": "Singleplayer"}, {"name": "Atmospheric"}],
                })
            if url.endswith("/screenshots"):
                return FakeResponse({"results": [
                    {"image": "https://img/s1.jpg"}, {"image": "https://img/s2.jpg"},
                ]})
            if url.endswith("/movies"):
                return FakeResponse({"results": [
                    {"data": {"max": "https://img/trailer.mp4"}},
                ]})
            return FakeResponse(blob=b"BINARY")  # an image or a video

    # The search picks the exact name, not merely the first result.
    session = FakeSession()
    rawg = md.RawgClient("test-key", session)
    check("the closest name wins over the first result",
          rawg.search("Hollow Meridian")["id"], 1)
    check("punctuation and case don't decide it",
          md.best_match("hollow-meridian!", [{"name": "Hollow Meridian", "id": 1}])["id"], 1)
    check("nothing found is not a match", md.best_match("x", []), None)

    fetch_dir = tmp / "fetch" / "Hollow Meridian"
    fetch_dir.mkdir(parents=True)
    result = md.fill_game_folder(fetch_dir, "Hollow Meridian", rawg, max_screenshots=2)

    check("it matched the right game", result.matched, "Hollow Meridian")
    check("...and wrote the whole set", sorted(result.wrote),
          ["README.md", "cover.jpg", "game.json", "screenshot-01.jpg",
           "screenshot-02.jpg", "trailer.mp4"])
    check("the README is the shape the scanner parses",
          (fetch_dir / "README.md").read_text().splitlines()[0],
          "# Hollow Meridian (2021)")
    check("...with the description under it",
          "A long dark corridor" in (fetch_dir / "README.md").read_text(), True)
    check("the sidecar carries the genre and tags",
          json.loads((fetch_dir / "game.json").read_text()),
          {"genre": "RPG", "tags": ["Singleplayer", "Atmospheric"], "release_year": 2021})
    check("no .part files are left behind",
          list(fetch_dir.glob("*.part")), [])

    # And the scanner reads back exactly what was written.
    fetched = md_scan = None
    from library import _read_game_folder
    fetched = _read_game_folder(fetch_dir, "PC")
    check("the scanner sees the year", fetched.release_year, 2021)
    check("...the description", fetched.description.startswith("A long dark"), True)
    check("...the cover", fetched.cover, "cover.jpg")
    check("...every screenshot", len(fetched.screenshots), 3)
    check("...the trailer", fetched.trailer, "trailer.mp4")
    check("...and the genre from the sidecar", fetched.genre, "RPG")

    # A second run leaves a curated folder alone.
    again = md.fill_game_folder(fetch_dir, "Hollow Meridian", rawg, max_screenshots=2)
    check("a second pass writes nothing", again.wrote, [])
    check("...and says what it skipped", sorted(again.skipped),
          ["README.md", "cover.jpg", "game.json", "screenshots", "trailer.mp4"])

    (fetch_dir / "README.md").write_text("# Mine (1999)\n\nHand-written.\n")
    md.fill_game_folder(fetch_dir, "Hollow Meridian", rawg, max_screenshots=2)
    check("a hand-written README is never overwritten",
          (fetch_dir / "README.md").read_text().startswith("# Mine"), True)
    forced = md.fill_game_folder(fetch_dir, "Hollow Meridian", rawg,
                                 overwrite=True, max_screenshots=2)
    check("...unless overwrite says so",
          (fetch_dir / "README.md").read_text().startswith("# Hollow Meridian"), True)
    check("...which rewrites everything", len(forced.wrote), 6)

    # Failures worth naming rather than a stack trace.
    for status, expected in ((401, "rejected the API key"), (429, "rate limit")):
        try:
            md.RawgClient("test-key", FakeSession(status=status)).search("x")
            check(f"a {status} is reported", "no error raised", expected)
        except md.MetadataError as e:
            check(f"a {status} is reported plainly", expected in str(e), True)
    try:
        md.RawgClient("", FakeSession())
        check("a missing key is refused", "no error raised", "an error")
    except md.MetadataError as e:
        check("a missing key is refused before any request",
              "no RAWG API key" in str(e), True)

    print("\n--- metadata endpoints ---")
    # The routes, with the network stubbed the same way.
    srv.requests = type("FakeRequests", (), {"Session": lambda self=None: FakeSession()})()
    srv.RAWG_API_KEY = "test-key"

    bare = root / "PS1" / "Bare Title"
    bare.mkdir()
    (bare / "bare.cue").write_bytes(b"CUE")
    srv._reload_catalog()

    r = client.post("/metadata/PS1/Bare Title")
    check("one game can be filled in on request", r.status_code, 200)
    check("...reporting what it wrote", sorted(r.get_json()["wrote"]),
          ["README.md", "cover.jpg", "game.json", "screenshot-01.jpg",
           "screenshot-02.jpg", "trailer.mp4"])
    check("...and the catalog reflects it immediately",
          srv._catalog["PS1/Bare Title"].cover, "cover.jpg")
    check("an unknown game is refused",
          client.post("/metadata/PS1/Nothing Here").status_code, 404)

    # The library-wide pass only touches what is incomplete.
    everything = client.post("/metadata")
    check("a library-wide pass is accepted", everything.status_code, 200)
    filled = {r["title"] for r in everything.get_json()["results"]}
    check("...and skips a game that already has both", "Bare Title" in filled, False)

    srv.SAVE_TOKEN = "s3cret-token"
    check("fetching is behind the write token",
          client.post("/metadata/PS1/Bare Title").status_code, 401)
    check("...and passes with it",
          client.post("/metadata/PS1/Bare Title",
                      headers={"Authorization": "Bearer s3cret-token"}).status_code, 200)
    srv.SAVE_TOKEN = ""

    srv.RAWG_API_KEY = ""
    check("no key configured is a clear 502, not a crash",
          client.post("/metadata/PS1/Bare Title").status_code, 502)
    check("...but a key sent with the request is enough on its own",
          client.post("/metadata/PS1/Bare Title?overwrite=1",
                      headers={"X-RAWG-Key": "from-the-app"}).status_code, 200)
    srv.RAWG_API_KEY = "test-key"

    # Working through a list, the caller asks for the rescan to be
    # skipped: one per game would cost more than the fetching does.
    scans_before = scans["count"] if "scans" in dir() else None
    real_reload = srv._reload_catalog
    reloads = {"count": 0}
    srv._reload_catalog = lambda: (reloads.__setitem__("count", reloads["count"] + 1),
                                   real_reload())[1]
    client.post("/metadata/PS1/Bare Title?overwrite=1")
    check("a single fetch rescans afterwards", reloads["count"], 1)
    client.post("/metadata/PS1/Bare Title?overwrite=1&rescan=0")
    check("...and a bulk one does not", reloads["count"], 1)
    srv._reload_catalog = real_reload

    shutil.rmtree(bare)
    srv._reload_catalog()

    print("\n--- catalog caching ---")
    # /library used to rescan the whole tree on every call. It now
    # rescans when the tree looks different, and not otherwise.
    scans = {"count": 0}
    real_scan = srv.scan_library

    def counting_scan(path):
        scans["count"] += 1
        return real_scan(path)

    srv.scan_library = counting_scan
    srv._reload_catalog()          # prime it, and count that one
    before = scans["count"]

    client.get("/library")
    client.get("/library")
    check("repeat calls do not rescan", scans["count"], before)

    # A new game appears: the directory holding it changes, which is
    # what the signature is watching for.
    new_game = root / "PS1" / "Late Arrival"
    new_game.mkdir()
    (new_game / "late.cue").write_bytes(b"FILE")
    with srv.app.test_request_context():
        listing = json.loads(client.get("/library").data)
    check("a new game triggers a rescan", scans["count"], before + 1)
    check("...and is in the catalog",
          any(g["id"] == "PS1/Late Arrival" for g in listing), True)

    client.get("/library")
    check("and then it settles again", scans["count"], before + 1)

    # An edit deep inside a tree is what the signature cannot see, so
    # there is an explicit way to say so.
    forced = client.post("/rescan")
    check("a forced rescan is accepted", forced.status_code, 200)
    check("...and reports the catalog size",
          forced.get_json()["games"], len(srv._catalog))
    check("...having actually rescanned", scans["count"], before + 2)

    shutil.rmtree(new_game)
    srv._reload_catalog()
    srv.scan_library = real_scan

    print("\n--- metadata sidecar ---")
    # Everything else about a game is inferred from its folder; this is
    # the one place to state something outright.
    sidecar_dir = root / "PC" / "Hollow Meridian"
    (sidecar_dir / "game.json").write_text(json.dumps({
        "genre": "RPG",
        "tags": ["singleplayer", "moody"],
        "players": 1,
        "release_year": 2019,
        "description": "A stated description, not a parsed one.",
    }))
    srv._reload_catalog()
    with srv.app.test_request_context():
        listing = json.loads(client.get("/library").data)
    hm = next(g for g in listing if g["id"] == "PC/Hollow Meridian")
    check("the sidecar's genre reaches the catalog", hm["genre"], "RPG")
    check("...its tags too", hm["tags"], ["singleplayer", "moody"])
    check("...its player count", hm["players"], 1)
    check("...and it overrides what the folder implied",
          (hm["release_year"], hm["description"]),
          (2019, "A stated description, not a parsed one."))
    check("the sidecar is not served as part of the game",
          any(f["filename"] == "game.json" for f in hm["files"]), False)

    # The shapes a game.json plausibly arrives in. RAWG's own JSON uses
    # objects with a `name`, so anybody copying from it by hand writes
    # that — and a file that looks right and is silently ignored is the
    # worst of both worlds.
    from library import _read_sidecar

    shapes = {
        "the fetcher's own": {"genre": "RPG", "tags": ["Open World"]},
        "RAWG's, copied":    {"genres": [{"name": "RPG"}], "tags": [{"name": "Open World"}]},
        "plain lists":       {"genres": ["RPG"], "tags": ["Open World"]},
    }
    probe = root / "PC" / "Hollow Meridian" / "game.json"
    for label, shape in shapes.items():
        probe.write_text(json.dumps(shape))
        parsed = _read_sidecar(probe)
        check(f"a sidecar in {label} shape is read",
              (parsed.get("genre"), parsed.get("tags")), ("RPG", ["Open World"]))
    probe.write_text(json.dumps({"genre": 42, "release_year": "2011", "players": True}))
    check("...and values of the wrong type are ignored rather than fatal",
          _read_sidecar(probe), {})

    (sidecar_dir / "game.json").write_text("{ this is not json,,, }")
    srv._reload_catalog()
    with srv.app.test_request_context():
        listing = json.loads(client.get("/library").data)
    hm = next(g for g in listing if g["id"] == "PC/Hollow Meridian")
    check("a malformed sidecar is ignored rather than fatal", hm["genre"], None)
    check("...and the game is still listed with its own facts",
          hm["title"], "Hollow Meridian")

    (sidecar_dir / "game.json").unlink()
    srv._reload_catalog()

    print("\n--- whole-title archive ---")
    # A PC game is thousands of files, and one request per file spends
    # more on connections than on bytes.
    import io, tarfile
    r = client.get("/archive/PC/Hollow Meridian")
    check("the archive is served", r.status_code, 200)
    check("...as a tar", r.mimetype, "application/x-tar")

    with tarfile.open(fileobj=io.BytesIO(r.data), mode="r:") as archive:
        names = sorted(archive.getnames())
        contents = {n: archive.extractfile(n).read() for n in names}

    with srv.app.test_request_context():
        listing = json.loads(client.get("/library").data)
    catalog_names = sorted(
        f["filename"]
        for f in next(g for g in listing if g["id"] == "PC/Hollow Meridian")["files"]
    )
    check("it carries exactly the files the catalog lists", names, catalog_names)
    for name in names:
        source = (root / "PC" / "Hollow Meridian" / name).read_bytes()
        check(f"...and {name} byte-for-byte", contents[name], source)

    check("an unknown title is refused", client.get("/archive/PC/Nothing/").status_code, 404)

    print("\n--- resumable downloads ---")
    # An interrupted install continues each file from what is already
    # on disk, which only works if the server honours a Range request.
    whole = client.get("/download/PC/Hollow Meridian/bin/HollowMeridian.exe")
    partial = client.get("/download/PC/Hollow Meridian/bin/HollowMeridian.exe",
                         headers={"Range": "bytes=4-"})
    check("a range request is answered as one", partial.status_code, 206)
    check("...returning exactly the remainder", partial.data, whole.data[4:])
    check("...and saying where it starts",
          partial.headers.get("Content-Range"),
          f"bytes 4-{len(whole.data) - 1}/{len(whole.data)}")
    check("a range past the end is refused rather than answered with nothing",
          client.get("/download/PC/Hollow Meridian/bin/HollowMeridian.exe",
                     headers={"Range": f"bytes={len(whole.data) + 10}-"}).status_code, 416)

    print("\n--- save endpoint auth ---")
    # The save endpoints are the only writable surface here, and the
    # only one carrying data that isn't just a copy of what's already
    # on the drive.
    srv.SAVE_TOKEN = "s3cret-token"
    check("an unauthenticated upload is refused",
          client.post("/saves/Switch/198X", data=blob(1),
                      content_type="application/octet-stream").status_code, 401)
    check("an unauthenticated listing is refused too",
          client.get("/saves/Switch/198X").status_code, 401)
    check("a wrong token is refused",
          client.get("/saves/Switch/198X",
                     headers={"Authorization": "Bearer wrong"}).status_code, 401)
    check("a bare token without the scheme is refused",
          client.get("/saves/Switch/198X",
                     headers={"Authorization": "s3cret-token"}).status_code, 401)

    auth = {"Authorization": "Bearer s3cret-token"}
    check("the right token is accepted",
          client.get("/saves/Switch/198X", headers=auth).status_code, 200)
    stored = client.post("/saves/Switch/198X?device=laptop&saved_at=2026-09-18T10:00:00Z",
                         data=blob(4), content_type="application/octet-stream",
                         headers=auth)
    check("...for an upload as well", stored.status_code, 200)
    check("...and for fetching one back",
          client.get(f"/saves/Switch/198X/{stored.get_json()['version']}",
                     headers=auth).status_code, 200)

    # The catalog and the game files are the whole point of the server
    # on a LAN, and stay open whatever the saves require.
    check("the catalog is not behind the token",
          client.get("/library").status_code, 200)
    check("neither is a game file",
          client.get("/download/PC/Hollow Meridian/bin/HollowMeridian.exe").status_code, 200)

    srv.SAVE_TOKEN = ""
    check("with no token configured the saves are open again",
          client.get("/saves/Switch/198X").status_code, 200)

    print("\n--- conversion cache ---")
    # Everything in the cache can be regenerated by decompressing the
    # source again, but nothing ever removed it, and a decompressed
    # .nsp is roughly twice its .nsz — a library browsed long enough
    # would fill the disk.
    cache = tmp / "prune-cache"
    srv.CACHE_DIR = cache
    (cache / "Switch" / "Old").mkdir(parents=True)
    (cache / "Switch" / "New").mkdir(parents=True)
    old_nsp = cache / "Switch" / "Old" / "old.nsp"
    new_nsp = cache / "Switch" / "New" / "new.nsp"
    old_nsp.write_bytes(b"x" * 400)
    new_nsp.write_bytes(b"y" * 400)
    # The one touched longest ago is the one that goes.
    os.utime(old_nsp, (1_000_000, 1_000_000))
    os.utime(new_nsp, (2_000_000, 2_000_000))
    # Not a conversion artefact, and not the sweep's business.
    keep_me = cache / "notes.txt"
    keep_me.write_text("not a conversion")

    srv._prune_cache(limit=500)
    check("the least recently used conversion is evicted", old_nsp.exists(), False)
    check("the recent one is kept", new_nsp.exists(), True)
    check("a non-.nsp file is left alone", keep_me.exists(), True)
    check("the emptied game directory goes too",
          (cache / "Switch" / "Old").exists(), False)

    srv._prune_cache(limit=10_000)
    check("a cache under its cap is untouched", new_nsp.exists(), True)

    print("\n--- a library across two drives ---")
    second = tmp / "library-2"
    write_second = second / "PS2" / "Cobalt Drift"
    write_second.mkdir(parents=True)
    (write_second / "Cobalt Drift.iso").write_bytes(b"iso" * 100)
    (write_second / "README.md").write_text("Cobalt Drift (2004)\n\nOn the other drive.")

    config.LIBRARY_ROOTS = [root, second]
    srv._reload_catalog()
    check("a game on the second drive is in the catalog",
          "PS2/Cobalt Drift" in srv._catalog, True)
    check("...and the first drive's games are still there",
          "PC/Hollow Meridian" in srv._catalog, True)
    check("...and it is served from the drive it is on",
          client.get("/download/PS2/Cobalt%20Drift/Cobalt%20Drift.iso").status_code, 200)
    check("...with the right bytes", len(client.get(
        "/download/PS2/Cobalt%20Drift/Cobalt%20Drift.iso").data), 300)

    # The same title on both drives: the first one listed wins, which
    # is what makes copying a game across while the server runs safe.
    duplicate = second / "PC" / "Hollow Meridian"
    duplicate.mkdir(parents=True)
    (duplicate / "HollowMeridian.exe").write_bytes(b"x" * 10)
    srv._reload_catalog()
    check("a title on two drives is taken from the first",
          srv._game_dirs["PC/Hollow Meridian"], root / "PC" / "Hollow Meridian")
    check("...so its files are still the established ones",
          client.get("/download/PC/Hollow%20Meridian/bin/HollowMeridian.exe").status_code, 200)

    # A drive that isn't mounted is skipped rather than emptying the
    # catalog of the drives that are.
    config.LIBRARY_ROOTS = [root, tmp / "not-mounted"]
    srv._reload_catalog()
    check("an absent drive doesn't take the others with it",
          "PC/Hollow Meridian" in srv._catalog, True)
    check("...and its games are simply not listed",
          "PS2/Cobalt Drift" in srv._catalog, False)

    config.LIBRARY_ROOTS = [tmp / "not-mounted"]
    try:
        srv._reload_catalog()
        check("every drive missing is still an error", "no error", "FileNotFoundError")
    except FileNotFoundError:
        check("every drive missing is still an error", True, True)

    config.LIBRARY_ROOTS = [root, second]
    srv._reload_catalog()
    status = json.loads(client.get("/status").data)
    check("status reports each library drive",
          [r["path"] for r in status["library_roots"]], [str(root), str(second)])
    check("...with the room left on it",
          all(r["free_bytes"] > 0 for r in status["library_roots"]), True)
    check("...and how many games each holds",
          [r["games"] > 0 for r in status["library_roots"]], [True, True])

    # The metadata fetcher writes into the folder the game is actually
    # in, which on a second drive is a different drive entirely.
    far = second / "PS1" / "Distant Signal"
    far.mkdir(parents=True)
    (far / "Distant Signal.cue").write_bytes(b"CUE")
    srv._reload_catalog()

    r = client.post("/metadata/PS1/Distant%20Signal")
    check("a game on the second drive can be filled in", r.status_code, 200)
    check("...and its files land on that drive",
          sorted(f.name for f in far.iterdir() if f.name != "Distant Signal.cue"),
          ["README.md", "cover.jpg", "game.json", "screenshot-01.jpg",
           "screenshot-02.jpg", "trailer.mp4"])
    check("...rather than on the first one",
          (root / "PS1" / "Distant Signal").exists(), False)
    check("...and the catalog picks the description up from there",
          bool(srv._catalog["PS1/Distant Signal"].description), True)

    # The library-wide pass covers every drive, not just the first.
    far_two = second / "PS1" / "Second Signal"
    far_two.mkdir(parents=True)
    (far_two / "Second Signal.cue").write_bytes(b"CUE")
    srv._reload_catalog()
    filled = {entry["title"] for entry in client.post("/metadata").get_json()["results"]}
    check("a library-wide fetch reaches the second drive",
          "Second Signal" in filled, True)
    check("...writing there too", (far_two / "cover.jpg").exists(), True)

    shutil.rmtree(second, ignore_errors=True)
    config.LIBRARY_ROOTS = [root]
    srv._reload_catalog()

    print("\n--- switch keys ---")
    # The failure this exists for: nsz finds keys relative to the HOME
    # of whoever runs it, and the service runs as its own user, so the
    # prod.keys that works from a shell is invisible here. The path is
    # resolved by this process and handed over explicitly.
    keys_dir = tmp / "keys"
    keys_dir.mkdir()
    real_keys = keys_dir / "prod.keys"
    real_keys.write_text("header_key = 00\n")

    config.KEYS_FILE = str(real_keys)
    check("a configured keys file is found", srv.keys_file(), real_keys)
    config.KEYS_FILE = str(keys_dir)
    check("...as is a directory holding one", srv.keys_file(), real_keys)
    config.KEYS_FILE = str(tmp / "nowhere" / "prod.keys")
    check("a configured path that isn't there finds nothing",
          srv.keys_file(), None)

    status = json.loads(client.get("/status").data)
    check("status says the keys are missing", status["keys_found"], False)
    check("...and where it looked", status["keys_searched"],
          str(tmp / "nowhere" / "prod.keys"))
    config.KEYS_FILE = str(real_keys)
    status = json.loads(client.get("/status").data)
    check("status says when they are there", status["keys_found"], True)
    check("...and which file it will use", status["keys_path"], str(real_keys))

    # nsz's own message for this says nothing about why the keys are
    # invisible from here, so it is translated into the fix.
    config.KEYS_FILE = ""
    plain = srv._conversion_error("Exception: Could not load keys file.", None)
    check("a missing-keys failure names the setting that fixes it",
          "KEYS_FILE" in plain and "NSZ_KEYS" in plain, True)
    check("...and says whose home it is not",
          "readable" in plain, True)
    with_keys = srv._conversion_error("Could not load keys file.", real_keys)
    check("a keys file that exists but doesn't work names the file",
          str(real_keys) in with_keys, True)
    other = srv._conversion_error("Traceback: something else broke", None)
    check("any other failure is passed through as nsz said it",
          other, "nsz conversion failed: Traceback: something else broke")

    # The conversion itself, against a stand-in nsz: the real one needs
    # real keys and a real .nsz, and what is worth testing here is that
    # the keys reach it.
    fake_bin = tmp / "bin"
    fake_bin.mkdir()
    (fake_bin / "nsz").write_text(
        "#!/bin/sh\n"
        'echo "$@" > "$0.args"\n'
        # Mimic nsz: write the .nsp the server expects to find.
        'out=""; next=""\n'
        'for arg in "$@"; do\n'
        '  if [ "$next" = "1" ]; then out="$arg"; next=""; fi\n'
        '  if [ "$arg" = "--output" ]; then next=1; fi\n'
        'done\n'
        'mkdir -p "$out"\n'
        'printf NSP > "$out/update.nsp"\n'
    )
    (fake_bin / "nsz").chmod(0o755)
    os.environ["PATH"] = f"{fake_bin}{os.pathsep}{os.environ['PATH']}"
    config.KEYS_FILE = str(real_keys)
    shutil.rmtree(srv.CACHE_DIR, ignore_errors=True)

    converted = client.get("/download/Switch/198X/update.nsz")
    check("a .nsz converts and is served", converted.status_code, 200)
    passed = (fake_bin / "nsz.args").read_text()
    check("...with the keys handed to nsz rather than left to be found",
          f"--keys {real_keys}" in passed, True)

    # An nsz too old to know --keys: it refuses the argument, and the
    # retry gives it a HOME shaped the way that version reads.
    (fake_bin / "nsz").write_text(
        "#!/bin/sh\n"
        'for arg in "$@"; do\n'
        '  if [ "$arg" = "--keys" ]; then\n'
        '    echo "nsz: error: unrecognized arguments: --keys" >&2; exit 2\n'
        '  fi\n'
        'done\n'
        'test -f "$HOME/.switch/prod.keys" || { echo "Could not load keys file." >&2; exit 1; }\n'
        'out=""; next=""\n'
        'for arg in "$@"; do\n'
        '  if [ "$next" = "1" ]; then out="$arg"; next=""; fi\n'
        '  if [ "$arg" = "--output" ]; then next=1; fi\n'
        'done\n'
        'mkdir -p "$out"\n'
        'printf NSP > "$out/update.nsp"\n'
    )
    (fake_bin / "nsz").chmod(0o755)
    shutil.rmtree(srv.CACHE_DIR, ignore_errors=True)
    legacy = client.get("/download/Switch/198X/update.nsz")
    check("an nsz without --keys still gets the keys", legacy.status_code, 200)

    # With no keys to be found at all, the failure says what to do.
    config.KEYS_FILE = str(tmp / "nowhere" / "prod.keys")
    shutil.rmtree(srv.CACHE_DIR, ignore_errors=True)
    refused = client.get("/download/Switch/198X/update.nsz")
    check("no keys anywhere is a 500 that explains itself",
          refused.status_code, 500)
    check("...naming the setting rather than quoting a traceback",
          "KEYS_FILE" in refused.get_data(as_text=True), True)

    config.KEYS_FILE = ""

    print("\n--- refusals ---")
    check("path traversal refused",
          client.get("/download/PC/Hollow Meridian/../../../etc/passwd").status_code
          in (400, 404), True)
    check("a file not in the catalog 404s",
          client.get("/download/PC/Hollow Meridian/nope.dll").status_code, 404)
    check("unknown game id 404s",
          client.get("/download/PC/Nothing Here/x.exe").status_code, 404)
    check("media serves the cover",
          client.get("/media/PC/Hollow Meridian/cover.png").status_code, 200)
    check("media refuses a non-media file",
          client.get("/media/PC/Hollow Meridian/bin/HollowMeridian.exe").status_code, 403)

    with srv.app.test_request_context():
        catalog = json.loads(client.get("/library").data)
    hm_json = next(g for g in catalog if g["id"] == "PC/Hollow Meridian")
    check("catalog JSON exposes every file", len(hm_json["files"]), 6)
    check("cover URL is absolute and correctly escaped",
          hm_json["cover"], "http://testhost/media/PC/Hollow%20Meridian/cover.png")

    shutil.rmtree(tmp, ignore_errors=True)
    print(f"\n{'ALL PASS' if not failures else f'{failures} FAILURES'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
