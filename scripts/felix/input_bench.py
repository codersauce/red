"""Time verified single-cursor inputs from PTY write to a Red statusline update."""

import argparse
from bisect import bisect_right
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import threading
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from workspace_scroll_bench import Driver, stats
from perf_trace import TIMING

BEGIN = b"\x1b[?2026h"
END = b"\x1b[?2026l"
TOKEN = re.compile(rb"\x1b\[(\d+);(\d+)H|\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x1b\x07]*(?:\x07|\x1b\\)|[^\x1b]+")
POSITION = re.compile(rb"\b(\d+):(\d+)\b")


class InputDriver(Driver):
    """Observe the position Red actually emitted at the end of synchronized frames."""

    def __init__(self, args):
        self.changed = threading.Condition()
        self.frames = []
        super().__init__(args)

    def drain(self):
        pending = bytearray()
        status = bytearray(b" " * self.args.cols)
        while True:
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                return
            if not chunk:
                return
            observed = time.monotonic_ns()
            self.bytes += len(chunk)
            self.capture.extend(chunk)
            pending.extend(chunk)
            while (end := pending.find(END)) >= 0:
                frame = pending[:end]
                del pending[:end + len(END)]
                start = frame.rfind(BEGIN)
                if start < 0:
                    continue
                row, col = 0, 0
                for token in TOKEN.finditer(frame, start + len(BEGIN)):
                    if token[1]:
                        row, col = int(token[1]), int(token[2]) - 1
                    elif row == self.args.rows - 1 and not token[0].startswith(b"\x1b"):
                        # The fixture statusline uses ASCII and single-cell theme icons.
                        data = token[0].decode("utf-8", "replace").encode("ascii", "replace")[:self.args.cols - col]
                        status[col:col + len(data)] = data
                        col += len(data)
                pos = POSITION.search(status)
                if pos:
                    with self.changed:
                        self.frames.append((observed, (int(pos[1]), int(pos[2]))))
                        self.changed.notify_all()

    def wait_position(self, pos, since):
        deadline = time.monotonic() + 12
        with self.changed:
            index = bisect_right(self.frames, since, key=lambda frame: frame[0])
            while True:
                for timestamp, cursor in self.frames[index:]:
                    if cursor == pos:
                        return timestamp
                index = len(self.frames)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"expected {pos}; last frames: {self.frames[-4:]}")
                self.changed.wait(remaining)


def workload(args):
    binary = Path(args.binary).resolve(strict=True)
    fixture_line = 'fn value() { let x = 123; }'
    with tempfile.TemporaryDirectory(prefix="felix-input-") as scratch:
        root = Path(scratch)
        file = root / ("fixture." + args.extension)
        source = ((fixture_line + 'x' * args.padding + '\n') * args.lines).encode()
        if args.source:
            source = args.source.read_bytes()
            file = root / args.source.name
        file.write_bytes(source)
        args.root, args.file, args.split_file = root, file, file
        args.output = root / "capture"
        args.binary = binary
        args.lsp = False
        args.config_override = [f"editing_model={args.model}", "disable_ai=true",
                                "formatting.on_save=false", *args.config_override]
        driver = InputDriver(args)
        try:
            if args.split:
                driver.send(b"\x17v")
                time.sleep(0.25)
            row = args.line or max(2, len(source.splitlines()) // 2)
            now = time.monotonic_ns()
            driver.send(f"{row}G".encode())
            driver.wait_position((row, 1), now)
            if args.scenario == "typing":
                driver.send(b"i")
                cycle = [(b"x", 0, 1), (b" ", 0, 1), (b"\x7f", 0, -1), (b"\x7f", 0, -1)]
            elif args.scenario == "horizontal":
                cycle = [(b"l", 0, 1), (b"h", 0, -1)]
            else:
                cycle = [(b"j", 1, 0), (b"k", -1, 0)]
            time.sleep(0.15)
            col = 1
            latencies = []
            bytes_before = offset = 0
            for index in range(args.warmup + args.events):
                if index == args.warmup:
                    bytes_before = driver.bytes
                    offset = driver.log.stat().st_size if driver.log.exists() else 0
                key, dy, dx = cycle[index % len(cycle)]
                row += dy
                col += dx
                sent = time.monotonic_ns()
                driver.send(key)
                shown = driver.wait_position((row, col), sent)
                if index >= args.warmup:
                    latencies.append(round((shown - sent) / 1000))
                remaining = args.interval_ms / 1000 - (time.monotonic_ns() - sent) / 1e9
                if remaining > 0:
                    time.sleep(remaining)
            output_bytes = driver.bytes - bytes_before
            time.sleep(0.05)
            spans = {}
            if args.perf_mode == "trace":
                with driver.log.open("rb") as stream:
                    stream.seek(offset)
                    for match in TIMING.finditer(stream.read().decode(errors="replace")):
                        spans.setdefault(match[1], []).append(int(match[3]))
                if len(spans.get("event", [])) != args.events:
                    raise ValueError(f"unaccounted inputs: {len(spans.get('event', []))}/{args.events}")
            burst = None
            inserted = b""
            if args.burst:
                # For motion, the final j is an unambiguous barrier after a reversal.
                if args.scenario == "typing":
                    inserted = b"ordinary typing "
                    keys = inserted
                    col += len(inserted)
                else:
                    keys = b"l" * 12 + b"h" * 12 + b"j"
                    row += 1
                start = time.monotonic_ns()
                for key in keys:
                    driver.send(bytes([key]))
                    last_send = time.monotonic_ns()
                    if args.burst_ms:
                        time.sleep(args.burst_ms / 1000)
                shown = driver.wait_position((row, col), start)
                burst = dict(inputs=len(keys), elapsed_us=(shown - start) // 1000,
                             drain_us=max(0, (shown - last_send) // 1000))
            # The edit marker verifies the document cursor independently of the statusline.
            if args.scenario != "typing":
                driver.send(b"i")
            driver.send(b"Z\x1b")
            time.sleep(0.1)
            driver.command("w")
            expected = source.splitlines(keepends=True)
            # Insert sessions finish their measured cycle at the original column.
            offset_in_line = col - 1 - len(inserted)
            line = expected[row - 1]
            expected[row - 1] = line[:offset_in_line] + inserted + b"Z" + line[offset_in_line:]
            driver.wait_for(lambda: file.read_bytes() == b"".join(expected), 5, "saved marker")
            with binary.open("rb") as stream:
                binary_hash = hashlib.file_digest(stream, "sha256").hexdigest()
            return dict(model=args.model, scenario=args.scenario, events=args.events,
                        warmup=args.warmup, lines=len(expected), extension=args.extension,
                        rows=args.rows, cols=args.cols, padding=args.padding, split=args.split,
                        perf_mode=args.perf_mode, interval_ms=args.interval_ms,
                        binary_sha256=binary_hash,
                        source_file=file.name, source_sha256=hashlib.sha256(source).hexdigest(),
                        output_bytes=output_bytes, burst=burst, latency=stats(latencies), latency_us=latencies,
                        spans={key: stats(values) for key, values in spans.items()})
        finally:
            driver.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--model", choices=("helix", "vim"), required=True)
    parser.add_argument("--scenario", choices=("typing", "horizontal", "vertical"), required=True)
    parser.add_argument("--source", type=Path, help="copy a real fixture into the isolated workspace")
    parser.add_argument("--line", type=int, help="one-based unindented starting line")
    parser.add_argument("--lines", type=int, default=2000)
    parser.add_argument("--events", type=int, default=1000)
    parser.add_argument("--warmup", type=int, default=100)
    parser.add_argument("--extension", choices=("rs", "txt", "md"), default="rs")
    parser.add_argument("--padding", type=int, default=0)
    parser.add_argument("--interval-ms", type=float, default=0)
    parser.add_argument("--perf-mode", choices=("trace", "off"), default="off")
    parser.add_argument("--config-override", action="append", default=[])
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--split", action="store_true")
    parser.add_argument("--burst", action="store_true")
    parser.add_argument("--burst-ms", type=float, default=0)
    args = parser.parse_args()
    if args.lines < 4 or args.events < 4 or args.warmup % 4 or args.events % 4:
        parser.error("use at least four lines and whole four-input cycles")
    print(json.dumps(workload(args)))
