---
title: "Release Red"
summary: "This guide describes the Red release flow from prepare-release workflow through reviewed campaign resolution, tag publishing, archive smoke tests, Homebrew update, installer verification, and announcement."
topics: [guides, release, ci, installers]
sources:
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
  - id: plugin-check
    type: file
    path: .github/workflows/plugin-check.yml
  - id: performance
    type: file
    path: .github/workflows/performance.yml
  - id: security-audit
    type: file
    path: .github/workflows/security-audit.yml
  - id: readme-release
    type: file
    path: scripts/readme_release.py
  - id: release-campaign
    type: file
    path: scripts/release_campaign.py
  - id: campaign
    type: file
    path: release/campaign.toml
  - id: social-release
    type: file
    path: scripts/social_release.py
  - id: discord-release
    type: file
    path: scripts/discord_release.py
  - id: release-pr-body
    type: file
    path: .github/release-pr-body.md
  - id: release-cliff
    type: file
    path: cliff.release.toml
  - id: whats-new
    type: file
    path: src/whats_new.rs
---

Use this guide to publish a Red release without mixing up release preparation, reviewed campaign resolution, the tag build, draft release review, Homebrew publication, installer smoke tests, and announcement. The release process is split on purpose: a prepare workflow opens a normal release PR, an annotated tag builds and smoke-tests archives into a draft GitHub release, publishing that release updates Homebrew, and a separate Discord workflow announces only published non-prerelease releases unless manually invoked [@prepare-release] [@release] [@announce-discord]. The reviewed campaign is the shared editorial source for GitHub release introductions, Discord and in-app highlights, and preview-only X and Bluesky copy [@campaign] [@release-campaign] [@social-release].

## Prerequisites

Start only when CI is green on `main` and the release secrets are available. The release documentation requires `codersauce/homebrew-tap` with a `Formula/` directory, `HOMEBREW_TAP_TOKEN` that can push to that tap, and a `RELEASE_PR_TOKEN` with repository-scoped Contents and Pull requests read/write permissions [@releasing]. The release workflow uses the repository `GITHUB_TOKEN` to create a draft release, and `release.yml` grants `contents: write` only to the publishing job [@releasing] [@release].

Choose the next SemVer version without a leading `v`. The prepare workflow validates that the input matches SemVer, that `RELEASE_PR_TOKEN` is present, that the tag does not already exist, and that the requested version is newer than the current package version [@prepare-release].

Keep `release/campaign.toml` at `version = "next"` until release preparation resolves it [@campaign] [@releasing]. The campaign is reviewed product copy: each story should be evidence-backed, ordered intentionally, and labeled as `new`, `improved`, or `existing` according to what the release actually ships [@campaign] [@releasing]. Use [Release Campaign](../../reference/releases/release-campaign) for the exact manifest and rendering contract.

## Prepare The Release PR

Run the prepare workflow from `main`:

```shell
gh workflow run prepare-release.yml --ref main -f version=0.2.0
```

The workflow checks out `main`, generates release notes from Conventional Commit subjects with git-cliff, updates `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, README release references, and the release campaign version, then creates or updates `release/v<version>` [@prepare-release]. The README update is performed by `scripts/readme_release.py --set`, and the workflow immediately runs `scripts/readme_release.py --check` to verify the marker, release link, and installer pin match `Cargo.toml` [@prepare-release] [@readme-release]. It also runs `scripts/release_campaign.py set-version "$VERSION"` and validates the exact campaign version before opening the PR [@prepare-release] [@release-campaign].

Review the release PR as a normal ready-for-review PR. The release docs call out the generated changelog, package version, README release link, pinned installer example, reviewed release campaign, Clippy, bundled-runtime self-check, README and campaign version checks, and generated-versus-committed changelog comparison as required gates [@releasing] [@ci]. The generated PR body also asks reviewers to verify shortcuts, supported platforms, prerequisites, safety claims, linked PRs, social previews, upcoming-release wording, demo media, and the separate website update [@release-pr-body]. For local equivalents, use [Build, Test, And Validate](../development/build-test-and-validate) and [CI And Validation](../../reference/validation/ci-and-validation).

## Tag The Merge Commit

After merging the release PR, update local `main`, verify the merge commit, and then create an annotated tag from that commit. Before tagging, confirm that `main` contains the release merge commit and that the post-merge validation surface has completed for that exact commit. The main CI workflow covers workflow lint, README release-version validation, Discord announcement tests, cross-platform tests, formatting, bundled runtime self-check, changelog checks, release binary builds, documentation, and the selected paid test mode [@ci]. Separate path-filtered workflows cover Husk plugin checks, deterministic performance, and Rust dependency audit when their trigger paths match the release merge [@plugin-check] [@performance] [@security-audit]. This matters because the tag push immediately starts archive creation, and the draft release should be built from the same commit whose merge validation passed [@release].

```shell
git checkout main
git pull --ff-only origin main
git tag -a v0.2.0 -m 'Release v0.2.0'
git push origin v0.2.0
```

The release documentation requires the `v` prefix on the tag even though the prepare workflow input omits it [@releasing]. For release candidates, include `alpha`, `beta`, or `rc` in the version; the release workflow marks matching tags as prereleases when it creates or edits release metadata [@releasing] [@release].

## Watch The Release Workflow

The Release workflow runs on `v*` tag pushes and manual dispatch with an existing tag name [@release]. It builds four archives: Linux x86_64 tarball, macOS Intel tarball, macOS Apple Silicon tarball, and Windows x86_64 zip [@release]. Each archive contains the release binary plus `README.md`, `LICENSE`, and `default_config.toml`; runtime plugins, themes, and default config are embedded in the binary [@releasing] [@release].

The smoke job extracts every archive and runs `--self-check`. It requires the final line to be `red self-check ok`, fails on unhealthy plugin status lines, and checks that the binary does not contain the GitHub workspace path [@release]. This is the packaging-level runtime gate; the exact command behavior is covered by [Self Check](../../reference/runtime/self-check).

The publish job verifies package version and committed release changelog, regenerates release changelog content, copies `install/install.sh` and `install/install.ps1` into the release assets, generates `SHA256SUMS.txt`, generates public GitHub release notes with `cliff.release.toml`, prepends the exact-version campaign introduction, appends installation and checksum guidance, creates or updates a draft GitHub release, and uploads all assets [@release] [@release-campaign] [@release-cliff].

## Review And Publish The Draft

Before publishing, confirm the draft release contains all four archives, `SHA256SUMS.txt`, `install.sh`, and `install.ps1`, that install instructions match the release tag, and that change authors, pull request numbers, and first-time contributors are credited correctly [@releasing]. Do not publish if the package version, changelog section, checksums, assets, or public notes do not match the intended tag. If a tag was pushed with the wrong version, the release docs require deleting the draft release and tag, fixing the version, and pushing a new tag before anything is published [@releasing].

Publishing the GitHub release triggers a second `Release` workflow run for the `release.published` event; its build, smoke, and draft-publish jobs are skipped, and only the `homebrew` job is eligible [@release]. That job requires `HOMEBREW_TAP_TOKEN`, downloads release tarballs and checksums, writes `Formula/red.rb` with OS-specific URLs and SHA-256 values, and pushes the formula update if it changed [@release]. Use `gh run list --event release` when checking publication automation, because the release-event `Release` run and the tag-push `Release` run have the same workflow name [@release].

## Verify Installers And Announcement

After publishing, verify Homebrew and the stable installers. The release docs require `brew update`, `brew install codersauce/tap/red`, `red --version`, and temporary-directory installer checks for Unix and Windows [@releasing]. Follow [Release Installers](../installers/release-installers) for installer-specific checks.

The Discord announcement workflow runs on published releases and manual dispatch, but it skips prereleases for automatic release events [@announce-discord]. It reads the published GitHub release, uses the matching release campaign when available, runs `scripts/discord_release.py` to build JSON and a Markdown summary, checks the webhook only when not in dry-run mode, and posts through `curl` with retry options [@announce-discord] [@discord-release]. The same workflow renders X and Bluesky campaign previews through `scripts/social_release.py` and writes them to the job summary without posting them [@announce-discord] [@social-release]. The helper selects announcement sections from the campaign or from "Features", "Performance", and "Bug Fixes", stops before installation boilerplate, builds a compact embed, chooses an image based on release scopes, and can include `@everyone` only when the workflow passes the flag [@discord-release].

The in-app What's New panel uses the embedded `CHANGELOG.md` as an immediate offline fallback, then can replace the current version's notes with the matching published GitHub release body when it is available [@whats-new]. It keeps contributor bullets in the full release notes but excludes them from the short highlight extraction, so first-time contributor credit remains visible without becoming a product-change highlight [@whats-new].

When release documentation mentions branch-local or sibling-worktree artifacts, follow the checked-repository link rule in [Release Campaign](../../reference/releases/release-campaign) rather than linking to sibling worktrees.
