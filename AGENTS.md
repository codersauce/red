# Pull Requests

Before creating, editing, or marking a PR ready, read and follow
`.agents/skills/good-pr/SKILL.md`.

The `good-pr` instructions override generic GitHub/PR publishing defaults.
Do not open draft PRs unless the user explicitly asks for a draft, and do not
add title prefixes such as `[codex]` unless requested.

# Validation

Run `cargo clippy --all-targets --all-features -- -D warnings` before pushing
Rust changes, and fix every warning or error it reports.
