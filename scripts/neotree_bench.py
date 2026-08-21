#!/usr/bin/env python3
"""Measure complete Neo-tree browsing and scaling through a real editor PTY.

Build with `cargo build --locked --release --bin red`, then run:

    python3 scripts/neotree_bench.py --sizes 128 512 2048 8192 --samples 3

Pass `--require-complete` to turn missing final entries into a regression.
"""

import argparse
from collections import defaultdict
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import statistics
import struct
import subprocess
import tempfile
import termios
import threading
import time


ROOT = Path(__file__).resolve().parent.parent
TIMING = re.compile(r"\[PERF\] (\S+)(?: (.*?))?: (\d+)us")
TARGET = "zz-final-target.rs"


def cpu_seconds(pid):
    value = subprocess.check_output(
        ["ps", "-o", "time=", "-p", str(pid)], text=True
    ).strip()
    total = 0.0
    for component in value.replace("-", ":").split(":"):
        total = total * 60 + float(component)
    return total


def rss_kib(pid):
    value = subprocess.check_output(
        ["ps", "-o", "rss=", "-p", str(pid)], text=True
    ).strip()
    return int(value)


def timing_samples(contents):
    samples = defaultdict(list)
    for line in contents.splitlines():
        match = TIMING.search(line)
        if not match:
            continue
        label, detail, micros = match.group(1), match.group(2) or "", int(match.group(3))
        if label in ("drain", "notify", "husk:notify") and detail:
            label = f"{label} {detail.split()[0]}"
        samples[label].append(micros)
    return dict(samples)


def percentile(values, percentile):
    if not values:
        return 0
    ordered = sorted(values)
    return ordered[(len(ordered) - 1) * percentile // 100]


def summarize(samples):
    grouped = defaultdict(list)
    for sample in samples:
        grouped[sample["entries"]].append(sample)

    result = []
    for entries, cases in sorted(grouped.items()):
        result.append(
            {
                "entries": entries,
                "samples": len(cases),
                "complete": all(case["target_reachable"] for case in cases),
                "open_ms_median": round(
                    statistics.median(case["open_ms"] for case in cases), 2
                ),
                "directory_ms_median": round(
                    statistics.median(case["directory_ms"] for case in cases), 2
                ),
                "navigation_p95_us_median": round(
                    statistics.median(case["navigation_p95_us"] for case in cases)
                ),
                "rss_delta_kib_median": round(
                    statistics.median(case["rss_delta_kib"] for case in cases)
                ),
                "idle_cpu_ms_median": round(
                    statistics.median(case["idle_cpu_ms"] for case in cases), 2
                ),
                "truncation_marker": any(case["truncation_marker"] for case in cases),
            }
        )
    return result


class Driver:
    def __init__(self, binary, root, config_home, rows, cols, timeout, active_target=False):
        self.log = config_home / "red.log"
        config_dir = config_home / "red"
        config_dir.mkdir(parents=True)
        config_dir.joinpath("config.toml").write_text(
            f"log_file = {json.dumps(str(self.log))}\n"
            "show_whats_new = false\nfetch_release_notes = false\n"
            "[lsp]\nenabled = false\n",
            encoding="utf-8",
        )

        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.capture = bytearray()
        active_file = TARGET if active_target else "000-open.rs"
        self.process = subprocess.Popen(
            [str(binary), "--root", str(root), str(root / active_file)],
            cwd=root,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
            env=dict(
                os.environ,
                RED_PERF="trace",
                XDG_CONFIG_HOME=str(config_home),
                TERM="xterm-256color",
                COLORTERM="truecolor",
            ),
        )
        os.close(slave)
        threading.Thread(target=self.drain, daemon=True).start()
        self.wait_for(
            lambda: "[PERF] startup:interactive:" in self.read_log(),
            timeout,
            "first editor frame",
        )
        time.sleep(0.1)

    def drain(self):
        while True:
            try:
                data = os.read(self.master, 1 << 20)
            except OSError:
                return
            if not data:
                return
            self.capture.extend(data)

    def read_log(self):
        if not self.log.exists():
            return ""
        return self.log.read_text(encoding="utf-8", errors="replace")

    def wait_for(self, condition, timeout, description):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if condition():
                return
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"editor exited during {description}: {self.process.returncode}"
                )
            time.sleep(0.01)
        raise TimeoutError(description)

    def send(self, payload):
        while payload:
            count = os.write(self.master, payload)
            if count <= 0:
                raise RuntimeError("PTY write made no progress")
            payload = payload[count:]

    def run(self, entries, navigation_presses, timeout):
        before_rss = rss_kib(self.process.pid)
        before_log = len(self.read_log())
        before_capture = len(self.capture)
        started = time.monotonic()
        self.send(b":NeoTree\r")

        def opened():
            content = self.read_log()[before_log:]
            directory_finished = (
                "[PERF] drain ListDirectory:" in content
                or "[PERF] drain DirectoryListed:" in content
            )
            updates = content.count("[PERF] drain UpdatePanel:")
            updates += content.count("[PERF] drain UpdateTreePanel:")
            populated = b"item-000000.rs" in self.capture[before_capture:]
            return directory_finished and updates >= 2 and populated

        self.wait_for(opened, timeout, "populated Neo-tree")
        open_ms = (time.monotonic() - started) * 1000
        time.sleep(0.08)
        after_rss = rss_kib(self.process.pid)

        idle_before = cpu_seconds(self.process.pid)
        time.sleep(0.35)
        idle_cpu_ms = max(0.0, cpu_seconds(self.process.pid) - idle_before) * 1000

        navigation_log = len(self.read_log())
        screen_start = len(self.capture)
        self.send(b"G")
        self.wait_for(
            lambda: "panel:event:neotree" in self.read_log()[navigation_log:],
            timeout,
            "jump to final tree entry",
        )
        time.sleep(0.05)
        final_screen = bytes(self.capture[screen_start:])
        target_reachable = TARGET.encode() in final_screen
        truncation_marker = (
            b"tree limited" in final_screen or b"listing truncated" in final_screen
        )

        self.send(b"g")
        time.sleep(0.03)
        for _ in range(navigation_presses):
            self.send(b"j")
            time.sleep(0.003)
        time.sleep(0.1)
        navigation = timing_samples(self.read_log()[navigation_log:])
        opened_log = timing_samples(self.read_log()[before_log:navigation_log])
        directory_spans = opened_log.get("drain ListDirectory", [])
        directory_spans.extend(opened_log.get("drain DirectoryListed", []))

        return {
            "entries": entries,
            "target_reachable": target_reachable,
            "truncation_marker": truncation_marker,
            "open_ms": round(open_ms, 2),
            "directory_ms": round(sum(directory_spans) / 1000, 2),
            "navigation_p95_us": percentile(navigation.get("event", []), 95),
            "rss_delta_kib": max(0, after_rss - before_rss),
            "idle_cpu_ms": round(idle_cpu_ms, 2),
        }

    def close(self):
        if self.process.poll() is None:
            self.send(b"q:q!\r")
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                self.process.wait(timeout=5)
        os.close(self.master)


def populate(root, entries):
    root.mkdir(parents=True)
    root.joinpath("000-open.rs").write_text("fn open() {}\n", encoding="utf-8")
    for index in range(entries):
        root.joinpath(f"item-{index:06}.rs").write_text("", encoding="utf-8")
    root.joinpath(TARGET).write_text("fn final_target() {}\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/red")
    parser.add_argument("--sizes", type=int, nargs="+", default=[128, 512, 2048, 8192])
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--navigation-presses", type=int, default=50)
    parser.add_argument("--rows", type=int, default=45)
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--require-complete", action="store_true")
    parser.add_argument(
        "--active-target",
        action="store_true",
        help="open the final file first to exercise large-directory reveal",
    )
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"release binary does not exist: {binary}")
    if args.samples < 1 or any(size < 1 for size in args.sizes):
        parser.error("samples and every size must be positive")

    measurements = []
    with tempfile.TemporaryDirectory(prefix="red-neotree-bench-") as temp_name:
        temp = Path(temp_name)
        for entries in args.sizes:
            root = temp / f"workspace-{entries}"
            populate(root, entries)
            for sample in range(args.samples):
                driver = Driver(
                    binary,
                    root,
                    temp / f"config-{entries}-{sample}",
                    args.rows,
                    args.cols,
                    args.timeout,
                    args.active_target,
                )
                try:
                    measurement = driver.run(
                        entries, args.navigation_presses, args.timeout
                    )
                    measurements.append(measurement)
                    print(json.dumps(measurement), flush=True)
                finally:
                    driver.close()

    result = {
        "binary": str(binary),
        "sizes": args.sizes,
        "samples_per_size": args.samples,
        "measurements": measurements,
        "summary": summarize(measurements),
    }
    print(json.dumps({"summary": result["summary"]}, indent=2))
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    if args.require_complete and any(not case["complete"] for case in result["summary"]):
        raise SystemExit("Neo-tree did not expose every directory entry")


if __name__ == "__main__":
    main()
