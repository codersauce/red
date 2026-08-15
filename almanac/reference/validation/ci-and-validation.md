---
title: "CI And Validation"
summary: "Red's validation surface combines GitHub Actions jobs for tests, clippy, formatting, performance, runtime self-check, plugin checks, nightly Rust, and release archives with the local clippy policy in AGENTS.md."
topics: [reference, validation, ci, release]
sources:
  - id: ci
    type: file
    path: .github/workflows/ci.yml
  - id: plugin-check
    type: file
    path: .github/workflows/plugin-check.yml
  - id: nightly
    type: file
    path: .github/workflows/nightly.yml
  - id: release
    type: file
    path: .github/workflows/release.yml
  - id: agents
    type: file
    path: AGENTS.md
---

Red's validation contract spans local commands and GitHub Actions workflows. CI runs workflow lint, cross-platform tests, clippy with warnings denied, formatting, a deterministic performance gate, the bundled runtime self-check, and changelog checks [@ci]. Separate workflows validate bundled Husk plugins, run the suite on nightly Rust, build and smoke-test release archives, publish draft GitHub releases, and update the Homebrew tap after a release is published [@plugin-check] [@nightly] [@release]. For runtime context, the bundled self-check is also part of the [runtime assets](../../architecture/runtime/runtime-assets) area.

## Local Policy

`AGENTS.md` requires `cargo clippy --all-targets --all-features -- -D warnings` before pushing Rust changes and requires every warning or error to be fixed [@agents]. It also says PR work must follow `.agents/skills/good-pr/SKILL.md`, including repository-specific PR publishing defaults [@agents].

The local clippy command matches the CI clippy job. The workflow installs the stable Rust toolchain with clippy on Ubuntu, macOS, and Windows, then runs `cargo clippy --all-targets --all-features -- -D warnings` [@ci].

## Main CI Workflow

The main CI workflow runs on pushes and pull requests targeting `main` or `develop`, and it can also be started manually with `workflow_dispatch` [@ci]. Its global environment enables colored Cargo output and `RUST_BACKTRACE=1` [@ci].

| Job | Main checks |
| --- | --- |
| `workflow-lint` | Checks out the repository, validates GitHub Actions workflows with actionlint, verifies the README release version, and runs Discord release announcement tests [@ci]. |
| `test` | Runs on Ubuntu, macOS, and Windows for stable Rust; installs ripgrep per OS, caches Cargo data, and runs all-target all-feature tests with verbose Cargo output [@ci]. |
| `clippy` | Runs clippy on Ubuntu, macOS, and Windows with all targets and all features, denying every warning [@ci]. |
| `perf` | Runs the release-mode `husk_cursor_bench` example with `--assert` as a deterministic performance gate [@ci]. |
| `fmt` | Runs `cargo fmt --all -- --check` [@ci]. |
| `self-check` | Runs `cargo run --locked -- --self-check` to initialize and validate bundled runtime state [@ci]. |
| `changelog` | Validates `cliff.toml`, and on `release/v*` PRs regenerates and diffs release changelog content against `CHANGELOG.md` [@ci]. |

The Windows ripgrep install path in the `test` job downloads the official `ripgrep-15.2.0-x86_64-pc-windows-msvc.zip` release archive, verifies SHA-256 `71b2fef860abe467217a538ff31de02f5258807c0129f771846f87bd029aafc5`, extracts it under `RUNNER_TEMP`, adds the extracted directory to `GITHUB_PATH`, and runs `rg.exe --version` [@ci]. This avoids depending on Chocolatey availability for Windows tests that exercise project search and other `rg`-backed paths [@ci].

## Plugin Check Workflow

The Husk Plugin Check workflow is path-filtered to plugin, Husk, asset, configuration, self-check, and example changes [@plugin-check]. It runs on push and pull request events when those paths change [@plugin-check].

The `bundled-plugins` job runs targeted Rust tests for Husk and plugin runtime areas, runs all-feature tests across Husk crates, executes locked Husk CLI tests for bundled `git_core` and `neotree_core` plugins, validates example plugin metadata with `python3 -m json.tool`, and initializes the bundled runtime with `cargo run --all-features -- --self-check` [@plugin-check].

## Nightly Rust Workflow

The Nightly Rust workflow runs on a scheduled Monday cron and can be started manually [@nightly]. It installs the nightly toolchain on Ubuntu, installs ripgrep, caches Cargo data, and runs the all-target all-feature test command with verbose Cargo output [@nightly].

## Release Workflow

The Release workflow runs for `v*` tag pushes, manual dispatch with an existing tag name, and published release events [@release]. For tag and manual runs, it builds release binaries for Linux x86_64, macOS Intel, macOS Apple Silicon, and Windows x86_64, with `RUSTFLAGS` remapping the workspace path to `/red` [@release].

The smoke job downloads each archive, extracts it, runs `--self-check`, requires the final line to be `red self-check ok`, fails if plugin health output reports pending, disabled, quarantined, reload-rejected, or error states, and checks that the binary does not contain the GitHub workspace path [@release]. The publish job downloads archives, adds installer scripts, verifies the package version and committed changelog, regenerates the changelog with git-cliff, generates SHA-256 sums, writes release notes, and creates or updates a draft release before uploading artifacts [@release].

When a GitHub release is published, the `homebrew` job requires `HOMEBREW_TAP_TOKEN`, downloads release assets and checksums, checks out `codersauce/homebrew-tap`, writes `Formula/red.rb`, and commits and pushes the formula if it changed [@release].
