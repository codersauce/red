#!/usr/bin/env python3
"""Update and validate the release version advertised in README.md."""

from argparse import ArgumentParser
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parent.parent
README = ROOT / "README.md"
CARGO_MANIFEST = ROOT / "Cargo.toml"
SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[+-][0-9A-Za-z.-]+)?")
MARKER = re.compile(r"<!-- current-release: ([^ ]+) -->")
RELEASE_LINK = re.compile(
    r"The current documented release is\s+"
    r"\[v([^\]]+)\]\("
    r"https://github\.com/codersauce/red/releases/tag/v([^)]+)"
    r"\)\."
)
INSTALL_PIN = re.compile(r"\bRED_VERSION=([^\s]+)")


def single_match(pattern: re.Pattern[str], contents: str, label: str) -> tuple[str, ...]:
    matches = pattern.findall(contents)
    if len(matches) != 1:
        raise ValueError(f"expected exactly one {label}, found {len(matches)}")
    match = matches[0]
    return match if isinstance(match, tuple) else (match,)


def package_version() -> str:
    contents = CARGO_MANIFEST.read_text(encoding="utf-8")
    package = re.search(
        r"(?ms)^\[package\]\s*$\n(.*?)(?=^\[[^\]]+\]\s*$|\Z)", contents
    )
    if package is None:
        raise ValueError("Cargo.toml has no [package] section")
    version = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*$', package.group(1))
    if version is None:
        raise ValueError("Cargo.toml [package] section has no version")
    return version.group(1)


def advertised_versions(contents: str) -> dict[str, str]:
    marker = single_match(MARKER, contents, "current-release marker")[0]
    link_label, link_target = single_match(
        RELEASE_LINK, contents, "current release link"
    )
    install_pin = single_match(INSTALL_PIN, contents, "RED_VERSION install pin")[0]
    if link_label != link_target:
        raise ValueError(
            f"README release link label v{link_label} targets v{link_target}"
        )
    return {
        "marker": marker,
        "release link": link_label,
        "install pin": install_pin,
    }


def check() -> None:
    expected = package_version()
    versions = advertised_versions(README.read_text(encoding="utf-8"))
    mismatches = [
        f"{label} is {version}, expected {expected}"
        for label, version in versions.items()
        if version != expected
    ]
    if mismatches:
        raise ValueError("; ".join(mismatches))
    print(f"README release references match Cargo.toml version {expected}")


def update(version: str) -> None:
    if SEMVER.fullmatch(version) is None:
        raise ValueError(f"{version!r} is not a supported semantic version")

    contents = README.read_text(encoding="utf-8")
    current_versions = advertised_versions(contents)
    old_versions = set(current_versions.values())
    if len(old_versions) != 1:
        details = ", ".join(
            f"{label}={value}" for label, value in current_versions.items()
        )
        raise ValueError(f"README release references already disagree: {details}")
    old_version = old_versions.pop()

    replacements = {
        f"<!-- current-release: {old_version} -->": (
            f"<!-- current-release: {version} -->"
        ),
        (
            "The current documented release is\n"
            f"[v{old_version}]"
            "(https://github.com/codersauce/red/releases/"
            f"tag/v{old_version})."
        ): (
            "The current documented release is\n"
            f"[v{version}]"
            "(https://github.com/codersauce/red/releases/"
            f"tag/v{version})."
        ),
        f"RED_VERSION={old_version}": f"RED_VERSION={version}",
    }
    for old, new in replacements.items():
        if contents.count(old) != 1:
            raise ValueError(f"expected exactly one README occurrence of {old!r}")
        contents = contents.replace(old, new)

    README.write_text(contents, encoding="utf-8")
    print(f"updated README release references from {old_version} to {version}")


def main() -> None:
    parser = ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true")
    action.add_argument("--set", metavar="VERSION")
    args = parser.parse_args()

    if args.set:
        update(args.set)
    else:
        check()


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        raise SystemExit(f"readme_release.py: {error}") from error
