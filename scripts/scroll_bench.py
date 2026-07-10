#!/usr/bin/env python3
"""Drive the red editor in a PTY, hold `j`, and summarize RED_PERF timings.

Usage: scroll_bench.py [rows] [cols] [presses] [delay_ms]
"""

import fcntl
import os
import pty
import re
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from collections import defaultdict

drained_bytes = [0]

ROWS = int(sys.argv[1]) if len(sys.argv) > 1 else 50
COLS = int(sys.argv[2]) if len(sys.argv) > 2 else 120
PRESSES = int(sys.argv[3]) if len(sys.argv) > 3 else 200
DELAY = (int(sys.argv[4]) if len(sys.argv) > 4 else 25) / 1000.0
BIN = os.path.join(os.path.dirname(__file__), "..", "target", "release", "red")
FILE = os.path.join(os.path.dirname(__file__), "..", "src", "editor.rs")


def main():
    with tempfile.TemporaryDirectory(prefix="red-perf-") as temp_dir:
        run_benchmark(temp_dir)


def run_benchmark(temp_dir):
    log = os.path.join(temp_dir, "red.log")
    config_dir = os.path.join(temp_dir, "red")
    os.makedirs(config_dir)
    with open(os.path.join(config_dir, "config.toml"), "w") as config:
        config.write(f'log_file = "{log}"\n')

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    env = dict(os.environ, RED_PERF="trace", XDG_CONFIG_HOME=temp_dir)
    proc = subprocess.Popen(
        [BIN, "--config-override", "lsp.enabled = false", FILE],
        stdin=slave,
        stdout=slave,
        stderr=subprocess.DEVNULL,
        env=env,
        close_fds=True,
    )
    os.close(slave)

    # Drain editor output continuously so the PTY never applies backpressure.
    def drain_loop():
        while True:
            try:
                data = os.read(master, 1 << 20)
            except OSError:
                return
            if not data:
                return
            drained_bytes[0] += len(data)

    threading.Thread(target=drain_loop, daemon=True).start()

    # Let the editor, plugins, and rust-analyzer settle.
    time.sleep(8)

    # Move to where every j scrolls, then mark the start of the measured window.
    os.write(master, b"100j")
    time.sleep(1)

    with open(log, "a") as f:
        f.write("[BENCH] begin\n")

    bytes_before = drained_bytes[0]
    start = time.time()
    for _ in range(PRESSES):
        os.write(master, b"j")
        time.sleep(DELAY)
    # Allow queued work to finish.
    time.sleep(2)
    elapsed = time.time() - start
    bytes_during = drained_bytes[0] - bytes_before

    with open(log, "a") as f:
        f.write("[BENCH] end\n")

    os.write(master, b":q!\r")
    time.sleep(1)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()

    report(log, elapsed, bytes_during)


def report(log, elapsed, bytes_during=0):
    in_window = False
    durations = defaultdict(list)
    startup = {}
    pattern = re.compile(r"\[PERF\] (\S+)(?: (.*?))?: (\d+)us")
    with open(log) as f:
        for line in f:
            if "[BENCH] begin" in line:
                in_window = True
            elif "[BENCH] end" in line:
                in_window = False
            elif in_window:
                m = pattern.search(line)
                if m:
                    label, detail, us = m.group(1), m.group(2) or "", int(m.group(3))
                    if label == "event":
                        detail = re.sub(r"\s*kind:.*", "", detail)
                        detail = detail[:40]
                    if label in ("notify", "drain"):
                        name = detail.split()[0] if detail else ""
                        name = re.sub(r":\d+$", "", name)
                        key = f"{label} {name}"
                    elif label == "event":
                        key = f"{label} {detail}"
                    else:
                        key = label
                    durations[key].append(us)
            else:
                m = pattern.search(line)
                if m and m.group(1).startswith("startup:"):
                    startup[m.group(1)] = int(m.group(3))

    print(
        f"\n=== {ROWS}x{COLS}, {PRESSES} presses @ {DELAY*1000:.0f}ms,"
        f" wall {elapsed:.1f}s, output {bytes_during/1024:.0f}KB ==="
    )
    print(f"{'label':<48} {'n':>5} {'total ms':>9} {'mean us':>8} {'p95 us':>8} {'max us':>8}")
    for label, micros in sorted(startup.items()):
        print(f"{label:<48} {'1':>5} {micros/1000:>9.1f} {micros:>8} {micros:>8} {micros:>8}")
    for key, vals in sorted(durations.items(), key=lambda kv: -sum(kv[1])):
        vals.sort()
        total = sum(vals)
        p95 = vals[int(len(vals) * 0.95) - 1] if len(vals) > 1 else vals[0]
        print(
            f"{key:<48} {len(vals):>5} {total/1000:>9.1f} {total/len(vals):>8.0f}"
            f" {p95:>8} {vals[-1]:>8}"
        )


if __name__ == "__main__":
    main()
