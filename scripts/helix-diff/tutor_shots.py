#!/usr/bin/env python3
"""Side-by-side tutor screenshots: Helix on the left, Red on the right."""

from __future__ import annotations

import os
import tempfile
import time
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont
from render_ansi import parse
from style_check import capture, open_session, tmux, wait_for_text
from tutor import tutor_cases

OUT = Path("/opt/cursor/artifacts/helix-parity")
SHOTS = ("tutor-1.4", "tutor-3.6", "tutor-5.2")


def send(session: str, keys: str) -> None:
    from probe_hx import tokenize

    for kind, value in tokenize(keys):
        if kind == "char":
            text = value.replace(";", "\\;")
            if text:
                tmux("send-keys", "-t", session, "-l", text)
        else:
            tmux("send-keys", "-t", session, value)
        time.sleep(0.05)
    time.sleep(0.3)


def pane_image(session: str) -> Image.Image:
    rows = parse(capture(session))
    cell_w, cell_h = 8, 16
    width = max((len(row) for row in rows), default=1)
    height = max(len(rows), 1)
    image = Image.new("RGB", (width * cell_w, height * cell_h), (24, 24, 24))
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype(
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf", 12
    )
    for y, row in enumerate(rows):
        for x, (char, fg, bg) in enumerate(row):
            draw.rectangle(
                (x * cell_w, y * cell_h, (x + 1) * cell_w, (y + 1) * cell_h),
                fill=bg,
            )
            draw.text((x * cell_w, y * cell_h), char, font=font, fill=fg)
    return image


def main() -> None:
    os.environ.setdefault("HOME", "/tmp/helix-style-home")
    Path("/tmp/helix-style-home").mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    cases = {item["id"]: item for item in tutor_cases()}
    root = Path(tempfile.mkdtemp(prefix="tutor-shot-"))
    hx_config = root / "hx.toml"
    hx_config.write_text(
        "theme = \"base16_default_dark\"\n[editor]\nmouse = false\ntrue-color = true\n"
        "auto-completion = false\nline-number = \"absolute\"\nbufferline = \"never\"\n"
        "scrolloff = 3\ngutters = []\nsoft-wrap.enable = false\n",
        encoding="utf-8",
    )
    red = Path(__file__).resolve().parents[2] / "target" / "debug" / "red"
    for case_id in SHOTS:
        item = cases[case_id]
        fixture_hx = root / f"{case_id}-hx.txt"
        fixture_red = root / f"{case_id}-red.txt"
        fixture_hx.write_text(item["text"], encoding="utf-8")
        fixture_red.write_text(item["text"], encoding="utf-8")
        open_session("tutorshot-hx", ["hx", "--config", str(hx_config)], fixture_hx)
        open_session(
            "tutorshot-red",
            [str(red), "-c", 'editing_model="helix"'],
            fixture_red,
        )
        try:
            wait_for_text("tutorshot-hx", item["text"].strip().split("\n")[0][:12])
            try:
                wait_for_text("tutorshot-red", item["text"].strip().split("\n")[0][:12], timeout=1.2)
            except RuntimeError:
                tmux("send-keys", "-t", "tutorshot-red", "Escape")
                wait_for_text("tutorshot-red", item["text"].strip().split("\n")[0][:12])
            send("tutorshot-hx", item["keys"])
            send("tutorshot-red", item["keys"])
            time.sleep(0.4)
            left = pane_image("tutorshot-hx")
            right = pane_image("tutorshot-red")
        finally:
            tmux("kill-session", "-t", "tutorshot-hx", check=False)
            tmux("kill-session", "-t", "tutorshot-red", check=False)
        gap = 24
        canvas = Image.new(
            "RGB",
            (left.width + gap + right.width, max(left.height, right.height) + 28),
            (16, 16, 20),
        )
        draw = ImageDraw.Draw(canvas)
        font = ImageFont.truetype(
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 14
        )
        draw.text((8, 4), f"Helix  {case_id}", font=font, fill=(220, 220, 220))
        draw.text((left.width + gap + 8, 4), f"Red  {case_id}", font=font, fill=(220, 220, 220))
        canvas.paste(left, (0, 28))
        canvas.paste(right, (left.width + gap, 28))
        destination = OUT / f"{case_id}-side-by-side.png"
        canvas.save(destination)
        print(destination)


if __name__ == "__main__":
    main()
