#!/usr/bin/env python3
"""Drive the installed Helix binary in tmux and record selection state.

Geometry is a 100x24 truecolor tmux pane. Helix's status line is
``NOR  line:col  N sel  L chars``. The column is the 1-based visual column
of the block cursor. Flipping the selection (F1, bound to
``flip_selections``) reports the other end. Inclusive anchor and head are
those two cursor characters.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

from cases import all_cases

TMUX_CONF = "/exec-daemon/tmux.portal.conf"
SESSION = "hxdiff"
COLS = 100
ROWS = 24
STATUS_RE = re.compile(
    r"(NOR|SEL|INS)\s+(\d+):(\d+)\s+(?:(\d+)/)?(\d+)\s+sels?\s+(\d+)\s+chars?"
)
MODE_NAME = {"NOR": "normal", "SEL": "select", "INS": "insert"}


def tmux(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["tmux", "-f", TMUX_CONF, *args],
        check=check,
        text=True,
        capture_output=True,
    )


def capture() -> str:
    result = tmux("capture-pane", "-p", "-t", SESSION)
    return result.stdout


def send_literal(text: str) -> None:
    # tmux treats a bare ';' as a command separator even after -l.
    text = text.replace(";", "\\;")
    if text:
        tmux("send-keys", "-t", SESSION, "-l", text)


def send_named(name: str) -> None:
    tmux("send-keys", "-t", SESSION, name)


def helix_version() -> str:
    binary = shutil.which("hx") or "hx"
    result = subprocess.run([binary, "--version"], check=True, text=True, capture_output=True)
    return result.stdout.strip()


def column_to_offset(line: str, column: int, tab_width: int = 4) -> int:
    """Map a 1-based Helix status column to a scalar offset.

    Helix 25.07.1 counts one column per scalar, including wide characters,
    and expands tabs to the tab stop. A cursor on the line's newline is the
    column just past that text.
    """
    if column <= 1:
        return 0
    target = column - 1
    visual = 0
    for index, char in enumerate(line):
        if visual >= target:
            return index
        if char == "\t":
            visual = (visual // tab_width + 1) * tab_width
        else:
            visual += 1
    return len(line)


def position_to_index(text: str, line: int, column: int) -> int:
    lines = text.split("\n")
    index = 0
    for number, content in enumerate(lines, start=1):
        if number == line:
            return index + column_to_offset(content, column)
        index += len(content) + 1
    return len(text)


def parse_status(pane: str) -> dict | None:
    found = None
    for line in pane.splitlines():
        match = STATUS_RE.search(line)
        if match:
            found = {
                "mode": MODE_NAME[match.group(1)],
                "line": int(match.group(2)),
                "column": int(match.group(3)),
                "primary_number": int(match.group(4) or 1),
                "selections": int(match.group(5)),
                "length": int(match.group(6)),
                "raw": line,
            }
    return found


def prompt_mode(pane: str) -> str | None:
    lines = [line for line in pane.splitlines() if line.strip()]
    if not lines:
        return None
    last = lines[-1].lstrip()
    if last.startswith("search:"):
        return "search"
    if last.startswith(":"):
        return "command"
    if last.startswith("/") or last.startswith("?"):
        return "search"
    return None


class HelixSession:
    def __init__(self, root: Path):
        self.root = root
        self.config = root / "config.toml"
        self.buffer_path = root / "buffer.txt"
        self.fixture = root / "fixture.txt"
        self._write_config()

    def _write_config(self) -> None:
        output = str(self.buffer_path)
        self.config.write_text(
            f"""
theme = "base16_default_dark"

[editor]
mouse = false
true-color = true
auto-completion = false
auto-format = false
auto-save = false
auto-pairs = false
insert-final-newline = false
trim-final-newlines = false
trim-trailing-whitespace = false
line-number = "absolute"
bufferline = "never"
cursorline = false
scrolloff = 3
rulers = []
gutters = []
soft-wrap.enable = false

[editor.statusline]
left = ["mode", "position", "selections", "primary-selection-length"]
center = []
right = []

[keys.normal]
F1 = "flip_selections"
F12 = ":write! {output}"

[keys.select]
F1 = "flip_selections"
F12 = ":write! {output}"

[keys.insert]
F12 = ":write! {output}"
""",
            encoding="utf-8",
        )

    def close(self) -> None:
        tmux("kill-session", "-t", SESSION, check=False)

    def open(self, text: str, suffix: str) -> None:
        self.close()
        name = "fixture" + suffix
        self.fixture = self.root / name
        self.fixture.write_bytes(text.encode("utf-8"))
        if self.buffer_path.exists():
            self.buffer_path.unlink()
        tmux(
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-x",
            str(COLS),
            "-y",
            str(ROWS),
            "env",
            "TERM=tmux-256color",
            "COLORTERM=truecolor",
            "hx",
            "--config",
            str(self.config),
            str(self.fixture),
        )
        tmux(
            "resize-window",
            "-t",
            SESSION,
            "-x",
            str(COLS),
            "-y",
            str(ROWS),
            check=False,
        )
        self.wait_status(timeout=4.0)

    def wait_status(self, timeout: float = 2.0) -> dict:
        deadline = time.time() + timeout
        pane = ""
        while time.time() < deadline:
            pane = capture()
            status = parse_status(pane)
            if status:
                return status
            time.sleep(0.04)
        raise RuntimeError(f"helix status did not appear:\n{pane}")

    def settle(self, timeout: float = 1.5) -> str:
        deadline = time.time() + timeout
        last = None
        stable = 0
        pane = ""
        while time.time() < deadline:
            pane = capture()
            if pane == last:
                stable += 1
                if stable >= 2:
                    return pane
            else:
                last = pane
                stable = 0
            time.sleep(0.04)
        return pane

    def press(self, keys: str) -> None:
        if not keys:
            return
        tokens = tokenize(keys)
        for kind, value in tokens:
            if kind == "char":
                send_literal(value)
            else:
                send_named(value)
        self.settle()

    def save_buffer(self) -> str:
        if self.buffer_path.exists():
            self.buffer_path.unlink()
        send_named("F12")
        deadline = time.time() + 2.0
        while time.time() < deadline:
            if self.buffer_path.exists() and self.buffer_path.stat().st_size >= 0:
                # The write is synchronous; the file existing is enough.
                time.sleep(0.05)
                return self.buffer_path.read_bytes().decode("utf-8")
            time.sleep(0.04)
        raise RuntimeError(f"helix did not write the buffer:\n{capture()}")

    def flip_ends(self, text: str) -> tuple[int, int, dict]:
        status = parse_status(capture())
        if status is None:
            raise RuntimeError(f"missing status before flip:\n{capture()}")
        head = position_to_index(text, status["line"], status["column"])
        length = status["length"]
        count = status["selections"]
        send_named("F1")
        self.settle(timeout=0.8)
        flipped = parse_status(capture())
        if flipped is None:
            raise RuntimeError(f"missing status after flip:\n{capture()}")
        anchor = position_to_index(text, flipped["line"], flipped["column"])
        send_named("F1")
        self.settle(timeout=0.8)
        covered = abs(head - anchor) + 1
        if covered != length and not (length == 0 and anchor == head):
            raise RuntimeError(
                f"range length {covered} != status {length} "
                f"(anchor {anchor}, head {head}, status {status}, flip {flipped})"
            )
        return anchor, head, {"selections": count, "length": length}

    def measure(self, text: str, keys: str, suffix: str) -> dict:
        self.open(text, suffix)
        before = capture()
        self.press(keys)
        pane = capture()
        status = parse_status(pane)
        prompted = prompt_mode(pane)
        if status is None and prompted is None:
            raise RuntimeError(f"unreadable pane after {keys!r}:\n{pane or before}")
        if prompted == "search" and status is not None:
            head = position_to_index(text, status["line"], status["column"])
            length = status["length"]
            anchor = head + 1 - length if length else head
            return {
                "mode": "search",
                "text": text,
                "primary": 0,
                "ranges": [{"anchor": anchor, "head": head}],
            }
        if prompted:
            return {
                "mode": prompted,
                "text": text,
                "primary": None,
                "ranges": None,
            }
        mode = status["mode"]
        count = status["selections"]
        # Save after reading the mode. F12's typable command leaves normal
        # and select mode, and it leaves insert mode as well in 25.07.1.
        if mode == "insert":
            ends_first = None
        else:
            ends_first = None
        written = self.save_buffer()
        if mode in {"normal", "select"}:
            ranges = []
            for index in range(max(count, 1)):
                anchor, head, _info = self.flip_ends(written)
                ranges.append({"anchor": anchor, "head": head})
                if index + 1 < count:
                    send_literal(")")
                    self.settle(timeout=0.8)
            primary_range = ranges[0]
            ordered = sorted(
                ranges,
                key=lambda item: (
                    min(item["anchor"], item["head"]),
                    max(item["anchor"], item["head"]),
                    item["anchor"],
                    item["head"],
                ),
            )
            primary = ordered.index(primary_range)
            return {
                "mode": mode,
                "text": written,
                "primary": primary,
                "ranges": ordered,
            }
        # Insert: the status cursor is the block on the selected scalar.
        refreshed = parse_status(capture()) or status
        head = position_to_index(written, status["line"], status["column"])
        return {
            "mode": mode,
            "text": written,
            "primary": 0,
            "ranges": [{"anchor": head, "head": head}],
            "note": "insert cursor taken before save",
            "refreshed": refreshed["raw"] if refreshed else "",
        }


def tokenize(keys: str) -> list[tuple[str, str]]:
    tokens: list[tuple[str, str]] = []
    index = 0
    while index < len(keys):
        if keys[index] == "{":
            end = keys.index("}", index)
            spec = keys[index + 1 : end]
            tokens.append(("named", named_key(spec)))
            index = end + 1
        else:
            start = index
            while index < len(keys) and keys[index] != "{":
                index += 1
            tokens.append(("char", keys[start:index]))
    return tokens


def named_key(spec: str) -> str:
    if spec.startswith("alt-"):
        # tmux treats ';' as a command separator unless it is escaped.
        char = spec[len("alt-") :].replace(";", "\\;")
        return "M-" + char
    if spec.startswith("ctrl-"):
        return "C-" + spec[len("ctrl-") :]
    table = {
        "enter": "Enter",
        "esc": "Escape",
        "bspace": "BSpace",
        "tab": "Tab",
        "space": "Space",
        "home": "Home",
        "end": "End",
        "left": "Left",
        "right": "Right",
        "up": "Up",
        "down": "Down",
        "pageup": "PageUp",
        "pagedown": "PageDown",
    }
    if spec not in table:
        raise SystemExit(f"unknown key token {{{spec}}}")
    return table[spec]


def suffix_for(case: dict) -> str:
    path = case.get("path") or ""
    if path.endswith(".rs"):
        return ".rs"
    return ".txt"


def run_cases(selected: list[dict]) -> dict:
    version = helix_version()
    results = []
    with tempfile.TemporaryDirectory(prefix="helix-diff-") as directory:
        session = HelixSession(Path(directory))
        try:
            for item in selected:
                record = {
                    "id": item["id"],
                    "keys": item["keys"],
                }
                try:
                    measured = session.measure(item["text"], item["keys"], suffix_for(item))
                    record.update(measured)
                except Exception as error:  # noqa: BLE001 - keep going and report
                    record["error"] = str(error)
                    record["pane"] = capture()
                results.append(record)
                state = record.get("error") or record.get("mode")
                print(f"{item['id']}: {state}", flush=True)
        finally:
            session.close()
    return {"helix": version, "columns": COLS, "rows": ROWS, "results": results}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--ids", default="")
    args = parser.parse_args()
    cases = all_cases()
    if args.ids:
        wanted = set(args.ids.split(","))
        cases = [item for item in cases if item["id"] in wanted]
    if args.limit:
        cases = cases[: args.limit]
    payload = run_cases(cases)
    Path(args.out).write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    failed = [item["id"] for item in payload["results"] if "error" in item]
    print(f"wrote {args.out} ({len(payload['results'])} cases, {len(failed)} errors)")
    if failed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
