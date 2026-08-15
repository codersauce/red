#!/usr/bin/env python3
"""Measure equivalent Cargo and nextest suites without changing the default runner."""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import time
from dataclasses import asdict, dataclass
from pathlib import Path


@dataclass(frozen=True)
class Measurement:
    runner: str
    command: list[str]
    elapsed_seconds: float
    exit_code: int


def test_arguments(scope: str, package: str | None = None) -> list[str]:
    targets = "--tests" if scope == "workspace" or package is not None else "--all-targets"
    arguments = ["--locked", targets, "--all-features"]
    if scope == "workspace":
        arguments.insert(0, "--workspace")
    if package is not None:
        arguments.extend(["-p", package])
    return arguments


def command_for(
    runner: str,
    scope: str,
    *,
    nextest_profile: str,
    package: str | None = None,
) -> list[str]:
    if runner == "cargo":
        return ["cargo", "test", *test_arguments(scope, package)]
    return [
        "cargo",
        "nextest",
        "run",
        *test_arguments(scope, package),
        "--profile",
        nextest_profile,
    ]


def measure(runner: str, command: list[str]) -> Measurement:
    print("\nRunning:", " ".join(command), flush=True)
    started = time.perf_counter()
    completed = subprocess.run(command, check=False)
    elapsed = time.perf_counter() - started
    print(f"{runner} finished in {elapsed:.3f}s (exit {completed.returncode})", flush=True)
    return Measurement(runner, command, round(elapsed, 6), completed.returncode)


def report(
    *,
    scope: str,
    package: str | None = None,
    prebuild: Measurement | None,
    measurements: list[Measurement],
) -> dict[str, object]:
    medians = {
        runner: round(
            statistics.median(
                measurement.elapsed_seconds
                for measurement in measurements
                if measurement.runner == runner and measurement.exit_code == 0
            ),
            6,
        )
        for runner in dict.fromkeys(measurement.runner for measurement in measurements)
        if any(
            measurement.runner == runner and measurement.exit_code == 0
            for measurement in measurements
        )
    }
    return {
        "scope": scope,
        "package": package,
        "prebuild": asdict(prebuild) if prebuild is not None else None,
        "measurements": [asdict(measurement) for measurement in measurements],
        "median_seconds": medians,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", choices=("cargo", "nextest", "both"), default="both")
    parser.add_argument("--scope", choices=("root", "workspace"), default="root")
    parser.add_argument("--package", help="benchmark one workspace package instead of the root")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--nextest-profile", default="ci")
    parser.add_argument("--no-prebuild", action="store_true")
    parser.add_argument("--timings", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("target/test-performance/results.json"))
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be at least 1")
    if args.scope == "workspace" and args.package:
        parser.error("--package cannot be combined with --scope workspace")

    selected = ["cargo", "nextest"] if args.runner == "both" else [args.runner]
    measurements: list[Measurement] = []
    prebuild = None
    if not args.no_prebuild:
        command = ["cargo", "test", *test_arguments(args.scope, args.package), "--no-run"]
        if args.timings:
            command.append("--timings")
        prebuild = measure("prebuild", command)

    if prebuild is None or prebuild.exit_code == 0:
        for _ in range(args.runs):
            for runner in selected:
                result = measure(
                    runner,
                    command_for(
                        runner,
                        args.scope,
                        nextest_profile=args.nextest_profile,
                        package=args.package,
                    ),
                )
                measurements.append(result)
                if result.exit_code != 0:
                    break
            else:
                continue
            break

    result = report(
        scope=args.scope,
        package=args.package,
        prebuild=prebuild,
        measurements=measurements,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print("\nMedian successful runtimes:", result["median_seconds"], flush=True)
    print("Report:", args.output, flush=True)

    failed = [measurement.exit_code for measurement in measurements if measurement.exit_code]
    if prebuild is not None and prebuild.exit_code:
        return prebuild.exit_code
    return failed[0] if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
