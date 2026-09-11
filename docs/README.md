# Games Galore — live demo

This folder is a self-contained, standalone build of the frontend, meant to be
hosted directly via GitHub Pages so people can click around the UI without
installing anything.

It's the exact same `index.html` as the real app — nothing was forked or
reimplemented separately. The app already had a browser-preview fallback path
built in (`const TAURI = window.__TAURI__ || null`), originally just for
testing the UI without the Tauri backend running. Opening this file directly
in a browser means that object is never defined, so every Tauri-specific call
throughout the app automatically takes its "preview mode" branch instead:
mock game data, a simulated install progress sequence, and a message
explaining that launching is disabled, rather than actually downloading files
or spawning emulators.

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
