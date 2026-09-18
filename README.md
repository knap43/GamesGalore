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

### License

MIT — see [LICENSE](LICENSE). Swap it for something else if that isn't what you
want; it was chosen as the least surprising default for a tool like this, not
because you asked for it specifically.

### Demo:
https://knap43.github.io/GamesGalore/

### Screenshots:
<img width="2664" height="1704" alt="image" src="https://github.com/user-attachments/assets/eafddd76-e406-4fbd-851b-1d035a8687a9" />
<img width="2664" height="1704" alt="image" src="https://github.com/user-attachments/assets/6a5f94cf-0688-4e6c-a7cc-69b64b8bc360" />
