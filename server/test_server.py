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

    config.LIBRARY_ROOT = root
    config.CACHE_DIR = tmp / "cache"
    import server as srv

    srv.LIBRARY_ROOT = root
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
