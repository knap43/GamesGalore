# Games Galore backend (Rust/Tauri)

The desktop client: a Tauri app wrapping the plain HTML/CSS/JS frontend in
`frontend/`, talking to the Python library server (`../vault-server/`) over
HTTP. It has no local knowledge of where the library lives — only a base URL
from settings — and never touches the library filesystem or runs `nsz` itself.

## Architecture

| File | Responsibility |
| --- | --- |
| `server.rs` | Fetches the catalog (`GET /library`) and the server's `nsz` status (`GET /status`). `Game` and `GameFile` mirror the server's JSON shape exactly. |
| `install_state.rs` | The local record of what's on this machine (`installs.json` in the app data dir, keyed by `Game.id`), plus `install_game`, `uninstall_game` and `cancel_install`. |
| `launcher.rs` | `launch_game` — spawns the configured emulator for a platform, detached, in fullscreen. Resolves which file to hand it by searching the install directory recursively, and `list_launch_candidates` backs the UI's picker for titles with more than one; see below. |
| `dependencies.rs` | `check_dependency` — whether a configured emulator is actually present, so a missing tool surfaces in Settings rather than mid-Play. Flatpak-aware; see below. |
| `settings.rs` | `settings.json` alongside `installs.json`: server address, install root, sound preference, per-platform emulator config, and any per-game launch overrides. |

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
(`../vault-server/`):

```
cd vault-server
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
"constantly running" setup this is meant for, install `vault-server.service` as
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
cd tauri-backend/src-tauri
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

---

## First-run configuration

Nothing works until you open Settings (the gear at the bottom of the sidebar)
and fill in:

- **Library server address** — the vault-server machine's LAN address, e.g.
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
- **Playtime tracking.** Deliberately deferred, not an oversight. The "Recently
  played" and "Playtime" sort options work for mock/browser-preview games but do
  nothing for real ones, since no `hours`/`lastPlayedDaysAgo` field exists for
  them and nothing records playtime. Hooking into `launch_game` to time a
  session is a real feature, not a wiring gap; it's a harmless no-op until
  someone wants it.
- **Settings keyboard navigation.** The emulator inputs and Check buttons are
  standard Tab-navigable elements but aren't wired into the custom
  arrow-key/gamepad navigation chain the rest of the app uses — a lot of added
  complexity for a screen you configure once. Worth reconsidering if that
  assumption turns out wrong.

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
arithmetic, path-segment encoding, and error-detail extraction. Run both from
`src-tauri/`. Note that `cargo check` needs the system webview headers listed
under prerequisites even though it never links a GUI.

The frontend is exercised against a real DOM under jsdom with a mocked Tauri
runtime — including the launch picker end to end: that it appears only for a
title with several candidates, offers them best-first, persists a choice,
passes it to `launch_game`, drops an override that merely restates the default,
disappears on uninstall, keeps the arrow-key chain free of dead steps whether
or not it is showing, and leaves Play working when the candidate lookup fails
outright.

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
