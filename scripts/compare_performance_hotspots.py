#!/usr/bin/env python3
"""Compare pinned Red hotspot benchmark binaries using repeated median samples."""

import argparse
import json
from pathlib import Path
import statistics
import subprocess
import sys


SCENARIOS = (
    "decorations",
    "gutters",
    "agent",
    "picker",
    "panel",
    "viewport",
    "timers",
    "search",
    "completion",
    "rows",
    "json",
    "render",
    "preferences",
    "detached",
    "startup",
    "word-motion",
    "word-next",
    "word-prev",
    "resolver-next",
    "resolver-prev",
    "resolver-range",
    "paragraph",
    "sentence",
    "undo-prune",
    "highlight",
    "tree-selection",
    "workspace-navigation",
    "inline-stream",
    "statusline",
    "lsp-routing",
    "session-restore",
    "workspace-files",
    "workspace-search",
    "plugin-events",
    "session-write",
    "frame-full",
    "git-discovery",
    "startup-files",
    "lsp-changes",
    "textarea-typing",
    "husk-completion",
    "husk-config",
    "husk-update",
)


def measure(binary, scenario):
    process = subprocess.run(
        [str(binary), scenario],
        check=True,
        capture_output=True,
        text=True,
    )
    results = json.loads(process.stdout)
    if len(results) != 1:
        raise ValueError(f"{binary} returned {len(results)} results for {scenario}")
    return results[0]


def compare(before, after, scenario, samples):
    before_samples = []
    after_samples = []
    before_result = None
    after_result = None
    for sample in range(samples):
        if sample % 2:
            after_result = measure(after, scenario)
            before_result = measure(before, scenario)
        else:
            before_result = measure(before, scenario)
            after_result = measure(after, scenario)
        if before_result["iterations"] != after_result["iterations"]:
            raise ValueError(f"iteration count differs for {scenario}")
        before_samples.append(before_result["elapsed_us"])
        after_samples.append(after_result["elapsed_us"])

    before_median = statistics.median(before_samples)
    after_median = statistics.median(after_samples)
    improvement = (
        round((before_median - after_median) / before_median * 100, 2)
        if before_median
        else 0.0
    )
    return {
        "scenario": before_result["scenario"],
        "samples": samples,
        "iterations": before_result["iterations"],
        "before_median_us": before_median,
        "after_median_us": after_median,
        "improvement_percent": improvement,
        "before_samples_us": before_samples,
        "after_samples_us": after_samples,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--scenarios", nargs="+", choices=SCENARIOS, default=SCENARIOS)
    parser.add_argument("--minimum-improvement", type=float)
    arguments = parser.parse_args()
    if arguments.samples < 1:
        parser.error("--samples must be at least 1")

    results = [
        compare(arguments.before, arguments.after, scenario, arguments.samples)
        for scenario in arguments.scenarios
    ]
    print(json.dumps({"results": results}, indent=2))

    if arguments.minimum_improvement is not None:
        failures = [
            result
            for result in results
            if result["improvement_percent"] < arguments.minimum_improvement
        ]
        if failures:
            for failure in failures:
                print(
                    f"{failure['scenario']}: {failure['improvement_percent']}% is below "
                    f"the {arguments.minimum_improvement}% improvement target",
                    file=sys.stderr,
                )
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
