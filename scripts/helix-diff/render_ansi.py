#!/usr/bin/env python3
"""Render a tmux capture-pane -e stream to a PNG."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

SGR = re.compile(r"\x1b\[([0-9;]*)m")
CELL_W = 11
CELL_H = 22
FONT_PATH = "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf"


def parse(text: str) -> list[list[tuple[str, tuple[int, int, int], tuple[int, int, int]]]]:
    default_fg = (220, 220, 220)
    default_bg = (16, 16, 20)
    rows: list[list[tuple[str, tuple[int, int, int], tuple[int, int, int]]]] = []
    row: list[tuple[str, tuple[int, int, int], tuple[int, int, int]]] = []
    fg, bg = default_fg, default_bg
    reverse = False
    index = 0
    while index < len(text):
        if text[index] == "\x1b":
            match = SGR.match(text, index)
            if match:
                parts = [int(part) for part in match.group(1).split(";") if part]
                if not parts:
                    parts = [0]
                cursor = 0
                while cursor < len(parts):
                    code = parts[cursor]
                    if code == 0:
                        fg, bg, reverse = default_fg, default_bg, False
                    elif code == 7:
                        reverse = True
                    elif code == 27:
                        reverse = False
                    elif code == 39:
                        fg = default_fg
                    elif code == 49:
                        bg = default_bg
                    elif code in (38, 48) and cursor + 1 < len(parts) and parts[cursor + 1] == 2:
                        color = tuple(parts[cursor + 2 : cursor + 5])
                        if len(color) == 3:
                            if code == 38:
                                fg = color  # type: ignore[assignment]
                            else:
                                bg = color  # type: ignore[assignment]
                        cursor += 4
                    cursor += 1
                index = match.end()
                continue
            index += 1
            continue
        char = text[index]
        index += 1
        if char == "\n":
            rows.append(row)
            row = []
            continue
        if char == "\r":
            continue
        paint_fg, paint_bg = (bg, fg) if reverse else (fg, bg)
        row.append((char, paint_fg, paint_bg))
    if row:
        rows.append(row)
    return rows


def render(text: str, destination: Path) -> None:
    rows = parse(text)
    width = max((len(row) for row in rows), default=1)
    height = max(len(rows), 1)
    image = Image.new("RGB", (width * CELL_W + 16, height * CELL_H + 16), (16, 16, 20))
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype(FONT_PATH, 16)
    for y, row in enumerate(rows):
        for x, (char, fg, bg) in enumerate(row):
            draw.rectangle(
                (8 + x * CELL_W, 8 + y * CELL_H, 8 + (x + 1) * CELL_W, 8 + (y + 1) * CELL_H),
                fill=bg,
            )
            draw.text((8 + x * CELL_W, 8 + y * CELL_H), char, font=font, fill=fg)
    image.save(destination)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("capture")
    parser.add_argument("png")
    args = parser.parse_args()
    render(Path(args.capture).read_text(encoding="utf-8", errors="replace"), Path(args.png))


if __name__ == "__main__":
    main()
