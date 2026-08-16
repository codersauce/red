#!/usr/bin/env python3
"""Run serial, optionally paired keyboard-scroll measurements."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--before-binary")
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--file")
    parser.add_argument("--split-file")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--output", required=True)
    parser.add_argument("--group", choices=("main", "controls", "debug"), default="main")
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("--repetitions must be positive")
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    cases = []
    if args.group == "main":
        for repetition in range(1, args.repetitions + 1):
            for layout, perf in (("editor", "trace"), ("workspace", "trace"), ("workspace", "off")):
                cases.append((f"{layout}-{perf}-{repetition}", layout, perf, []))
    elif args.group == "debug":
        cases = [(f"workspace-trace-{repetition}", "workspace", "trace", [])
                 for repetition in range(1, args.repetitions + 1)]
    else:
        cases = [
            ("no-indent", "workspace", "trace", ["--config-override", 'disabled_plugins=["indent_guides"]']),
            ("no-syntax", "workspace", "trace", ["--syntax-off"]),
            ("no-wrap", "workspace", "trace", ["--config-override", "wrap=false"]),
            ("relative", "workspace", "trace", ["--config-override", "relative_line_numbers=true"]),
            ("fast-repeat", "workspace", "trace", ["--delay-ms", "5"]),
            ("slow-output", "workspace", "trace", ["--read-kib-per-second", "256"]),
            ("file-limits", "editor", "trace", ["--phases", "bof", "eof"]),
        ]
    index = []
    root = Path(args.root).resolve()
    fixture = ["--root", str(root), "--file", args.file or str(root / "src/editor.rs"),
               "--split-file", args.split_file or str(root / "src/editor/rendering.rs")]
    for case_index, (name, layout, perf, extra) in enumerate(cases):
        variants = [("after", args.binary)]
        if args.before_binary:
            variants.insert(0, ("before", args.before_binary))
            if case_index % 2:
                variants.reverse()
        for variant, binary in variants:
            destination = output / variant / name if args.before_binary else output / name
            command = [sys.executable, str(ROOT / "scripts/keyboard_scroll_bench.py"),
                       "--binary", binary, "--layout", layout, "--perf-mode", perf,
                       "--output", str(destination), *fixture, *extra]
            print(f"=== {variant}/{name} ===", flush=True)
            subprocess.run(command, check=True)
            result = json.loads((destination / "results.json").read_text())
            compact = {}
            for phase, value in result.items():
                compact[phase] = {key: value[key] for key in
                    ("input_events", "handled_key_events", "process_cpu_seconds", "output_bytes",
                     "nonempty_terminal_frames", "synchronized_output_begin", "synchronized_output_end")}
                compact[phase]["spans"] = {key: value["timings"].get(key, {}) for key in
                    ("event key", "navigation:publish", "render:motion_delta", "render:motion_frame", "render:editor_windows",
                     "render:full", "highlight:miss", "notify cursor:moved", "notify viewport:changed")}
            index.append({"name": name, "variant": variant, "layout": layout, "perf": perf,
                          "extra": extra, "results": compact})
            (output / (args.group + "-summary.json")).write_text(json.dumps(index, indent=2) + "\n")
    print(json.dumps(index, indent=2), flush=True)


if __name__ == "__main__":
    main()
