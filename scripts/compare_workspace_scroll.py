#!/usr/bin/env python3
"""Run serial, matched before/after PTY measurements and summarize medians."""

import argparse
import json
from pathlib import Path
import statistics
import subprocess
import sys

HARNESS = Path(__file__).with_name("workspace_scroll_bench.py")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True)
    parser.add_argument("--before-uninstrumented", required=True)
    parser.add_argument("--after", required=True)
    parser.add_argument("--fixture-root", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--controls-only", action="store_true")
    parser.add_argument("--control", action="append",
                        choices=("shared-wrap", "streaming", "burst"))
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    output = Path(args.output).resolve()
    root = Path(args.fixture_root).resolve()
    output.mkdir(parents=True, exist_ok=True)
    runs = json.loads((output / "runs.json").read_text()) if args.controls_only else []

    def run(name, version, layout, perf_mode="trace", extra=()):
        binary = args.after if version == "after" else (
            args.before_uninstrumented if perf_mode == "off" else args.before)
        destination = output / name
        command = [sys.executable, str(HARNESS), "--binary", binary,
                   "--root", str(root), "--file", str(root / "src/editor.rs"),
                   "--split-file", str(root / "src/editor/rendering.rs"),
                   "--layout", layout, "--perf-mode", perf_mode,
                   "--output", str(destination), *extra]
        print(f"\n=== {name} ===", flush=True)
        subprocess.run(command, check=True)
        runs[:] = [entry for entry in runs if entry["name"] != name]
        runs.append({"name": name, "version": version, "layout": layout,
                     "perf_mode": perf_mode, "extra": list(extra)})
        (output / "runs.json").write_text(json.dumps(runs, indent=2) + "\n")

    for repetition in (() if args.controls_only else range(1, args.repetitions + 1)):
        for layout in ("editor", "agent", "workspace"):
            for version in ("before", "after"):
                run(f"{version}-{layout}-{repetition}", version, layout)
        for version in ("before", "after"):
            run(f"{version}-untraced-{repetition}", version, "workspace", "off",
                ("--phases", "idle", "mouse", "wheel", "mixed"))

    controls = (
        ("shared-wrap", ("--split-file", str(root / "src/editor.rs"),
                         "--mouse-coordinate-width", "4",
                         "--config-override", "relative_line_numbers=true",
                         "--config-override", "wrap=true",
                         "--phases", "mouse", "wheel", "mixed")),
        ("streaming", ("--phases", "streaming")),
        ("burst", ("--delay-ms", "0", "--mouse-coordinate-width", "4",
                   "--phases", "wheel", "mixed")),
    )
    for control, extra in controls:
        if args.control and control not in args.control:
            continue
        for version in ("before", "after"):
            run(f"{version}-{control}", version, "workspace", extra=extra)

    summary = {}
    for layout in ("editor", "agent", "workspace", "untraced"):
        summary[layout] = {}
        for version in ("before", "after"):
            samples = [json.loads((output / f"{version}-{layout}-{i}" / "results.json").read_text())
                       for i in range(1, args.repetitions + 1)]
            summary[layout][version] = {}
            for phase in samples[0]:
                values = [sample[phase] for sample in samples]
                med = lambda field: statistics.median(value[field] for value in values)
                frame_count = lambda label: statistics.median(
                    value["timings"].get(label, {}).get("count", 0) for value in values)
                summary[layout][version][phase] = {
                    "cpu_seconds": round(med("process_cpu_seconds"), 3),
                    "output_bytes": med("output_bytes"),
                    "full_frames": frame_count("render:full"),
                    "motion_frames": frame_count("render:motion_frame"),
                    "editor_windows_frames": frame_count("render:editor_windows"),
                    "minimum_delivered": min(value["delivered_events"] for value in values)
                    if values[0]["delivered_events"] is not None else None,
                }
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
