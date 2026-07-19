## Why

Prepare Red {{VERSION}} with a reviewed version bump and a changelog generated from Git history.

## What changed

- Updated the package, lockfile, and README release versions.
- Prepended the generated release section to `CHANGELOG.md`.

## How to test

1. Confirm the package and README versions are `{{VERSION}}` and match the
   requested release tag.
2. Review every generated changelog group and entry.
3. Confirm CI, including Clippy and the packaged runtime self-check, passes.

Merging this PR does not publish the release. After merge, create and push the annotated `v{{VERSION}}` tag to start the release workflow.
