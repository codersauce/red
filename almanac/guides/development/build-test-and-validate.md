---
title: "Build, Test, And Validate"
summary: "This guide explains the local validation path for Red changes and how to line it up with CI, plugin checks, release checks, and runtime self-checks."
topics: [guides, development, validation, testing, ci, plugins]
sources:
  - id: readme
    type: file
    path: README.md
  - id: agents
    type: file
    path: AGENTS.md
  - id: ci
    type: file
    path: .github/workflows/ci.yml
  - id: plugin-check
    type: file
    path: .github/workflows/plugin-check.yml
  - id: editor
    type: file
    path: src/editor.rs
  - id: terminal-cleanup-session
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/08/08/rollout-2026-08-08T00-33-51-019fdf6f-2571-7d91-8f8d-8c7dc3fe8803.jsonl
---

Use this guide when a Red change needs local confidence before review, push, or release preparation. A complete validation pass starts with the smallest command that matches the changed area, then finishes with the repository policy command for Rust changes, the relevant plugin or runtime self-checks, and any workflow-specific checks that CI will enforce [@agents] [@ci] [@plugin-check]. CI is the full contract, and the local commands here help catch the same classes of failure before waiting for GitHub Actions.

## Start From The Changed Area

For ordinary Rust changes, run the normal test suite first:

```shell
cargo test --all-targets --all-features
```

CI runs that command with `--verbose` on Ubuntu, macOS, and Windows, then runs a second no-default-features test pass with `cargo test --all-targets --no-default-features --verbose` [@ci]. Run the no-default-features pass locally when the change touches feature-gated code, dependency declarations, CLI utility behavior, or anything that may compile differently without default features.

For Rust changes that may be pushed, the repository policy is stricter than a test-only pass:

```shell
cargo clippy --all-targets --all-features -- -D warnings
```

`AGENTS.md` requires this command before pushing Rust changes and requires every warning or error to be fixed [@agents]. The CI clippy job runs the same command across Ubuntu, macOS, and Windows [@ci].

## Match The Main CI Gates

When a change is broad, prepare for the main CI workflow rather than guessing at one command. The CI workflow runs on pushes and pull requests to `main` and `develop`, and it can also be started manually [@ci].

Run these local equivalents when the changed area warrants them:

```shell
cargo test --all-targets --all-features
cargo test --all-targets --no-default-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo run --locked -- --self-check
cargo run --locked --release --example husk_cursor_bench -- --assert
python3 scripts/readme_release.py --check
python3 -m unittest tests.test_discord_release
```

The workflow lint job validates GitHub Actions, checks the README release version, and runs Discord announcement tests [@ci]. The CI `fmt`, `self-check`, and `perf` jobs respectively run rustfmt, `cargo run --locked -- --self-check`, and the release-mode Husk cursor benchmark with `--assert` [@ci]. For the details of the CI surface, see [CI And Validation](../../reference/validation/ci-and-validation); for benchmark thresholds and workstation baselines, see [Performance Checks](../performance/performance-checks); for the runtime diagnostic itself, see [Self Check](../../reference/runtime/self-check).

## Handle Terminal Output Tests

Tests that assert exact terminal escape bytes are platform-sensitive. The
terminal cleanup regression writes `Editor::restore_terminal_output` into a byte
buffer and searches for the alternate-screen exit plus cursor-show sequence,
which matches the Unix ANSI writer path [@editor]. A Windows cleanup session
found that crossterm uses Windows console APIs for those commands, so byte
buffers do not exercise the same behavior there; guard byte-sequence assertions
to Unix or split them into platform-specific checks, then let the Windows CI job
execute the Windows backend [@terminal-cleanup-session] [@ci].

## Validate Bundled Husk And Plugin Changes

If the change touches `crates/husk*`, `plugins/`, `src/plugin/`, `src/assets.rs`, `src/config.rs`, `src/self_check.rs`, examples, or the plugin workflow itself, run the plugin-specific checks before relying on the main suite. The plugin workflow is path-filtered to those areas and initializes the bundled runtime after targeted plugin and Husk checks [@plugin-check].

The workflow runs these families of commands:

```shell
cargo test --all-features -p red husk
cargo test --all-features -p red plugin::runtime::tests::lsp_symbols_
cargo test --all-features -p red plugin::runtime::tests::git_
cargo test --all-features -p red plugin::runtime::tests::embedded_git_core_
cargo test --all-features -p red plugin::runtime::tests::neotree_
cargo test --all-features -p red plugin::runtime::tests::embedded_neotree_core_
cargo test --all-features -p husk -p husk-ast -p husk-diagnostics -p husk-lexer -p husk-package -p husk-parser -p husk-semantic -p husk-stdlib -p husk-types
cargo run --all-features -p husk-cli -- test --locked plugins/git_core
cargo run --all-features -p husk-cli -- test --locked plugins/neotree_core
python3 -m json.tool examples/example-plugin/package.json > /dev/null
cargo run --all-features -- --self-check
```

Run only the relevant subset while iterating, then run the whole workflow-equivalent set when a plugin or Husk change is ready for review. The final self-check matters because Red embeds the editor defaults, plugins, themes, and runtime assets into the executable [@readme]. The startup path behind that check is described in [Runtime Lifecycle](../../architecture/startup/runtime-lifecycle).

## Release-Adjacent Validation

Release preparation has extra gates even before the tag build. On `release/v*` pull requests, CI regenerates the release changelog with git-cliff and diffs it against the committed `CHANGELOG.md` section [@ci]. The workflow lint job also verifies README release references with `scripts/readme_release.py --check` [@ci].

If a change alters release text, workflows, installer commands, or announcement content, run the README version checker and Discord release announcement tests locally. The release-specific runbook is [Release Red](../releases/release-red), and installer verification is covered by [Release Installers](../installers/release-installers).

## When A Gate Fails

Treat the failing gate as the owner of the first investigation. A Clippy failure means the policy command has found a Rust warning that must be fixed before push [@agents]. A self-check failure points at bundled runtime, plugin activation, theme parsing, or asset resolution. A plugin workflow failure points at Husk runtime, bundled plugin initialization, or example metadata. A CI changelog failure means the generated release section and committed release section disagree [@ci].

Do not hide a failure by narrowing the local command unless the narrower command is used only to reproduce the first error faster. The successful end state is a changed area that passes its targeted command and the repository-wide gates that CI will apply.
