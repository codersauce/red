#!/usr/bin/env python3
"""Measure PR Replay navigation and repaint latency through a real terminal.

Build first with `cargo build --locked --release`, then run:
    python3 scripts/replay_bench.py --assert

Use `--profile debug` to inspect the same unoptimized build used by `cargo run`.
"""

import argparse
from collections import defaultdict
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import shutil
import struct
import subprocess
import tempfile
import termios
import threading
import time


ROOT = Path(__file__).resolve().parent.parent
TIMING = re.compile(r"\[PERF\] (\S+)(?: (.*?))?: (\d+)us")
NAVIGATION = (b"j", b"j", b"j", b"j", b"k", b"k", b"k", b"k")


def percentile(samples, value):
    return samples[(len(samples) - 1) * value // 100]


def append_marker(log, marker):
    with log.open("a", encoding="utf-8") as stream:
        stream.write(f"[REPLAY BENCH] {marker}\n")


def window_samples(log, begin, end):
    active = False
    samples = defaultdict(list)
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        if f"[REPLAY BENCH] {begin}" in line:
            active = True
            continue
        if f"[REPLAY BENCH] {end}" in line:
            active = False
            continue
        if not active:
            continue
        match = TIMING.search(line)
        if not match:
            continue
        label, detail, micros = match.group(1), match.group(2) or "", int(match.group(3))
        if label == "event":
            if "Char('j')" not in detail and "Char('k')" not in detail:
                continue
            label = "replay:key_event"
        elif label == "notify":
            if "panel:event:replay-coach" not in detail:
                continue
            label = "replay:plugin_action"
        elif label == "drain":
            if not any(part in detail for part in ("Replay", "TextPanel", "FocusPanel")):
                continue
            label = f"replay:drain {detail.split()[0]}"
        elif not label.startswith(("replay:", "workspace:", "render:")):
            continue
        samples[label].append(micros)
    return samples


def print_samples(title, samples):
    print(f"\n=== {title} ===")
    print(f"{'label':<38} {'n':>6} {'p50 us':>10} {'p95 us':>10} {'p99 us':>10} {'max us':>10}")
    for label, values in sorted(samples.items(), key=lambda entry: -sum(entry[1])):
        values.sort()
        print(
            f"{label:<38} {len(values):>6} {percentile(values, 50):>10} "
            f"{percentile(values, 95):>10} {percentile(values, 99):>10} {values[-1]:>10}"
        )


def wait_until(predicate, process, timeout, message):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        if process.poll() is not None:
            detail = ""
            if process.stderr is not None:
                stderr = process.stderr.read().decode("utf-8", errors="replace").strip()
                if stderr:
                    detail = f" ({stderr.splitlines()[0][:300]})"
            raise RuntimeError(f"editor exited before {message}: {process.returncode}{detail}")
        time.sleep(0.002)
    raise RuntimeError(f"timed out waiting for {message}")


def run(args):
    binary = Path(args.binary).resolve()
    root = Path(args.root).resolve()
    source = Path(args.file).resolve()
    if not binary.is_file():
        raise SystemExit(f"build the {args.profile} binary first: {binary}")
    if not root.is_dir() or not source.is_file():
        raise SystemExit("benchmark root and source file must exist")
    snapshot = Path(args.session_snapshot).expanduser().resolve() if args.session_snapshot else None
    if snapshot is not None and not snapshot.is_file():
        raise SystemExit(f"recoverable session snapshot does not exist: {snapshot}")
    review_changes = 5
    if snapshot is not None:
        with snapshot.open(encoding="utf-8") as stream:
            recovered = json.load(stream)
        sessions = recovered.get("replay", {}).get("controller", {}).get("sessions", [])
        if not sessions:
            raise SystemExit("session snapshot contains no recoverable PR Replay review")
        review_changes = max(len(session.get("steps", [])) for session in sessions)
    navigation = {
        "oscillate": NAVIGATION,
        "forward": (b"j",),
        "backward": (b"k",),
    }[args.navigation]

    with tempfile.TemporaryDirectory(prefix="red-replay-perf-") as directory:
        config_home = Path(directory)
        config_dir = config_home / "red"
        config_dir.mkdir()
        log = config_home / "red.log"
        (config_dir / "config.toml").write_text(f'log_file = "{log}"\n', encoding="utf-8")
        if snapshot is not None:
            session_dir = config_dir / "sessions"
            session_dir.mkdir()
            shutil.copyfile(snapshot, session_dir / "latest.json")

        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", args.rows, args.cols, 0, 0),
        )
        command = [str(binary)]
        if snapshot is None:
            command.extend(["--root", str(root)])
        if not args.enable_lsp:
            command.extend(["--config-override", "lsp.enabled = false"])
        if snapshot is None:
            command.append(str(source))
        else:
            command.append("--resume")
        process = subprocess.Popen(
            command,
            stdin=slave,
            stdout=slave,
            stderr=subprocess.PIPE,
            env=dict(os.environ, RED_PERF="trace", XDG_CONFIG_HOME=str(config_home)),
            close_fds=True,
        )
        os.close(slave)
        terminal = {"bytes": 0, "last_output": 0.0}

        def drain():
            while True:
                try:
                    data = os.read(master, 1 << 20)
                except OSError:
                    return
                if not data:
                    return
                terminal["bytes"] += len(data)
                terminal["last_output"] = time.monotonic()

        threading.Thread(target=drain, daemon=True).start()
        try:
            wait_until(
                lambda: log.exists()
                and "[PERF] startup:interactive:" in log.read_text(
                    encoding="utf-8", errors="replace"
                ),
                process,
                args.startup_timeout,
                "first editor frame",
            )
            if snapshot is None:
                os.write(master, b":ReplayDemo\r")
            wait_until(
                lambda: "[PERF] replay:panel_render:" in log.read_text(
                    encoding="utf-8", errors="replace"
                ),
                process,
                args.startup_timeout,
                "restored Replay review" if snapshot is not None else "Replay demo",
            )
            time.sleep(0.1)

            for key in NAVIGATION:
                os.write(master, key)
                time.sleep(0.025)

            append_marker(log, "interactive begin")
            bytes_before = terminal["bytes"]
            visible_latencies = []
            for index in range(args.cycles):
                started = time.monotonic()
                previous_bytes = terminal["bytes"]
                os.write(master, navigation[index % len(navigation)])
                wait_until(
                    lambda: terminal["bytes"] != previous_bytes,
                    process,
                    args.navigation_timeout,
                    "Replay navigation repaint",
                )
                while time.monotonic() - terminal["last_output"] < args.settle_ms / 1000:
                    if process.poll() is not None:
                        raise RuntimeError("editor exited during Replay navigation")
                    time.sleep(0.001)
                visible_latencies.append(int((terminal["last_output"] - started) * 1_000_000))
            append_marker(log, "interactive end")
            interactive_bytes = terminal["bytes"] - bytes_before

            append_marker(log, "burst begin")
            bytes_before = terminal["bytes"]
            started = time.monotonic()
            for index in range(args.burst):
                last_key = time.monotonic()
                os.write(master, navigation[index % len(navigation)])
                if args.delay_ms:
                    time.sleep(args.delay_ms / 1000)
            wait_until(
                lambda: terminal["last_output"] >= last_key,
                process,
                args.navigation_timeout,
                "final sustained Replay navigation repaint",
            )
            while time.monotonic() - terminal["last_output"] < args.settle_ms / 1000:
                if process.poll() is not None:
                    raise RuntimeError("editor exited during sustained Replay navigation")
                time.sleep(0.001)
            burst_elapsed = time.monotonic() - started
            burst_tail = max(0.0, terminal["last_output"] - last_key)
            append_marker(log, "burst end")
            burst_bytes = terminal["bytes"] - bytes_before

            os.write(master, b"\x1b:q!\r")
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

            interactive = window_samples(log, "interactive begin", "interactive end")
            interactive["replay:visible_settle"] = visible_latencies
            burst = window_samples(log, "burst begin", "burst end")
            if len(interactive.get("replay:update_panel", [])) != args.cycles:
                raise RuntimeError(
                    "Replay benchmark did not exercise one actual step change per keypress"
                )
            if len(interactive.get("render:full", [])) > args.cycles + 2:
                raise RuntimeError("Replay navigation repainted the complete terminal repeatedly")
            if len(burst.get("replay:key_event", [])) != args.burst:
                raise RuntimeError("Replay benchmark did not drain all sustained navigation keys")
            if args.trace_output:
                Path(args.trace_output).expanduser().write_bytes(log.read_bytes())
            print(
                f"profile={args.profile} scenario={'restored' if snapshot else 'demo'} "
                f"navigation={args.navigation} changes={review_changes} "
                f"terminal={args.cols}x{args.rows} "
                f"steps={args.cycles} output={interactive_bytes / 1024:.1f}KiB "
                f"burst={args.burst} burst_wall={burst_elapsed * 1000:.1f}ms "
                f"burst_tail={burst_tail * 1000:.1f}ms "
                f"burst_output={burst_bytes / 1024:.1f}KiB"
            )
            print_samples("individual change navigation", interactive)
            print_samples("sustained change navigation", burst)

            if args.assert_budget:
                visible_latencies.sort()
                p95 = percentile(visible_latencies, 95)
                if p95 > args.p95_ms * 1000:
                    raise SystemExit(
                        f"Replay navigation p95 {p95 / 1000:.2f}ms "
                        f"exceeds {args.p95_ms:g}ms budget"
                    )
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("debug", "release"), default="release")
    parser.add_argument("--binary")
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--file", default=str(ROOT / "src" / "editor.rs"))
    parser.add_argument("--session-snapshot", help="copy and resume a real saved review safely")
    parser.add_argument(
        "--navigation", choices=("oscillate", "forward", "backward"), default="oscillate"
    )
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--cycles", type=int, default=32)
    parser.add_argument("--burst", type=int, default=80)
    parser.add_argument("--delay-ms", type=float, default=2)
    parser.add_argument("--settle-ms", type=float, default=20)
    parser.add_argument("--startup-timeout", type=float, default=20)
    parser.add_argument("--navigation-timeout", type=float, default=3)
    parser.add_argument("--trace-output", help="retain the complete editor performance trace")
    parser.add_argument("--enable-lsp", action="store_true")
    parser.add_argument("--assert", dest="assert_budget", action="store_true")
    parser.add_argument("--p95-ms", type=float)
    args = parser.parse_args()
    if args.binary is None:
        args.binary = str(ROOT / "target" / args.profile / "red")
    if args.p95_ms is None:
        args.p95_ms = 16 if args.profile == "release" else 50
    if args.rows < 8 or args.cols < 30 or args.cycles < 1 or args.burst < 1:
        parser.error("rows >= 8, cols >= 30, cycles >= 1, and burst >= 1 are required")
    if min(args.delay_ms, args.settle_ms, args.p95_ms) < 0:
        parser.error("delay, settle window, and performance budget cannot be negative")
    run(args)


if __name__ == "__main__":
    main()
