#!/usr/bin/env python3
"""Compare rendered selection cells, not just selection state.

``w`` selects a word plus its trailing space, and the head is that space.
Helix paints the head with the cursor style (or leaves it unpainted when the
terminal is unfocused). It must not wear the selection background. This drives
both editors in tmux and reads ``capture-pane -e``.
"""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
import time
from pathlib import Path

ANSI = re.compile(r"\x1b\[[0-9;]*m")

from render_ansi import parse

TMUX_CONF = "/exec-daemon/tmux.portal.conf"
TEXT = "this is just a line\n"
# "this " is the first word motion. The head is the space after "this".
HEAD_COLUMN = 4


def tmux(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["tmux", "-f", TMUX_CONF, *args],
        check=check,
        text=True,
        capture_output=True,
    )


def capture(session: str) -> str:
    result = tmux("capture-pane", "-e", "-p", "-t", session)
    return result.stdout


def open_session(session: str, command: list[str], fixture: Path) -> None:
    tmux("kill-session", "-t", session, check=False)
    tmux(
        "new-session",
        "-d",
        "-s",
        session,
        "-x",
        "100",
        "-y",
        "24",
        "env",
        "TERM=tmux-256color",
        "COLORTERM=truecolor",
        *command,
        str(fixture),
    )
    tmux("resize-window", "-t", session, "-x", "100", "-y", "24", check=False)
    tmux("set-option", "-t", session, "focus-events", "on")


def wait_for_text(session: str, needle: str, timeout: float = 6.0) -> str:
    deadline = time.time() + timeout
    pane = ""
    while time.time() < deadline:
        pane = ANSI.sub("", capture(session))
        if needle in pane:
            return pane
        time.sleep(0.05)
    raise RuntimeError(f"{session} did not show {needle!r}:\n{pane}")


def selection_mask(session: str) -> list[bool]:
    """True where a content cell shares the background of the first selected cell."""
    pane = capture(session)
    rows = parse(pane)
    content = None
    for row in rows:
        text = "".join(cell[0] for cell in row)
        index = text.find("this is")
        if index >= 0:
            content = row[index : index + len("this is")]
            break
    if content is None:
        raise RuntimeError(f"{session} content row not found:\n{pane}")
    selection_bg = content[0][2]
    return [cell[2] == selection_bg for cell in content]


def check_editor(name: str, command: list[str], root: Path) -> list[bool]:
    session = f"hxstyle-{name}"
    fixture = root / f"{name}.txt"
    fixture.write_text(TEXT, encoding="utf-8")
    open_session(session, command, fixture)
    try:
        try:
            wait_for_text(session, "this is", timeout=1.5)
        except RuntimeError:
            tmux("send-keys", "-t", session, "Escape")
            wait_for_text(session, "this is")
        tmux("send-keys", "-t", session, "-l", "w")
        time.sleep(0.4)
        # Focus lost: Helix stops painting the primary head and shows an underscore.
        tmux("send-keys", "-t", session, "Escape", "[O")
        time.sleep(0.3)
        mask = selection_mask(session)
    finally:
        tmux("kill-session", "-t", session, check=False)
    if len(mask) <= HEAD_COLUMN:
        raise RuntimeError(f"{name} selection mask is too short: {mask}")
    if not any(mask[:HEAD_COLUMN]):
        raise RuntimeError(f"{name} did not paint the word before the head: {mask}")
    if mask[HEAD_COLUMN]:
        raise RuntimeError(
            f"{name} paints the head cell with the selection background: {mask}"
        )
    return mask


def main() -> None:
    os.environ.setdefault("HOME", "/tmp/helix-style-home")
    Path("/tmp/helix-style-home").mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="helix-style-"))
    hx_config = root / "hx.toml"
    hx_config.write_text(
        """
theme = "base16_default_dark"

[editor]
mouse = false
true-color = true
auto-completion = false
line-number = "absolute"
bufferline = "never"
scrolloff = 3
gutters = []
soft-wrap.enable = false
""",
        encoding="utf-8",
    )
    red = Path(__file__).resolve().parents[2] / "target" / "debug" / "red"
    helix = check_editor("hx", ["hx", "--config", str(hx_config)], root)
    red_mask = check_editor(
        "red",
        [str(red), "-c", 'editing_model="helix"'],
        root,
    )
    if helix[: HEAD_COLUMN + 1] != red_mask[: HEAD_COLUMN + 1]:
        raise SystemExit(
            f"selection mask differs\n  hx  {helix[: HEAD_COLUMN + 2]}\n"
            f"  red {red_mask[: HEAD_COLUMN + 2]}"
        )
    print("head cell is outside the selection in both editors", helix[: HEAD_COLUMN + 2])


if __name__ == "__main__":
    main()
