#!/usr/bin/env python3
"""Exercise detached editing, resize, mouse, large paste, and reattach in a PTY.

Usage: detach_bench.py [rows] [cols] [edits] [paste_kib]
"""

from collections import defaultdict
import fcntl
import os
from pathlib import Path
import pty
import re
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time


ROWS = int(sys.argv[1]) if len(sys.argv) > 1 else 50
COLS = int(sys.argv[2]) if len(sys.argv) > 2 else 120
EDITS = int(sys.argv[3]) if len(sys.argv) > 3 else 120
PASTE_KIB = int(sys.argv[4]) if len(sys.argv) > 4 else 1536
ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "red"


class PtyClient:
    def __init__(self, argv, env, rows, cols):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.bytes = 0
        self.process = subprocess.Popen(
            argv,
            stdin=slave,
            stdout=slave,
            stderr=subprocess.DEVNULL,
            env=env,
            close_fds=True,
        )
        os.close(slave)
        threading.Thread(target=self._drain, daemon=True).start()

    def _drain(self):
        while True:
            try:
                data = os.read(self.master, 1 << 20)
            except OSError:
                return
            if not data:
                return
            self.bytes += len(data)

    def send(self, data):
        pending = memoryview(data)
        while pending:
            written = os.write(self.master, pending[:8192])
            if written == 0:
                raise RuntimeError("PTY closed while writing input")
            pending = pending[written:]

    def resize(self, rows, cols):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        os.kill(self.process.pid, signal.SIGWINCH)

    def wait(self, timeout=5):
        try:
            self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=timeout)
        finally:
            os.close(self.master)


def wait_for(path, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.02)
    raise RuntimeError(f"timed out waiting for {path}")


def report(log, elapsed, output_bytes):
    timings = defaultdict(list)
    counters = {}
    gauges = {}
    timing = re.compile(r"\[PERF\] (detach:\S+).*?: (\d+)us")
    counter = re.compile(r"\[PERF\] counter (detach:\S+): (\d+)")
    gauge = re.compile(r"\[PERF\] gauge (detach:\S+): (\d+)")
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        if match := timing.search(line):
            timings[match[1]].append(int(match[2]))
        if match := counter.search(line):
            counters[match[1]] = int(match[2])
        if match := gauge.search(line):
            gauges[match[1]] = int(match[2])

    print(
        f"\n=== detached {ROWS}x{COLS}, {EDITS} edits, {PASTE_KIB}KiB paste, "
        f"wall {elapsed:.2f}s, output {output_bytes / 1024:.0f}KiB ==="
    )
    print(f"{'label':<36} {'n':>6} {'p50 us':>10} {'p95 us':>10} {'max us':>10}")
    for label, samples in sorted(timings.items()):
        samples.sort()
        p50 = samples[(len(samples) - 1) * 50 // 100]
        p95 = samples[(len(samples) - 1) * 95 // 100]
        print(f"{label:<36} {len(samples):>6} {p50:>10} {p95:>10} {samples[-1]:>10}")
    for label, value in sorted(counters.items()):
        print(f"{label:<36} {'counter':>6} {value:>10}")
    for label, value in sorted(gauges.items()):
        print(f"{label:<36} {'max':>6} {value:>10}")


def main():
    if not BIN.exists():
        raise SystemExit("build the release binary first: cargo build --locked --release")
    if ROWS < 3 or COLS < 8 or EDITS < 1 or PASTE_KIB < 1:
        raise SystemExit("rows >= 3, cols >= 8, edits >= 1, and paste_kib >= 1 are required")

    with tempfile.TemporaryDirectory(prefix="red-detach-perf-") as temp:
        temp = Path(temp)
        config_dir = temp / "red"
        config_dir.mkdir()
        log = temp / "red.log"
        (config_dir / "config.toml").write_text(f'log_file = "{log}"\n', encoding="utf-8")
        source = temp / "wide-buffer.txt"
        source.write_text(
            "".join(f"line {line:04}: start 👋 漢字 👩‍💻 e\u0301 끝 end\n" for line in range(2500)),
            encoding="utf-8",
        )
        env = dict(os.environ, RED_PERF="trace", XDG_CONFIG_HOME=str(temp), NO_COLOR="1")
        session = "perf"
        socket = config_dir / "run" / f"{session}.sock"
        first = PtyClient(
            [str(BIN), "--config-override", "lsp.enabled = false", f"--detach={session}", str(source)],
            env,
            ROWS,
            COLS,
        )
        second = None
        try:
            wait_for(socket)
            time.sleep(0.5)
            bytes_before = first.bytes
            started = time.monotonic()
            first.send(b"100ji")
            for index in range(EDITS):
                first.send(f"a{index % 10}\x1b[B".encode())
                time.sleep(0.003)
            first.send(b"\x1b")
            time.sleep(0.08)
            first.send(b"\x1b[<0;8;3M\x1b[<0;8;3m")
            first.send(b"\x1b[<65;8;3M")
            for rows, cols in [
                (max(3, ROWS // 2), COLS),
                (ROWS, max(8, COLS // 2)),
                (ROWS, COLS),
            ]:
                first.resize(rows, cols)
                time.sleep(0.08)
            fragment = "paste 👋 漢字 e\u0301\n"
            repeat = (PASTE_KIB * 1024 // len(fragment.encode())) + 1
            paste = (fragment * repeat).encode()[: PASTE_KIB * 1024]
            while True:
                try:
                    paste.decode("utf-8")
                    break
                except UnicodeDecodeError:
                    paste = paste[:-1]
            first.send(b"i\x1b[200~" + paste + b"\x1b[201~\x1b")
            time.sleep(1)
            first.send(b"\x1c")
            first.wait()

            second = PtyClient([str(BIN), "--attach", session], env, ROWS, COLS)
            time.sleep(0.6)
            second.send(b"j")
            time.sleep(0.2)
            output_bytes = first.bytes - bytes_before + second.bytes
            elapsed = time.monotonic() - started
            subprocess.run(
                [str(BIN), "--stop", session],
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=True,
                timeout=8,
            )
            second.wait()
            report(log, elapsed, output_bytes)
        finally:
            if first.process.poll() is None:
                first.process.kill()
            if second is not None and second.process.poll() is None:
                second.process.kill()
            subprocess.run(
                [str(BIN), "--stop", session],
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                timeout=8,
            )


if __name__ == "__main__":
    main()
