"""
Scans the source game library and classifies each game's files.

Folder layout:

    <library_root>/
      PS1/  PS2/  PC/  Switch/
        <Game Title>/
          *.bin/*.cue | *.iso | *.exe | *.nsz | *.nsp   <- game file(s)
          *.png / *.jpg                                  <- loose screenshots
          trailer.mp4                                    <- optional
          README.md                                      <- "Title (Year)\n\nDescription..."

Switch is the one platform where a folder can hold more than one game
file — a base game plus updates or DLC — and each of those files is
independently either already .nsp (ready to serve as-is) or .nsz
(needs decompression first). Every file is checked on its own; nothing
here assumes a folder is uniformly one format or the other.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Optional

KNOWN_PLATFORMS = {"PS1", "PS2", "PC", "Switch"}
TRAILER_EXTENSIONS = {".mp4", ".mkv", ".webm", ".mov", ".avi"}
IMAGE_EXTENSIONS = {".png", ".jpg", ".jpeg", ".webp"}
README_NAME = "README.md"
SWITCH_EXTENSIONS = {".nsz", ".nsp"}

YEAR_RE = re.compile(r"\(([0-9]{4})\)\s*$")


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

    return Game(
        id=f"{platform}/{title}",
        title=title,
        platform=platform,
        release_year=release_year,
        description=description,
        files=_find_game_files(game_dir, platform),
        screenshots=screenshots,
        cover=_pick_cover(screenshots),
        trailer=_find_trailer(game_dir),
    )


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


def _find_game_files(game_dir: Path, platform: str) -> list:
    candidates = [
        f
        for f in game_dir.iterdir()
        if f.is_file()
        and f.name != README_NAME
        and not _is_trailer_file(f.name)
        and f.suffix.lower() not in IMAGE_EXTENSIONS
    ]

    if platform == "Switch":
        return [
            GameFile(
                filename=f.name,
                format=f.suffix.lower().lstrip("."),
                needs_conversion=f.suffix.lower() == ".nsz",
                size_bytes=f.stat().st_size,
            )
            for f in candidates
            if f.suffix.lower() in SWITCH_EXTENSIONS
        ]

    if platform in {"PS1", "PS2"}:
        cue = next((f for f in candidates if f.suffix.lower() == ".cue"), None)
        chosen = cue or (candidates[0] if candidates else None)
    else:
        chosen = candidates[0] if candidates else None

    if chosen is None:
        return []

    # For a .cue, the actual game data sits in the sibling .bin(s), not
    # the tiny text file itself — total footprint means every candidate
    # file in the folder, even though only `chosen` is what gets handed
    # to the emulator.
    total_size = sum(f.stat().st_size for f in candidates)
    return [
        GameFile(
            filename=chosen.name,
            format=chosen.suffix.lower().lstrip("."),
            needs_conversion=False,
            size_bytes=total_size,
        )
    ]
