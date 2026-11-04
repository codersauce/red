"""Compare five terminal replays of each fixed workload with the M1 calibration."""

import argparse
import json
from pathlib import Path
import statistics
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    directory = Path(__file__).resolve().parent
    calibration = json.loads((directory / "performance-baseline.json").read_text())
    binary = args.binary.resolve(strict=True)
    results = {}
    for name in ("undo", "paste", "2", "16", "128"):
        scaling = name.isdigit()
        reference = calibration["felix_scaling"]["runs"][name] if scaling else None
        budget = max(reference["event_ms"] if scaling else calibration["vim_baseline"][name])
        argv = ["--scenario", "felix-change", "--block-rows", name] if scaling else ["--scenario", name]
        observations = []
        for _ in range(5):
            result = json.loads(subprocess.check_output(
                [sys.executable, str(directory.parent / "edit_replay_bench.py"), "--binary",
                 str(binary), "--unicode", *argv], text=True))
            if scaling and (result["source_sha256"] != reference["source_sha256"]
                            or result["selections"] != int(name)):
                raise ValueError(f"{name} selections: workload differs from calibration")
            observations.append(result["event_ms"])
        median = statistics.median(observations)
        results[name] = dict(observations_ms=observations, median_ms=median, limit_ms=budget, passed=median <= budget)
    print(json.dumps(results, indent=2))
    return 0 if all(result["passed"] for result in results.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
