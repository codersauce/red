# Releasing Red

This document describes how to publish a Red release.

## Prerequisites

- CI is green on `main`.
- `codersauce/homebrew-tap` exists and contains a `Formula/` directory.
- The `codersauce/red` repository has an Actions secret named `HOMEBREW_TAP_TOKEN`.
- `HOMEBREW_TAP_TOKEN` can push to `codersauce/homebrew-tap`.
- The repository has a `RELEASE_PR_TOKEN` fine-grained token with repository-scoped
  Contents and Pull requests read/write permissions. This lets the release-preparation
  pull request trigger the normal CI gates.

The release workflow uses the repository `GITHUB_TOKEN` to create a draft
release, so `.github/workflows/release.yml` grants `contents: write` only to
the release publishing job.

`cliff.toml` generates the versioned `CHANGELOG.md` embedded in Red as an
offline fallback. `cliff.release.toml` separately generates the complete public GitHub release
changelog, including pull request authors and first-time contributors.

`release/campaign.toml` is the reviewed editorial source of truth for the GitHub
release introduction, Discord highlights, in-app release highlights, and X and
Bluesky previews. Keep its version as `next` while preparing an upcoming release;
the release-preparation workflow resolves it to the selected exact version.
Campaign stories must distinguish genuinely new features from improvements to
existing capabilities, and their order determines how users encounter them.

Preview the current campaign without choosing a release version:

```shell
python3 scripts/release_campaign.py validate
python3 scripts/release_campaign.py render --channel github
python3 scripts/release_campaign.py render --channel discord
python3 scripts/release_campaign.py render --channel x
python3 scripts/release_campaign.py render --channel bluesky
```

X and Bluesky are preview-only in release workflows: no credentials are
required, no network request is made, and nothing is published to either
service. `scripts/social_release.py` validates the rendered previews without
publishing. Its explicit `--publish` option is reserved for a separate,
deliberate invocation after the account credentials and final copy have been
reviewed; release workflows never pass that flag. Discord remains the only
automatically posted announcement.

## Release Process

1. Choose the next semantic version, for example `0.2.0`.
2. Run the **Prepare Release** workflow with the version and no `v` prefix:

   ```shell
   gh workflow run prepare-release.yml --ref main -f version=0.2.0
   ```

   The workflow generates the release changelog from Conventional Commit
   subjects, updates `Cargo.toml`, `Cargo.lock`, the release references in
   `README.md`, and the version in `release/campaign.toml`, then opens or updates
   a ready-for-review `release/v0.2.0` pull request.

3. Review the generated changelog, package version, README release link, pinned
   installer example, and release campaign. Confirm each flagship story is
   evidence-backed, accurately labeled as new or improved, and references real
   keyboard shortcuts, feature prerequisites, and supported platforms.

   In particular, full Agent edits are mediated by the editor and saved to disk.
   Exact foreground inline edits apply unsaved by default and remain undoable;
   wider or background inline changes require explicit review. Normal-mode inline
   assistance targets the enclosing function when available, while a visual
   selection preserves its exact boundaries. Detachable sessions require macOS
   or Linux, and Agent support requires an installed and authenticated Codex CLI.

   Preview every destination against the exact release version:

   ```shell
   python3 scripts/release_campaign.py validate --version 0.2.0
   python3 scripts/release_campaign.py render --all \
     --version 0.2.0 --output-dir /private/tmp/red-release-preview
   ```

   Promote or remove any "Coming in the next release" wording from the README,
   documentation, and separate website before publication. Review screenshots or
   recordings, verify that website copy matches the shipping Agent and inline
   safety behavior, and coordinate the website release update. Wait for every
   release-PR gate to pass. In particular, Clippy, the
   bundled-runtime self-check, README and campaign version checks, and the
   generated-versus-committed changelog comparison must be green.
4. Merge the release pull request and update the local `main` branch:

   ```shell
   git checkout main
   git pull --ff-only origin main
   ```

5. Create and push an annotated tag from the release merge commit:

   ```shell
   git tag -a v0.2.0 -m 'Release v0.2.0'
   git push origin v0.2.0
   ```

6. Watch the **Release** workflow in GitHub Actions. It verifies the package version
   and matching `CHANGELOG.md` section, builds all four archives, and runs the
   extracted editor's embedded-runtime self-check on each target platform. It also
   generates GitHub release notes with the reviewed campaign highlights followed
   by the complete grouped changelog, pull request authors, and a contributors
   section that identifies first-time contributors.
7. Review the draft GitHub release and confirm:
   - all four archives are attached
   - `SHA256SUMS.txt` is attached
   - `install.sh` and `install.ps1` are attached
   - install instructions match the release tag
   - the curated flagship stories lead the complete changelog
   - change authors, pull requests, and first-time contributors are credited correctly
8. Publish the draft release only after the announcement copy and website plan
   have been reviewed.
9. Watch the **Release** workflow run triggered by the `release.published`
   event. This updates `Formula/red.rb` in `codersauce/homebrew-tap`. The
   **Announce Discord release** workflow posts the reviewed Discord announcement
   and adds X and Bluesky previews to its job summary without posting them.
10. Verify Homebrew:

   ```shell
   brew update
   brew install codersauce/tap/red
   red --version
   ```

11. Verify the stable installers against the published release in temporary
    directories:

    ```shell
    RED_VERSION=0.2.0 RED_INSTALL_DIR="$(mktemp -d)/bin" \
      sh install/install.sh
    ```

    ```powershell
    ./install/install.ps1 -Version 0.2.0 `
      -InstallDir (Join-Path $env:TEMP "red-release-check") -NoModifyPath
    ```

    Both commands must print the expected version and end their self-check with
    `red self-check ok`.

## What the Workflow Builds

The release workflow builds:

| Target | Archive |
| --- | --- |
| `x86_64-unknown-linux-gnu` | `red-x86_64-unknown-linux-gnu.tar.gz` |
| `x86_64-apple-darwin` | `red-x86_64-apple-darwin.tar.gz` |
| `aarch64-apple-darwin` | `red-aarch64-apple-darwin.tar.gz` |
| `x86_64-pc-windows-msvc` | `red-x86_64-pc-windows-msvc.zip` |

Each archive contains the `red` binary, `README.md`, `LICENSE`, and
`default_config.toml`. Runtime plugins, themes, and default config are embedded
in `red`. Agent support requires a separately installed Codex CLI version
0.144.1 or newer and a completed `codex login`.

## Release Candidates

Prepare and review a release-candidate pull request with the full prerelease version,
then tag its merge commit. Tags containing `alpha`, `beta`, or `rc` are marked as
prereleases:

```shell
git tag -a v0.2.0-rc1 -m 'Release v0.2.0-rc1'
git push origin v0.2.0-rc1
```

## Troubleshooting

- If release creation fails, confirm the workflow job has `contents: write`.
- If release preparation cannot open a pull request or its checks do not start,
  confirm `RELEASE_PR_TOKEN` is present and has Contents and Pull requests read/write
  permissions.
- If the release workflow rejects the tag before packaging, confirm the merged
  `Cargo.toml`/`Cargo.lock` version, `CHANGELOG.md` section, and
  `release/campaign.toml` version match the tag exactly.
- If an older release announcement has no matching reviewed campaign, the Discord
  workflow safely falls back to its existing changelog-based highlights and skips
  X and Bluesky previews.
- If the Homebrew job fails at checkout, confirm `codersauce/homebrew-tap`
  exists and `HOMEBREW_TAP_TOKEN` can push to it.
- If the Homebrew formula is not updated, confirm the GitHub release was
  published. Draft releases intentionally do not update the tap.
- If `brew install` fails checksum validation, compare the formula values with
  the release `SHA256SUMS.txt`.
- If a tag was pushed with the wrong version, delete the draft release and tag,
  fix the version, and push a new tag before publishing anything.
