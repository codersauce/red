---
title: "Release Red"
summary: "This guide describes the Red release flow from prepare-release workflow through tag publishing, archive smoke tests, Homebrew update, installer verification, and Discord announcement."
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
  - id: readme-release
    type: file
    path: scripts/readme_release.py
  - id: discord-release
    type: file
    path: scripts/discord_release.py
  - id: release-cliff
    type: file
    path: cliff.release.toml
  - id: whats-new
    type: file
    path: src/whats_new.rs
---

Use this guide to publish a Red release without mixing up release preparation, the tag build, draft release review, Homebrew publication, installer smoke tests, and announcement. The release process is split on purpose: a prepare workflow opens a normal release PR, an annotated tag builds and smoke-tests archives into a draft GitHub release, publishing that release updates Homebrew, and a separate Discord workflow announces only published non-prerelease releases unless manually invoked [@prepare-release] [@release] [@announce-discord].

## Prerequisites

Start only when CI is green on `main` and the release secrets are available. The release documentation requires `codersauce/homebrew-tap` with a `Formula/` directory, `HOMEBREW_TAP_TOKEN` that can push to that tap, and a `RELEASE_PR_TOKEN` with repository-scoped Contents and Pull requests read/write permissions [@releasing]. The release workflow uses the repository `GITHUB_TOKEN` to create a draft release, and `release.yml` grants `contents: write` only to the publishing job [@releasing] [@release].

Choose the next SemVer version without a leading `v`. The prepare workflow validates that the input matches SemVer, that `RELEASE_PR_TOKEN` is present, that the tag does not already exist, and that the requested version is newer than the current package version [@prepare-release].

## Prepare The Release PR

Run the prepare workflow from `main`:

```shell
gh workflow run prepare-release.yml --ref main -f version=0.2.0
```

The workflow checks out `main`, generates release notes from Conventional Commit subjects with git-cliff, updates `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, and README release references, then creates or updates `release/v<version>` [@prepare-release]. The README update is performed by `scripts/readme_release.py --set`, and the workflow immediately runs `scripts/readme_release.py --check` to verify the marker, release link, and installer pin match `Cargo.toml` [@prepare-release] [@readme-release].

Review the release PR as a normal ready-for-review PR. The release docs call out the generated changelog, package version, README release link, pinned installer example, Clippy, bundled-runtime self-check, README version check, and generated-versus-committed changelog comparison as required gates [@releasing]. For local equivalents, use [Build, Test, And Validate](../development/build-test-and-validate) and [CI And Validation](../../reference/validation/ci-and-validation).

## Tag The Merge Commit

After merging the release PR, update local `main`, verify the merge commit, and then create an annotated tag from that commit. Before tagging, confirm that `main` contains the release merge commit and that the post-merge CI surface has completed for that exact commit. The main-branch CI run covers workflow lint, README release-version validation, Discord announcement tests, cross-platform tests, Clippy, formatting, bundled runtime self-check, changelog checks, performance, release binary builds, documentation, and security audit jobs; the separate Husk plugin check covers Red and Husk package tests plus bundled plugin tests [@ci] [@plugin-check]. This matters because the tag push immediately starts archive creation, and the draft release should be built from the same commit whose merge validation passed [@release].

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

The publish job verifies package version and committed release changelog, regenerates release changelog content, copies `install/install.sh` and `install/install.ps1` into the release assets, generates `SHA256SUMS.txt`, generates public GitHub release notes with `cliff.release.toml`, appends installation and checksum guidance, creates or updates a draft GitHub release, and uploads all assets [@release] [@release-cliff].

## Review And Publish The Draft

Before publishing, confirm the draft release contains all four archives, `SHA256SUMS.txt`, `install.sh`, and `install.ps1`, that install instructions match the release tag, and that change authors, pull request numbers, and first-time contributors are credited correctly [@releasing]. Do not publish if the package version, changelog section, checksums, assets, or public notes do not match the intended tag. If a tag was pushed with the wrong version, the release docs require deleting the draft release and tag, fixing the version, and pushing a new tag before anything is published [@releasing].

Publishing the GitHub release triggers a second `Release` workflow run for the `release.published` event; its build, smoke, and draft-publish jobs are skipped, and only the `homebrew` job is eligible [@release]. That job requires `HOMEBREW_TAP_TOKEN`, downloads release tarballs and checksums, writes `Formula/red.rb` with OS-specific URLs and SHA-256 values, and pushes the formula update if it changed [@release]. Use `gh run list --event release` when checking publication automation, because the release-event `Release` run and the tag-push `Release` run have the same workflow name [@release].

## Verify Installers And Announcement

After publishing, verify Homebrew and the stable installers. The release docs require `brew update`, `brew install codersauce/tap/red`, `red --version`, and temporary-directory installer checks for Unix and Windows [@releasing]. Follow [Release Installers](../installers/release-installers) for installer-specific checks.

The Discord announcement workflow runs on published releases and manual dispatch, but it skips prereleases for automatic release events [@announce-discord]. It reads the published GitHub release, runs `scripts/discord_release.py` to build JSON and a Markdown summary, checks the webhook only when not in dry-run mode, and posts through `curl` with retry options [@announce-discord]. The helper selects announcement sections from "Features", "Performance", and "Bug Fixes", stops before installation boilerplate, builds a compact embed, chooses an image based on release scopes, and can include `@everyone` only when the workflow passes the flag [@discord-release].

The in-app What's New panel uses the embedded `CHANGELOG.md` as an immediate offline fallback, then can replace the current version's notes with the matching published GitHub release body when it is available [@whats-new]. It keeps contributor bullets in the full release notes but excludes them from the short highlight extraction, so first-time contributor credit remains visible without becoming a product-change highlight [@whats-new].
