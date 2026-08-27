#!/usr/bin/env python3
"""Validate and render Red's reviewed, version-specific release campaign."""

from __future__ import annotations

from argparse import ArgumentParser
from collections.abc import Mapping
from pathlib import Path
import re
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CAMPAIGN = ROOT / "release" / "campaign.toml"
SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[+-][0-9A-Za-z.-]+)?")
STORY_ID = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
SUPPORTED_CHANNELS = ("github", "discord", "x", "bluesky", "in_app")
SUPPORTED_STATUSES = ("new", "improved", "existing")
SOCIAL_LIMITS = {"x": 280, "bluesky": 300}


class CampaignError(ValueError):
    """A release campaign cannot be safely validated or rendered."""


def load_campaign(path: Path = DEFAULT_CAMPAIGN) -> dict[str, Any]:
    """Read and validate the dependency-free TOML campaign manifest."""
    try:
        with path.open("rb") as source:
            campaign = tomllib.load(source)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise CampaignError(f"cannot read campaign {path}: {error}") from error
    validate_campaign(campaign)
    return campaign


def validate_campaign(
    campaign: Mapping[str, Any], expected_version: str | None = None
) -> None:
    """Reject ambiguous stories, unsupported channels, and version mismatches."""
    if campaign.get("schema_version") != 1:
        raise CampaignError("schema_version must be 1")

    version = campaign.get("version")
    if not isinstance(version, str) or (
        version != "next" and SEMVER.fullmatch(version) is None
    ):
        raise CampaignError("version must be next or a semantic version")
    if expected_version is not None and version != expected_version:
        raise CampaignError(
            f"campaign version {version!r} does not match expected {expected_version!r}"
        )

    for key in ("headline", "summary", "website"):
        value = campaign.get(key)
        if not isinstance(value, str) or not value.strip():
            raise CampaignError(f"{key} must be a nonempty string")
    if not campaign["website"].startswith("https://"):
        raise CampaignError("website must be an HTTPS URL")

    stories = campaign.get("stories")
    if not isinstance(stories, list) or not stories:
        raise CampaignError("at least one campaign story is required")

    seen: set[str] = set()
    for position, story in enumerate(stories, start=1):
        if not isinstance(story, dict):
            raise CampaignError(f"story {position} must be a TOML table")
        story_id = story.get("id")
        if not isinstance(story_id, str) or STORY_ID.fullmatch(story_id) is None:
            raise CampaignError(f"story {position} needs a lowercase hyphenated id")
        if story_id in seen:
            raise CampaignError(f"duplicate story id: {story_id}")
        seen.add(story_id)

        for key in ("title", "summary"):
            value = story.get(key)
            if not isinstance(value, str) or not value.strip():
                raise CampaignError(f"story {story_id}: {key} must be nonempty")
        if story.get("status") not in SUPPORTED_STATUSES:
            raise CampaignError(f"story {story_id}: unsupported status")

        channels = story.get("channels")
        if not isinstance(channels, list) or not channels:
            raise CampaignError(f"story {story_id}: channels must be a nonempty list")
        if len(channels) != len(set(channels)):
            raise CampaignError(f"story {story_id}: duplicate channel")
        unsupported = set(channels).difference(SUPPORTED_CHANNELS)
        if unsupported:
            raise CampaignError(
                f"story {story_id}: unsupported channels: {', '.join(sorted(unsupported))}"
            )

        pull_requests = story.get("pull_requests", [])
        if not isinstance(pull_requests, list) or any(
            not isinstance(number, int) or isinstance(number, bool) or number <= 0
            for number in pull_requests
        ):
            raise CampaignError(f"story {story_id}: pull_requests must contain positive integers")


def stories_for_channel(campaign: Mapping[str, Any], channel: str) -> list[dict[str, Any]]:
    """Preserve reviewed editorial order while filtering for one destination."""
    if channel not in SUPPORTED_CHANNELS:
        raise CampaignError(f"unsupported channel: {channel}")
    return [story for story in campaign["stories"] if channel in story["channels"]]


def render_github(campaign: Mapping[str, Any]) -> str:
    """Render a reviewed introduction that precedes the complete changelog."""
    version = campaign["version"]
    heading = (
        f"## Red v{version}: {campaign['headline']}"
        if version != "next"
        else f"## Red: {campaign['headline']}"
    )
    lines = [heading, "", campaign["summary"], "", "### Release highlights", ""]
    for story in stories_for_channel(campaign, "github"):
        label = "New" if story["status"] == "new" else "Improved"
        lines.append(f"- **{label}: {story['title']}.** {story['summary']}")
    lines.extend(("", "Agent support requires an installed Codex CLI and `codex login`."))
    return "\n".join(lines) + "\n"


def render_bullets(campaign: Mapping[str, Any], channel: str) -> str:
    """Render ordered channel-specific bullets for Discord or in-app previews."""
    return "\n".join(
        f"- {story['title']}" for story in stories_for_channel(campaign, channel)
    ) + "\n"


def render_social(campaign: Mapping[str, Any], channel: str) -> str:
    """Build a bounded, preview-only social post without publishing anything."""
    limit = SOCIAL_LIMITS[channel]
    version = campaign["version"]
    introduction = "Red: a modal editor with an editor-aware coding agent"
    if version != "next":
        introduction = f"Red v{version}: modal editing meets editor-aware agents"
    ending = "\n\n" + campaign["website"]
    lines = [introduction]
    for story in stories_for_channel(campaign, channel):
        candidate = "\n".join([*lines, f"- {story['title']}"]) + ending
        if len(candidate) <= limit:
            lines.append(f"- {story['title']}")
    result = "\n".join(lines) + ending
    if len(lines) == 1:
        raise CampaignError(f"no campaign stories fit within the {channel} limit")
    if len(result) > limit:
        raise CampaignError(f"{channel} preview exceeds {limit} characters")
    return result + "\n"


def render(campaign: Mapping[str, Any], channel: str) -> str:
    """Render one reviewed channel without network access or posting."""
    if channel == "github":
        return render_github(campaign)
    if channel in ("discord", "in_app"):
        return render_bullets(campaign, channel)
    if channel in SOCIAL_LIMITS:
        return render_social(campaign, channel)
    raise CampaignError(f"unsupported channel: {channel}")


def set_version(path: Path, version: str) -> None:
    """Update only the unique root-level version while preserving human edits."""
    if SEMVER.fullmatch(version) is None:
        raise CampaignError(f"{version!r} is not a supported semantic version")
    campaign = load_campaign(path)
    current = campaign["version"]
    contents = path.read_text(encoding="utf-8")
    expression = re.compile(
        rf'(?m)^(version\s*=\s*"){re.escape(current)}("\s*)$'
    )
    contents, count = expression.subn(
        lambda match: f"{match.group(1)}{version}{match.group(2)}", contents
    )
    if count != 1:
        raise CampaignError("campaign must contain exactly one root version field")
    path.write_text(contents, encoding="utf-8")
    validate_campaign(load_campaign(path), expected_version=version)


def main() -> None:
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("--campaign", type=Path, default=DEFAULT_CAMPAIGN)
    actions = parser.add_subparsers(dest="action", required=True)

    validate = actions.add_parser("validate", help="validate the reviewed campaign")
    validate.add_argument("--version", help="require this exact resolved version")

    update = actions.add_parser("set-version", help="resolve the selected release version")
    update.add_argument("version")

    render_command = actions.add_parser("render", help="render one or all preview channels")
    selection = render_command.add_mutually_exclusive_group(required=True)
    selection.add_argument("--channel", choices=SUPPORTED_CHANNELS)
    selection.add_argument("--all", action="store_true")
    render_command.add_argument("--output", type=Path)
    render_command.add_argument("--output-dir", type=Path)
    render_command.add_argument("--version", help="require this exact resolved version")
    args = parser.parse_args()

    if args.action == "set-version":
        set_version(args.campaign, args.version)
        print(f"resolved release campaign version to {args.version}")
        return

    campaign = load_campaign(args.campaign)
    validate_campaign(campaign, expected_version=args.version)
    if args.action == "validate":
        print(f"validated {len(campaign['stories'])} stories for {campaign['version']}")
        return

    if args.all:
        if args.output is not None or args.output_dir is None:
            raise CampaignError("render --all requires --output-dir and cannot use --output")
        args.output_dir.mkdir(parents=True, exist_ok=True)
        for channel in SUPPORTED_CHANNELS:
            destination = args.output_dir / f"{channel}.md"
            destination.write_text(render(campaign, channel), encoding="utf-8")
        print(f"rendered {len(SUPPORTED_CHANNELS)} channel previews to {args.output_dir}")
    else:
        if args.output_dir is not None:
            raise CampaignError("--output-dir is only valid with --all")
        output = render(campaign, args.channel)
        if args.output is not None:
            args.output.write_text(output, encoding="utf-8")
        else:
            sys.stdout.write(output)


if __name__ == "__main__":
    try:
        main()
    except CampaignError as error:
        raise SystemExit(f"release_campaign.py: {error}") from error
