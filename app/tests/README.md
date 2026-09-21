# Frontend tests

The app is one HTML file with no build step, so these suites load that
exact file into a real DOM (jsdom) and drive it through the events a
person would generate. Nothing is stubbed inside the app itself; the
only thing mocked is the Tauri runtime it expects to find on `window`,
which is also the seam the real backend sits behind.

```sh
npm install          # jsdom only
npm test             # every *.test.js, one process each
node saves.test.js   # or a single suite

npm run build:demo   # regenerate docs/index.html from the app's frontend
npm run check:demo   # fail if it is out of date (CI runs this)
```

| Suite | Covers |
| --- | --- |
| `startup-cache.test.js` | The two-pass startup: the cached shelf painting before a deliberately slow `fetch_library` resolves, the catalog replacing it, an unreachable server leaving it standing, and the reconciliation not undoing a filter the user changed mid-flight. |
| `install.test.js` | The install lifecycle: a queued title saying so, progress against the whole title, cancelling from either state, and a refusal (no room, a truncated transfer) reaching the screen. |
| `playtime.test.js` | Playtime: what the cards and detail header show, both time-based sorts, a finished session updating them live, and the sort control itself. |
| `metadata.test.js` | The optional `game.json`: genre on cards and in the detail header, the genre filter, and a library with no metadata at all. |
| `picker.test.js` | The launch picker — when it appears, the order it offers executables in, what it passes to `launch_game`, persistence of a choice, and its place in the keyboard chain. |
| `saves.test.js` | Cloud saves — sync before launch, upload after exit, the conflict prompt, and Title ID resolution from filenames, archives and a watched session. |
| `logs.test.js` | The Logs window: lines from before it was opened, lines arriving while it is, log text shown rather than interpreted as markup, Copy, and Escape closing the logs without closing Settings behind them. |
| `settings.test.js` | Settings under keyboard and gamepad control: every control reachable, nothing hidden or disabled focused, both ends of the form, Escape from a field committing before it closes, the list of install drives, and the PC runtime switch between Wine and Proton. |
| `demo.test.js` | The published demo in `docs/`: that it still works with no Tauri runtime at all, and that it carries every element id the app does (the drift guard for a file kept in step by hand). |

`harness.js` holds everything jsdom doesn't implement and the app
needs — Web Audio, `scrollIntoView`, `offsetParent`, blocking dialogs —
so an environment gap is fixed once rather than in four places.

`screenshots.js` is a separate tool, not part of `npm test`: it renders
the app in headless Chromium for design review. It needs Playwright,
which is deliberately not a dependency of the suites.
