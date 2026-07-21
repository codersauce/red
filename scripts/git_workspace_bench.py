#!/usr/bin/env python3
"""Measure Git workspace row churn and core-owned diff navigation in a PTY.

Build first with `cargo build --locked --release`, then run:
    python3 scripts/git_workspace_bench.py --files 80 --presses 120
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


def run_git(root, *args):
    subprocess.run(
        ["git", *args],
        cwd=root,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def make_repository(root, files):
    run_git(root, "init", "-q")
    for index in range(files):
        (root / f"file_{index:03}.rs").write_text(
            f"fn value_{index}() -> usize {{ {index} }}\n", encoding="utf-8"
        )
    run_git(root, "add", ".")
    run_git(
        root,
        "-c",
        "user.name=Red Bench",
        "-c",
        "user.email=red-bench@example.test",
        "commit",
        "-qm",
        "baseline",
    )
    for index in range(files):
        (root / f"file_{index:03}.rs").write_text(
            f"fn value_{index}() -> usize {{ {index} + 1 }}\n", encoding="utf-8"
        )


def timing_report(log, begin, end):
    active = False
    samples = defaultdict(list)
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        if begin in line:
            active = True
            continue
        if end in line:
            active = False
            continue
        if not active:
            continue
        match = TIMING.search(line)
        if not match:
            continue
        label, detail, micros = match.group(1), match.group(2) or "", int(match.group(3))
        if label in ("notify", "drain"):
            label = f"{label} {detail.split()[0]}"
        samples[label].append(micros)
    return samples


def process_count(samples):
    return sum(label.startswith("notify process:") for label in samples)


def print_samples(title, samples, invocations):
    print(f"\n=== {title}: git invocations={invocations} ===")
    print(f"{'label':<45} {'n':>6} {'p50 us':>10} {'p95 us':>10} {'max us':>10}")
    for label, values in sorted(samples.items(), key=lambda item: -sum(item[1])):
        values.sort()
        print(
            f"{label:<45} {len(values):>6} {percentile(values, 50):>10} "
            f"{percentile(values, 95):>10} {values[-1]:>10}"
        )


def run(args):
    if not BIN.exists():
        raise SystemExit("build the release binary first: cargo build --locked --release")
    with tempfile.TemporaryDirectory(prefix="red-git-workspace-perf-") as temp_name:
        temp = Path(temp_name)
        repository = temp / "repository"
        repository.mkdir()
        make_repository(repository, args.files)
        config_home = temp / "config"
        config_dir = config_home / "red"
        config_dir.mkdir(parents=True)
        log = temp / "red.log"
        (config_dir / "config.toml").write_text(f'log_file = "{log}"\n', encoding="utf-8")

        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", args.rows, args.cols, 0, 0),
        )
        process = subprocess.Popen(
            [
                str(BIN),
                "--root",
                str(repository),
                "--config-override",
                "lsp.enabled = false",
                str(repository / "file_000.rs"),
            ],
            stdin=slave,
            stdout=slave,
            stderr=subprocess.DEVNULL,
            env=dict(
                os.environ,
                RED_PERF="trace",
                XDG_CONFIG_HOME=str(config_home),
            ),
            close_fds=True,
        )
        os.close(slave)

        def drain():
            while True:
                try:
                    if not os.read(master, 1 << 20):
                        return
                except OSError:
                    return

        threading.Thread(target=drain, daemon=True).start()
        try:
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                if log.exists() and "[PERF] startup:interactive:" in log.read_text(
                    encoding="utf-8", errors="replace"
                ):
                    break
                if process.poll() is not None:
                    raise RuntimeError("editor exited before first paint")
                time.sleep(0.02)
            else:
                raise RuntimeError("editor did not reach first paint")

            os.write(master, b":GitDashboard\r")
            time.sleep(1.2)
            with log.open("a", encoding="utf-8") as stream:
                stream.write("[GIT BENCH] rows begin\n")
            for _ in range(args.presses):
                os.write(master, b"j")
                time.sleep(args.delay_ms / 1000)
            time.sleep(0.3)
            with log.open("a", encoding="utf-8") as stream:
                stream.write("[GIT BENCH] rows end\n")
            row_samples = timing_report(
                log, "[GIT BENCH] rows begin", "[GIT BENCH] rows end"
            )
            row_processes = process_count(row_samples)

            os.write(master, b"\t")
            time.sleep(0.1)
            with log.open("a", encoding="utf-8") as stream:
                stream.write("[GIT BENCH] detail begin\n")
            for _ in range(args.presses):
                os.write(master, b"j")
                time.sleep(args.delay_ms / 1000)
            time.sleep(0.2)
            with log.open("a", encoding="utf-8") as stream:
                stream.write("[GIT BENCH] detail end\n")
            detail_samples = timing_report(
                log, "[GIT BENCH] detail begin", "[GIT BENCH] detail end"
            )
            detail_processes = process_count(detail_samples)

            contents = log.read_text(encoding="utf-8", errors="replace")
            if "maximum of 16 active processes" in contents or "quarantined plugin `git`" in contents:
                raise RuntimeError("Git plugin exceeded its process budget or was quarantined")
            print_samples(
                "file-list churn",
                row_samples,
                row_processes,
            )
            print_samples(
                "diff navigation",
                detail_samples,
                detail_processes,
            )
            if row_processes > 2:
                raise RuntimeError(f"row churn spawned {row_processes} Git processes; expected <= 2")
            if detail_processes != 0:
                raise RuntimeError(f"core-owned diff navigation spawned {detail_processes} Git processes")
        finally:
            if process.poll() is None:
                os.write(master, b"q")
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
            os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--files", type=int, default=80)
    parser.add_argument("--presses", type=int, default=120)
    parser.add_argument("--delay-ms", type=float, default=2)
    parser.add_argument("--rows", type=int, default=30)
    parser.add_argument("--cols", type=int, default=100)
    args = parser.parse_args()
    if args.files < 2 or args.presses < 1 or args.delay_ms < 0:
        parser.error("files >= 2, presses >= 1, and delay-ms >= 0 are required")
    run(args)


if __name__ == "__main__":
    main()
