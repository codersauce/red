---
title: "Release Campaign"
summary: "The release campaign manifest is Red's reviewed, versioned release-message source for GitHub release introductions, Discord and in-app highlights, and preview-only X and Bluesky copy."
topics: [reference, release]
sources:
  - id: campaign
    type: file
    path: release/campaign.toml
  - id: release-campaign
    type: file
    path: scripts/release_campaign.py
  - id: social-release
    type: file
    path: scripts/social_release.py
  - id: releasing
    type: file
    path: docs/RELEASING.md
  - id: prepare-release
    type: file
    path: .github/workflows/prepare-release.yml
  - id: release
    type: file
    path: .github/workflows/release.yml
  - id: announce-discord
    type: file
    path: .github/workflows/announce-discord.yml
  - id: ci
    type: file
    path: .github/workflows/ci.yml
  - id: release-pr-body
    type: file
    path: .github/release-pr-body.md
  - id: markdown-links
    type: file
    path: scripts/check_markdown_links.py
  - id: pr-351-doc-links
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/24/rollout-2026-08-24T20-33-04-01a0361e-d26f-7740-9ac1-d9b12eb81cb7.jsonl
---

# Release Campaign

The release campaign is Red's reviewed editorial manifest for release messaging. `release/campaign.toml` is the source for the GitHub release introduction, Discord highlights, in-app highlights, and X and Bluesky preview copy [@campaign] [@releasing]. The scripts render copy and validate bounds; routine release workflows do not publish to X or Bluesky, and social posting requires a separate explicit `--publish` invocation [@release-campaign] [@social-release] [@announce-discord].

## Manifest Contract

The manifest is dependency-free TOML with `schema_version = 1`, a root `version`, `headline`, `summary`, `website`, and at least one `[[stories]]` entry [@campaign] [@release-campaign]. `version` is either `next` while preparing an upcoming release or an exact semantic version after release preparation resolves it [@campaign] [@release-campaign]. The `website` field must be an HTTPS URL [@release-campaign].

Each story has a lowercase hyphenated `id`, nonempty `title` and `summary`, a `status` of `new`, `improved`, or `existing`, a nonempty unique `channels` list, and optional positive integer `pull_requests` [@release-campaign]. Supported channels are `github`, `discord`, `x`, `bluesky`, and `in_app` [@release-campaign]. Story order is editorial order: the release documentation says it determines how users encounter the flagship stories, and the renderer preserves that order when selecting stories for one destination [@releasing] [@release-campaign].

## Version Resolution

Keep the manifest version as `next` on normal development branches [@releasing]. The prepare-release workflow updates `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, README release references, and then runs `scripts/release_campaign.py set-version "$VERSION"` followed by exact-version validation [@prepare-release]. The generated release PR body tells reviewers to confirm the package, README, and release campaign versions, review story status labels, verify shortcuts and prerequisites, preview all destinations, and coordinate the separate website update [@release-pr-body].

The tag release workflow validates `release/campaign.toml` against the tag-derived version before prepending the rendered GitHub campaign introduction to the complete generated release notes [@release]. If the campaign version does not match the tag, the release docs treat that as a release-blocking mismatch alongside package and changelog mismatches [@releasing].

## Rendering And Posting

`scripts/release_campaign.py render` is a local renderer, not a publisher [@release-campaign]. GitHub output is a reviewed introduction that precedes the generated changelog. Discord and in-app output are ordered story-title bullets. X and Bluesky output are bounded social previews with platform-specific limits of 280 and 300 characters [@release-campaign].

`scripts/social_release.py` validates reviewed social text and optional local media before any network access [@social-release]. Without `--publish`, it returns a preview record with `published: false` and does not inspect credentials or call external APIs [@social-release]. With `--publish`, X requires `X_ACCESS_TOKEN`; Bluesky requires `BLUESKY_IDENTIFIER` and `BLUESKY_APP_PASSWORD` [@social-release].

The Discord announcement workflow is the automated external posting path. On a published non-prerelease, or on manual dispatch, it reads the GitHub release, uses a matching campaign when available, posts Discord only when not in dry-run mode, and writes X and Bluesky previews to the GitHub step summary without posting them [@announce-discord]. If an older release has no exact matching campaign, it falls back to changelog-only Discord highlights and skips social previews [@announce-discord] [@releasing].

## Validation And Documentation Links

The main CI workflow validates the current reviewed campaign during workflow lint and runs unit tests for the release campaign, Discord release, social release, CI policy, doctest package, and test-performance helpers [@ci]. Release-campaign changes therefore affect the normal documentation and workflow-lint surface even when no Rust code changes.

Markdown links in release documentation must resolve inside the checked repository. The Markdown checker ignores external URLs but reports unresolved local links and links that escape the repository root [@markdown-links]. A PR #351 documentation failure came from archived release-communication Markdown linking to sibling local worktrees that CI could not see; the repair converted those references into code-formatted path descriptions instead of Markdown links [@pr-351-doc-links]. Use that pattern when recording branch or sibling-worktree artifacts that are not part of the checked-out repository.

Use [Release Red](../../guides/releases/release-red) for the full release procedure and [CI and validation](../validation/ci-and-validation) for the jobs that enforce this contract.
