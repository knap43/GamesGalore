"""
Fills in what a game folder is missing, from RAWG.

The library's catalog is read entirely off the disk: a README.md gives
the title, year and description, loose images are the screenshots, one
of them with "cover" in its name is the cover, a file with "trailer" in
its name is the trailer, and an optional game.json carries the genre
and tags. That is a pleasant format to read and a tedious one to write
five hundred times, which is why this exists.

Written to be interrupted and re-run. Every file is skipped if it is
already there, so a second pass costs one search request per game and
fills only what is still missing; `overwrite=True` is the escape hatch
for a folder whose data is wrong rather than absent. Nothing here ever
deletes anything.

The HTTP layer is injected rather than imported, so the tests drive
this against a recorded set of responses instead of against RAWG —
which keeps them fast, offline, and free of anybody's rate limit.
"""

from __future__ import annotations

import json
import re
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

BASE_URL = "https://api.rawg.io/api"

# What a filled-in folder looks like. Each name is chosen to match what
# library.py already looks for — see _pick_cover and _is_trailer_file
# there — so a fetched folder and a hand-made one are the same thing.
README_NAME = "README.md"
SIDECAR_NAME = "game.json"
COVER_NAME = "cover.jpg"
TRAILER_NAME = "trailer.mp4"


class MetadataError(RuntimeError):
    """A failure worth showing someone, rather than a stack trace."""


@dataclass
class FetchResult:
    """What one game's fetch did, for a caller to report."""

    title: str
    matched: Optional[str] = None
    wrote: list = field(default_factory=list)
    skipped: list = field(default_factory=list)
    error: Optional[str] = None

    def to_dict(self) -> dict:
        return {
            "title": self.title,
            "matched": self.matched,
            "wrote": self.wrote,
            "skipped": self.skipped,
            "error": self.error,
        }


class RawgClient:
    """
    The four RAWG calls this needs, and nothing else.

    `session` is anything with a `get(url, params=..., timeout=...,
    stream=...)` that answers like requests'. Injected so the tests can
    hand over a fake one; production passes a real requests.Session.
    """

    def __init__(self, api_key: str, session, base_url: str = BASE_URL, timeout: int = 15):
        if not api_key:
            raise MetadataError(
                "no RAWG API key configured — set RAWG_API_KEY in config.py "
                "or in the environment (a free key comes from https://rawg.io/apidocs)"
            )
        self.api_key = api_key
        self.session = session
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def _get(self, path: str, **params) -> dict:
        params["key"] = self.api_key
        response = self.session.get(
            f"{self.base_url}{path}", params=params, timeout=self.timeout
        )
        status = getattr(response, "status_code", 200)
        # Two failures worth naming rather than letting them arrive as a
        # generic HTTP error halfway through a library-wide run.
        if status == 401:
            raise MetadataError("RAWG rejected the API key")
        if status == 429:
            raise MetadataError("RAWG rate limit reached — try again later")
        response.raise_for_status()
        return response.json()

    def search(self, query: str) -> Optional[dict]:
        results = self._get("/games", search=query, page_size=5).get("results", [])
        return best_match(query, results)

    def details(self, game_id: int) -> dict:
        return self._get(f"/games/{game_id}")

    def screenshots(self, game_id: int) -> list:
        results = self._get(f"/games/{game_id}/screenshots").get("results", [])
        return [shot["image"] for shot in results if shot.get("image")]

    def trailers(self, game_id: int) -> list:
        results = self._get(f"/games/{game_id}/movies").get("results", [])
        return [m["data"]["max"] for m in results if m.get("data", {}).get("max")]

    def download(self, url: str, destination: Path, retries: int = 3) -> None:
        for attempt in range(1, retries + 1):
            try:
                response = self.session.get(url, timeout=60, stream=True)
                response.raise_for_status()
                # Written to a partial file and renamed on success, so an
                # interrupted download can never be mistaken for a
                # finished one by the next run — which would skip it.
                partial = destination.with_suffix(destination.suffix + ".part")
                with open(partial, "wb") as f:
                    for chunk in response.iter_content(chunk_size=64 * 1024):
                        if chunk:
                            f.write(chunk)
                partial.replace(destination)
                return
            except Exception:
                if attempt == retries:
                    raise
                time.sleep(attempt)  # 1s, then 2s


def normalise(name: str) -> str:
    """Folder names and store names rarely agree on punctuation."""
    return re.sub(r"[^a-z0-9]+", "", name.lower())


def best_match(query: str, results: list) -> Optional[dict]:
    """
    Picks the closest of RAWG's results rather than simply the first.

    Searching for "DOOM" returns a dozen DOOMs, and the first is not
    reliably the one whose name is actually "DOOM" — so an exact match
    on the normalised name wins, then one that starts with it, and only
    then does position decide. Cheap, and it is the difference between
    a library of right answers and a library of plausible ones.
    """
    if not results:
        return None
    wanted = normalise(query)

    exact = [r for r in results if normalise(r.get("name", "")) == wanted]
    if exact:
        return exact[0]
    prefixed = [r for r in results if normalise(r.get("name", "")).startswith(wanted)]
    if prefixed:
        return prefixed[0]
    return results[0]


def readme_text(name: str, year: Optional[str], description: str) -> str:
    """The exact shape library.py's _read_readme parses back out."""
    heading = f"# {name} ({year})" if year else f"# {name}"
    body = (description or "").strip()
    return f"{heading}\n\n{body}\n"


def sidecar_data(details: dict, max_tags: int = 6) -> dict:
    """
    The subset of RAWG's answer that game.json has a place for.

    Tags are RAWG's crowd-sourced list and run to dozens per game,
    most of them noise ("Steam Achievements", "Partial Controller
    Support"); the first few are ordered by how many people applied
    them, so taking those and stopping is the whole filter.
    """
    genres = details.get("genres") or []
    tags = details.get("tags") or []
    released = details.get("released") or ""

    data = {}
    if genres:
        data["genre"] = genres[0].get("name")
    picked = [t.get("name") for t in tags[:max_tags] if t.get("name")]
    if picked:
        data["tags"] = picked
    if released[:4].isdigit():
        data["release_year"] = int(released[:4])
    return data


def fill_game_folder(
    game_dir: Path,
    title: str,
    client: RawgClient,
    *,
    overwrite: bool = False,
    max_screenshots: int = 6,
) -> FetchResult:
    """
    Fetches whatever `game_dir` is missing. Never touches the game
    files themselves — only the catalog furniture beside them.
    """
    result = FetchResult(title=title)

    game = client.search(title)
    if not game:
        result.error = "no match on RAWG"
        return result
    result.matched = game.get("name")

    def wanted(name: str) -> bool:
        """Whether to write this file, and record why if not."""
        if not overwrite and (game_dir / name).exists():
            result.skipped.append(name)
            return False
        return True

    # Defensive: a details payload that isn't an object at all — a
    # proxy's error page, a truncated response — should cost this game
    # its description, not take down a run of five hundred.
    details = client.details(game["id"])
    if not isinstance(details, dict):
        details = {}

    if wanted(README_NAME):
        (game_dir / README_NAME).write_text(
            readme_text(
                game.get("name", title),
                (game.get("released") or "")[:4] or None,
                details.get("description_raw", ""),
            ),
            encoding="utf-8",
        )
        result.wrote.append(README_NAME)

    if wanted(SIDECAR_NAME):
        data = sidecar_data(details)
        if data:
            (game_dir / SIDECAR_NAME).write_text(
                json.dumps(data, indent=2) + "\n", encoding="utf-8"
            )
            result.wrote.append(SIDECAR_NAME)

    if game.get("background_image") and wanted(COVER_NAME):
        client.download(game["background_image"], game_dir / COVER_NAME)
        result.wrote.append(COVER_NAME)

    # Numbered from one and zero-padded, so they sort the way they were
    # served rather than 1, 10, 2 — the scanner sorts by filename.
    existing_shots = list(game_dir.glob("screenshot-*.jpg"))
    if overwrite or not existing_shots:
        for index, url in enumerate(client.screenshots(game["id"])[:max_screenshots], 1):
            name = f"screenshot-{index:02d}.jpg"
            client.download(url, game_dir / name)
            result.wrote.append(name)
    else:
        result.skipped.append("screenshots")

    trailers = None
    if wanted(TRAILER_NAME):
        trailers = client.trailers(game["id"])
        if trailers:
            client.download(trailers[0], game_dir / TRAILER_NAME)
            result.wrote.append(TRAILER_NAME)

    return result


def _cli() -> int:
    """
    Fills in the whole library from the command line, without the app
    or the server running:

        RAWG_API_KEY=... .venv/bin/python metadata.py
        RAWG_API_KEY=... .venv/bin/python metadata.py --overwrite "Hollow Meridian"

    With no names, every game that is missing a description or a cover
    is looked up and the rest are left alone, so this is safe to run
    again after adding a few titles.
    """
    import argparse

    import requests

    from config import METADATA_MAX_SCREENSHOTS, RAWG_API_KEY
    from library import scan_library
    from server import library_roots

    parser = argparse.ArgumentParser(description="Fill in game folders from RAWG.")
    parser.add_argument("titles", nargs="*", help="only these games (default: all incomplete)")
    parser.add_argument("--overwrite", action="store_true",
                        help="replace files that are already there")
    parser.add_argument("--pause", type=float, default=0.3,
                        help="seconds between games, to stay well inside RAWG's rate limit")
    args = parser.parse_args()

    try:
        client = RawgClient(RAWG_API_KEY, requests.Session())
    except MetadataError as e:
        print(f"error: {e}")
        return 2

    wanted = {t.lower() for t in args.titles}
    # Every drive the library spans, and the directory each game came
    # from, so a game on the second drive is filled in where it is.
    found = []
    for root in library_roots():
        try:
            games_on_root = scan_library(root)
        except FileNotFoundError:
            print(f"library root is not there, skipping it: {root}")
            continue
        for game in games_on_root:
            found.append((game, root / game.platform / game.title))

    seen = set()
    games = []
    for game, directory in found:
        if game.id in seen:
            continue
        seen.add(game.id)
        if (not wanted or game.title.lower() in wanted) and (
            args.overwrite or not (game.description and game.cover)
        ):
            games.append((game, directory))
    if not games:
        print("nothing to fetch — every game already has a description and a cover")
        return 0

    print(f"fetching {len(games)} game(s)")
    for index, (game, directory) in enumerate(games, 1):
        try:
            result = fill_game_folder(
                directory, game.title, client,
                overwrite=args.overwrite, max_screenshots=METADATA_MAX_SCREENSHOTS,
            )
        except MetadataError as e:
            # A bad key or a rate limit will fail every remaining game
            # too; stopping says so once instead of four hundred times.
            print(f"stopped: {e}")
            return 1
        except Exception as e:  # noqa: BLE001 - one bad game is not the run
            print(f"[{index}/{len(games)}] {game.title}: {e}")
            continue

        if result.error:
            print(f"[{index}/{len(games)}] {game.title}: {result.error}")
        else:
            matched = "" if result.matched == game.title else f" (matched {result.matched!r})"
            wrote = ", ".join(result.wrote) or "nothing missing"
            print(f"[{index}/{len(games)}] {game.title}{matched}: {wrote}")

        time.sleep(args.pause)
    return 0


if __name__ == "__main__":
    raise SystemExit(_cli())
