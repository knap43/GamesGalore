# Games Galore backend (Rust/Tauri) — HTTP client for the Python library server

## How to build and run this

### 1. The library server, first

The Tauri app is useless without it. On the machine with the mounted
drive (`../vault-server/`):

```
cd vault-server
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

Edit `config.py` — set `LIBRARY_ROOT` to the actual mount point.
Run it directly to confirm it works:

```
.venv/bin/python server.py
```

Then `curl http://<that machine's LAN IP>:8420/library` from another
machine on the network — you should get back JSON. If this doesn't
work, nothing downstream will either, so don't move on until it does.
For the "constantly running" setup this is meant for, install
`vault-server.service` as a systemd unit instead of running it by hand
(see that project's README for the exact steps).

### 2. Prerequisites for the Tauri app

**Rust: use `rustup`, not your distro's package.** I hit this directly
while verifying this project — Ubuntu's packaged `rustc` (1.75 at time
of writing) is too old for current dependency releases, which
increasingly require newer Cargo editions. Get a current toolchain from
<https://rustup.rs> rather than `apt install rustc cargo`.

**System webview libraries (Linux).** Tauri wraps the OS's native
webview rather than bundling Chromium, which means it needs that
webview's dev headers at build time. Package names differ by distro —
this project has no distro assumption baked in anywhere else, so use
whichever matches yours:

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

(macOS needs Xcode's command line tools; Windows needs the MSVC build
tools and WebView2, which ships with Windows 10/11 by default.)

**The Tauri CLI.** This project has no npm dependencies at all — the
frontend is plain HTML/CSS/JS with no build step — so there's no need
for Node.js here. Install the CLI as a Cargo subcommand instead:

```
cargo install tauri-cli --locked
```

### 3. Run it

**On NVIDIA, set these first** — WebKitGTK's DMA-BUF renderer has a
well-documented Wayland crash on NVIDIA drivers (`Gdk-Message: Error 71
(Protocol error) dispatching to Wayland display`), which came up
earlier in this project. Since it was never confirmed which specific
workaround actually fixed it, the safe move is baking in all of the
harmless ones rather than relying on memory every time you launch:

```
mkdir -p ~/.config/environment.d
cat >> ~/.config/environment.d/webkit-nvidia.conf << 'EOF'
WEBKIT_DISABLE_DMABUF_RENDERER=1
__NV_DISABLE_EXPLICIT_SYNC=1
EOF
```

Log out and back in for this to take effect — `environment.d` is read
by `systemd --user` at session start, so it applies to anything in
your graphical session, including a build launched by double-clicking
an icon later, not just things run from a terminal. If Wayland issues
persist after that, the next things to check are whether
`nvidia_drm.modeset=1` is set as a kernel boot parameter (needed on
driver versions older than 545), and `GDK_BACKEND=x11` as a fallback
to force XWayland for just this app.

```
cd tauri-backend/src-tauri
cargo tauri dev
```

First run will take a while — it's compiling the whole dependency
graph, including the webview bindings. Since `frontendDist` points
directly at the static `frontend/` folder with no bundler in between,
editing `frontend/index.html` and reloading the window picks up changes
immediately; no separate frontend build step to re-run.

**For the actual build**, once `cargo tauri dev` is working correctly:

```
cargo tauri build
```

`bundle.targets` in `tauri.conf.json` is set to `["appimage"]`
specifically for Arch — the default `"all"` would also attempt `.deb`
and `.rpm` packaging, which need `dpkg-deb`/`rpmbuild` that Arch
doesn't ship by default and would likely just fail partway through.
The result lands at
`target/release/bundle/appimage/games-galore_0.1.0_amd64.AppImage`
(exact filename depends on the version in `tauri.conf.json`). Since
the NVIDIA environment variables above are session-wide via
`environment.d`, the built AppImage picks them up automatically too —
no separate flags needed when launching it versus `cargo tauri dev`.

### 4. First-run configuration

Nothing works until you open Settings (the gear at the bottom of the
sidebar) and fill in:

- **Library server address** — the vault-server machine's LAN address,
  e.g. `http://192.168.1.20:8420`
- **Install directory** — use the Browse button; it's a real native
  folder picker
- **Each emulator's command/args** — see below

Then use the **Check** button next to each emulator (and the one next
to "Library server (nsz)") to confirm what you configured actually
resolves, before trying to install or launch anything for real.

### 5. Configuring emulators, especially Flatpaks

For a native binary on PATH, Command is just the binary name
(`duckstation-qt`, `wine`, etc.) and Args stays empty — the defaults
already assume this. For a Flatpak — likely for PCSX2 and Eden, per
how those are commonly packaged — set:

- **Command:** `flatpak`
- **Args:** `run <app-id> --`, e.g. `run net.pcsx2.PCSX2 --`

Find the exact app-id with `flatpak list --app` on that machine if
you're not sure of it. The trailing `--` matters — it's Flatpak's own
separator ending its option parsing, distinct from whatever separator
the emulator itself wants afterward; dropping it means Flatpak may try
to interpret the emulator's own flags as flags meant for `flatpak run`
itself.

---

## Third pass: Python server split

With the library moved behind a separate Python server
(`../vault-server/`), this side no longer touches any filesystem or
mount directly. It's now a thin HTTP client plus local install-state
bookkeeping.

## What each file does

- `server.rs` — fetches the catalog (`GET /library`) and the server's
  `nsz` status (`GET /status`) from the Python server. `Game` and
  `GameFile` mirror the server's JSON shape exactly (`files` is a list
  because Switch titles can have several — base game, update, DLC —
  each independently already-`.nsp` or needing conversion, though the
  client never has to know which; see below).
- `install_state.rs` — the local record of what's actually on this
  machine (`installs.json` in the app's data dir, keyed by `Game.id`),
  plus `install_game` and the new `uninstall_game` that the UI's
  Uninstall button now calls.
- `dependencies.rs` — checks whether a configured emulator (or the
  server's `nsz`, via `fetch_server_status` instead) is actually
  present, so a missing tool shows up in Settings rather than mid-Play.
  As of the fifth/sixth pass, this is Flatpak-aware — see below — since
  PCSX2 and Eden are commonly installed that way.

## Why installing got simpler, not just moved

The Python server already resolves any `.nsz` → `.nsp` conversion
before a file is ever sent over the wire. That means `install_game`
doesn't need a Switch-specific code path at all anymore — it just
downloads every file in `game.files` the same way regardless of
platform, streaming each to disk and reporting progress as it goes. If
the requested file happened to be `.nsz`, what actually arrives is
already a `.nsp`; the client renames it accordingly and never invokes
anything.

## A bug worth flagging, since I initially got it wrong

`Game.id` looks like `"Switch/198X"` — a real `/` in it. Naively
percent-encoding the whole id for the URL would turn that into `%2F`,
which isn't reliably treated as a path separator by the time it
reaches Flask's routing (this is a known ambiguity with encoded
slashes in URLs generally). `encode_path_segments` in
`install_state.rs` encodes `"Switch"` and `"198X"` independently and
rejoins them with a literal `/`, so the separator survives and only
the actual title text gets percent-encoded. Verified directly against
the `urlencoding` crate:

```
Switch/198X       -> Switch/198X
PC/Moth & Ember   -> PC/Moth%20%26%20Ember
```

## Wiring into the frontend

```js
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

const SERVER = 'http://<laptop-ip>:8420'; // from settings, not yet built

async function loadGames() {
  return await invoke('fetch_library', { serverBase: SERVER });
}

async function loadInstallStates() {
  return await invoke('get_install_states');
}

listen('install:status', (event) => {
  const [id, status] = event.payload;
  // status.status: "not_installed" | "downloading" | "installed" | "failed"
  // status.pct / status.file present while downloading
});

async function installGame(game, installRoot) {
  await invoke('install_game', { game, serverBase: SERVER, installRoot });
}

async function uninstallGame(gameId) {
  await invoke('uninstall_game', { gameId });
}
```

## Verification status, honestly

The three source files parse as syntactically valid Rust — checked
directly with `rustc`, which reported only the expected "crate not
found" errors for `tauri`/`reqwest`/`serde`/etc. and nothing else. I
also built a minimal stand-in for the exact Tauri surface this code
touches (`AppHandle`, `Manager`, `Emitter`, the `#[command]` attribute)
to attempt a full `cargo check` against the real `reqwest`/`tokio`
dependency graph, but this sandbox's toolchain (Rust 1.75 via apt) is
old enough that current releases of `reqwest`'s own transitive
dependencies now require a newer Cargo edition than it supports —
pinning around it turned into chasing one dependency after another
with no end in sight, so I stopped rather than keep burning time on
toolchain archaeology. None of that reflects on the code itself; it
just means this hasn't been type-checked end-to-end, only reviewed and
syntax-verified. Worth a real `cargo check` in your actual project
before trusting it fully.

## Not yet built

- Aggregate progress across a multi-file game (currently reports
  per-file percentage, not overall bytes across all of a title's files)
- Playtime tracking — deliberately deferred, not an oversight. The
  "Recently played" and "Playtime" sort options stay in the UI and work
  correctly for mock/browser-preview games, but do nothing for real
  games (no `hours`/`lastPlayedDaysAgo` field exists for them yet) since
  nothing records playtime. That's a real feature — hooking into
  `launch_game` to time a session — not a wiring gap, and it's fine as
  a harmless no-op until it's actually wanted.

## Project scaffold — fourth pass

Both "not yet built" items above from the last pass are done now:
`tauri.conf.json`, `capabilities/default.json`, and a real
`settings.rs` backing the settings modal's server-address and
install-directory fields. The frontend also moved: `frontend/index.html`
is now the canonical copy of the game-library mockup, living inside
this project tree rather than as a separate file — this is what
`tauri.conf.json`'s `frontendDist` actually points at.

### tauri.conf.json

- No `devUrl` / `beforeDevCommand` — the frontend has no build step
  (plain HTML/CSS/JS), so `frontendDist: "../frontend"` is served
  directly in both dev and build. If a bundler gets introduced later,
  this is the first thing that needs to change.
- `"app": { "withGlobalTauri": true }` exposes `window.__TAURI__` as a
  plain global in the webview, since there's no `import` machinery to
  pull `@tauri-apps/api` from npm. `frontend/index.html` reads it as
  `window.__TAURI__.core.invoke(...)` and `window.__TAURI__.event.listen(...)`
  — both part of the core API package `withGlobalTauri` actually exposes.
  The folder picker is the one exception: `@tauri-apps/plugin-dialog` is
  a separate npm package shipped as a real ES module, not a
  global-attaching script, so `window.__TAURI__.dialog` was never going
  to exist here. The frontend calls the plugin's underlying command
  directly instead — `invoke('plugin:dialog|open', { options })` — which
  is what that package's own `open()` does internally anyway.
- **`bundle.icon` now points at a real generated set.** `icons/` held
  nothing when this was first scaffolded; it now has a full desktop set
  (`32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns`, `icon.ico`,
  plus a couple of extra sizes) generated by the actual `@tauri-apps/cli`
  `icon` command from a placeholder source image — a rounded diamond in
  the app's own pink-violet gradient on its dark background, not a
  generic default. It's a placeholder in the sense that nobody designed
  it, not in the sense that it's fake or half-finished — the `.ico` is a
  real multi-resolution file (16 through 256px), and the `.icns` is a
  real multi-resolution macOS icon. Swap the source image and re-run
  `tauri icon <path>` from `src-tauri/` whenever there's an actual
  design to use instead.
- `security.csp: null` disables Tauri's content security policy
  entirely. Fine while everything is same-origin and there's no
  remote/untrusted content loaded into the webview; worth tightening
  if that ever changes.

### capabilities/default.json

Custom `#[tauri::command]` functions (everything in `server.rs`,
`install_state.rs`, `settings.rs`, `dependencies.rs`) don't need a
capability entry — Tauri v2 only gates *plugin* and *core* JS APIs this
way. This file exists for two things the frontend actually calls
through the injected `__TAURI__` global: `core:event:default` (so
`listen('install:status', ...)` is allowed) and `dialog:default` (so
the folder-picker button works). `dialog:default` is the plugin's
broad permission set rather than a narrower one — worth tightening to
just the specific open-folder permission once you've confirmed that's
the only dialog capability actually in use.

### settings.rs

Same shape as `install_state.rs`'s `installs.json`: a small JSON file
in the app's data directory, read-modify-written on every change. The
sound toggle moved in here too — it only lived in frontend memory
before this pass, resetting on every reload, which was a real gap.

### The frontend is dual-mode on purpose

`frontend/index.html` still opens fine as a plain file in a normal
browser — how it's been iterated on throughout — because every Tauri
call is guarded behind a `TAURI` constant that's `null` outside an
actual Tauri webview:

```js
const TAURI = window.__TAURI__ || null;
```

Settings just stay at their in-memory defaults in that mode, and the
Browse button quietly no-ops instead of throwing. This means you can
keep iterating on the UI in a browser tab without needing a Tauri build
for every visual change, and it'll pick up real persistence the moment
it's actually running inside the app.

## Fifth pass — real data wiring, and closing out the icon blocker

Two things from the last round are resolved:

**The catalog and install status are no longer the mock array.**
`loadGames()` calls `fetch_library` and `get_install_states`, merges
them onto each game, and a live `install:status` listener keeps things
current while a download is in progress. `installGame`/`uninstallGame`/
`launchGame` call the real Rust commands. `launcher.rs` — the actual
`launch_game` command spawning DuckStation/PCSX2/Wine/Eden — didn't
exist before this pass despite being part of the original plan; it does
now. Real games also render actual `<img>`/`<video>` screenshots and
trailers instead of the synthesized placeholders, which now only appear
for mock/browser-preview games.

Getting there surfaced a few real bugs, caught by actually running the
code rather than just reading it:

- `window.__TAURI__.dialog` doesn't exist without a bundler —
  `@tauri-apps/plugin-dialog` ships as a real ES module, not a
  global-attaching script, so `withGlobalTauri` never exposes it.
  Confirmed by downloading the actual package and reading its source;
  fixed to call `invoke('plugin:dialog|open', { options })` directly,
  which is what that package's own `open()` does internally.
- The mock data used `"Nintendo Switch"`; the server and everything
  built around it uses `"Switch"`. Would have silently broken platform
  filtering the moment real data replaced the mock array.
- The PS1/PS2 size calculation was sizing the `.cue` file alone —
  a few hundred bytes — instead of summing it with the `.bin` that
  actually holds the disc data. Caught and fixed in `library.py` before
  it shipped, verified against a fixture.
- A leftover `Number(card.dataset.id)` from when IDs were purely
  numeric — would have turned every real game ID (`"Switch/198X"`) into
  `NaN` and broken every detail-view open.
- A genuine `ReferenceError`: the `install:status` listener setup ran
  at top level and referenced `TAURI` before its own `const`
  declaration further down the file. Caught by actually *executing* the
  frontend in a real DOM (jsdom) rather than just checking syntax —
  moved the listener registration into `init()`, which runs after
  everything is declared.

That last class of bug is exactly why the verification step for this
pass went further than a syntax check: the full mock-mode flow (load →
open a game → install → uninstall → back → open settings → toggle
sound → close) was actually executed against a real DOM and confirmed
error-free, including the specific install/uninstall state transitions
and the sidebar counts updating live.

**The icon blocker is closed, not just narrowed.** `icons/` now holds a
real desktop icon set — `32x32.png`, `128x128.png`, `128x128@2x.png`,
`icon.ico` (a genuine multi-resolution Windows icon, 16 through 256px),
and `icon.icns` (genuine multi-resolution macOS icon) — generated by
the actual `@tauri-apps/cli icon` command, not hand-rolled. The source
image is a simple rounded diamond in the app's own pink-violet gradient
on its dark background, so it's on-brand rather than a generic default;
it's a placeholder only in the sense that nobody designed it. Swap in a
real design and re-run `tauri icon <path>` from `src-tauri/` whenever
one exists — everything downstream already expects exactly this file
set, so nothing else needs to change.

Playtime tracking stays deliberately out of scope for now — see "Not
yet built" above.

## Sixth pass — configurable emulators, and closing the dependency-check gap

Two things flagged last round are done:

**Emulator launch config moved out of hardcoded Rust and into
Settings.** `launcher.rs` previously had a fixed table: `duckstation-qt`
for PS1, `pcsx2-qt` for PS2, `wine` for PC, `eden-cli` for Switch — no
way to override any of it. That assumption doesn't survive contact with
Flatpak installs, which are likely for at least PCSX2 and Eden. Each
platform now has a full `EmulatorConfig` in `settings.rs`: `command`,
`args_prefix` (inserted before the platform's own fullscreen/path args
— empty for a native binary, `["run", "<app-id>", "--"]` for a
Flatpak), and `version_flag`. `launcher.rs` reads this at launch time
instead of consulting its own table. The Settings UI exposes all three
fields per platform directly, with inline guidance on the Flatpak shape
specifically since that's the case most likely to need it.

**`check_dependency` and `fetch_server_status` are no longer dead
code.** Both existed since early in this project but were never called
from anywhere. Settings now has a "Check" button per emulator plus one
for the server's `nsz`, calling exactly these. The dependency check
itself needed a real fix, not just a UI hookup: `flatpak --version`
only confirms Flatpak itself is installed, not whether any particular
app's Flatpak is present — checking a Flatpak-configured emulator that
way would report "found" even when the actual emulator isn't installed
at all. `check_dependency` now branches on whether `command == "flatpak"`
and, if so, runs `flatpak info <app-id>` instead — pulling the app-id
out of the same `args_prefix` value the launch command itself uses, so
there's exactly one place per platform where that id needs to be typed.

Both changes were executed in jsdom against a real DOM, not just
syntax-checked — all four emulator rows render with correct defaults,
editing a field and firing `change` updates state without throwing, and
every "Check" button no-ops cleanly in browser-preview mode rather than
crashing on a missing `TAURI` global.

One deliberate scope line: the new Settings fields (emulator inputs,
Check buttons) are standard Tab-navigable elements but aren't wired
into the custom arrow-key/gamepad navigation chain the rest of the app
uses. Extending that chain across five rows of three fields each felt
like a lot of added complexity for a screen you'll configure once and
rarely revisit — worth reconsidering if that assumption turns out
wrong in practice.
