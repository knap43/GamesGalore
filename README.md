# GamesGalore
A game library front-end that connects to a self-hosted server (server included).

Disclaimer: the code in its entirety was written by an LLM — use it at your own discretion.

This is still kind of a WIP, as I wasn't able to resolve some bugs. Known gaps
are listed at the bottom of each subproject's README.

### Layout

| Path | What it is |
| --- | --- |
| `app/` | The desktop client — a Tauri app around a single-file HTML/CSS/JS frontend, plus its Rust backend and test suites. |
| `server/` | The Python library server that scans the drive and serves the catalog, the game files and the cloud saves. |
| `docs/` | The GitHub Pages demo: the app's frontend, generated with its Tauri calls stubbed out. |
| `packaging/` | A `PKGBUILD` and desktop entry, for installing the client as an Arch package. |

### Installing

There are two halves and they go on different machines: the **server**, on
whatever box holds the games, and the **client**, on the machine you play from.
Either works without the other being finished — the client just shows an empty
shelf until it has a server address.

#### The server, on the machine with the library

```sh
cd server
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

Edit `config.py`: point `LIBRARY_ROOTS` at the mount holding the games — it is a
list, so a second drive is one more line when the first fills up — and set
`SAVE_ROOT` to somewhere you actually back up, since it holds the only copy of
your cloud saves. Then:

```sh
.venv/bin/python server.py          # http://0.0.0.0:8420
```

For the always-on setup this is meant for, `games-galore-server.service` is
included: adjust `WorkingDirectory` and `User`, drop it in
`/etc/systemd/system/`, then `systemctl enable --now games-galore-server`.

Switch titles stored as `.nsz` need [`nsz`](https://pypi.org/project/nsz/) on
this machine — it is in `requirements.txt`, and only the server ever needs it.
`GET /status` reports whether it was found.

#### The client, on Arch

```sh
cd packaging
makepkg -si                          # --nocheck skips the test suite
```

pacman then owns the files and the launcher appears in your applications menu.
The PKGBUILD builds the checkout it sits in, so whatever you have on disk is
what gets packaged.

#### The client, anywhere else

You need a current Rust toolchain from [rustup](https://rustup.rs) — distro
packages are often too old — and the system webview headers:

```sh
# Debian/Ubuntu
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Then either build a bundle:

```sh
cargo install tauri-cli --locked
cd app/src-tauri && cargo tauri build      # AppImage in target/release/bundle/
```

or skip the bundler and build the binary alone, which is self-contained — the
frontend is compiled into it:

```sh
cd app/src-tauri && cargo build --release  # target/release/games-galore
```

On Arch the bundler also wants `fuse2` installed alongside the system's fuse3,
which is the usual reason an AppImage build stops after producing the binary.
The package above avoids the question entirely.

#### First run

Open Settings (the gear at the bottom of the sidebar) and fill in the library
server's address — `http://<that machine>:8420` — and an install directory. More
than one directory can be added when one drive runs out: games already installed
stay where they are, and new ones go to whichever drive has the most room. The
library server takes a list of drives too, in its own `config.py`.

Settings also has a **View logs** button, which shows what this session has
printed — the command each game was launched with, where each install went,
anything that went wrong, and the running game's own output — whether or not the
app was started from a terminal.
Nothing works until those two are set. The platforms are PS1 (DuckStation), PS2
(PCSX2), PS4 (shadPS4), PC (Wine or Proton) and Switch (Eden). Emulator commands for each platform have
sensible defaults and are worth checking if you use Flatpaks. PC titles run
through Wine by default and can be switched to Proton in the same row, which
needs `umu-launcher` from `extra` and no Steam at all.

#### Filling in covers and descriptions

A game's folder can carry a `README.md`, a cover, screenshots, a trailer and a
`game.json` of genre and tags, all of which the app displays — and writing those
by hand for a large library is nobody's idea of an evening. They can be fetched
from [RAWG](https://rawg.io/apidocs) instead:

- **In the app.** Settings → *Game metadata*: paste a free RAWG key and press
  **Fetch metadata**. It works through every game missing a description or a
  cover, one at a time, saying which one it is on. A single game can also be
  filled in from its own page, with **Fetch details**.
- **From the command line**, with no app running:
  `RAWG_API_KEY=... .venv/bin/python metadata.py` in `server/`.

The key is sent to the library server with each request, so the server does not
need one of its own — though `RAWG_API_KEY` in its environment works too, and is
what the command line uses.

**Nothing already in a game's folder is overwritten.** Every existing file is
skipped, so a run after adding a few titles only touches the new ones, and a
folder you curated by hand stays as you left it. `--overwrite` (or `?overwrite=1`)
is there for data that is wrong rather than missing.

`app/README.md` and `server/README.md` go into all of this properly, including
NVIDIA notes, the emulator configuration and the known gaps.

### Demo:
https://knap43.github.io/GamesGalore/

### Screenshots:
<img width="2664" height="1704" alt="image" src="https://github.com/user-attachments/assets/eafddd76-e406-4fbd-851b-1d035a8687a9" />
<img width="2664" height="1704" alt="image" src="https://github.com/user-attachments/assets/6a5f94cf-0688-4e6c-a7cc-69b64b8bc360" />

### License

[MIT](LICENSE)
