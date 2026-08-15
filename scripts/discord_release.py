#!/usr/bin/env python3
"""Build a concise Discord announcement from a GitHub release."""

from __future__ import annotations

from argparse import ArgumentParser
from collections import defaultdict
import json
from pathlib import Path
import re
from typing import Any


SECTION = re.compile(r"^###\s+(.+?)\s*$")
BULLET = re.compile(r"^-\s+(.+?)\s*$")
TRAILING_LINK = re.compile(r"\s*\(\[[^\]]+\]\([^)]+\)\)\s*$")
SCOPE = re.compile(r"^\*\*([^*:]+):\*\*")
SUPPORTED_SECTIONS = ("Features", "Performance", "Bug Fixes")
RED = 0xE5484D
IMPACT_TERMS = (
    "interactive",
    "language server",
    "file management",
    "standard library",
    "comment operator",
    "matching bracket",
    "workflow",
    "theme-aware",
    "hang",
    "failure",
    "recover",
    "latency",
    "prevent",
)


def clean_item(item: str) -> str:
    """Remove changelog provenance links while preserving useful Markdown."""
    while TRAILING_LINK.search(item):
        item = TRAILING_LINK.sub("", item)
    return item.strip()


def release_sections(body: str) -> dict[str, list[str]]:
    """Extract announcement-worthy changelog sections from release notes."""
    sections: dict[str, list[str]] = defaultdict(list)
    current: str | None = None
    for line in body.splitlines():
        if line.startswith("## Installation"):
            break
        if line.startswith("## "):
            current = None
            continue
        heading = SECTION.match(line)
        if heading:
            current = heading.group(1)
            continue
        bullet = BULLET.match(line)
        if current in SUPPORTED_SECTIONS and bullet:
            sections[current].append(clean_item(bullet.group(1)))
    return dict(sections)


def select_image(sections: dict[str, list[str]]) -> str:
    """Choose the most relevant existing website capture for the release."""
    preferred_items = sections.get("Features") or [
        item for items in sections.values() for item in items
    ]
    scopes = {scope for item in preferred_items if (scope := item_scope(item))}
    choices = (
        (("agent",), "agent-pane-dark.png"),
        (("theme", "themes"), "themes-dark.png"),
        (("lsp",), "lsp-dialog-dark.png"),
        (("picker",), "palette-dark.png"),
        (("search", "grep"), "grep-dark.png"),
        (("neotree", "git"), "editor-dark.png"),
    )
    for candidate_scopes, filename in choices:
        if scopes.intersection(candidate_scopes):
            return f"https://getred.dev/{filename}"
    return "https://getred.dev/editing-dark.png"


def plural(count: int, singular: str, plural_form: str | None = None) -> str:
    return singular if count == 1 else (plural_form or f"{singular}s")


def item_scope(item: str) -> str | None:
    match = SCOPE.match(item)
    return match.group(1).lower() if match else None


def ranked_items(items: list[str]) -> list[str]:
    """Prefer broad, user-visible changes while retaining scope variety."""
    indexed = list(enumerate(items))

    def score(entry: tuple[int, str]) -> tuple[int, int]:
        index, item = entry
        lowered = item.lower()
        impact = sum(
            len(IMPACT_TERMS) - position
            for position, term in enumerate(IMPACT_TERMS)
            if term in lowered
        )
        return impact, -index

    ranked = sorted(indexed, key=score, reverse=True)
    diverse: list[tuple[int, str]] = []
    repeated: list[tuple[int, str]] = []
    seen_scopes: set[str] = set()
    for entry in ranked:
        scope = item_scope(entry[1])
        if scope is None or scope in seen_scopes:
            repeated.append(entry)
        else:
            diverse.append(entry)
            seen_scopes.add(scope)
    return [item for _, item in [*diverse, *repeated]]


def limited_bullets(items: list[str], limit: int, max_chars: int = 950) -> str:
    selected: list[str] = []
    for item in ranked_items(items)[:limit]:
        candidate = "\n".join([*selected, f"• {item}"])
        if len(candidate) > max_chars:
            break
        selected.append(f"• {item}")
    return "\n".join(selected)


def build_payload(release: dict[str, Any], mention_everyone: bool = False) -> dict[str, Any]:
    tag = str(release["tagName"])
    body = str(release.get("body") or "")
    sections = release_sections(body)
    features = sections.get("Features", [])
    performance = sections.get("Performance", [])
    fixes = sections.get("Bug Fixes", [])

    counts: list[str] = []
    if features:
        counts.append(f"{len(features)} new {plural(len(features), 'feature')}")
    if performance:
        counts.append(f"{len(performance)} performance {plural(len(performance), 'improvement')}")
    if fixes:
        counts.append(f"{len(fixes)} {plural(len(fixes), 'fix', 'fixes')}")
    count_summary = ", ".join(counts[:-1])
    if len(counts) > 1:
        count_summary += f" and {counts[-1]}"
    elif counts:
        count_summary = counts[0]

    description = "A new release of **Red** is available."
    if count_summary:
        description = f"A new release of **Red** is available with {count_summary}."

    fields: list[dict[str, Any]] = []
    feature_highlights = limited_bullets(features, 5)
    if feature_highlights:
        fields.append({"name": "✨ Highlights", "value": feature_highlights, "inline": False})

    polish = [*performance, *fixes]
    polish_highlights = limited_bullets(polish, 3)
    if polish_highlights:
        fields.append({"name": "🛠️ Fixes & polish", "value": polish_highlights, "inline": False})

    fields.append(
        {
            "name": "Install",
            "value": (
                "```bash\n"
                "brew install codersauce/tap/red\n"
                "# or\n"
                "curl --proto '=https' --tlsv1.2 -fsSL https://getred.dev/install.sh | sh\n"
                "```"
            ),
            "inline": False,
        }
    )

    footer_parts = [
        f"{len(features)} {plural(len(features), 'feature')}",
        f"{len(fixes)} {plural(len(fixes), 'fix', 'fixes')}",
    ]
    payload: dict[str, Any] = {
        "username": "Red Releases",
        "avatar_url": "https://github.com/codersauce.png",
        "allowed_mentions": {"parse": ["everyone"] if mention_everyone else []},
        "embeds": [
            {
                "title": f"🚀 Red Editor {tag} is out!",
                "url": release["url"],
                "description": description,
                "color": RED,
                "fields": fields,
                "image": {"url": select_image(sections)},
                "footer": {"text": " • ".join(footer_parts)},
                **(
                    {"timestamp": release["publishedAt"]}
                    if release.get("publishedAt")
                    else {}
                ),
            }
        ],
    }
    if mention_everyone:
        payload["content"] = "@everyone A new Red release is ready."
    return payload


def markdown_preview(payload: dict[str, Any]) -> str:
    embed = payload["embeds"][0]
    lines = [f"# {embed['title']}", "", embed["description"], ""]
    for field in embed["fields"]:
        lines.extend((f"## {field['name']}", "", field["value"], ""))
    lines.extend((f"Image: {embed['image']['url']}", "", f"Release: {embed['url']}", ""))
    return "\n".join(lines)


def main() -> None:
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("--release-json", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--summary", type=Path)
    parser.add_argument("--mention-everyone", action="store_true")
    args = parser.parse_args()

    release = json.loads(args.release_json.read_text(encoding="utf-8"))
    payload = build_payload(release, mention_everyone=args.mention_everyone)
    args.output.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    if args.summary:
        args.summary.write_text(markdown_preview(payload), encoding="utf-8")


if __name__ == "__main__":
    main()
