"""Compare median run percentiles on the reference workstation.

Usage: input_limits.py <results-directory> <candidate-label>
Input files follow baseline-PROFILE-SCENARIO-MODE-RUN.json from input_bench.py.
"""

import argparse
import json
from pathlib import Path
from statistics import median

BASELINE = Path(__file__).with_name("input-baseline.json")

SCENARIOS = ("typing", "horizontal", "vertical")


def runs(directory, label, profile, scenario, model):
    return [json.loads((directory / f"{label}-{profile}-{scenario}-{model}-{number}.json").read_text())
            for number in range(1, 6)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("label")
    args = parser.parse_args()
    calibration = json.loads(BASELINE.read_text())
    report = []
    profile_hashes = {}
    for profile in ("dev", "release"):
        for scenario in SCENARIOS:
            reference = calibration["profiles"][profile]["runs"][scenario]
            control = reference["vim"]
            for model in ("helix", "vim"):
                base = reference[model]
                candidate = runs(args.directory, args.label, profile, scenario, model)
                overrides = [f"editing_model={model}", "disable_ai=true", "formatting.on_save=false"]
                for row in candidate:
                    if row["config_overrides"] != overrides or row["burst"] or row["project_root"]:
                        raise ValueError("candidate uses different editor settings or pacing")
                    if profile_hashes.setdefault(profile, row["binary_sha256"]) != row["binary_sha256"]:
                        raise ValueError(f"mixed {profile} binaries in candidate series")
                    if (row["model"] != model or row["scenario"] != scenario or
                            any(row[k] != v for k, v in calibration["workload"].items()) or
                            row["events"] != row["latency"]["count"] or
                            len(row["latency_us"]) != row["events"] or row["events"] < 1000):
                        raise ValueError("input workload or measurement count mismatch")
                for percentile, ceiling in (("p95_us", 16000), ("p99_us", 33000)):
                    # The absolute user-facing cap also applies when both modes are slow.
                    # Allow one millisecond of workstation jitter on fast interactions.
                    limit = min(ceiling, 1000 + max(base[percentile]),
                                1000 + max(control[percentile]))
                    measured = median(r["latency"][percentile] for r in candidate)
                    report.append(dict(profile=profile, scenario=scenario, model=model,
                                       metric=percentile, limit_us=limit, result_us=measured,
                                       passed=measured <= limit))
    if profile_hashes["dev"] == profile_hashes["release"]:
        raise ValueError("development and release profiles use the same binary")
    print(json.dumps(report, indent=2))
    return 0 if all(row["passed"] for row in report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
