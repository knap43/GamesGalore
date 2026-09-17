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
      *.bin/*.cue | *.iso | *.nsz | *.nsp            <- game file(s)
      <a whole installed tree, for PC>               <- see below
      *.png / *.jpg                                  <- loose screenshots
      *trailer*.mp4                                  <- optional
      README.md                                      <- "Title (Year)\n\nDescription..."
```

A game's id is `<Platform>/<Title>`. The cover is whichever screenshot has
"cover" in its filename, falling back to the first alphabetically.

Only the screenshots, trailer and README have to sit at the top level — those
three are catalog metadata. Everything else under a game's folder, at any depth,
is the game, and counts toward its reported size.

## PC games are trees, not files

A PC title is normally a full installed tree rather than a single file, which
affects two things:

- **Size** is the whole folder, walked recursively. Measuring only the top level
  reports a fraction of a real game — frequently just its uninstaller.
- **The executable** is searched for across the whole tree, since a game's `.exe`
  is as often in a `bin/` subdirectory as beside its data. The pick prefers an
  executable that isn't an installer or bundled runtime (`unins*`, `vcredist`,
  `dxsetup`, crash handlers and so on), then one whose name matches the game's
  folder title, then the shallowest, then the largest, breaking ties on name so
  the result is stable across scans. If everything present looks like an
  installer, the best of those is still returned rather than reporting no game
  file at all.

A game file's `filename` is therefore relative to the game folder and may
contain subdirectories (`bin/game.exe`). The client applies the same rule again
on its own side at launch time, against what actually landed on disk.

## What a title's file list contains

`/library` lists **every** file the client needs in order to play a title, each
with its own real size — the whole tree for a PC game, both halves of a
`.bin`/`.cue` pair, each of a Switch title's `.nsp`/`.nsz` files. A title's
total is simply the sum of what will actually be transferred.

The entry point is listed first, which costs nothing and means the most
interesting file arrives before a long tail of assets. It carries no other
marking: the client re-derives what to launch from the install directory
itself, because what matters at launch is what landed on disk rather than what
the source library looked like — and because a title with several executables,
or several discs, is one the client lets you choose between at launch time
rather than deciding for you here.

## Endpoints

- `GET /library` — full catalog as JSON. Rescans the filesystem on every call,
  which is fine at ~500 titles; worth caching plus a manual rescan trigger if
  that ever gets slow.
- `GET /media/<platform>/<title>/<filename>` — a screenshot or trailer.
- `GET /download/<platform>/<title>/<filename>` — a game file. If it's already
  `.nsp` (or any non-Switch format), it's streamed as-is. If it's `.nsz`, it's
  decompressed first — see below.
- `GET /status` — whether `nsz` is installed on this machine, and its version.
  The client checks this before offering to install any Switch title, rather
  than finding out mid-transfer.
- `GET /saves/<platform>/<title>` — stored save versions, newest first, with
  size, hash, the device that uploaded each and when it was last played.
- `POST /saves/<platform>/<title>` — stores the raw body as a new version. An
  upload identical to the newest stored version is a no-op rather than a new
  entry, so quitting a game without playing doesn't push real history out.
  The last `SAVE_VERSIONS_KEPT` versions are retained.
- `GET /saves/<platform>/<title>/<version>` — one save archive.

The game id makes up the first two segments of the media and download paths
rather than being matched as one greedy `<path:>` converter, so that a
`filename` containing its own subdirectories splits correctly. Matched the other
way, `PC/Some Game/bin/game.exe` would be read as the id `PC/Some Game/bin` and
the filename `game.exe`, which resolves to nothing.

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

## Tests

```
.venv/bin/python test_server.py
```

Builds real game folders in a temporary directory, scans them, and runs the
Flask app against that library — including a simulated install that fetches
every file a title's catalog entry lists and compares the reconstructed tree
byte-for-byte against the source. Needs nothing installed beyond `flask`; it
never touches your real library and cleans up after itself.

## Cloud saves

Saves are stored under `SAVE_ROOT`, one directory per game, as timestamped
`.tar.gz` archives with a JSON sidecar each. Deliberately **not** under
`CACHE_DIR`: everything in the cache can be regenerated by decompressing from
the library again, whereas a save is the only copy of someone's progress. Point
`SAVE_ROOT` at something you back up.

The server treats the archive as opaque — it never unpacks one — so the whole
question of where saves live on a given machine stays on the client side.

## Known gaps

- **No auth on any of this** — fine on a trusted LAN, not fine beyond it. Note
  that the save endpoints are *writable*, which makes this materially more
  serious than it was when every route was read-only: anyone who can reach the
  port can overwrite save data.
- **`/library` rescans on every call and serves one file per request.** Both
  are fine at this scale and both are the obvious things to change first if a
  large PC library makes installs feel slow: a cached catalog with a manual
  rescan trigger, and an endpoint that streams a whole title as one archive
  instead of a request per file.
- **Nothing prunes `CACHE_DIR`.** Converted `.nsp` files accumulate there
  indefinitely; there's no size cap and no eviction.
- **The catalog is only populated by `/library`.** `_reload_catalog()` runs at
  startup and on each `/library` call; `/media` and `/download` both look games
  up in it. That holds under `python server.py`, but a deployment that imports
  `app` from a WSGI server without running `__main__` would start with an empty
  catalog until something hits `/library` first.
