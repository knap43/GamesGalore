# Games Galore library server (Python)

Runs on the laptop with the mounted drive — separately from whatever else that
machine already serves. Exposes the catalog and game files over HTTP so the
desktop client never touches the filesystem directly.

## Why the work lives here

The library lives on a different, always-on machine, so doing the NSZ→NSP
conversion server-side means the desktop client needs **no Python and no `nsz`
at all** — that whole dependency stays on the one machine you're already
managing as a server, and the client becomes a plain HTTP consumer. The client
holds only its own install bookkeeping; see `../app/`.

## Setup

```
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

Edit `config.py`: set `LIBRARY_ROOTS` to the actual mount point(s) and
`CACHE_DIR` to wherever converted `.nsp` files should be cached.

### Switch keys

Decompressing a `.nsz` needs `prod.keys`, and nsz finds them relative to the
HOME of whoever runs it. That is why "it decompresses fine when I do it by
hand" and "the server returns 500: Could not load keys file" are both true at
once: the unit runs as its own system user, whose home is not the one holding
your keys.

The server therefore resolves the path itself and hands it to nsz with
`--keys`, rather than leaving it to be discovered. Point `KEYS_FILE` at a file
(or a directory holding `prod.keys`/`keys.txt`) that the service's account can
read:

```python
KEYS_FILE = Path("/etc/games-galore/prod.keys")
```

or, in the unit, `Environment=NSZ_KEYS=/etc/games-galore/prod.keys`. Left empty,
it looks in `~/.switch/`, `~/.config/nsz/` (or `$XDG_CONFIG_HOME/nsz/`) and
`/etc/games-galore/` — of that user's home, which is the trap above.

`GET /status` reports which file it will use, and the client shows it under
"Library server" in Settings, so a missing key file is visible before a
download fails rather than after. An nsz too old to understand `--keys` is
handled too: the retry hands it a HOME with the keys where that version looks.

### More than one drive

`LIBRARY_ROOTS` is a list, so a library that has outgrown its disk simply gains
another entry:

```python
LIBRARY_ROOTS = [
    Path("/mnt/game-library"),
    Path("/mnt/game-library-2"),
]
```

Each drive is scanned in order and the results are one catalog, with the same
`<Platform>/<Title>` layout expected on each. Three things follow:

- **A title present on two drives is served from the first one listed.** That is
  what makes moving a game between drives safe while the server is running: the
  copy being made is ignored until the original is deleted.
- **A drive that isn't mounted is skipped**, with a line saying so, rather than
  emptying the catalog of the drives that are. Every drive missing is still the
  error it always was.
- **Nothing is written to the library**, so nothing has to decide which drive a
  new game belongs on. You put games where you want them; the server finds them.

The environment wins where it is set, which suits a systemd unit:

```
LIBRARY_ROOTS=/mnt/game-library:/mnt/game-library-2
```

A `config.py` from before this existed, with only `LIBRARY_ROOT` in it, keeps
working unchanged and is read as a library of one drive.

The cache and the saves used to live under `vault-server`, before the project
settled on one name. An existing install needs no attention: on startup the
server moves each of those directories to its new path if nothing is there
yet, and says so when it does. `LEGACY_CACHE_DIR` and `LEGACY_SAVE_ROOT` in
`config.py`, and `migrate_legacy_state()` in `server.py`, can both be deleted
once no deployment is old enough to need them.

Run directly to test:
```
.venv/bin/python server.py
```

For the constantly-running setup this is meant for, install it as a systemd
service instead — `games-galore-server.service` is included; adjust `WorkingDirectory`
and `User`, drop it in `/etc/systemd/system/`, then:
```
systemctl daemon-reload
systemctl enable --now games-galore-server
```

## Expected library layout

```
<each LIBRARY_ROOTS entry>/
  PS1/  PS2/  PS4/  PC/  Switch/
    <Game Title>/
      *.cue/*.bin | *.chd | *.iso | *.mdf | *.nsz | *.nsp  <- game file(s)
      <a whole installed tree, for PC and PS4>             <- see below
      *.png / *.jpg                                        <- loose screenshots
      *trailer*.mp4                                        <- optional
      README.md                                            <- "Title (Year)\n\nDescription..."
    <Game Title> (Disc 1).chd                              <- or discs sitting loose
    <Game Title> (Disc 2).chd                                 in the platform folder

<METADATA_ROOT>/                                           <- outside the library
  PS1/  PS2/  PS4/  PC/  Switch/
    <Game Title>/
      README.md  cover.jpg  screenshot-01.jpg  trailer.mp4  game.json
```

### The metadata store

Every game's catalog furniture — `README.md`, `cover.jpg`, screenshots,
`trailer.mp4`, `game.json` — lives in one place, arranged by platform:

```
<METADATA_ROOT>/
  PS1/
    Velvet Requiem/
      README.md  cover.jpg  screenshot-01.jpg  trailer.mp4  game.json
  PS2/  PS4/  PC/  Switch/
```

`METADATA_ROOT` defaults to `~/.local/share/games-galore-server/metadata` and
takes an environment override of the same name. It sits **outside every library
root** on purpose:

- A library is a collection of games. Files this server generated are not part
  of it, and a backup of the collection shouldn't carry them.
- With the library spanning several drives, a game that moves between them would
  otherwise leave its description behind.
- Loose discs share a platform folder, so there is nowhere beside them to put
  one game's cover without putting it next to everybody else's.
- The library stays readable-only, which is the strongest thing that can be said
  about a tool pointed at somebody's collection.

**A library filled in before the store keeps working.** Metadata still sitting
in a game's own folder is read from there, and served from there, exactly as
before. It is never written to again: a fetch for such a game writes to the
store, and from then on the store is what the catalog reads.

**Moving it across** is one command, and it moves rather than copies, since two
descriptions is how they start disagreeing:

```sh
.venv/bin/python metadata.py --migrate
```

It prints every file it moves. A file the store already has is left where it is
rather than overwritten — the store is the newer statement by construction — and
said so at the end. Game files are never touched; a game's own folder keeps
exactly the game.

`GET /status` reports the store's path, so the client can show where it is.

### Discs: either shape, most formats

A disc platform takes a game either way.

**A folder per game** is the tidier shape, and the one the metadata fetcher
fills in place. The entry point is whichever disc format is worth opening
first, searched through the whole folder rather than just its top level — so a
game whose discs sit in a `discs/` subdirectory is found like any other.

**Files loose in the platform folder** is what most rips actually look like,
and they are now read as games too. Discs of one game become one game: the disc
marker is taken off each filename — `(Disc 2)`, `[CD 1]`, `- Disk 3 of 4` — and
what remains is the title they share. So

```
PS1/Velvet Requiem (Disc 1).chd
PS1/Velvet Requiem (Disc 2).chd
PS1/Velvet Requiem (Disc 3).chd
```

is one catalog entry with three files, not three entries. A `.cue` and the
`.bin` it describes group the same way, as does an `.mdf` with its `.mds`.

Their README, cover, screenshots and `game.json` have nowhere to sit beside
them — the platform folder belongs to every loose title in it — so they live in
`<platform>/.metadata/<Title>/`, which the fetcher writes and the scanner skips
when looking for games. Dot directories are never games.

**Formats**, best entry point first:

| Platform | Opened |
| --- | --- |
| PS1 | `.m3u`, `.cue`, `.chd`, `.pbp`, `.ecm`, `.iso`, `.img`, `.mdf`, `.bin` |
| PS2 | `.m3u`, `.iso`, `.chd`, `.cso`, `.zso`, `.gz`, `.cue`, `.mdf`, `.nrg`, `.img`, `.bin` |
| PS4 | the `eboot.bin` of an extracted game (a `.pkg` is listed, but shadPS4 installs those rather than booting them) |
| Switch | `.nsp`, or `.nsz` decompressed on the way out |
| PC | the installed tree, entry point picked by name, depth and size |

An `.m3u` beats the discs it lists, and a `.cue` beats the `.bin` it describes:
both are one disc named twice otherwise.

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
- `GET /status` — whether `nsz` is installed on this machine and its version,
  whether it has Switch keys it can read and which file it will use, plus each
  library drive with the room left on it and how many games it holds.
  The client shows that under "Library server" in Settings, so a drive filling
  up is visible from the sofa rather than over ssh.
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

## Catalog caching

`/library` used to rescan the entire tree on every call, which on a large
library is seconds of an empty grid every time the app opens. It now rescans
only when the library looks different or the cached scan is old.

The check is a fingerprint of every platform and game directory with its
modification time — two levels deep and no further. A full scan walks every PC
game's whole tree to size it; this stats a few hundred directories and is
imperceptible. A directory's mtime moves when anything is added to, removed from
or renamed inside it, so new, deleted and renamed games are all caught
immediately.

What it cannot see is a file *edited in place* several levels down — an `.exe`
replaced by a patch, say — which changes a game's size without changing any
directory the check looks at. `CATALOG_TTL_SECONDS` (ten minutes) is the
backstop, and `POST /rescan` forces one immediately for anyone who has just
changed something and doesn't want to wait.

## Whole-title archives

`GET /archive/<platform>/<title>` streams every file of one game as a single
uncompressed tar. A PC game is a tree of thousands of files, and installing one
meant thousands of HTTP requests — correct, and fine on a LAN, but each pays for
a connection, a route lookup and a catalog hit, and for small files that dwarfs
the transfer itself.

Uncompressed on purpose: game files are already compressed, and gzip would spend
CPU to make the transfer slower. `.nsz` conversion still happens per file before
its entry is written, so a Switch title arrives as the `.nsp` the client expects
— which is also why the response carries no `Content-Length`: the converted
sizes aren't known until they are produced, and guessing would be worse than
streaming without one.

Everything that can fail cleanly — an unknown title, a missing file, a
conversion — is resolved before the first byte goes out, while `abort()` can
still produce an error the client can read.

## Fetching metadata

Writing a README, finding a cover and pulling a few screenshots is pleasant for
one game and unbearable for five hundred, so `metadata.py` does it from
[RAWG](https://rawg.io/apidocs). A free key is instant; put it in the
environment rather than in the file:

```sh
RAWG_API_KEY=... .venv/bin/python metadata.py                 # everything incomplete
RAWG_API_KEY=... .venv/bin/python metadata.py "Hollow Meridian"
RAWG_API_KEY=... .venv/bin/python metadata.py --overwrite      # replace what's there
```

It writes exactly what the scanner reads — `README.md` with the title, year and
description, `cover.jpg`, `screenshot-01.jpg` onward, `trailer.mp4`, and a
`game.json` of genre and tags — so a fetched folder and a hand-made one are the
same thing.

**It writes to the store, never to the library.** Everything above lands in
`<METADATA_ROOT>/<Platform>/<Title>/`; the games are only ever read. See
**The metadata store** below, including how to move a library that was filled
in before the store existed.

**Nothing is overwritten unless you ask.** Every file that already exists is
skipped and reported as skipped, which makes a second run cheap and makes a
curated folder safe. `--overwrite` is the escape hatch for a folder whose data is
wrong rather than missing.

The search picks the closest name rather than the first result: an exact match on
the normalised name wins, then a prefix match, and only then position. Searching
for "DOOM" returns a dozen DOOMs, and the first is not reliably the one called
DOOM.

Downloads land in a `.part` file and are renamed on success, so an interrupted
run leaves nothing that a later run would mistake for finished.

The same thing is available over HTTP, which is what the client's **Fetch
details** button calls:

```
POST /metadata/<platform>/<title>     one game
POST /metadata                        every game missing a description or cover
```

Both take `?overwrite=1`, and both are behind `SAVE_TOKEN` when one is set, since
they are the one writable surface besides the saves. The single-game route also
takes `?rescan=0`, which the app passes while working through a list — one full
library rescan per game would cost far more than the fetching does, and the next
`/library` call picks the changes up anyway, because the store's shape is part
of the signature the catalog watches.

An `X-RAWG-Key` header overrides `RAWG_API_KEY` for that request, which is how
the app supplies a key without one being written into this machine's config. It
is a header rather than a query parameter so it stays out of access logs. A bad key or a rate limit stops a library-wide run rather
than making four hundred more requests that cannot work; one game failing for its
own reasons is recorded and the run continues.

## Per-game metadata

Everything the catalog knows is otherwise inferred: the title from the folder
name, the year from a parenthesis in it, the description from a README. An
optional `game.json` beside a game's files is the one place to state something
outright:

```json
{
  "genre": "RPG",
  "tags": ["singleplayer", "moody"],
  "players": 1,
  "release_year": 2019,
  "description": "Overrides the README, if you'd rather write it here.",
  "title": "Overrides the folder name too."
}
```

Every key is optional and a malformed file is treated as an absent one — a
catalog that refused to list a game because somebody left a trailing comma in
its metadata would be a worse outcome than a game with no genre.

`genre` and `tags` are read in whichever shape they arrive: a string, a list of
strings, or a list of `{"name": …}` objects, which is RAWG's own shape and
therefore what anybody copying from it by hand produces. `genres` is accepted as
a synonym for `genre`, taking the first. A file that looks right and is silently
ignored is the worst of both worlds.

The client uses all three: the genre styles the card and drives a sidebar
filter, the tags are clickable buttons in the detail view that filter the
library, and the player count shows in the detail header. Search covers the
genre and the tags as well as the title.

## Conversion and caching

`.nsz` files are decompressed on first download into `CACHE_DIR/<game_id>/`, and
every later download of that same file is served straight from the cache — no
repeat decompression. The source library folder is never written to.

The cache is swept back under `CACHE_MAX_BYTES` after each conversion, least
recently used first. A decompressed `.nsp` is roughly twice the `.nsz` it came
from, so a library browsed for long enough would otherwise fill whatever disk
the server runs on. "Least recently used" reads atime where the filesystem keeps
one — a file served to a client was read, which is exactly the signal wanted —
and falls back to mtime, since a `noatime` mount reports a stale atime rather
than none at all. Evicting too eagerly costs only CPU on the next download of
that title, which is why the cap can be set as low as the disk requires.

## Authenticating the save endpoints

`SAVE_TOKEN` in `config.py` is empty by default, which means no check at all —
a LAN tool that refuses to work until it is configured is a LAN tool nobody
configures. Note what the difference actually is, though: with it unset, anyone
who can reach the port can read, overwrite and grow your saves, while everything
else here is read-only.

Set it to any long random string (`openssl rand -hex 32`) and put the same value
in the client's Settings. Requests then need `Authorization: Bearer <token>` on
all three `/saves` routes — reads included, since a save is personal in a way the
catalog is not — and are compared in constant time. `/library`, `/media` and
`/download` stay open: they are the browsing surface this whole tool exists to
expose on a LAN.

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

- **No auth on anything but the saves** — fine on a trusted LAN, not fine
  beyond it. The save endpoints can be locked with `SAVE_TOKEN`; the catalog,
  media and downloads deliberately cannot, since they are what the tool exists
  to expose.
- **The catalog cache cannot see a file edited in place.** Directory mtimes
  catch anything added, removed or renamed; a patched `.exe` several levels
  down changes a game's size without touching them, and waits for the TTL or a
  `POST /rescan`.
- **The catalog is only populated by `/library`.** `_reload_catalog()` runs at
  startup and on each `/library` call; `/media` and `/download` both look games
  up in it. That holds under `python server.py`, but a deployment that imports
  `app` from a WSGI server without running `__main__` would start with an empty
  catalog until something hits `/library` first.
