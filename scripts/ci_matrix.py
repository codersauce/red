#!/usr/bin/env python3
"""Select the paid test matrix for a GitHub Actions CI event."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path


UBUNTU = {
    "name": "ubuntu-latest",
    "standard": "ubuntu-latest",
    "warp": "warp-ubuntu-latest-x64-8x",
}
MACOS = {
    "name": "macos-latest",
    "standard": "macos-latest",
    "warp": "warp-macos-latest-arm64-12x",
}
WINDOWS = {
    "name": "windows-latest",
    "standard": "windows-latest",
    "warp": "warp-windows-latest-x64-32x",
}


def is_documentation_path(path: str) -> bool:
    normalized = path.strip().lstrip("./")
    return bool(normalized) and (
        normalized.endswith(".md")
        or normalized == "LICENSE"
        or normalized.startswith("almanac/")
        or normalized.startswith("docs/")
    )


def select_mode(event: str, changed_paths: list[str], manual_scope: str) -> str:
    if event == "workflow_dispatch":
        return manual_scope
    if event == "pull_request":
        if changed_paths and all(is_documentation_path(path) for path in changed_paths):
            return "docs"
        return "full"
    if event == "push":
        return "smoke"
    raise ValueError(f"unsupported event: {event}")


def matrix_for(mode: str) -> dict[str, list[dict[str, str]]]:
    if mode == "full":
        return {"include": [UBUNTU, MACOS, WINDOWS]}
    if mode == "smoke":
        return {"include": [UBUNTU]}
    if mode == "docs":
        # The test job is skipped for this mode, but a valid matrix keeps the
        # workflow expression well-formed before the job-level condition runs.
        return {"include": [UBUNTU]}
    raise ValueError(f"unsupported validation mode: {mode}")


def write_outputs(path: Path, *, mode: str, matrix: dict[str, object]) -> None:
    with path.open("a", encoding="utf-8") as output:
        output.write(f"mode={mode}\n")
        output.write(f"matrix={json.dumps(matrix, separators=(',', ':'))}\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", required=True)
    parser.add_argument("--changed-paths", type=Path)
    parser.add_argument("--manual-scope", choices=("full", "smoke"), default="full")
    parser.add_argument(
        "--github-output",
        type=Path,
        default=Path(os.environ["GITHUB_OUTPUT"]) if "GITHUB_OUTPUT" in os.environ else None,
    )
    args = parser.parse_args()

    changed_paths = []
    if args.changed_paths is not None and args.changed_paths.exists():
        changed_paths = args.changed_paths.read_text(encoding="utf-8").splitlines()

    mode = select_mode(args.event, changed_paths, args.manual_scope)
    matrix = matrix_for(mode)
    if args.github_output is not None:
        write_outputs(args.github_output, mode=mode, matrix=matrix)
    else:
        print(json.dumps({"mode": mode, "matrix": matrix}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
