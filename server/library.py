"""
Scans the source game library and classifies each game's files.

Folder layout:

    <library_root>/
      PS1/  PS2/  PC/  Switch/
        <Game Title>/
          *.bin/*.cue | *.iso | *.nsz | *.nsp            <- game file(s)
          <a whole installed tree, for PC>               <- see below
          *.png / *.jpg                                  <- loose screenshots
          *trailer*.mp4                                  <- optional
          README.md                                      <- "Title (Year)\n\nDescription..."

Only the screenshots, trailer and README are required to sit at the top
level of a game's folder; those three are catalog metadata, and
everything else under the folder, at any depth, is the game itself.

PC is the platform where that distinction matters most. A PC title is
normally a full installed tree — an .exe somewhere among its data
directories — rather than a single file, so both the reported size and
the choice of what to hand Wine have to consider the whole tree. Only
looking at the top level gets a title's size badly wrong and, when the
.exe lives in a subdirectory, finds no game file at all.

Switch is the one platform where a folder can hold more than one game
file — a base game plus updates or DLC — and each of those files is
independently either already .nsp (ready to serve as-is) or .nsz
(needs decompression first). Every file is checked on its own; nothing
here assumes a folder is uniformly one format or the other.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Optional

KNOWN_PLATFORMS = {"PS1", "PS2", "PC", "Switch"}
TRAILER_EXTENSIONS = {".mp4", ".mkv", ".webm", ".mov", ".avi"}
IMAGE_EXTENSIONS = {".png", ".jpg", ".jpeg", ".webp"}
README_NAME = "README.md"
# Optional, per game folder. Everything in it is optional too; anything
# absent falls back to what the folder itself can be made to say.
SIDECAR_NAME = "game.json"
SWITCH_EXTENSIONS = {".nsz", ".nsp"}

YEAR_RE = re.compile(r"\(([0-9]{4})\)\s*$")

# Executables that ship alongside a PC game but aren't the game: its
# uninstaller, bundled runtime installers, crash reporters, separate
# config tools. Matched as a substring of the filename, case-insensitively.
NON_GAME_EXE_MARKERS = (
    "unins",
    "setup",
    "install",
    "redist",
    "vcredist",
    "directx",
    "dxsetup",
    "dotnet",
    "oalinst",
    "crashhandler",
    "crashreport",
    "crashpad",
    "config",
)

NON_ALNUM_RE = re.compile(r"[^a-z0-9]+")


def _normalized(text: str) -> str:
    """Lowercased, stripped of punctuation and spacing, for comparing a
    filename against a folder title without tripping over "Moth & Ember"
    vs. "MothAndEmber" style differences."""
    return NON_ALNUM_RE.sub("", text.lower())


def _is_trailer_file(filename: str) -> bool:
    """
    "Contains trailer" (case-insensitive) with a video extension —
    matches "198X_trailer.mp4", not just an exact "trailer.mp4". Same
    fix as covers: an exact-name check was silently missing every real
    trailer, since none of them are named exactly that.
    """
    lower = filename.lower()
    return "trailer" in lower and Path(lower).suffix in TRAILER_EXTENSIONS


@dataclass
class GameFile:
    filename: str
    format: str  # "nsz" | "nsp" | extension without the dot, for other platforms
    needs_conversion: bool
    size_bytes: int  # size on disk in the source library — for .nsz this
                      # is the COMPRESSED size, smaller than what actually
                      # lands on disk once nsz decompresses it on install


@dataclass
class Game:
    id: str
    title: str
    platform: str
    release_year: Optional[int]
    description: str
    files: list = field(default_factory=list)         # list[GameFile]
    screenshots: list = field(default_factory=list)    # list[str] (filenames)
    cover: Optional[str] = None                        # filename, or None if no images at all
    trailer: Optional[str] = None
    # From an optional game.json; None/empty where there isn't one. The
    # client already styles cards by genre and shows it on the card and
    # in the detail header — until now that only ever worked for its
    # own mock catalog, because nothing on this side had anywhere to
    # put one.
    genre: Optional[str] = None
    tags: list = field(default_factory=list)           # list[str]
    players: Optional[int] = None

    def to_dict(self) -> dict:
        return asdict(self)


def scan_library(root: Path) -> list:
    if not root.is_dir():
        raise FileNotFoundError(f"Library root does not exist: {root}")

    games = []
    for platform_dir in sorted(root.iterdir()):
        if not platform_dir.is_dir() or platform_dir.name not in KNOWN_PLATFORMS:
            continue
        for game_dir in sorted(platform_dir.iterdir()):
            if not game_dir.is_dir():
                continue
            game = _read_game_folder(game_dir, platform_dir.name)
            if game is not None:
                games.append(game)
    return games


def _read_game_folder(game_dir: Path, platform: str) -> Optional[Game]:
    title = game_dir.name
    release_year, description = _read_readme(game_dir / README_NAME)
    screenshots = _find_screenshots(game_dir)
    extra = _read_sidecar(game_dir / SIDECAR_NAME)

    return Game(
        id=f"{platform}/{title}",
        title=extra.get("title") or title,
        platform=platform,
        # The sidecar wins where it says anything: it is the one place
        # someone deliberately wrote a fact down, as against a year
        # parsed out of a folder name.
        release_year=extra.get("release_year", release_year),
        description=extra.get("description") or description,
        files=_find_game_files(game_dir, platform),
        screenshots=screenshots,
        cover=_pick_cover(screenshots),
        trailer=_find_trailer(game_dir),
        genre=extra.get("genre"),
        tags=extra.get("tags") or [],
        players=extra.get("players"),
    )


def _read_sidecar(path: Path) -> dict:
    """
    Reads an optional `game.json` beside a game's files.

    Everything the catalog knows is otherwise inferred — the title from
    the folder name, the year from a parenthesis in it, the description
    from a README — which covers a library nobody wants to annotate and
    leaves no way to say anything else at all. This is that way: genre,
    tags, player count, and overrides for the three inferred fields.

    Every key is optional, and a malformed or unreadable file is
    treated as an absent one. A catalog that refuses to list a game
    because somebody left a trailing comma in its metadata would be a
    worse outcome than a game with no genre.
    """
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    if not isinstance(data, dict):
        return {}

    out = {}
    for key in ("title", "genre", "description"):
        value = data.get(key)
        if isinstance(value, str) and value.strip():
            out[key] = value.strip()
    for key in ("release_year", "players"):
        value = data.get(key)
        if isinstance(value, int) and not isinstance(value, bool):
            out[key] = value
    tags = data.get("tags")
    if isinstance(tags, list):
        out["tags"] = [t.strip() for t in tags if isinstance(t, str) and t.strip()]
    return out


def _pick_cover(screenshots: list) -> Optional[str]:
    """
    Prefers whichever screenshot has "cover" in its filename — the
    convention this library actually uses — and falls back to the
    first screenshot alphabetically if no file is named that way, so a
    folder with images but no explicit cover still gets *something*
    shown on the grid rather than nothing.
    """
    for name in screenshots:
        if "cover" in name.lower():
            return name
    return screenshots[0] if screenshots else None


def _read_readme(path: Path) -> tuple:
    if not path.is_file():
        return None, ""
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    if not lines:
        return None, ""

    first_line = lines[0].strip()
    match = YEAR_RE.search(first_line)
    release_year = int(match.group(1)) if match else None

    rest = lines[1:]
    while rest and not rest[0].strip():
        rest.pop(0)
    description = "\n".join(rest).strip()

    return release_year, description


def _find_screenshots(game_dir: Path) -> list:
    return sorted(
        f.name
        for f in game_dir.iterdir()
        if f.is_file() and f.suffix.lower() in IMAGE_EXTENSIONS
    )


def _find_trailer(game_dir: Path):
    matches = sorted(
        f.name for f in game_dir.iterdir() if f.is_file() and _is_trailer_file(f.name)
    )
    return matches[0] if matches else None


def _is_catalog_metadata(game_dir: Path, path: Path) -> bool:
    """
    Whether a file is catalog furniture rather than part of the game —
    the README, the trailer, the loose screenshots. Only ever true at
    the top level of a game folder: images nested inside a PC game's
    own subdirectories are its assets, and they count toward its size.
    """
    if path.parent != game_dir:
        return False
    return (
        path.name == README_NAME
        or path.name == SIDECAR_NAME
        or _is_trailer_file(path.name)
        or path.suffix.lower() in IMAGE_EXTENSIONS
    )


def _game_content_files(game_dir: Path) -> list:
    """
    Every file belonging to the game, at any depth. A PC game is usually
    a whole installed tree — an .exe next to its data directories — so
    anything that only looks at the top level of the folder sees a
    fraction of it, or, when the .exe sits in a subdirectory, nothing
    at all.
    """
    return sorted(
        f
        for f in game_dir.rglob("*")
        if f.is_file() and not _is_catalog_metadata(game_dir, f)
    )


def _pick_pc_executable(game_dir: Path, files: list):
    """
    Picks the .exe to hand Wine, from anywhere in the game's tree.

    Ranking, in order: prefer something that isn't an installer or
    bundled runtime; then an executable whose name matches the game's
    folder title; then the shallowest one, since a game's entry point
    normally sits at the root of its own tree rather than buried in a
    bin/ or redist/ subdirectory; then the largest, the main binary
    usually being bigger than helper tools next to it. Ties break on
    name so the choice is stable across scans.
    """
    exes = [f for f in files if f.suffix.lower() == ".exe"]
    if not exes:
        return None

    lower = {f: f.name.lower() for f in exes}
    preferred = [f for f in exes if not any(m in lower[f] for m in NON_GAME_EXE_MARKERS)]
    # Every executable looking like an installer is better than claiming
    # the game has none — fall back to the full list rather than bailing.
    candidates = preferred or exes

    title = _normalized(game_dir.name)

    def rank(f: Path):
        stem = _normalized(f.stem)
        if stem == title:
            title_match = 0
        elif stem and (stem in title or title in stem):
            title_match = 1
        else:
            title_match = 2
        depth = len(f.relative_to(game_dir).parts) - 1
        return (title_match, depth, -f.stat().st_size, f.name.lower())

    return min(candidates, key=rank)


def _game_file(game_dir: Path, path: Path) -> GameFile:
    """
    One catalog entry for one real file. `filename` is relative to the
    game folder, so it carries the subdirectory when there is one
    ("bin/game.exe"); the /download and /media routes accept that shape.
    """
    suffix = path.suffix.lower()
    return GameFile(
        filename=path.relative_to(game_dir).as_posix(),
        format=suffix.lstrip("."),
        needs_conversion=suffix == ".nsz",
        size_bytes=path.stat().st_size,
    )


def _find_game_files(game_dir: Path, platform: str) -> list:
    """
    Every file the client needs in order to play the title, each with
    its own real size — so a title's total is simply the sum of what
    will actually be transferred.

    Listing only the entry point (the .cue, the .exe) was the shape
    this used to have, and it could not work: the download route serves
    nothing that isn't in this list, so the .bin holding a disc's data,
    or the tree a PC game needs, could never be fetched. The client
    installed one file, marked the title installed, and the emulator
    then failed on what wasn't there.

    The entry point is listed first, which costs nothing and means the
    most interesting file arrives before a long tail of assets. It
    carries no special marking beyond that: the client re-derives what
    to launch from the install directory itself, since what matters at
    launch is what actually landed on disk.
    """
    files = _game_content_files(game_dir)
    candidates = [f for f in files if f.parent == game_dir]

    if platform == "Switch":
        # A Switch title is its .nsz/.nsp files and nothing else —
        # there's no surrounding tree to bring along.
        return [
            _game_file(game_dir, f)
            for f in candidates
            if f.suffix.lower() in SWITCH_EXTENSIONS
        ]

    if platform == "PC":
        # Searched across the whole tree, not just the top level: a PC
        # game's .exe is as often in a subdirectory as beside its data.
        entry = _pick_pc_executable(game_dir, files)
    elif platform in {"PS1", "PS2"}:
        entry = next((f for f in candidates if f.suffix.lower() == ".cue"), None)
    else:
        entry = None

    if entry is None:
        # Nothing recognisable to lead with — prefer a top-level file,
        # then anything at all, so a title with an unusual layout still
        # appears in the catalog rather than vanishing from it.
        entry = candidates[0] if candidates else (files[0] if files else None)
    if entry is None:
        return []

    ordered = [entry] + [f for f in files if f != entry]
    return [_game_file(game_dir, f) for f in ordered]
