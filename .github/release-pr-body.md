## Why

Prepare Red {{VERSION}} with a reviewed version bump, an evidence-backed release
campaign, and a changelog generated from Git history.

## What changed

- Updated the package, lockfile, and README release versions.
- Prepended the generated release section to `CHANGELOG.md`.
- Resolved the reviewed release campaign to the exact release version.

## How to test

1. Confirm the package, README, and release campaign versions are `{{VERSION}}`.
2. Review every generated changelog group and entry.
3. Confirm the flagship stories distinguish new capabilities from existing improvements.
4. Verify shortcuts, supported platforms, prerequisites, safety claims, and linked PRs.
5. Preview the GitHub, Discord, X, and Bluesky campaign copy; social previews are not posted.
6. Promote or remove upcoming-release wording in the README and website copy.
7. Check demo media and coordinate the separate website release update.
8. Confirm CI, including Clippy and the packaged runtime self-check, passes.

Merging this PR does not publish the release. After merge, create and push the
annotated `v{{VERSION}}` tag to start the release workflow.
