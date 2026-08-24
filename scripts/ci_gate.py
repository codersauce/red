#!/usr/bin/env python3
"""Validate the terminal results behind Red's stable CI Gate check."""

from __future__ import annotations

import argparse
import json


ALWAYS_REQUIRED = (
    "plan",
    "workflow-lint",
    "clippy",
    "fmt",
    "self-check",
    "changelog",
    "docs",
)


def gate_errors(*, event: str, mode: str, needs: dict[str, dict[str, object]]) -> list[str]:
    expected = {job: "success" for job in ALWAYS_REQUIRED}
    expected["test"] = "skipped" if mode == "docs" else "success"
    expected["build"] = "skipped" if event == "pull_request" else "success"

    errors = []
    for job, expected_result in expected.items():
        actual = needs.get(job, {}).get("result", "missing")
        if actual != expected_result:
            errors.append(f"{job}: expected {expected_result}, got {actual}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", required=True)
    parser.add_argument("--mode", choices=("docs", "full", "smoke"), required=True)
    parser.add_argument("--needs-json", required=True)
    args = parser.parse_args()

    errors = gate_errors(
        event=args.event,
        mode=args.mode,
        needs=json.loads(args.needs_json),
    )
    if errors:
        for error in errors:
            print(error)
        return 1
    print(f"CI gate passed for {args.event}/{args.mode}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
