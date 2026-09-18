# Games Galore backend (Rust/Tauri)

The desktop client: a Tauri app wrapping the plain HTML/CSS/JS frontend in
`frontend/`, talking to the Python library server (`../server/`) over
HTTP. It has no local knowledge of where the library lives — only a base URL
from settings — and never touches the library filesystem or runs `nsz` itself.

## Architecture

| File | Responsibility |
| --- | --- |
| `server.rs` | Fetches the catalog (`GET /library`) and the server's `nsz` status (`GET /status`). `Game` and `GameFile` mirror the server's JSON shape exactly. |
| `install_state.rs` | The local record of what's on this machine (`installs.json` in the app data dir, keyed by `Game.id`), plus `install_game`, `uninstall_game` and `cancel_install`. |
| `catalog_cache.rs` | `installed-cache.json` beside it: the catalog entries for installed titles, so the shelf is on screen at launch without waiting on the server. See **Starting up before the server answers** below. |
| `launcher.rs` | `launch_game` — spawns the configured emulator for a platform, detached, in fullscreen. Resolves which file to hand it by searching the install directory recursively, and `list_launch_candidates` backs the UI's picker for titles with more than one; see below. |
| `dependencies.rs` | `check_dependency` — whether a configured emulator is actually present, so a missing tool surfaces in Settings rather than mid-Play. Flatpak-aware; see below. |
| `settings.rs` | `settings.json` alongside `installs.json`: server address, install root, sound preference, per-platform emulator config, per-game launch overrides, the Wine prefix root, and cloud-save configuration. |
| `playtime.rs` | `playtime.json` beside them: seconds played, last played and a session count per game, recorded by the launcher. See **Playtime** below. |
| `saves.rs` | Locates a game's save data, packs it as a tar.gz and syncs it with the server. See **Cloud saves** below. |

`Game.files` is a list because Switch titles can have several — base game,
update, DLC — each independently already-`.nsp` or needing conversion. The
client never has to know which.

### Installing is just downloading

The catalog lists every file a title needs — the whole tree for a PC game, both
halves of a `.bin`/`.cue` pair, each of a Switch title's `.nsp`/`.nsz` files —
so `install_game` downloads all of them, streaming each to disk under the same
relative path it has in the library. Parent directories are created as it goes,
since a filename can carry subdirectories of its own.

There is no Switch-specific path at all: the server resolves any `.nsz` → `.nsp`
conversion before a file is sent, so what arrives is always installable as-is.
The client renames a converted file accordingly and never invokes anything.

Progress is reported against the whole title rather than the file currently in
flight — a per-file percentage would race to 100% and reset for every one of a
PC game's thousands of files while saying nothing about how far along the
install actually is. Because a `.nsz` decompresses on the way out, the bytes
arriving can exceed the total the catalog advertised, so the percentage is
clamped rather than allowed to overshoot.

Only transitions are written to `installs.json` — an install starting,
finishing, failing or being cleared. Progress is emitted to the frontend
without touching it. Persisting each tick would have meant a read-modify-write
of the entire state file per percent per file, which is survivable for a
one-file title and hundreds of thousands of rewrites for a tree. The one
durable fact worth keeping mid-install is that one is in progress, and the
`Downloading` status persisted at the start records exactly that — which is
also what lets `cancel_install` recognise and clear an install orphaned by the
app closing mid-download.

### Starting up before the server answers

`GET /library` rescans the entire source tree on every call, so on a large
library the grid used to sit on "Loading your library…" for seconds after
launch — including for the handful of titles already on this machine, which the
app opens on and which it could have drawn from local knowledge the whole time.

`catalog_cache.rs` keeps those entries in `installed-cache.json`, and the
frontend's startup runs in two passes: `get_cached_library` paints the
installed shelf immediately, then `fetch_library` replaces it with the real
catalog when it arrives. The sidebar footer says the rest is still loading
while that is in flight, so the counts above it read as provisional rather than
wrong.

The cache is scoped to installed titles on purpose. A stale entry for something
on your disk costs at most an out-of-date name or blurb next to files that are
right there; a stale copy of the other several hundred would be offering
downloads of titles the server may no longer have. It also stays small enough
to read and parse without anyone noticing.

It is kept in step with `installs.json` by the same transitions that write it —
a finished install adds its entry, an uninstall or cancel removes it, and a
successful fetch refreshes whatever entries it still covers. Nothing here is
load-bearing: a missing, unreadable or malformed cache reads as empty and costs
a slower first paint, and a cache that cannot be written never fails the
install or fetch that triggered it.

One deliberate consequence: if the server is unreachable, the cached shelf
stays. The installed games are on disk and still launchable, and emptying the
grid would be the single response that makes the app useless offline.

### Installs are queued, checked and verified

Three guards sit around the download loop, all added after the fact and
all for failures that had actually happened or were one click away.

**A bounded queue.** Every click used to start its own download loop
immediately, so six queued-up titles meant six streams competing for
one link — each slower than it needed to be, each reporting progress as
though it were alone, and the one wanted first finishing last. Two
transfer at a time now (`MAX_CONCURRENT_INSTALLS`); the rest are
`Queued`, which the UI states plainly rather than showing a percentage
that hasn't moved. Cancelling from the queue is free: nothing has been
requested and nothing written.

**A free-space check**, once the destination directory exists, so
`statvfs` reports the filesystem the files will land on rather than
whatever ancestor happened to exist first. The requirement is not
simply the sum of the catalog's sizes: a `.nsz` decompresses on the way
out, so a converting file is budgeted at twice its listed size, plus a
256 MB margin for the filesystem's own overhead. If the check can't be
made — not Unix, unreadable path — it is skipped rather than treated as
a refusal.

**Verification of what arrived.** A stream can end early without
erroring at all: a dropped connection, a server that died mid-response.
That writes a short file, reports success, and surfaces weeks later as
an emulator crash nobody connects back to the install. Each file is now
measured against `Content-Length` where the server sent one and the
body wasn't encoded in transit, and against the catalog's size
otherwise — except for a converted file, where the catalog's number is
legitimately not what arrives. A mismatch deletes the partial file and
fails the install with both numbers in the message. Where neither check
applies (a chunked response for a converted file) the transfer is
accepted; claiming to detect what we cannot would be worse than the
gap.

### Playtime

The UI offered "Recently played" and "Playtime" as sort options from the first
version, and for a real library both did nothing: those fields existed only on
the mock catalog. Everything needed to fill them in was already here — the
launcher knows when a game starts, and the exit supervision added for cloud
saves knows when it stops — so `playtime.rs` records both in `playtime.json` and
pushes each change to the frontend, which needed no new rendering to use them.

A session is timed from launch to *after* `wineserver -w` returns, not to the
emulator process exiting: for a PC game that process is Wine's launcher, which
returns long before the game does, and stopping the clock there would record
every session as a few seconds. Under a minute is counted as a launch but not as
playtime — a game closed because it opened on the wrong monitor is not an hour
played, and a library where every mis-click adds a minute stops being a useful
sort within a week. Over twelve hours is capped rather than discarded: that is a
game left running overnight, and something was probably played.

Written once per session rather than on a timer, so a session ended by a power
cut is a session this does not record. That is the better trade against
rewriting the file every minute for the lifetime of every game anyone plays.

The sort control came back with it, as pills rather than a dropdown — WebKitGTK
draws a native `<select>` itself and ignores this stylesheet entirely, which is
why the original was removed and why the launch picker is a custom listbox too.
The chosen order is persisted with the rest of the settings.

### Choosing what to launch

`find_local_game_file` walks the install directory recursively rather than
listing its top level. A PC game is an installed tree, so the executable is
frequently not at the top level at all — and where it is, the first file
alphabetically beside it is as likely to be an uninstaller as the game.

For PC it applies the same ranking the server's `_pick_pc_executable` uses when
cataloguing: prefer an executable that isn't an installer or bundled runtime
(`unins*`, `vcredist`, `dxsetup`, crash handlers), then one whose name matches
the game's own folder title, then the shallowest, then the largest, breaking
ties on name so the choice is stable across launches. PS1/PS2 still resolve to
the `.cue`. The decision is made here against the real install directory rather
than trusting the catalog, since the catalog describes the source library, not
what actually landed on this disk.

The two rankings are duplicated deliberately — one is in Python on the server,
the other in Rust on the client — so if you change the exclusion list, change
both. `NON_GAME_EXE_MARKERS` exists under that name in each.

**When the automatic choice is wrong, the detail view offers a picker.** Some
titles have more than one thing worth launching: a separate 32- and 64-bit
executable, a launcher beside the game proper, or — for PS1/PS2 — a multi-disc
title with a `.cue` per disc. `list_launch_candidates` returns that list for an
installed title, ranked, and the Play row grows a dropdown whenever there are
two or more. One candidate is not a choice, so the picker stays hidden, which
is the common case.

The dropdown is a button and a panel rather than a `<select>`. A native
select's option list is drawn by the OS, not the page — `option` styling does
nothing in WebKitGTK, so it rendered as a grey box with a blue system highlight
regardless of the stylesheet. The replacement is assembled from parts already
in use: the secondary button's proportions for the trigger, the settings
modal's panel treatment for the menu, and the sidebar's active-row gradient for
the current choice. It carries `role="listbox"`, and because focus stays on the
trigger while the menu is open, arrow keys and the gamepad move a tracked
highlight rather than page focus — routed through the same `moveDirection` and
`activateFocused` everything else uses, with `goBack` closing the menu before
the detail view.

A selection is stored in `settings.json` under `launch_overrides`, keyed by
`Game.id` and valued with a path relative to that game's install directory —
and only when it differs from what the ranking would have picked anyway, so the
file doesn't fill up with entries restating the default. An override naming a
file that no longer exists (the title was reinstalled differently, say) falls
back to the automatic choice rather than failing.

`launch_game` takes that path as an optional `executable` argument and resolves
it through `resolve_chosen`, which refuses absolute paths, any `..` component,
and — after canonicalising, so a symlink can't stand in for one — anything
landing outside the install directory. This is stricter than the download path
deliberately: the value here becomes the program that gets spawned.

### Per-game Wine prefixes

Each PC title runs in its own Wine prefix rather than sharing the default
`~/.wine`. Two things follow from that. Games stop inheriting each other's
runtime installs and registry state, and — more usefully — a game's prefix
*becomes* its save data, which is what makes cloud saves work for PC without a
per-game manifest of where each game hides its saves.

Prefixes default to `.wine-prefixes` beside the install root, so this needs no
configuration; `prefix_root` in settings overrides the location. Wine creates a
missing prefix itself on first run, which makes the first launch of a title slow
and every one after it normal.

**This changes where existing PC saves are.** A game played before this existed
wrote into the shared `~/.wine`, and will not find those saves in its new
prefix. Copy them across by hand if you need them.

`launch_game` also now sets the working directory to the executable's own
folder. Plenty of Windows games resolve their data — and write their saves —
relative to the working directory, and inheriting the app's would scatter those
files wherever Games Galore happened to be started from.

### Cloud saves

Save data is archived, pushed to the library server, and pulled back on another
machine.

**On by default**, with a switch in Settings to turn it off. Defaulting to on is
safe because it stays inert until there is somewhere to sync from — a Switch
title needs its data directory and Title ID mapped, a PC title needs a prefix
that exists — so it costs nothing until it can actually work, and then works
without anyone having to find a setting first. It is also not a destructive
default: local saves remain the source of truth, and the only automatic
overwrite is restoring onto a machine that has no save of its own. Anything that
could lose progress asks first.

Turning it **off** does not stop games saving. Saves are always written locally,
and the emulators neither know nor care about this setting; all it decides is
whether those local saves are mirrored to the server. The switch says as much in
the line beneath it, because "cloud saves: off" could otherwise read as "saving
is off". An explicit opt-out is stored and honoured — the default applies only
to a settings file that has never carried the field.

**Where saves are.** This is the whole difficulty, and it differs by platform.
Switch saves live under the emulator's data directory in a fixed tree keyed by
the title's 16-hex-digit Title ID, which has no relationship to the library's
folder names. PC needs no mapping at all, because the prefix's `drive_c/users`
*is* the save data.

**Nothing about that is asked of the user.** There is no per-game configuration
and no hex to look up — an earlier version listed a dropdown of Title IDs per
installed game, which grew with the library and asked people to pick one
near-identical hex string out of several. Identification is automatic, by three
means in order of cost:

1. **The filename.** Dump tools overwhelmingly name Switch files with the id in
   brackets — `Bad North [0100C1F0051B4000][v0].nsp`. Free to read.
2. **The ticket inside the NSP.** An NSP is a PFS0 archive whose header and
   filename table are plain, unencrypted bytes, and a ticket is named for its
   rights ID, whose first 16 hex digits *are* the Title ID. So the id comes out
   without a key file and without decrypting anything — only the archive's table
   of contents is read, never its content, which keeps it cheap on a
   multi-gigabyte file.
3. **Watching a session.** If neither yields anything, the save directories are
   listed before the game runs and again after it exits; whichever appeared is
   that game's. This needs no format knowledge at all, and the moment it works
   is the moment the game first has a save worth syncing. If more than one
   directory appears, nothing is recorded — guessing would file one game's saves
   under another's name.

Whatever is found is normalised to the base title, since an update shares its
base game's save data and differs only in the low 12 bits. A folder holding a
base game, its update and DLC resolves to the base, which is what the emulator
files saves under.

The emulator's data directory is probed for too, across the locations the
Yuzu-derived emulators use, including Flatpak paths. A directory only counts if
it actually contains the save tree. Settings shows one line reporting how many
titles are identified, rather than a row per game.

**Symlinks are never followed, anywhere in the save path.** This is not a
hardening detail, it's load-bearing. A Wine prefix's Windows user profile is not
self-contained: Wine points Documents, Desktop, Downloads and the rest at the
real home directory. Following those turns "measure this game's save" into "walk
the user's entire home directory" — and because prefixes live under that home
directory by default, the graph contains a cycle and the walk never terminates.
That is what it did: `save_status` hung forever, so its promise never resolved,
the Play button did nothing at all, and a thread span at 100% for as long as the
app stayed open.

Refusing to follow them is also simply the right answer. What a prefix points
*out* at is the user's own files, which are not this game's save data; what it
*contains* — `AppData` above all — is. The install-directory scan in
`launcher.rs` refuses them for the same reason, and both walks carry a depth cap
as a second line of defence.

**The consequence, stated plainly:** a PC game that saves into `My Documents`
rather than `AppData` writes *outside* its prefix, through one of those
symlinks, and is therefore not captured. Wine can be configured to make those
profile folders real directories inside the prefix — `winecfg` → Desktop
Integration, or deleting the symlink and creating a directory in its place —
which brings such a game's saves back inside and into sync.

**Nothing heavy runs on the async runtime.** Walking a save tree and gzipping it
are handed to `spawn_blocking`. "Fast" is a property of the disk rather than of
this code, and a command that blocks a runtime worker stalls every other command
alongside it — which is how one slow walk became an app-wide freeze rather than
one slow button.

**Archives are positional.** Entries are stored relative to a root that
restoring puts them back under — `nand/user/save/<...>/<title id>/...` rather
than an absolute path — so an archive made on one machine lands correctly on
another whose emulator directory is somewhere else entirely.

**When it syncs.** Before launch, if the server's save is newer, and after the
game exits. Detecting the exit is why `launch_game` supervises the process it
spawns; the game's lifetime is still not tied to the app, the thread only
observes. For Wine the child is the wrong thing to wait on — `wine game.exe`
often returns long before the game does — so it additionally waits on
`wineserver -w` against that game's prefix, which is only meaningful *because*
each game has its own.

**Saves that land outside the prefix.** Wine's Desktop Integration points a
prefix's `Documents`, `Saved Games` and friends at the real home directory, and
the archive deliberately refuses to follow those links — what a prefix points
*out* at is the user's own files, not this game's save data, and following them
once meant archiving an entire home directory and then looping, since the
prefix lives under it. The consequence is that a game saving to Documents has
its save quietly left behind, with nothing about the sync looking wrong. Opening
such a game's detail view now says so, names the folders, and points at the
winecfg setting that fixes it. A link that stays inside the prefix, or a
dangling one, is not reported: neither is a hole.

**The save token.** If the server has `SAVE_TOKEN` set, put the same value in
Settings under cloud saves and every save request carries it as a bearer token.
Empty on both sides by default. The save endpoints are the only writable surface
the server exposes, and the only one carrying data that isn't simply a copy of
what is already on the drive.

**Conflicts.** A save only on the server restores without asking, since there is
nothing local to lose. A server save that is newer *than a local one* prompts,
showing both timestamps and which machine the remote came from. If a restore
fails, the launch is blocked rather than allowed to proceed — opening the game
would overwrite the newer save with the older one, which is the exact outcome
the feature exists to prevent. Restoring always moves the existing local save
aside as a `.bak-<timestamp>` directory rather than deleting it, and keeps the
three most recent of those. They were originally kept forever, on the reasoning
that a save costs kilobytes — true of a console save and not at all of a PC
prefix's user directory, which can be hundreds of megabytes and gets another
copy every restore. Three still covers what the backups are for, which is
noticing within a session or two that the wrong version came down.

The server keeps the last ten versions per game and ignores a re-upload whose
contents are identical, so quitting a game without playing doesn't push real
history out.

### Encoded slashes in game ids

`Game.id` looks like `"Switch/198X"` — a real `/` in it. Percent-encoding the
whole id for the URL would turn that into `%2F`, which isn't reliably treated
as a path separator by the time it reaches Flask's routing. `encode_path_segments`
in `install_state.rs` encodes each segment independently and rejoins them with a
literal `/`, so the separator survives and only the title text gets encoded:

```
Switch/198X       -> Switch/198X
PC/Moth & Ember   -> PC/Moth%20%26%20Ember
```

### The frontend is dual-mode on purpose

`frontend/index.html` opens fine as a plain file in a normal browser, because
every Tauri call is guarded behind a constant that's `null` outside a Tauri
webview:

```js
const TAURI = window.__TAURI__ || null;
```

Settings stay at their in-memory defaults in that mode, the catalog falls back
to mock data, and the Browse button quietly no-ops. This means the UI can be
iterated on in a browser tab without a Tauri build for every visual change.
`docs/` at the repo root is a deploy of exactly this mode; see `docs/README.md`
for the small set of demo-only differences that copy carries.

---

## Build and run

### 1. The library server, first

The Tauri app is useless without it. On the machine with the mounted drive
(`../server/`):

```
cd server
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

Edit `config.py` — set `LIBRARY_ROOT` to the actual mount point. Run it
directly to confirm it works:

```
.venv/bin/python server.py
```

Then `curl http://<that machine's LAN IP>:8420/library` from another machine on
the network — you should get back JSON. If this doesn't work, nothing
downstream will either, so don't move on until it does. For the
"constantly running" setup this is meant for, install `games-galore-server.service` as
a systemd unit instead of running it by hand (see that project's README).

### 2. Prerequisites

**Rust: use `rustup`, not your distro's package.** Ubuntu's packaged `rustc`
(1.75 at time of writing) is too old for current dependency releases, which
increasingly require newer Cargo editions. Get a current toolchain from
<https://rustup.rs> rather than `apt install rustc cargo`.

**System webview libraries (Linux).** Tauri wraps the OS's native webview
rather than bundling Chromium, so it needs that webview's dev headers at build
time. Package names differ by distro — nothing else in this project bakes in a
distro assumption, so use whichever matches yours.

Debian/Ubuntu:
```
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget \
  file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Arch:
```
sudo pacman -S --needed webkit2gtk-4.1 base-devel curl wget file \
  openssl appmenu-gtk-module libappindicator-gtk3 librsvg xdotool
```

(macOS needs Xcode's command line tools; Windows needs the MSVC build tools and
WebView2, which ships with Windows 10/11 by default.)

**The Tauri CLI.** This project has no npm dependencies at all — the frontend is
plain HTML/CSS/JS with no build step — so there's no need for Node.js here.
Install the CLI as a Cargo subcommand instead:

```
cargo install tauri-cli --locked
```

### 3. NVIDIA: set these first

WebKitGTK's DMA-BUF renderer has a well-documented Wayland crash on NVIDIA
drivers (`Gdk-Message: Error 71 (Protocol error) dispatching to Wayland
display`). It was never confirmed which specific workaround fixes it, so the
safe move is baking in all of the harmless ones rather than relying on memory
at every launch:

```
mkdir -p ~/.config/environment.d
cat >> ~/.config/environment.d/webkit-nvidia.conf << 'EOF'
WEBKIT_DISABLE_DMABUF_RENDERER=1
__NV_DISABLE_EXPLICIT_SYNC=1
EOF
```

Log out and back in for this to take effect — `environment.d` is read by
`systemd --user` at session start, so it applies to the whole graphical session,
including a build launched by double-clicking an icon later, not just things run
from a terminal. If Wayland issues persist, check whether `nvidia_drm.modeset=1`
is set as a kernel boot parameter (needed on driver versions older than 545),
and try `GDK_BACKEND=x11` to force XWayland for just this app.

### 4. Run and build

```
cd app/src-tauri
cargo tauri dev
```

First run will take a while — it's compiling the whole dependency graph,
including the webview bindings. Since `frontendDist` points directly at the
static `frontend/` folder with no bundler in between, editing
`frontend/index.html` and reloading the window picks up changes immediately.

Once `cargo tauri dev` works:

```
cargo tauri build
```

`bundle.targets` in `tauri.conf.json` is set to `["appimage"]` specifically for
Arch — the default `"all"` would also attempt `.deb` and `.rpm` packaging, which
need `dpkg-deb`/`rpmbuild` that Arch doesn't ship by default and would likely
fail partway through. The result lands under
`target/release/bundle/appimage/`. Since the NVIDIA environment variables above
are session-wide via `environment.d`, the built AppImage picks them up too — no
separate flags needed versus `cargo tauri dev`.

To build both formats anywhere that has `dpkg-deb` — Debian, Ubuntu, or CI —
override that setting on the command line rather than editing it:

```
cargo tauri build --bundles deb,appimage
```

### Cutting a release

Bump `version` in `tauri.conf.json` (and `Cargo.toml`, which should match), then:

```
git tag v0.2.0
git push origin v0.2.0
```

`.github/workflows/release.yml` builds a `.deb` and an AppImage on Ubuntu 22.04
— the oldest runner that still carries the webview headers, so the binaries run
on distributions older than the newest — and attaches them to a **draft**
release for you to check and publish. The workflow refuses to build if the tag
and the configured version disagree.

---

## First-run configuration

Nothing works until you open Settings (the gear at the bottom of the sidebar)
and fill in:

- **Library server address** — the library server machine's LAN address, e.g.
  `http://192.168.1.20:8420`
- **Install directory** — use the Browse button; it's a real native folder picker
- **Each emulator's command/args** — see below

Then use the **Check** button next to each emulator (and the one next to
"Library server (nsz)") to confirm what you configured actually resolves, before
trying to install or launch anything for real.

### Emulators, especially Flatpaks

Emulator invocation is configuration, not hardcoded: each platform has an
`EmulatorConfig` in `settings.rs` with `command`, `args_prefix` and
`version_flag`, and `launcher.rs` reads it at launch time. `args_prefix` is
inserted before the platform's own fullscreen/path arguments, which stay fixed
in `launcher.rs` (DuckStation's `-fullscreen -batch --` doesn't change based on
how DuckStation was installed).

For a native binary on PATH, Command is just the binary name (`duckstation-qt`,
`wine`, etc.) and Args stays empty — the defaults already assume this. For a
Flatpak — likely for PCSX2 and Eden, per how those are commonly packaged — set:

- **Command:** `flatpak`
- **Args:** `run <app-id> --`, e.g. `run net.pcsx2.PCSX2 --`

Find the exact app-id with `flatpak list --app` on that machine. The trailing
`--` matters: it's Flatpak's own separator ending its option parsing, distinct
from whatever separator the emulator itself wants afterward. Dropping it means
Flatpak may try to interpret the emulator's flags as flags meant for
`flatpak run` itself. A Flatpak PCSX2 ends up invoked as:

```
flatpak run net.pcsx2.PCSX2 -- -fullscreen -batch -- <path>
```

where the first `--` is Flatpak's and the second is PCSX2's.

The Switch default is deliberately left as an obvious placeholder rather than a
real binary name. Eden ships multiple builds with genuinely different CLI
conventions (a "standard" AppImage taking a bare positional path with `-f` for
fullscreen, vs. a separate `eden-cli` build using `--game`/`--fullscreen`), and
`platform_args()` is written for the AppImage shape — so a plausible-looking
default would silently send the wrong flags to anyone on a different build.

**Flatpak dependency checks ask a different question.** `flatpak --version` only
confirms Flatpak itself is installed, not whether a particular app's Flatpak is
present, so checking a Flatpak-configured emulator that way would report "found"
even when the emulator isn't installed at all. `check_dependency` branches on
`command == "flatpak"` and runs `flatpak info <app-id>` instead, pulling the
app-id out of the same `args_prefix` the launch command uses — so there's
exactly one place per platform where that id is typed.

---

## Configuration files

### `tauri.conf.json`

- No `devUrl` / `beforeDevCommand` — the frontend has no build step, so
  `frontendDist: "../frontend"` is served directly in both dev and build. If a
  bundler is ever introduced, this is the first thing that needs to change.
- `"app": { "withGlobalTauri": true }` exposes `window.__TAURI__` as a plain
  global, since there's no `import` machinery to pull `@tauri-apps/api` from npm.
  The frontend reads it as `window.__TAURI__.core.invoke(...)` and
  `window.__TAURI__.event.listen(...)`.

  The folder picker is the one exception: `@tauri-apps/plugin-dialog` ships as a
  real ES module, not a global-attaching script, so `window.__TAURI__.dialog`
  never exists here. The frontend calls the plugin's underlying command
  directly — `invoke('plugin:dialog|open', { options })` — which is what that
  package's own `open()` does internally anyway.
- `bundle.icon` points at the set in `icons/`: `32x32.png`, `128x128.png`,
  `128x128@2x.png`, a genuine multi-resolution `icon.ico` (16–256px) and a
  genuine multi-resolution `icon.icns`, generated by `@tauri-apps/cli icon` from
  a rounded diamond in the app's own pink-violet gradient. It's a placeholder in
  the sense that nobody designed it, not that it's unfinished. Swap the source
  image and re-run `tauri icon <path>` from `src-tauri/` when there's a real
  design; everything downstream already expects exactly this file set.
- `security.csp: null` disables Tauri's content security policy entirely. Fine
  while everything is same-origin with no remote or untrusted content in the
  webview; worth tightening if that changes.

### `capabilities/default.json`

Custom `#[tauri::command]` functions don't need a capability entry — Tauri v2
only gates *plugin* and *core* JS APIs this way. This file exists for the two
things the frontend calls through the injected `__TAURI__` global:
`core:event:default` (so `listen('install:status', ...)` is allowed) and
`dialog:default` (so the folder picker works). `dialog:default` is the plugin's
broad permission set — worth narrowing to just the open-folder permission once
you've confirmed that's the only dialog capability in use.

---

## Known gaps

- **One HTTP request per file.** A large PC game is thousands of files and
  therefore thousands of requests. Correct, and fine on a LAN, but an archive
  endpoint that streamed a whole title in one response would be considerably
  faster if this turns out to drag.
- **No resume.** An install interrupted partway starts over from the first
  file; already-complete files are downloaded again. The server supports range
  requests, so the pieces for resuming are there, but nothing uses them yet.
- **No content hashing.** Transfers are checked for length, not for
  correctness: a file that arrives complete but corrupted passes. A hash per
  file in the catalog would close that, at the cost of the server hashing every
  file it scans.
- **PC saves are the prefix's user directory only.** A game that writes its
  save next to its own executable puts it in the install directory instead,
  which is not archived. Nothing detects that case — unlike the linked-out
  profile folders above, which are now reported.
- **Sync-on-exit needs the app running.** If Games Galore is closed while a
  game is still open, nothing observes the exit and that session's save is not
  uploaded until the next time the game is launched and quit.

## Verification status

The Rust sources now type-check end-to-end: `cargo check` passes clean on Rust
1.94 with no errors and no warnings. An earlier attempt had run aground on a
toolchain too old for the dependency graph's current requirements, which is what
the `rustup` note above is about; that is no longer a live problem on a current
toolchain.

`cargo test` covers, against real temporary directories where files are
involved: `launcher.rs`'s file resolution — the PC executable search across
subdirectories, its exclusion of installers, the PS1/PS2 `.cue` rule, stability
of the Switch pick, argument quoting — and `install_state.rs`'s progress
arithmetic, path-segment encoding, and error-detail extraction, plus
`catalog_cache.rs`'s merge rules — live entries winning over cached ones, an
installed title the server no longer lists surviving, an uninstalled one being
dropped, and a missing or corrupt cache reading as empty rather than erroring. Run both from
`src-tauri/`. Note that `cargo check` needs the system webview headers listed
under prerequisites even though it never links a GUI.

The frontend is exercised against a real DOM under jsdom with a mocked Tauri
runtime — including the launch picker end to end: that it appears only for a
title with several candidates, offers them best-first, persists a choice,
passes it to `launch_game`, drops an override that merely restates the default,
disappears on uninstall, keeps the arrow-key chain free of dead steps whether
or not it is showing, and leaves Play working when the candidate lookup fails
outright.

Settings has its own suite: that arrowing through the modal reaches every
control including the ones added since the navigation was written, that it never
lands on something hidden or disabled, that it stops at both ends rather than
escaping the modal, that a text field can be left in any direction — the thing a
D-pad could not do before — and that Escape from a field commits what was typed
before closing.

Startup is covered the same way, against a deliberately slow `fetch_library`:
that the cached shelf is drawn before the fetch resolves, that the cache is
read before the server is asked, that the full catalog replaces it cleanly,
that an unreachable server leaves the shelf standing, that an empty cache
behaves exactly as before, that a cache whose titles are no longer installed
doesn't strand the user behind a filter, and that toggling that filter
mid-refresh isn't overridden when the catalog lands.

The server side's scanning and routes are covered separately by fixture tests
that build real game folders, including a simulated install that fetches every
file the catalog lists and checks the reconstructed tree byte-for-byte against
the source.

The frontend has been exercised harder: the full mock-mode flow (load → open a
game → install → uninstall → back → open settings → toggle sound → close) plus
the emulator rows and Check buttons were executed against a real DOM under
jsdom, not merely syntax-checked. That distinction caught real bugs a syntax
check cannot — notably a `ReferenceError` from registering the `install:status`
listener at top level, before `TAURI`'s own `const` declaration further down the
file.
