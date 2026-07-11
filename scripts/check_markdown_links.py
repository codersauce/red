#!/usr/bin/env python3
"""Check that repository-relative Markdown links resolve to repository content."""

from pathlib import Path
import html
import re
import sys
import tempfile
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
SKIP = {".git", "node_modules", "target"}
INLINE = re.compile(r"!?\[[^\]]*\]\(\s*(?:<([^>]+)>|([^\s)]+))")
REFERENCE = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*(?:<([^>]+)>|(\S+))", re.MULTILINE)
HEADING = re.compile(r"^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$")
SETEXT = re.compile(r"^\s{0,3}(?:=+|-+)\s*$")
FENCE = re.compile(r"^\s{0,3}(`{3,}|~{3,})")
EXPLICIT_ID = re.compile(r"\{#([A-Za-z0-9_.:-]+)\}")
HTML_ID = re.compile(r"\b(?:id|name)\s*=\s*['\"]([^'\"]+)['\"]", re.IGNORECASE)
INLINE_LINK = re.compile(r"!?\[([^\]]+)\]\([^)]*\)")
HTML_TAG = re.compile(r"<[^>]+>")


def targets(contents: str) -> list[str]:
    matches = [*INLINE.findall(contents), *REFERENCE.findall(contents)]
    return [bracketed or plain for bracketed, plain in matches]


def unfenced_lines(contents: str) -> list[str]:
    lines: list[str] = []
    fence: str | None = None
    for line in contents.splitlines():
        marker = FENCE.match(line)
        if marker:
            character = marker.group(1)[0]
            if fence is None:
                fence = character
            elif fence == character:
                fence = None
            lines.append("")
        elif fence is None:
            lines.append(line)
        else:
            lines.append("")
    return lines


def heading_slug(heading: str) -> str:
    heading = EXPLICIT_ID.sub("", heading)
    heading = INLINE_LINK.sub(r"\1", heading)
    heading = HTML_TAG.sub("", heading)
    heading = html.unescape(heading).replace("`", "").strip().lower()
    heading = "".join(
        character
        for character in heading
        if character.isalnum() or character in {" ", "_", "-"}
    )
    return heading.replace(" ", "-")


def anchors(contents: str) -> set[str]:
    lines = unfenced_lines(contents)
    result = {
        match.group(1)
        for line in lines
        for match in HTML_ID.finditer(line)
    }
    counts: dict[str, int] = {}
    for index, line in enumerate(lines):
        heading = HEADING.match(line)
        if heading:
            value = heading.group(1)
        elif index + 1 < len(lines) and line.strip() and SETEXT.match(lines[index + 1]):
            value = line.strip()
        else:
            continue

        result.update(EXPLICIT_ID.findall(value))
        slug = heading_slug(value)
        if not slug:
            continue
        count = counts.get(slug, 0)
        counts[slug] = count + 1
        result.add(slug if count == 0 else f"{slug}-{count}")
    return result


def self_test() -> None:
    sample = """# Heading, with punctuation!
## Phase 4 — Typed plugin compatibility contract (10–14 weeks)
## Duplicate heading
## Duplicate heading
## [`Inline code`](guide.md) and `ticks`
## Explicit target {#custom-anchor}
Setext heading
--------------
<a id="html-anchor"></a>
```markdown
## fenced heading
<a id="fenced-anchor"></a>
```
"""
    expected = {
        "heading-with-punctuation",
        "phase-4--typed-plugin-compatibility-contract-1014-weeks",
        "duplicate-heading",
        "duplicate-heading-1",
        "inline-code-and-ticks",
        "explicit-target",
        "custom-anchor",
        "setext-heading",
        "html-anchor",
    }
    actual = anchors(sample)
    assert expected <= actual, f"missing anchors: {sorted(expected - actual)}"
    assert "fenced-heading" not in actual
    assert "fenced-anchor" not in actual
    assert targets(
        "[local](guide.md#heading) ![image](<images/a b.png>)\n"
        "[reference]: ../README.md#quick-start\n"
        "[external](https://example.test/page#section)"
    ) == [
        "guide.md#heading",
        "images/a b.png",
        "https://example.test/page#section",
        "../README.md#quick-start",
    ]
    with tempfile.TemporaryDirectory(prefix="red-markdown-links-") as directory:
        root = Path(directory).resolve()
        guide = root / "guide.md"
        source = root / "source.md"
        guide.write_text("# Valid heading\n<a id=\"custom-id\"></a>\n", encoding="utf-8")
        source.write_text(
            "[valid](guide.md#valid-heading)\n"
            "[custom](guide.md#custom-id)\n"
            "[encoded](guide.md#valid%2Dheading)\n"
            "[missing](guide.md#missing-heading)\n"
            "[same-file](#missing-local)\n",
            encoding="utf-8",
        )
        errors = link_errors(root, [guide, source])
        assert errors == [
            "source.md: broken fragment `guide.md#missing-heading` "
            "(missing `#missing-heading`)",
            "source.md: broken fragment `#missing-local` (missing `#missing-local`)",
        ], errors
    print("markdown link checker self-test: ok")


def link_errors(root: Path, files: list[Path]) -> list[str]:
    errors: list[str] = []
    anchor_cache: dict[Path, set[str]] = {}
    for source in files:
        for target in targets(source.read_text(encoding="utf-8")):
            parsed = urlsplit(target)
            if parsed.scheme or parsed.netloc:
                continue
            destination = (
                (source.parent / unquote(parsed.path)).resolve()
                if parsed.path
                else source
            )
            if not destination.is_relative_to(root) or not destination.exists():
                errors.append(f"{source.relative_to(root)}: broken link `{target}`")
                continue
            if not parsed.fragment or destination.suffix.lower() not in {".md", ".markdown"}:
                continue
            destination_anchors = anchor_cache.setdefault(
                destination,
                anchors(destination.read_text(encoding="utf-8")),
            )
            fragment = unquote(parsed.fragment)
            if fragment not in destination_anchors:
                errors.append(
                    f"{source.relative_to(root)}: broken fragment `{target}` "
                    f"(missing `#{fragment}`)"
                )
    return errors


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    if len(sys.argv) != 1:
        print("usage: check_markdown_links.py [--self-test]", file=sys.stderr)
        return 2

    files = sorted(
        path
        for path in ROOT.rglob("*.md")
        if not any(part in SKIP for part in path.parts)
    )
    errors = link_errors(ROOT, files)

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"checked relative links in {len(files)} Markdown files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
