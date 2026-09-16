# Games Galore library server (Python)

Runs on the laptop with the mounted drive — separately from whatever else that
machine already serves. Exposes the catalog and game files over HTTP so the
desktop client never touches the filesystem directly.

## Why the work lives here

The library lives on a different, always-on machine, so doing the NSZ→NSP
conversion server-side means the desktop client needs **no Python and no `nsz`
at all** — that whole dependency stays on the one machine you're already
managing as a server, and the client becomes a plain HTTP consumer. The client
holds only its own install bookkeeping; see `../tauri-backend/`.

## Setup

```
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

Edit `config.py`: set `LIBRARY_ROOT` to the actual mount point and `CACHE_DIR`
to wherever converted `.nsp` files should be cached.

Run directly to test:
```
.venv/bin/python server.py
```

For the constantly-running setup this is meant for, install it as a systemd
service instead — `vault-server.service` is included; adjust `WorkingDirectory`
and `User`, drop it in `/etc/systemd/system/`, then:
```
systemctl daemon-reload
systemctl enable --now vault-server
```

## Expected library layout

```
<LIBRARY_ROOT>/
  PS1/  PS2/  PC/  Switch/
    <Game Title>/
      *.bin/*.cue | *.iso | *.exe | *.nsz | *.nsp   <- game file(s)
      *.png / *.jpg                                  <- loose screenshots
      *trailer*.mp4                                  <- optional
      README.md                                      <- "Title (Year)\n\nDescription..."
```

A game's id is `<Platform>/<Title>`. The cover is whichever screenshot has
"cover" in its filename, falling back to the first alphabetically.

## Endpoints

- `GET /library` — full catalog as JSON. Rescans the filesystem on every call,
  which is fine at ~500 titles; worth caching plus a manual rescan trigger if
  that ever gets slow.
- `GET /media/<game_id>/<filename>` — a screenshot or trailer.
- `GET /download/<game_id>/<filename>` — a game file. If it's already `.nsp` (or
  any non-Switch format), it's streamed as-is. If it's `.nsz`, it's decompressed
  first — see below.
- `GET /status` — whether `nsz` is installed on this machine, and its version.
  The client checks this before offering to install any Switch title, rather
  than finding out mid-transfer.

## Mixed nsz/nsp folders

`library.py`'s `_find_game_files` inspects every file in a Switch folder
individually — a base game as `.nsp`, an update or DLC as `.nsz`, in any mix —
and tags each one's `needs_conversion` independently. Nothing assumes a folder
is uniformly one format. Confirmed against a fixture folder with exactly that
mix (one `.nsp`, two `.nsz`):

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

`.nsz` files are decompressed on first download into `CACHE_DIR/<game_id>/`, and
every later download of that same file is served straight from the cache — no
repeat decompression. The source library folder is never written to.

## Path safety

`filename` and `game_id` come straight from the URL, so `_safe_join` rejects
absolute paths and any `..` segment before anything touches the filesystem, then
double-checks the resolved path is still inside the expected game folder.
`/media` additionally restricts what it will serve by extension. This is a
reasonable baseline for a LAN tool, not a substitute for review before exposing
it beyond your own network.

## Known gaps

- **PS1/PS2 titles that come as a `.bin`/`.cue` pair are broken.**
  `_find_game_files` returns only the `.cue` as the game's single file — while
  reporting the combined size of every file in the folder. Since
  `/download` refuses any filename not in that list, the `.bin` holding the
  actual disc data can never be fetched: the client installs a few-hundred-byte
  `.cue`, marks the title installed, and the emulator then fails to find the
  data it points at. The fix is to return every candidate file and keep the
  `.cue` as the one handed to the emulator, which the client's `launcher.rs`
  already selects for.
- **No auth on any of this** — fine on a trusted LAN, not fine beyond it.
- **The catalog is only populated by `/library`.** `_reload_catalog()` runs at
  startup and on each `/library` call; `/media` and `/download` both look games
  up in it. That holds under `python server.py`, but a deployment that imports
  `app` from a WSGI server without running `__main__` would start with an empty
  catalog until something hits `/library` first.
