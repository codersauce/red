#!/usr/bin/env python3
"""Repeatable release-mode scroll/mouse workload with a real, populated Agent pane.

The same executable acts as a deterministic local Codex app-server fixture. No
model request or user conversation is used. Results and raw PERF spans are kept
under --output; run with --layout editor, agent, or workspace for comparisons.
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
import termios
import threading
import time

ROOT = Path(__file__).resolve().parent.parent
TIMING = re.compile(r"\[PERF\] (\S+)(?: (.*?))?: (\d+)us")


def response_text():
    return "# Performance investigation\n\n" + "\n\n".join(
        f"## Observation {n}\n\n"
        "The editor should remain responsive while scrolling a highlighted file. "
        "Measure input handling, syntax work, panel layout, and terminal output.\n\n"
        "```rust\nfn render_frame(index: usize) -> usize {\n    index + 1\n}\n```\n\n"
        "- Preserve cursor position\n- Reuse unchanged rows\n- Verify mouse behavior"
        for n in range(1, 13)
    )


def mock_codex():
    output_lock = threading.Lock()
    turn_number = 0

    def send(value):
        with output_lock:
            print(json.dumps(value), flush=True)

    def finish_turn(turn_id, text, streaming):
        chunks = [text[index:index + 80] for index in range(0, len(text), 80)] if streaming else [text]
        for chunk in chunks:
            send({"method": "item/agentMessage/delta", "params": {
                "threadId": "red-perf-thread", "turnId": turn_id, "delta": chunk}})
            if streaming:
                time.sleep(0.05)
        send({"method": "item/completed", "params": {
            "threadId": "red-perf-thread", "turnId": turn_id,
            "item": {"id": turn_id + "-message", "type": "agentMessage", "text": text}}})
        send({"method": "turn/completed", "params": {
            "threadId": "red-perf-thread", "turn": {"id": turn_id, "status": "completed"}}})

    for line in sys.stdin:
        message = json.loads(line)
        method, ident = message.get("method"), message.get("id")
        result = None
        if method == "initialize":
            result = {"userAgent": "red-performance-fixture"}
        elif method == "account/read":
            result = {"account": {"type": "chatgpt"}, "requiresOpenaiAuth": False}
        elif method == "config/read":
            result = {"config": {"mcp_servers": {}}, "origins": {}}
        elif method == "configRequirements/read":
            result = {"requirements": None}
        elif method == "thread/start":
            result = {"thread": {"id": "red-perf-thread"}}
        elif method == "turn/start":
            turn_number += 1
            turn_id = f"red-perf-turn-{turn_number}"
            send({"id": ident, "result": {"turn": {"id": turn_id}}})
            text = response_text()
            streaming = message["params"]["input"][0]["text"] == "stream benchmark"
            threading.Thread(target=finish_turn, args=(turn_id, text, streaming), daemon=True).start()
            continue
        elif ident is not None:
            result = {}
        if ident is not None and result is not None:
            send({"id": ident, "result": result})


def stats(values):
    values = sorted(values)
    return {"count": len(values), "total_us": sum(values),
            **{f"p{p}_us": values[(len(values) - 1) * p // 100] for p in (50, 95, 99)},
            "max_us": values[-1]}


def cpu_seconds(pid):
    value = subprocess.check_output(["ps", "-o", "time=", "-p", str(pid)], text=True).strip()
    total = 0.0
    for component in value.replace("-", ":").split(":"):
        total = total * 60 + float(component)
    return total


def mouse(code, x, y, release=False, width=0):
    return f"\x1b[<{code};{x:0{width}d};{y:0{width}d}{'m' if release else 'M'}".encode()


class Driver:
    def __init__(self, args):
        self.args = args
        self.root = Path(args.root).resolve()
        self.file = Path(args.file).resolve()
        self.split_file = Path(args.split_file).resolve()
        self.output = Path(args.output).resolve()
        self.output.mkdir(parents=True, exist_ok=False)
        self.config_home = self.output / "config"
        config = self.config_home / "red"
        config.mkdir(parents=True)
        self.log = self.output / "red.log"
        config.joinpath("config.toml").write_text(
            f"log_file = {json.dumps(str(self.log))}\n"
            "show_whats_new = false\nfetch_release_notes = false\n"
            f"[lsp]\nenabled = {str(args.lsp).lower()}\n"
            f"[agent]\ncommand = {json.dumps(str(Path(__file__).resolve()))}\n")
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ,
                    struct.pack("HHHH", args.rows, args.cols, 0, 0))
        self.bytes = 0
        self.capture = bytearray()
        environment = dict(os.environ, RED_PERF=args.perf_mode, XDG_CONFIG_HOME=str(self.config_home),
                           TERM="xterm-256color", COLORTERM="truecolor")
        environment.pop("NO_COLOR", None)
        self.process = subprocess.Popen(
            [str(Path(args.binary).resolve()), "--root", str(self.root),
             *[value for override in args.config_override for value in ("--config-override", override)],
             str(self.file)],
            cwd=self.root, stdin=slave, stdout=slave, stderr=slave,
            env=environment,
            close_fds=True)
        os.close(slave)
        threading.Thread(target=self.drain, daemon=True).start()
        self.wait_for(lambda: ("[PERF] startup:interactive:" in self.read_log()
                               if args.perf_mode == "trace" else len(self.capture) > 1000),
                      90, "first paint")
        time.sleep(0.3)

    def drain(self):
        while True:
            try:
                data = os.read(self.master, 1 << 20)
            except OSError:
                return
            if not data:
                return
            self.bytes += len(data)
            self.capture.extend(data)

    def read_log(self):
        return self.log.read_text(errors="replace") if self.log.exists() else ""

    def wait_for(self, condition, timeout, description):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if condition():
                return
            if self.process.poll() is not None:
                raise RuntimeError(f"Red exited during {description}: {self.process.returncode}")
            time.sleep(0.02)
        raise TimeoutError(description)

    def send(self, data):
        while data:
            written = os.write(self.master, data)
            if written == 0:
                raise RuntimeError("PTY write made no progress")
            data = data[written:]

    def mouse(self, code, x, y, release=False):
        return mouse(code, x, y, release, self.args.mouse_coordinate_width)

    def command(self, command):
        self.send(b":" + command.encode() + b"\r")
        time.sleep(0.25)

    def setup(self):
        if self.args.lsp:
            self.wait_for(lambda: "[lsp] server initialized" in self.read_log(),
                          90, "LSP initialization")
        if self.args.layout == "workspace":
            self.command("sp " + str(self.split_file))
            if self.args.syntax_off:
                self.command("syntax off")
            self.send(b"\x17k")
            time.sleep(0.2)
        if self.args.layout != "editor":
            self.command("Agent")
            self.send(b"Show the deterministic performance fixture")
            time.sleep(0.1)
            self.send(b"\x1b")
            time.sleep(0.1)
            self.send(b"\r")
            preferences = self.config_home / "red/preferences.json"
            self.wait_for(lambda: ("notify agent:completed" in self.read_log()
                                   if self.args.perf_mode == "trace" else
                                   preferences.exists() and "Observation 12" in preferences.read_text()),
                          30, "agent completion")
            time.sleep(0.4)
            self.send(b"\x1b")
            time.sleep(0.1)
            self.send(self.mouse(0, 15, 8) + self.mouse(0, 15, 8, True))
            time.sleep(0.2)
        if self.args.layout == "workspace":
            self.command("NeoTree")
            time.sleep(0.4)
        self.x = 65 if self.args.layout == "workspace" else 15
        self.y = 8
        self.send(self.mouse(0, self.x, self.y) + self.mouse(0, self.x, self.y, True))
        time.sleep(0.2)
        if self.args.syntax_off:
            self.command("syntax off")
        self.command("100")
        time.sleep(0.4)
        self.output.joinpath("setup.ansi").write_bytes(self.capture)
        if self.args.perf_mode == "trace" and self.args.layout != "editor" and "drain UpdateTextPanel" not in self.read_log():
            raise RuntimeError("agent fixture did not populate its panel")
        if self.args.layout == "workspace" and "Handling command: NeoTree" not in self.read_log():
            raise RuntimeError("file tree did not open")

    def phase(self, name):
        self.command("100")
        time.sleep(0.15)
        if name == "streaming":
            self.command("Agent")
            self.send(b"stream benchmark")
            time.sleep(0.1)
            self.send(b"\x1b")
            time.sleep(0.1)
            self.send(b"\r")
            time.sleep(0.2)
            self.send(self.mouse(0, self.x, self.y) + self.mouse(0, self.x, self.y, True))
            time.sleep(0.1)
        offset = self.log.stat().st_size
        bytes_before, cpu_before = self.bytes, cpu_seconds(self.process.pid)
        started = time.monotonic()
        expected = 0
        count = self.args.presses
        for index in range(count):
            if name == "idle":
                payload = b""
            elif name == "mouse":
                payload = self.mouse(35, self.x + index % 7, self.y + index % 3)
                expected += 1
            elif name == "keys":
                payload = b"j"
                expected += 1
            elif name == "wheel":
                payload = self.mouse(65, self.x, self.y)
                expected += 1
            else:
                payload = b"".join(self.mouse(35, self.x + (index + n) % 7, self.y + n % 3)
                                    for n in range(self.args.moves_per_scroll))
                payload += self.mouse(65, self.x, self.y)
                expected += self.args.moves_per_scroll + 1
            if payload:
                self.send(payload)
            deadline = started + (index + 1) * self.args.delay_ms / 1000
            time.sleep(max(0, deadline - time.monotonic()))
        enqueue_seconds = time.monotonic() - started

        def phase_log():
            with self.log.open("rb") as stream:
                stream.seek(offset)
                return stream.read().decode(errors="replace")

        def delivered():
            return len(re.findall(r"\[PERF\] event (?:Mouse|Key)\(", phase_log()))

        if self.args.perf_mode != "trace":
            last_bytes, last_change = self.bytes, time.monotonic()
            deadline = last_change + 30
            while time.monotonic() < deadline and time.monotonic() - last_change < 0.2:
                time.sleep(0.01)
                if self.bytes != last_bytes:
                    last_bytes, last_change = self.bytes, time.monotonic()
        elif name == "keys":
            # Red intentionally discards stale repeated-motion keys. A quiet
            # queue is completion even when not every injected key survives.
            last_count, last_change = delivered(), time.monotonic()
            while delivered() < expected and time.monotonic() - last_change < 0.2:
                time.sleep(0.01)
                current = delivered()
                if current != last_count:
                    last_count, last_change = current, time.monotonic()
        elif expected:
            self.wait_for(lambda: delivered() >= expected, 60, f"{name} event drain")
        drained_seconds = time.monotonic() - started
        time.sleep(0.15)
        cpu = cpu_seconds(self.process.pid) - cpu_before
        content = phase_log()
        if name == "streaming" and self.args.perf_mode == "trace" and "notify agent:update" not in content:
            raise RuntimeError("Agent did not stream during the measured phase")
        self.output.joinpath(f"{name}.log").write_text(content)
        samples = defaultdict(list)
        for match in TIMING.finditer(content):
            label, detail, micros = match.group(1), match.group(2) or "", int(match.group(3))
            if label == "event":
                label += " mouse" if "kind: Moved" in detail else (
                    " wheel" if "kind: Scroll" in detail else " key")
            elif label in ("notify", "drain", "husk:notify", "panel:paint"):
                label += " " + detail.split()[0] if detail else ""
            samples[label].append(micros)
        observed = delivered() if self.args.perf_mode == "trace" else None
        expected_types = {
            "mouse": count if name == "mouse" else (
                count * self.args.moves_per_scroll if name in ("mixed", "streaming") else 0),
            "wheel": count if name in ("wheel", "mixed", "streaming") else 0,
            "key": count if name == "keys" else 0,
        }
        observed_types = {kind: len(samples.get("event " + kind, []))
                          for kind in expected_types} if observed is not None else None
        dropped = max(0, expected - observed) if observed is not None else None
        observation_ms = max(0, drained_seconds - enqueue_seconds) * 1000
        exact_drain = observed_types == expected_types
        result = {"input_events": expected, "delivered_events": observed,
                  "expected_event_types": expected_types,
                  "delivered_event_types": observed_types,
                  "dropped_events": dropped,
                  "enqueue_seconds": enqueue_seconds,
                  "drained_seconds": drained_seconds,
                  "drain_lag_ms": observation_ms if exact_drain else None,
                  "quiescence_observation_ms": None if exact_drain else observation_ms,
                  "process_cpu_seconds": cpu, "output_bytes": self.bytes-bytes_before,
                  "timings": {key: stats(values) for key, values in samples.items()}}
        self.output.joinpath(f"{name}.json").write_text(json.dumps(result, indent=2)+"\n")
        important = {key: value for key, value in result.items() if key != "timings"}
        important["p95_us"] = {key: value["p95_us"] for key, value in result["timings"].items()
                               if key.startswith("event ") or key in ("render:full", "render:windows", "render:chrome", "highlight:miss")}
        print(name, json.dumps(important), flush=True)
        if observed_types is not None and name != "keys" and observed_types != expected_types:
            raise RuntimeError(f"{name} decoded unexpected input: {observed_types} != {expected_types}")
        return result

    def close(self):
        if self.process.poll() is None:
            self.send(b"\x1b")
            time.sleep(0.1)
            self.send(b":qa!\r")
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                self.process.wait(timeout=5)
        os.close(self.master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=str(ROOT / "target/release/red"))
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--file", default=str(ROOT / "src/editor.rs"))
    parser.add_argument("--split-file", default=str(ROOT / "src/editor/rendering.rs"))
    parser.add_argument("--lsp", action="store_true")
    parser.add_argument("--config-override", action="append", default=[])
    parser.add_argument("--perf-mode", choices=("trace", "summary", "off"), default="trace")
    parser.add_argument("--layout", choices=("editor", "agent", "workspace"), default="workspace")
    parser.add_argument("--syntax-off", action="store_true")
    parser.add_argument("--rows", type=int, default=60)
    parser.add_argument("--cols", type=int, default=200)
    parser.add_argument("--presses", type=int, default=200)
    parser.add_argument("--delay-ms", type=float, default=5)
    parser.add_argument("--moves-per-scroll", type=int, default=3)
    parser.add_argument("--mouse-coordinate-width", type=int, default=0,
                        help="optional zero padding for fixed-width SGR stress input")
    parser.add_argument("--phases", nargs="+", default=["idle", "mouse", "keys", "wheel", "mixed"],
                        choices=("idle", "mouse", "keys", "wheel", "mixed", "streaming"))
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    if args.rows < 20 or args.cols < 100 or args.presses < 1 or args.delay_ms < 0 or args.moves_per_scroll < 0:
        parser.error("rows >= 20, cols >= 100, presses >= 1, delay-ms >= 0, and moves-per-scroll >= 0 are required")
    if not 0 <= args.mouse_coordinate_width <= 8:
        parser.error("mouse-coordinate-width must be between 0 and 8")
    if args.layout == "workspace" and (args.rows < 40 or args.cols < 160):
        parser.error("the fixed split/tree/agent workload requires at least 40 rows and 160 columns")
    if "streaming" in args.phases and args.layout == "editor":
        parser.error("streaming requires an Agent pane")
    driver = Driver(args)
    try:
        driver.setup()
        metadata = {"commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                    "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    "fixture_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=driver.root, text=True).strip(),
                    "file_sha256": hashlib.sha256(driver.file.read_bytes()).hexdigest(),
                    "split_file_sha256": hashlib.sha256(driver.split_file.read_bytes()).hexdigest(),
                    "args": vars(args), "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest(),
                    "fixture_bytes": len(response_text().encode()), "pid": driver.process.pid}
        driver.output.joinpath("metadata.json").write_text(json.dumps(metadata, indent=2)+"\n")
        results = {phase: driver.phase(phase) for phase in args.phases}
        driver.output.joinpath("results.json").write_text(json.dumps(results, indent=2)+"\n")
    finally:
        driver.close()


if __name__ == "__main__":
    if "--version" in sys.argv:
        print("codex-cli 0.144.5")
    elif len(sys.argv) > 1 and sys.argv[1] == "app-server":
        mock_codex()
    else:
        main()
