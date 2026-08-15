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
  - id: performance
    type: file
    path: .github/workflows/performance.yml
  - id: test-performance
    type: file
    path: .github/workflows/test-performance.yml
  - id: release
    type: file
    path: .github/workflows/release.yml
  - id: agents
    type: file
    path: AGENTS.md
---

Red's validation contract spans local commands and GitHub Actions workflows. CI
runs workflow lint, cross-platform tests, clippy with warnings denied,
formatting, the bundled runtime self-check, documentation checks, and
changelog checks [@ci]. Separate workflows validate bundled Husk plugins, run
a deterministic performance gate, test nightly Rust, benchmark test runners on
demand, build and smoke-test release archives, publish draft GitHub releases,
and update the Homebrew tap after a release is published [@plugin-check]
[@performance] [@nightly] [@test-performance] [@release]. For runtime context,
the bundled self-check is also part of the
[runtime assets](../../architecture/runtime/runtime-assets) area.

## Local Policy

`AGENTS.md` requires `cargo clippy --all-targets --all-features -- -D warnings` before pushing Rust changes and requires every warning or error to be fixed [@agents]. It also says PR work must follow `.agents/skills/good-pr/SKILL.md`, including repository-specific PR publishing defaults [@agents].

The local clippy command matches the CI clippy job. Pull requests run it on
Ubuntu; other CI events run it on Ubuntu, macOS, and Windows [@ci].

## Main CI Workflow

The main CI workflow runs on pushes and pull requests targeting `main` or
`develop`, and it can also be started manually with `workflow_dispatch` [@ci].
Its global environment enables colored Cargo output, `RUST_BACKTRACE=1`, and
line-table-only development and test debug information. Validation jobs share
toolchain-aware Rust caches by operating system; release builds use separate
target-specific caches [@ci].

| Job | Main checks |
| --- | --- |
| `workflow-lint` | Validates GitHub Actions workflows, checks the README release version, and runs Discord release announcement and test-tooling unit tests [@ci]. |
| `test` | Runs `cargo test --all-targets --all-features --verbose` with stable Rust and checksum-verified ripgrep on Ubuntu, macOS, and Windows [@ci]. |
| `clippy` | Denies every all-target, all-feature clippy warning; pull requests use Ubuntu, while push and manual runs cover Ubuntu, macOS, and Windows [@ci]. |
| `fmt` | Runs `cargo fmt --all -- --check` [@ci]. |
| `self-check` | Runs `cargo run --locked -- --self-check` to initialize and validate bundled runtime state [@ci]. |
| `changelog` | Validates `cliff.toml`, and on `release/v*` PRs regenerates and diffs release changelog content against `CHANGELOG.md` [@ci]. |
| `build` | Builds target-specific release binaries on non-pull-request events [@ci]. |
| `docs` | Builds workspace API documentation, discovers packages with runnable Rust examples before doctesting them without default features, and validates Markdown links [@ci]. |

The shared ripgrep action downloads pinned Linux and Windows ripgrep 15.2.0
release archives, verifies their published SHA-256 digests, and installs them
without refreshing Linux package indexes. macOS reuses an existing ripgrep or
installs it with Homebrew [@ci].

## Plugin Check Workflow

The Husk Plugin Check workflow is path-filtered to plugin, Husk, asset, configuration, self-check, and example changes [@plugin-check]. It runs on push and pull request events when those paths change [@plugin-check].

The `bundled-plugins` job runs Husk and plugin runtime test filters in one
root-package invocation, then runs
`cargo test --workspace --exclude red --all-features --tests` to cover every
Husk workspace crate. It executes locked Husk CLI tests for bundled `git_core`
and `neotree_core` plugins, validates example plugin metadata with
`python3 -m json.tool`, and initializes the bundled runtime with
`cargo run --all-features -- --self-check` [@plugin-check].

## Performance And Runner Benchmarks

The path-filtered Performance workflow runs the release-mode
`husk_cursor_bench` example with `--assert` as a deterministic performance
gate [@performance]. The separate manually dispatched Rust Test Performance
workflow compares `cargo test` and `cargo-nextest` on Linux, macOS, or Windows,
records Cargo build timings and JSON results, uploads optional nextest JUnit
reports, and supports explicit `sccache` and Linux `mold` experiments
[@test-performance]. Normal validation keeps `cargo test` as its default
because separate nextest processes are slower for Red's many short tests.

## Nightly Rust Workflow

The Nightly Rust workflow runs on a scheduled Monday cron and can be started
manually [@nightly]. It installs the nightly toolchain and checksum-verified
ripgrep on Ubuntu, restores a toolchain-aware Rust cache, and runs the same
all-target, all-feature test command used by the main CI test job [@nightly].

## Release Workflow

The Release workflow runs for `v*` tag pushes, manual dispatch with an existing
tag name, and published release events [@release]. For tag and manual runs, it
restores target-specific Rust caches and builds release binaries for Linux
x86_64, macOS Intel, macOS Apple Silicon, and Windows x86_64, with `RUSTFLAGS`
remapping the workspace path to `/red` [@release].

The smoke job downloads each archive, extracts it, runs `--self-check`, requires the final line to be `red self-check ok`, fails if plugin health output reports pending, disabled, quarantined, reload-rejected, or error states, and checks that the binary does not contain the GitHub workspace path [@release]. The publish job downloads archives, adds installer scripts, verifies the package version and committed changelog, regenerates the changelog with git-cliff, generates SHA-256 sums, writes release notes, and creates or updates a draft release before uploading artifacts [@release].

When a GitHub release is published, the `homebrew` job requires `HOMEBREW_TAP_TOKEN`, downloads release assets and checksums, checks out `codersauce/homebrew-tap`, writes `Formula/red.rb`, and commits and pushes the formula if it changed [@release].
