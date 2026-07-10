#!/usr/bin/env python3
"""Print reproducible source facts used by the project plan.

Run from anywhere with:

    python3 scripts/repository_inventory.py

This intentionally reports facts rather than enforcing fixed counts. Roadmap documents
can become stale without making otherwise-correct source changes fail CI.
"""

from __future__ import annotations

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def enum_body(source: str, name: str) -> str:
    """Return the text inside a Rust enum's outer braces."""

    declaration = re.search(rf"\b(?:pub\s+)?enum\s+{re.escape(name)}\s*{{", source)
    if declaration is None:
        raise ValueError(f"could not find enum {name}")

    opening_brace = source.find("{", declaration.start())
    depth = 0
    for index in range(opening_brace, len(source)):
        character = source[index]
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[opening_brace + 1 : index]

    raise ValueError(f"enum {name} has no closing brace")


def enum_variant_count(path: Path, name: str) -> int:
    """Count top-level variants in a conventionally formatted Rust enum."""

    body = enum_body(path.read_text(), name)
    nesting = 0
    variants = 0

    for line in body.splitlines():
        if nesting == 0 and re.match(r"^\s*[A-Za-z_][A-Za-z0-9_]*\s*(?:[({=,]|$)", line):
            variants += 1

        code = line.split("//", 1)[0]
        nesting += sum(code.count(character) for character in "({[")
        nesting -= sum(code.count(character) for character in ")}]")

    return variants


def line_count(path: Path) -> int:
    """Count text lines using the same semantics as common command-line tools."""

    with path.open() as file:
        return sum(1 for _ in file)


def main() -> None:
    editor = ROOT / "src" / "editor.rs"
    plugins = sorted((ROOT / "plugins").glob("*.hk"))
    git_plugin = ROOT / "plugins" / "git.hk"

    facts = (
        ("Action variants", enum_variant_count(editor, "Action")),
        ("PluginRequest variants", enum_variant_count(editor, "PluginRequest")),
        ("Bundled Husk plugins", len(plugins)),
        ("git.hk lines", line_count(git_plugin)),
        ("git.hk bytes", git_plugin.stat().st_size),
    )

    width = max(len(label) for label, _ in facts)
    for label, value in facts:
        print(f"{label:<{width}}  {value}")


if __name__ == "__main__":
    main()
