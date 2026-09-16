# Games Galore — live demo

This folder is a self-contained, standalone build of the frontend, meant to be
hosted directly via GitHub Pages so people can click around the UI without
installing anything.

## Its relationship to the real frontend

It started as a byte-identical copy of the app's own
`games-galore/vault/tauri-backend/frontend/index.html` and is still
substantially that file, so the UI, layout and navigation you see here are the
real ones rather than a reimplementation. It is no longer identical, though:
this copy carries a small set of demo-only changes, and the two files have to be
re-synced by hand whenever the app's frontend changes.

What this copy adds on top of the app's version:

- A banner across the top saying plainly that installs and playtime are
  simulated.
- `simulateDemoInstall()`, which plays out a realistic multi-file progress
  sequence. The app's own preview fallback just flips the game to "installed"
  instantly, which would skip straight past the progress bar and cancel button —
  both real, deliberate parts of the UI worth actually showing off here.
- A brief "Would launch via your emulator ↗" message on the Play button. The
  app's version silently no-ops outside Tauri, which reads as a broken button to
  a visitor who has no reason to expect otherwise.

Everything else about the dual-mode behaviour comes from the app itself, not
from this copy. The frontend already guards every Tauri call behind
`const TAURI = window.__TAURI__ || null`, originally so the UI could be iterated
on in a browser tab without a Tauri build. Opening this file directly means that
object is never defined, so each of those calls takes its preview branch and
falls back to the mock catalog.

## Enabling it

1. Push this repo to GitHub with this `docs/` folder at the root.
2. In the repo on GitHub: **Settings → Pages → Build and deployment → Source:
   "Deploy from a branch"**, then set the branch to `main` (or whichever is
   your default) and the folder to **`/docs`**.
3. GitHub will publish it at `https://<your-username>.github.io/<repo-name>/`
   within a minute or two.

## What's real vs. simulated here

- **Real:** the entire UI — grid virtualization, the detail view, search and
  platform filtering, settings, sound, and full keyboard/gamepad navigation
  all behave exactly as they do in the desktop app, because it's the same
  code.
- **Simulated:** the game catalog is fixed mock data (not your real library),
  installing plays out a realistic-looking progress sequence but doesn't
  download anything, and "Play" shows a brief message instead of launching an
  emulator, since a static webpage has no way to do either of those things —
  they require the actual Tauri desktop app and its Python server.
