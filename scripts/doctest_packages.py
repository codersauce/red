#!/usr/bin/env python3
"""Run doctests only for workspace libraries that contain Rust examples."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path


DOC_FENCE = re.compile(r"^\s*//(?:/|!)\s*(`{3,}|~{3,})(.*)$")
RUSTDOC_ATTRIBUTES = {
    "compile_fail",
    "no_run",
    "rust",
    "should_panic",
    "standalone_crate",
    "test_harness",
}


def contains_rust_doctest(source: str) -> bool:
    """Identify runnable fenced Rust examples in ordinary Rust doc comments."""

    open_fence: str | None = None
    for line in source.splitlines():
        match = DOC_FENCE.match(line)
        if match is None:
            continue

        fence, info = match.groups()
        if open_fence is not None:
            if fence.startswith(open_fence) and not info.strip():
                open_fence = None
            continue

        open_fence = fence
        tags = [tag for tag in re.split(r"[,\s]+", info.strip()) if tag]
        if any(tag == "ignore" or tag.startswith("ignore-") for tag in tags):
            continue
        if not tags or tags[0] in RUSTDOC_ATTRIBUTES or tags[0].startswith("edition"):
            return True

    return False


def doctest_packages(metadata: dict[str, object]) -> list[str]:
    """Find workspace library packages with runnable Rust documentation fences."""

    members = set(metadata["workspace_members"])
    packages: list[str] = []
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue

        for target in package["targets"]:
            if not {"lib", "proc-macro"}.intersection(target["kind"]):
                continue

            source_root = Path(target["src_path"]).parent
            if any(
                contains_rust_doctest(source.read_text(encoding="utf-8"))
                for source in source_root.rglob("*.rs")
            ):
                packages.append(package["name"])
                break

    return sorted(packages)


def cargo_command(packages: list[str], *, no_default_features: bool) -> list[str]:
    command = ["cargo", "test", "--locked", "--doc"]
    if no_default_features:
        command.append("--no-default-features")
    for package in packages:
        command.extend(["-p", package])
    return command


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="list packages without running tests")
    parser.add_argument(
        "--no-default-features",
        action="store_true",
        help="avoid feature-only dependencies while running documentation examples",
    )
    args = parser.parse_args()

    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
            text=True,
        )
    )
    packages = doctest_packages(metadata)
    print("Rust doctest packages:", ", ".join(packages) if packages else "none", flush=True)
    if args.list or not packages:
        return 0

    command = cargo_command(packages, no_default_features=args.no_default_features)
    print("Running:", " ".join(command), flush=True)
    return subprocess.run(command, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
