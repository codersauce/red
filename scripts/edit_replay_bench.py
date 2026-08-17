#!/usr/bin/env python3
"""Measure complete edit commands through a real PTY and verify their saved text.

Example: python3 scripts/edit_replay_bench.py --binary target/release/red
Use --plugins/--split for bundled callbacks and shared-buffer splits.
Use --lsp full|incremental to verify document synchronization with a local server.
"""

import argparse
from collections import defaultdict
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import tomllib

ROOT = Path(__file__).resolve().parent.parent
EVENT = re.compile(r"\[PERF\] event (?:Key\(|Paste\().*?: (\d+)us$")
TIMING = re.compile(r"\[PERF\] ([\w:+-]+)(?: (.*?))?: (\d+)us$")
SCENARIOS = ("dot", "macro", "delete", "block", "paste", "substitute", "indent", "undo")


def run(args, scenario):
    text = "x" * args.chars
    newline = "\r\n" if args.crlf else "\n"
    body = ' let emoji = "😀"; ' if args.unicode else ""
    ordinary = "".join(f"fn value_{i}() {{{body}}}{newline}" for i in range(args.lines))
    source = "z" * (args.chars * 2) + newline + ordinary if scenario == "delete" else ordinary
    with tempfile.TemporaryDirectory(prefix="red-edit-replay-") as name:
        root = Path(name)
        config = root / "config" / "red"
        config.mkdir(parents=True)
        log = root / "red.log"
        fixture = root / "fixture.rs"
        fixture.write_bytes(source.encode())
        lsp_state = root / "lsp-state.json"
        server_config = ""
        if args.lsp != "off":
            server_args = [str(ROOT / "scripts/edit_replay_lsp.py"), "--mode", args.lsp, "--state", str(lsp_state)]
            server_config = (
                f"[lsp.servers.rust]\ncommand = {json.dumps(sys.executable)}\n"
                f'args = {json.dumps(server_args)}\nlanguage_id = "rust"\n'
                'file_extensions = ["rs"]\nroot_markers = []\n'
            )
        disabled = [] if args.plugins else list(tomllib.loads((ROOT / "default_config.toml").read_text())["plugins"])
        (config / "config.toml").write_text(
            f"log_file = {json.dumps(str(log))}\n"
            f"disabled_plugins = {json.dumps(disabled)}\n"
            "disable_ai = true\nshow_whats_new = false\nfetch_release_notes = false\n"
            f"[lsp]\nenabled = {str(args.lsp != 'off').lower()}\n"
            "[formatting]\non_save = false\n[completion]\nauto_trigger = false\n"
            + server_config
        )
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.cols, 0, 0))
        process = subprocess.Popen(
            [str(args.binary), "--root", str(root), str(fixture)],
            stdin=slave, stdout=slave, stderr=slave,
            env=dict(os.environ, XDG_CONFIG_HOME=str(root / "config"), RED_PERF="trace"),
            close_fds=True,
        )
        os.close(slave)
        drained = [0]

        def drain():
            while True:
                try:
                    data = os.read(master, 1 << 20)
                except OSError:
                    return
                if not data:
                    return
                drained[0] += len(data)

        thread = threading.Thread(target=drain, daemon=True)
        thread.start()

        def contents():
            return log.read_text(errors="replace") if log.exists() else ""

        def wait_for(predicate, description):
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline:
                result = predicate()
                if result:
                    return result
                if process.poll() is not None:
                    raise RuntimeError(f"editor exited during {description}: {process.returncode}")
                time.sleep(0.005)
            raise TimeoutError(f"timed out during {description}")

        def synced(expected):
            expected_hash = hashlib.sha256(expected.encode()).hexdigest()

            def read_state():
                if not lsp_state.exists():
                    return None
                state = json.loads(lsp_state.read_text())
                if "error" in state:
                    raise AssertionError(f"LSP fixture: {state['error']}")
                return state if state["sha256"] == expected_hash else None

            return wait_for(read_state, "LSP document synchronization")

        def send(data, events=None):
            offset = len(contents())
            os.write(master, data)
            expected = len(data) if events is None else events

            def completed():
                segment = contents()[offset:]
                return segment if sum(bool(EVENT.search(line)) for line in segment.splitlines()) >= expected else None

            return wait_for(completed, repr(data[:40]))

        try:
            wait_for(lambda: "[PERF] startup:interactive:" in contents(), "startup")
            if args.lsp != "off":
                synced(source)
            if args.split:
                send(b"\x17v")
            expected_before = source
            if scenario in ("dot", "undo"):
                send(b"i" + text.encode())
                send(b"\x1b")
                expected_before = text + source
                trigger = b"." if scenario == "dot" else b"u"
                expected = text * 2 + source if scenario == "dot" else source
            elif scenario == "macro":
                send(b"qai" + text.encode())
                send(b"\x1b")
                send(b"q@")
                expected_before = text + source
                trigger, expected = b"a", text * 2 + source
            elif scenario == "delete":
                send(str(args.chars).encode())
                trigger, expected = b"x", "z" * args.chars + newline + ordinary
            elif scenario == "block":
                send(b"\x16" + str(args.block_rows - 1).encode() + b"jI" + text.encode())
                expected_before = text + source
                trigger = b"\x1b"
                lines = source.splitlines(keepends=True)
                expected = "".join((text if i < args.block_rows else "") + line for i, line in enumerate(lines))
            elif scenario == "paste":
                send(b"i")
                trigger, expected = b"\x1b[200~" + text.encode() + b"\x1b[201~", text + source
            elif scenario == "substitute":
                send(b":%s/value/item/g")
                trigger, expected = b"\r", source.replace("value", "item")
            else:
                send(b"ggV" + str(args.block_rows - 1).encode() + b"j")
                trigger = b">"
                lines = source.splitlines(keepends=True)
                expected = "".join(("    " if i < args.block_rows else "") + line for i, line in enumerate(lines))

            before_lsp = synced(expected_before) if args.lsp != "off" else None
            before_bytes = drained[0]
            segment = send(trigger, events=1)
            samples = defaultdict(list)
            event_us = []
            for line in segment.splitlines():
                if match := EVENT.search(line):
                    event_us.append(int(match.group(1)))
                if match := TIMING.search(line):
                    label, detail, micros = match.groups()
                    if label in ("notify", "husk:notify") and detail:
                        label += " " + detail
                    samples[label].append(int(micros))
            result = {
                "scenario": scenario, "chars": args.chars, "lines": args.lines,
                "plugins": args.plugins, "split": args.split,
                "event_ms": round(sum(event_us) / 1000, 3),
                "output_bytes": drained[0] - before_bytes,
                "full_renders": len(samples["render:full"]),
                "editor_window_renders": len(samples["render:editor_windows"]),
                "highlight_misses": len(samples["highlight:miss"]),
                "buffer_notifications": len(samples["notify buffer:changed"]),
            }
            if before_lsp is not None:
                after_lsp = synced(expected)
                result.update({"lsp": args.lsp, **{
                    f"lsp_{key}": after_lsp[key] - before_lsp[key]
                    for key in ("notifications", "full", "incremental", "text_bytes")
                }})
            if args.unicode or args.crlf:
                result.update(unicode=args.unicode, crlf=args.crlf)
            if scenario == "paste":
                send(b"\x1b")
            os.write(master, b":wq\r")
            process.wait(timeout=args.timeout)
            actual = fixture.read_bytes().decode()
            if actual != expected:
                raise AssertionError(f"{scenario}: saved text mismatch ({len(actual)} != {len(expected)} bytes)")
            if process.returncode != 0:
                raise RuntimeError(f"editor exited {process.returncode}")
            return result
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
            os.close(master)
            thread.join(timeout=1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/red")
    parser.add_argument("--scenario", choices=SCENARIOS, action="append")
    parser.add_argument("--chars", type=int, default=128)
    parser.add_argument("--lines", type=int, default=2000)
    parser.add_argument("--block-rows", type=int, default=16)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--timeout", type=float, default=45)
    parser.add_argument("--lsp", choices=("off", "full", "incremental"), default="off")
    parser.add_argument("--unicode", action="store_true", help="use emoji in the source")
    parser.add_argument("--crlf", action="store_true", help="use CRLF source lines")
    parser.add_argument("--plugins", action="store_true")
    parser.add_argument("--split", action="store_true")
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    if not args.binary.is_file() or not 1 <= args.chars <= 10000 or not 2 <= args.block_rows <= args.lines or args.rows < 5 or args.cols < 20:
        parser.error("require an existing binary, 1..10000 chars, 2 <= block-rows <= lines, rows >= 5, cols >= 20")
    for scenario in args.scenario or SCENARIOS:
        print(json.dumps(run(args, scenario), sort_keys=True), flush=True)


if __name__ == "__main__":
    main()
