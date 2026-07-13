#!/usr/bin/env python3
"""Measure user-visible typing, search, and picker latency through a PTY.

Examples:
    python3 scripts/interaction_bench.py typing
    python3 scripts/interaction_bench.py search --cycles 20 --delay-ms 10
    python3 scripts/interaction_bench.py picker --root ../codex \
        --file ../codex/codex-rs/tui/src/bottom_pane/chat_composer.rs \
        --query chat_composer.rs
"""

import argparse
from collections import defaultdict
import fcntl
import os
from pathlib import Path
import pty
import re
import struct
import subprocess
import tempfile
import termios
import threading
import time


ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "red"
TIMING = re.compile(r"\[PERF\] (\S+)(?: (.*?))?: (\d+)us")


def percentile(samples, value):
    return samples[(len(samples) - 1) * value // 100]


def send_keys(master, keys, delay):
    for key in keys:
        os.write(master, bytes([key]))
        time.sleep(delay)


def run(args):
    if not BIN.exists():
        raise SystemExit("build the release binary first: cargo build --locked --release")

    file_path = Path(args.file).resolve()
    root = Path(args.root).resolve()
    if not file_path.is_file():
        raise SystemExit(f"benchmark file does not exist: {file_path}")
    if not root.is_dir():
        raise SystemExit(f"benchmark root does not exist: {root}")

    with tempfile.TemporaryDirectory(prefix="red-interaction-perf-") as temp:
        temp = Path(temp)
        config_dir = temp / "red"
        config_dir.mkdir()
        log = temp / "red.log"
        (config_dir / "config.toml").write_text(f'log_file = "{log}"\n', encoding="utf-8")

        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", args.rows, args.cols, 0, 0),
        )
        argv = [
            str(BIN),
            "--root",
            str(root),
            "--config-override",
            "lsp.enabled = false",
        ]
        for override in args.config_override:
            argv.extend(["--config-override", override])
        argv.append(str(file_path))

        launched = time.monotonic()
        process = subprocess.Popen(
            argv,
            stdin=slave,
            stdout=slave,
            stderr=subprocess.DEVNULL,
            env=dict(os.environ, RED_PERF="trace", XDG_CONFIG_HOME=str(temp)),
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

        threading.Thread(target=drain, daemon=True).start()

        deadline = launched + args.startup_timeout
        while time.monotonic() < deadline:
            if log.exists() and "[PERF] startup:interactive:" in log.read_text(
                encoding="utf-8", errors="replace"
            ):
                break
            if process.poll() is not None:
                raise SystemExit(f"editor exited before first paint: {process.returncode}")
            time.sleep(0.02)
        else:
            process.kill()
            process.wait(timeout=5)
            raise SystemExit(
                f"editor did not produce a first frame within {args.startup_timeout:g}s"
            )

        first_paint_ms = (time.monotonic() - launched) * 1000
        time.sleep(0.4)
        os.write(master, b"100j")
        time.sleep(0.25)
        with log.open("a", encoding="utf-8") as stream:
            stream.write("[BENCH] begin\n")
        bytes_before = drained[0]
        started = time.monotonic()
        delay = args.delay_ms / 1000

        if args.scenario == "typing":
            os.write(master, b"i")
            for index in range(args.cycles):
                os.write(master, b"a" if index % 2 == 0 else "\u03bb".encode())
                time.sleep(delay)
            os.write(master, b"\x1b")
        elif args.scenario == "search":
            os.write(master, b"/")
            query = args.query.encode()
            for _ in range(args.cycles):
                send_keys(master, query, delay)
                send_keys(master, b"\x7f" * len(query), delay)
            os.write(master, b"\x1b")
        else:
            os.write(master, b"\x10")
            time.sleep(args.picker_load_wait)
            query = args.query.encode()
            send_keys(master, query, delay)
            for _ in range(args.cycles):
                os.write(master, b"\x7f")
                time.sleep(delay)
                os.write(master, query[-1:])
                time.sleep(delay)
            os.write(master, b"\x1b")

        time.sleep(0.8)
        elapsed = time.monotonic() - started
        with log.open("a", encoding="utf-8") as stream:
            stream.write("[BENCH] end\n")
        os.write(master, b":q!\r")
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)

        samples = defaultdict(list)
        startup = {}
        in_window = False
        for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
            if "[BENCH] begin" in line:
                in_window = True
                continue
            if "[BENCH] end" in line:
                in_window = False
                continue
            match = TIMING.search(line)
            if not match:
                continue
            label, detail, micros = match.group(1), match.group(2) or "", int(match.group(3))
            if not in_window and label.startswith("startup:"):
                startup[label] = micros
            if in_window:
                if label in ("notify", "drain"):
                    label = f"{label} {detail.split()[0]}"
                samples[label].append(micros)

        print(
            f"\n=== {args.scenario} {args.rows}x{args.cols}, cycles={args.cycles}, "
            f"delay={args.delay_ms:g}ms, first-paint={first_paint_ms:.1f}ms, "
            f"wall={elapsed:.2f}s, output={(drained[0] - bytes_before) / 1024:.0f}KiB, "
            f"log={log.stat().st_size / 1024:.0f}KiB ==="
        )
        print(f"{'label':<42} {'n':>6} {'p50 us':>10} {'p95 us':>10} {'p99 us':>10} {'max us':>10}")
        for label, micros in sorted(startup.items()):
            print(f"{label:<42} {'1':>6} {micros:>10} {micros:>10} {micros:>10} {micros:>10}")
        for label, values in sorted(samples.items(), key=lambda entry: -sum(entry[1])):
            values.sort()
            print(
                f"{label:<42} {len(values):>6} {percentile(values, 50):>10} "
                f"{percentile(values, 95):>10} {percentile(values, 99):>10} {values[-1]:>10}"
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", choices=("typing", "search", "picker"))
    parser.add_argument("--file", default=str(ROOT / "src" / "editor.rs"))
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--query", default=None)
    parser.add_argument("--rows", type=int, default=50)
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--cycles", type=int, default=None)
    parser.add_argument("--delay-ms", type=float, default=10)
    parser.add_argument("--picker-load-wait", type=float, default=1.5)
    parser.add_argument("--startup-timeout", type=float, default=12)
    parser.add_argument("--config-override", action="append", default=[])
    args = parser.parse_args()
    if args.cycles is None:
        args.cycles = {"typing": 200, "search": 20, "picker": 15}[args.scenario]
    if args.query is None:
        args.query = "self" if args.scenario == "search" else "src/editor.rs"
    if not args.query:
        parser.error("--query cannot be empty")
    if args.rows < 3 or args.cols < 8 or args.cycles < 1 or args.delay_ms < 0:
        parser.error("rows >= 3, cols >= 8, cycles >= 1, and delay-ms >= 0 are required")
    run(args)


if __name__ == "__main__":
    main()
