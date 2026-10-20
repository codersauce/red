# Pull Requests

Before creating, editing, or marking a PR ready, read and follow
`.agents/skills/good-pr/SKILL.md`.

The `good-pr` instructions override generic GitHub/PR publishing defaults.
Do not open draft PRs unless the user explicitly asks for a draft, and do not
add title prefixes such as `[codex]` unless requested.

# New Features

Unless strictly asked, create new features on new worktrees using the worktrunk
tool with the wt command.

# Validation

Run `cargo clippy --all-targets --all-features -- -D warnings` before pushing
Rust changes, and fix every warning or error it reports.

# Plugin Host API Versions

Treat host API versions as released compatibility boundaries, not as a counter
for implementation phases. Before changing `RED_HOST_API_VERSION`, inspect the
latest released Red version and its host API. Keep one pending host API version
for the entire unreleased release train, and fold additive calls and fields into
that pending version and its existing change-manifest entry. Do not create a new
host API version for each feature, phase, commit, or pull request in the same
unreleased train.

After a host API version ships, preserve its contract. Open the next pending
version only when work begins for a later release train or when an independently
shipped compatibility boundary requires it. Keep plugin manifests, protocol
docs, the canonical host API schema, compatibility tests, and examples aligned
to the earliest released or pending version that contains their required
surface.
