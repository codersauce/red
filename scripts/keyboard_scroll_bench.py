#!/usr/bin/env python3
"""Measure keyboard motion in a populated Red workspace without changing Red."""

import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time

sys.dont_write_bytecode = True
import workspace_scroll_bench as base

FRAME_START = b"\x1b[?25l\x1b[?7l"
SYNC_START = b"\x1b[?2026h"
SYNC_END = b"\x1b[?2026l"


def summarize(content):
    samples = defaultdict(list)
    for match in base.TIMING.finditer(content):
        label, detail, micros = match.group(1), match.group(2) or "", int(match.group(3))
        if label == "event":
            label += " " + ("key" if detail.startswith("Key(") else "other")
        elif label in ("notify", "drain", "husk:notify", "panel:paint"):
            label += " " + detail.split()[0] if detail else ""
        samples[label].append(micros)
    return {key: base.stats(values) for key, values in samples.items()}


def byte_stats(values):
    return {key.replace("_us", "_bytes"): value for key, value in base.stats(values).items()}


class Driver(base.Driver):
    def __init__(self, args):
        self.chunks = []
        super().__init__(args)

    def drain(self):
        while True:
            try:
                data = base.os.read(self.master, 65536)
            except OSError:
                return
            if not data:
                return
            self.chunks.append((time.monotonic(), self.bytes, len(data)))
            self.bytes += len(data)
            self.capture.extend(data)
            if self.args.read_kib_per_second:
                time.sleep(len(data) / (1024 * self.args.read_kib_per_second))

    def quiet(self, seconds=0.2, timeout=30):
        previous, changed = self.bytes, time.monotonic()
        deadline = changed + timeout
        while time.monotonic() < deadline:
            time.sleep(0.01)
            if self.bytes != previous:
                previous, changed = self.bytes, time.monotonic()
            elif time.monotonic() - changed >= seconds:
                return
        raise TimeoutError("terminal output did not settle")

    def position(self, phase):
        # All phases begin in the same source region. M and a short alternating
        # motion stay inside the viewport; repeated directional keys reach its
        # boundary before the measured interval starts.
        self.command(str(self.args.start_line))
        if phase == "inside":
            self.send(b"M")
        elif phase in ("down", "up"):
            self.send(str(self.args.rows * 2).encode() + (b"j" if phase == "down" else b"k"))
        elif phase == "bof":
            self.send(b"gg")
        elif phase == "eof":
            self.send(b"G")
        self.quiet(seconds=0.6)

    def keyboard_phase(self, name):
        self.position(name)
        offset = self.log.stat().st_size
        byte_start = self.bytes
        cpu_start = base.cpu_seconds(self.process.pid)
        started = time.monotonic()
        sampler = None
        if self.args.sample_phase == name:
            sampler = subprocess.Popen(["sample", str(self.process.pid), "4", "1", "-file",
                                        str(self.output / (name + "-sample.txt"))],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        sends = []
        for index in range(self.args.presses):
            key = (b"j" if index % 2 == 0 else b"k") if name == "inside" else (
                b"k" if name in ("up", "bof") else b"j")
            sends.append(time.monotonic() - started)
            self.send(key)
            deadline = started + (index + 1) * self.args.delay_ms / 1000
            time.sleep(max(0, deadline - time.monotonic()))
        enqueue_seconds = time.monotonic() - started
        self.quiet()
        finished = time.monotonic()
        cpu = base.cpu_seconds(self.process.pid) - cpu_start
        if sampler is not None:
            sampler.wait(timeout=15)
        with self.log.open("rb") as stream:
            stream.seek(offset)
            content = stream.read().decode(errors="replace")
        raw = bytes(self.capture[byte_start:self.bytes])
        timings = summarize(content)
        starts = [match.start() for match in re.finditer(re.escape(FRAME_START), raw)]
        frame_sizes = [end - start for start, end in zip(starts, starts[1:] + [len(raw)])]
        chunks = [(round(t - started, 6), pos - byte_start, size)
                  for t, pos, size in self.chunks if pos >= byte_start]
        result = {
            "input_events": self.args.presses,
            "handled_key_events": timings.get("event key", {}).get("count")
                if self.args.perf_mode == "trace" else None,
            "enqueue_seconds": enqueue_seconds,
            "quiet_observation_seconds": finished - started,
            "process_cpu_seconds": round(cpu, 4),
            "output_bytes": len(raw),
            "nonempty_terminal_frames": len(starts),
            "frame_bytes": byte_stats(frame_sizes) if frame_sizes else None,
            "synchronized_output_begin": raw.count(SYNC_START),
            "synchronized_output_end": raw.count(SYNC_END),
            "timings": timings,
        }
        self.output.joinpath(name + ".ansi").write_bytes(raw)
        self.output.joinpath(name + ".log").write_text(content)
        self.output.joinpath(name + "-io.json").write_text(json.dumps({"sends": sends, "chunks": chunks}) + "\n")
        self.output.joinpath(name + ".json").write_text(json.dumps(result, indent=2) + "\n")
        compact = {key: value for key, value in result.items() if key != "timings"}
        compact["spans"] = {key: value for key, value in timings.items()
                            if key in ("event key", "navigation:publish", "render:motion_delta", "render:motion_frame", "render:editor_windows", "render:full", "highlight:miss", "notify cursor:moved", "notify viewport:changed")}
        print(name, json.dumps(compact), flush=True)
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--root", default=str(base.ROOT))
    parser.add_argument("--file", default=str(base.ROOT / "src/editor.rs"))
    parser.add_argument("--split-file", default=str(base.ROOT / "src/editor/rendering.rs"))
    parser.add_argument("--layout", choices=("editor", "agent", "workspace"), default="workspace")
    parser.add_argument("--perf-mode", choices=("trace", "off"), default="trace")
    parser.add_argument("--config-override", action="append", default=[])
    parser.add_argument("--syntax-off", action="store_true")
    parser.add_argument("--lsp", action="store_true")
    parser.add_argument("--rows", type=int, default=60)
    parser.add_argument("--cols", type=int, default=200)
    parser.add_argument("--start-line", type=int, default=1500)
    parser.add_argument("--presses", type=int, default=200)
    parser.add_argument("--delay-ms", type=float, default=25)
    parser.add_argument("--read-kib-per-second", type=float, default=0)
    parser.add_argument("--sample-phase", choices=("inside", "down", "up", "bof", "eof"))
    parser.add_argument("--phases", nargs="+", default=["inside", "down", "up"],
                        choices=("inside", "down", "up", "bof", "eof"))
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    args.mouse_coordinate_width = 4
    driver = Driver(args)
    try:
        driver.setup()
        metadata = {"args": vars(args),
                    "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=base.ROOT, text=True).strip(),
                    "tree": subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=base.ROOT, text=True).strip(),
                    "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest(),
                    "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    "file_sha256": hashlib.sha256(driver.file.read_bytes()).hexdigest(),
                    "split_file_sha256": hashlib.sha256(driver.split_file.read_bytes()).hexdigest(),
                    "pid": driver.process.pid}
        driver.output.joinpath("metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
        results = {phase: driver.keyboard_phase(phase) for phase in args.phases}
        driver.output.joinpath("results.json").write_text(json.dumps(results, indent=2) + "\n")
    finally:
        driver.close()


if __name__ == "__main__":
    main()
