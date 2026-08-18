#!/usr/bin/env python3
"""Measure Enter-to-screen-restore and Enter-to-process-exit on Unix.

Use --dirty to verify refused/forced quit and recovery, or --storage-updates 200
to exercise a busy plugin exit hook. All runs use disposable editor state.
"""

import argparse
import fcntl
import json
import os
import pty
import re
import select
import statistics
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

LEAVE = b"\x1b[?1049l"
TIMING = re.compile(r"\[PERF\] ((?:shutdown:|session:snapshot)[^:]*): (\d+)us")


def run_once(binary, size, dirty=False, storage_updates=0, preference_kib=0):
    with tempfile.TemporaryDirectory(prefix="red-quit-bench-") as directory:
        root = Path(directory)
        config = root / "red"
        config.mkdir()
        if preference_kib:
            (config / "preferences.json").write_text(
                json.dumps(
                    {
                        "plugin_storage": {
                            "quit_bench:payload": "x" * (preference_kib * 1024)
                        }
                    }
                )
            )
        log = root / "red.log"
        extra_config = ""
        if storage_updates:
            plugin = root / "quit_bench.hk"
            plugin.write_text(
                '#[red::lifecycle("before_exit")]\nfn before_exit(snapshot: Json) {\n'
                + "".join(
                    f'    red::execute("SetStorage", "key-{i}", "quit-bench-value-{i}");\n'
                    for i in range(storage_updates)
                )
                + "}\n"
            )
            extra_config = f"\n[plugins]\nquit_bench = {json.dumps(str(plugin))}\n"
        (config / "config.toml").write_text(
            f"log_file = {json.dumps(str(log))}\nshow_whats_new = false\n"
            "disable_ai = true\n[lsp]\nenabled = false\n" + extra_config
        )
        source = root / "fixture.txt"
        source.write_text("quit latency fixture\n" * size)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 100, 0, 0))
        process = subprocess.Popen(
            [str(binary), "--root", str(root), str(source)],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
            env=dict(
                os.environ,
                TERM="xterm-256color",
                RED_PERF="trace",
                XDG_CONFIG_HOME=str(root),
            ),
        )
        output = bytearray()

        def drain(wait=0.02):
            if select.select([master], [], [], wait)[0]:
                try:
                    output.extend(os.read(master, 1 << 20))
                except OSError:
                    pass

        def wait_until(predicate, timeout=30):
            deadline = time.monotonic() + timeout
            while not predicate():
                if time.monotonic() >= deadline:
                    raise RuntimeError("timed out: " + repr(bytes(output[-300:])))
                if process.poll() is not None:
                    raise RuntimeError(
                        f"editor exited early ({process.returncode}): {bytes(output[-300:])!r}"
                    )
                drain()

        def settle(seconds=0.10):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                drain(min(0.01, max(0, deadline - time.monotonic())))

        def send_and_wait(keys, code):
            marker = "[BENCH] input\n"
            with log.open("a") as stream:
                stream.write(marker)
            os.write(master, keys)
            wait_until(
                lambda: (
                    f"event Key(KeyEvent {{ code: {code},"
                    in log.read_text(errors="replace").rsplit(marker, 1)[-1]
                )
            )

        try:
            wait_until(
                lambda: (
                    log.exists()
                    and "[PERF] startup:interactive:" in log.read_text(errors="replace")
                )
            )
            settle()
            if dirty:
                output.clear()
                os.write(master, b"iUNSAVED-QUIT-MARKER")
                output.clear()
                send_and_wait(b"\x1b", "Esc")
                send_and_wait(b":q", "Char('q')")
                os.write(master, b"\r")
                # Differential rendering may move the cursor between words.
                wait_until(lambda: b"unwritten" in output and b"changes:" in output)
                assert LEAVE not in output, "rejected quit left alternate screen"
                assert process.poll() is None, "rejected quit exited"
            output.clear()
            send_and_wait(
                b":q!" if dirty else b":q", "Char('!')" if dirty else "Char('q')"
            )
            output.clear()
            with log.open("a") as stream:
                stream.write("[BENCH] quit\n")
            started = time.monotonic_ns()
            os.write(master, b"\r")
            restored = None
            deadline = time.monotonic() + 30
            while process.poll() is None:
                drain(0.001)
                if restored is None and LEAVE in output:
                    restored = time.monotonic_ns()
                if time.monotonic() > deadline:
                    raise RuntimeError("quit timed out")
            exited = time.monotonic_ns()
            drain(0)
            if restored is None and LEAVE in output:
                restored = time.monotonic_ns()
            assert process.returncode == 0, f"exit status {process.returncode}"
            assert restored is not None, "missing alternate-screen restore"
            assert output.count(LEAVE) == 1, "terminal restored more than once"
            modes = termios.tcgetattr(slave)
            assert modes[3] & termios.ICANON and modes[3] & termios.ECHO, (
                "raw mode was not restored"
            )
            if dirty:
                assert "UNSAVED-QUIT-MARKER" not in source.read_text(), (
                    "forced quit overwrote source"
                )
                assert any(
                    "UNSAVED-QUIT-MARKER" in p.read_text(errors="replace")
                    for p in (config / "sessions").rglob("*.json")
                ), "recovery snapshot lost edit"
            preferences = json.loads((config / "preferences.json").read_text())
            assert preferences["command_history"][-1] == ("q!" if dirty else "q"), (
                "quit command history was not persisted"
            )
            if storage_updates:
                assert (
                    f"quit-bench-value-{storage_updates - 1}"
                    in preferences["plugin_storage"].values()
                ), "exit-hook storage was not persisted"
            return {
                "screen_ms": round((restored - started) / 1e6, 3),
                "exit_ms": round((exited - started) / 1e6, 3),
                "leave_count": output.count(LEAVE),
                "dirty_guard": dirty,
                "timings_us": {
                    label: int(micros)
                    for label, micros in TIMING.findall(log.read_text(errors="replace"))
                },
                "quit_timings_us": {
                    label: int(micros)
                    for label, micros in re.findall(
                        r"\[PERF\] (.*): (\d+)us",
                        log.read_text(errors="replace").rsplit("[BENCH] quit\n", 1)[-1],
                    )
                    if int(micros) >= 1000 and not label.startswith("timing ")
                },
            }
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--lines", type=int, default=100)
    parser.add_argument("--dirty", action="store_true")
    parser.add_argument("--storage-updates", type=int, default=0)
    parser.add_argument("--preference-kib", type=int, default=0)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    if (
        args.runs < 1
        or args.lines < 1
        or args.storage_updates < 0
        or args.preference_kib < 0
    ):
        parser.error(
            "runs and lines must be positive; storage-updates and preference-kib must be nonnegative"
        )
    results = []
    for _ in range(args.runs):
        results.append(
            run_once(
                args.binary.resolve(),
                args.lines,
                args.dirty,
                args.storage_updates,
                args.preference_kib,
            )
        )
        if args.json:
            args.json.write_text(json.dumps({"runs": results}, indent=2) + "\n")
    report = {
        "binary": str(args.binary.resolve()),
        "lines": args.lines,
        "dirty": args.dirty,
        "storage_updates": args.storage_updates,
        "preference_kib": args.preference_kib,
        "runs": results,
        "summary": {
            key: {
                "median": round(statistics.median(r[key] for r in results), 3),
                "max": max(r[key] for r in results),
            }
            for key in ("screen_ms", "exit_ms")
        },
    }
    if args.json:
        args.json.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
