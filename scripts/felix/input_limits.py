"""Compare median run percentiles on the reference workstation.

Usage: input_limits.py <results-directory> <candidate-label>
Input files follow baseline-PROFILE-SCENARIO-MODE-RUN.json from input_bench.py.
"""

import argparse
import json
from pathlib import Path
from statistics import median

SCENARIOS = ("typing", "horizontal", "vertical")
MATCH = ("source_sha256", "rows", "cols", "events", "warmup", "perf_mode",
         "interval_ms", "scenario", "model", "split", "padding")


def runs(directory, label, profile, scenario, model):
    return [json.loads((directory / f"{label}-{profile}-{scenario}-{model}-{number}.json").read_text())
            for number in range(1, 6)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("label")
    args = parser.parse_args()
    report = []
    for profile in ("dev", "release"):
        for scenario in SCENARIOS:
            control = runs(args.directory, "baseline", profile, scenario, "vim")
            for model in ("helix", "vim"):
                base = runs(args.directory, "baseline", profile, scenario, model)
                candidate = runs(args.directory, args.label, profile, scenario, model)
                for row in base + candidate:
                    if (any(row[key] != base[0][key] for key in MATCH) or
                            row["events"] != row["latency"]["count"] or row["events"] < 1000):
                        raise ValueError("input workload or measurement count mismatch")
                for percentile, ceiling in (("p95_us", 16000), ("p99_us", 33000)):
                    # The absolute user-facing cap also applies when both modes are slow.
                    # Allow one millisecond of workstation jitter on fast interactions.
                    limit = min(ceiling, 1000 + max(r["latency"][percentile] for r in base),
                                1000 + max(r["latency"][percentile] for r in control))
                    measured = median(r["latency"][percentile] for r in candidate)
                    report.append(dict(profile=profile, scenario=scenario, model=model,
                                       metric=percentile, limit_us=limit, result_us=measured,
                                       passed=measured <= limit))
    print(json.dumps(report, indent=2))
    return 0 if all(row["passed"] for row in report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
