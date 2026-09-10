# Games Galore library server (Python)

Runs on the laptop with the mounted drive — separately from whatever
else that machine already serves. Exposes the catalog and game files
over HTTP so the desktop client never touches the filesystem directly.

## Why this is a better split than the Rust version

The earlier Tauri/Rust design assumed the client could reach the
library as a mounted path and would run `nsz` itself. Now that the
library actually lives on a different, always-on machine, doing the
NSZ→NSP conversion here instead means the desktop client needs **no
Python and no `nsz` at all** — that whole dependency moves to the one
machine you're already comfortable managing as a server, and the
client becomes a plain HTTP consumer.

## Setup

```
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

Edit `config.py`: set `LIBRARY_ROOT` to the actual mount point and
`CACHE_DIR` to wherever converted `.nsp` files should be cached.

Run directly to test:
```
.venv/bin/python server.py
```

For the constantly-running setup this is meant for, install it as a
systemd service instead — `vault-server.service` is included; adjust
`WorkingDirectory` and `User`, drop it in `/etc/systemd/system/`, then:
```
systemctl daemon-reload
systemctl enable --now vault-server
```

## Endpoints

- `GET /library` — full catalog as JSON. Rescans the filesystem on
  every call, which is fine at ~500 titles; worth caching plus a
  manual rescan trigger if that ever gets slow.
- `GET /media/<game_id>/<filename>` — a screenshot or trailer.
- `GET /download/<game_id>/<filename>` — a game file. If it's already
  `.nsp` (or any non-Switch format), it's streamed as-is. If it's
  `.nsz`, it's decompressed first — see below.
- `GET /status` — whether `nsz` is installed on this machine, and its
  version. The client should check this before offering to install any
  Switch title, rather than finding out mid-transfer.

## The nsz/nsp check you flagged

`library.py`'s `_find_game_files` inspects every file in a Switch
folder individually — a base game as `.nsp`, an update or DLC as
`.nsz`, in any mix — and tags each one's `needs_conversion`
independently. Nothing assumes a folder is uniformly one format. I
built a small fixture folder with exactly that mix (one `.nsp`, two
`.nsz`) and ran the scanner against it directly to confirm:

```json
{
  "files": [
    { "filename": "update.nsz", "format": "nsz", "needs_conversion": true },
    { "filename": "dlc.nsz",    "format": "nsz", "needs_conversion": true },
    { "filename": "base.nsp",   "format": "nsp", "needs_conversion": false }
  ]
}
```

## Conversion and caching

`.nsz` files are decompressed on first download into
`CACHE_DIR/<game_id>/`, and every later download of that same file is
served straight from the cache — no repeat decompression. The source
library folder is never written to.

## Path safety

`filename` and `game_id` come straight from the URL, so `_safe_join`
rejects absolute paths and any `..` segment before anything touches
the filesystem, then double-checks the resolved path is still inside
the expected game folder. Tested against a live request (see below) —
this is a reasonable baseline for a LAN tool, not a substitute for
review before exposing it beyond your own network.

## What changes on the Tauri/Rust side

The Rust code from the earlier pass (`library.rs`, `install_state.rs`)
assumed direct filesystem access and a local `nsz`. Both are now
mostly obsolete for Switch:

- `scan_library` (Rust) → replaced by a `fetch('http://<server>:8420/library')`
- `install_switch_game`'s copy-and-convert logic → replaced by a plain
  streamed download from `/download/<id>/<filename>` per file the game
  needs, tracking install state locally exactly as before
- `dependencies.rs`'s `nsz` check → no longer needed client-side at all;
  the server's `/status` route covers it

`dependencies.rs`'s emulator checks (DuckStation, PCSX2, Wine, Eden)
are unaffected — those still run locally on whichever machine plays
the games.

## Not yet built

- PS1/PS2/PC are scanned but have no server-side conversion need — the
  download route already handles them as-is
- No auth on any of this — fine on a trusted LAN, not fine beyond it
- The Rust `install_state.rs` rewrite — **done**, see `../tauri-backend/`:
  `server.rs` now fetches this catalog over HTTP instead of scanning a
  local path, and `install_state.rs` streams each file from
  `/download/...` instead of copying + shelling out to `nsz` — the
  client no longer touches `nsz` or Python at all.
