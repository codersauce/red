# Releasing Red

This document describes how to publish a Red release.

## Prerequisites

- CI is green on `master`.
- `codersauce/homebrew-tap` exists and contains a `Formula/` directory.
- The `codersauce/red` repository has an Actions secret named `HOMEBREW_TAP_TOKEN`.
- `HOMEBREW_TAP_TOKEN` can push to `codersauce/homebrew-tap`.

The release workflow uses the repository `GITHUB_TOKEN` to create a draft
release, so `.github/workflows/release.yml` grants `contents: write` only to
the release publishing job.

## Release Process

1. Choose the next semantic version, for example `0.1.0`.
2. Update `Cargo.toml`:

   ```toml
   version = "0.1.0"
   ```

3. Refresh the lockfile if needed:

   ```shell
   cargo check
   ```

4. Commit the version bump:

   ```shell
   git add Cargo.toml Cargo.lock
   git commit -m 'chore: bump version to 0.1.0'
   git push origin master
   ```

5. Create and push an annotated tag:

   ```shell
   git tag -a v0.1.0 -m 'Release v0.1.0'
   git push origin v0.1.0
   ```

6. Watch the `Release` workflow in GitHub Actions.
7. Review the draft GitHub release:
   - all four archives are attached
   - `SHA256SUMS.txt` is attached
   - install instructions match the release tag
8. Publish the draft release.
9. Watch the `Release` workflow run triggered by the `release.published`
   event. This updates `Formula/red.rb` in `codersauce/homebrew-tap`.
10. Verify Homebrew:

   ```shell
   brew update
   brew install codersauce/tap/red
   red --version
   ```

## What the Workflow Builds

The release workflow builds:

| Target | Archive |
| --- | --- |
| `x86_64-unknown-linux-gnu` | `red-x86_64-unknown-linux-gnu.tar.gz` |
| `x86_64-apple-darwin` | `red-x86_64-apple-darwin.tar.gz` |
| `aarch64-apple-darwin` | `red-aarch64-apple-darwin.tar.gz` |
| `x86_64-pc-windows-msvc` | `red-x86_64-pc-windows-msvc.zip` |

Each archive contains the binary, `README.md`, `LICENSE`, and
`default_config.toml`. Runtime plugins, themes, and default config are also
embedded in the binary.

## Release Candidates

Tags containing `alpha`, `beta`, or `rc` are marked as prereleases:

```shell
git tag -a v0.2.0-rc1 -m 'Release v0.2.0-rc1'
git push origin v0.2.0-rc1
```

## Troubleshooting

- If release creation fails, confirm the workflow job has `contents: write`.
- If the Homebrew job fails at checkout, confirm `codersauce/homebrew-tap`
  exists and `HOMEBREW_TAP_TOKEN` can push to it.
- If the Homebrew formula is not updated, confirm the GitHub release was
  published. Draft releases intentionally do not update the tap.
- If `brew install` fails checksum validation, compare the formula values with
  the release `SHA256SUMS.txt`.
- If a tag was pushed with the wrong version, delete the draft release and tag,
  fix the version, and push a new tag before publishing anything.
