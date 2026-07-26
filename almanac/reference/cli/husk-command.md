---
title: "Husk Command"
summary: "The Husk command reference covers the standalone husk CLI and Red's forwarded red husk entrypoint."
topics: [husk, cli, reference]
sources:
  - id: cli-code
    type: file
    path: crates/husk-cli/src/lib.rs
  - id: cli-tests
    type: file
    path: crates/husk-cli/tests/cli.rs
  - id: red-main
    type: file
    path: src/main.rs
  - id: language-guide
    type: file
    path: docs/HUSK_LANGUAGE_GUIDE.md
---

The `husk` command compiles, checks, runs, tests, packages, extends, and serves the Husk language. Red exposes the same command surface as `red husk ...`: the Red process detects `husk` as its first argument and forwards the remaining arguments into `husk_cli::run_from` with the display name `red husk` [@red-main]. The standalone parser is named `husk` and defines the subcommands listed below [@cli-code].

## Entrypoints

| Entrypoint | Behavior |
| --- | --- |
| `husk ...` | Runs the standalone Husk CLI parser [@cli-code]. |
| `red husk ...` | Forwards to the same parser without starting the editor [@red-main]. |
| `husk lsp --stdio` | Runs the Husk language server over standard input and output [@cli-code]. |

The forwarding test asserts that only the `husk` subcommand is intercepted; a normal file argument such as `red file.txt` does not use the Husk CLI path [@red-main].

## Package Commands

| Command | Purpose |
| --- | --- |
| `husk new PATH [--name NAME]` | Creates a package directory or initializes `.` with `Husk.toml`, `Husk.lock`, `src/main.hk`, and a `.gitignore` that excludes `/.husk/` [@cli-code] [@cli-tests]. |
| `husk check [--extension BUNDLE] [--locked] PATH` | Parses and type-checks a script, package directory, or `Husk.toml`; package inputs refresh `Husk.lock` unless `--locked` is set [@cli-code]. |
| `husk run [--extension BUNDLE] [--locked] PATH [-- ARGS...]` | Compiles the input and calls `main`, passing `ARGS` only when `main` accepts `[String]` [@cli-code]. |
| `husk test [--extension BUNDLE] [--locked] [--include-ignored] [--list] PATH [FILTER]` | Runs functions marked `#[test]`; `FILTER` is a substring match on qualified test names, `--list` prints names, and `--include-ignored` runs ignored tests [@cli-code] [@cli-tests]. |
| `husk install [--package PATH] [--locked] [--offline]` | Installs the exact extension bundles recorded in `Husk.lock` into `.husk/extensions` [@cli-code]. |

For package inputs, a path that is a directory or named `Husk.toml` is resolved as a package; other paths are treated as single scripts, and `--locked` with a single script is rejected [@cli-code]. Unlocked package commands write `Husk.lock`; locked commands require an existing lock that matches the current manifest and resolved inputs [@cli-code]. The tests cover package check, run, locked mode, missing locks, changed locks, and path extension packages [@cli-tests]. The architecture behind those rules is in [Husk Packages And Locks](../../architecture/husk/packages-and-locks).

## Run Status And Main Contracts

`husk run` accepts `main` with no arguments or with one `[String]` argument list, and it supports unit, `i32`, or `Result<(), E>` result shapes [@cli-code]. Unit and `Ok(())` return success, an `i32` result becomes the process exit code after validation into `0..=255`, and `Err(value)` is rendered as a CLI failure [@cli-code]. The language guide lists the public status conventions: success is `0`, source or runtime failures use `1`, invalid command-line usage uses clap's usage exit, and `husk run` may return a validated script `i32` status [@language-guide].

Single-file script loading reads bounded UTF-8 and replaces a first-line shebang with spaces before compilation so diagnostic locations remain stable [@cli-code]. Tests cover unit exits, integer exits, argument passing, shebang handling, compiler diagnostics, runtime diagnostics, and `Result` propagation [@cli-tests].

## REPL And LSP

| Command | Purpose |
| --- | --- |
| `husk repl [--extension BUNDLE]...` | Starts an interactive session with optional pure portable bundles [@cli-code]. |
| `husk lsp --stdio` | Runs the first-party Husk language server; `--stdio` is currently required [@cli-code]. |

The REPL prints prompts only for an interactive terminal, supports `:help`, `:reset`, and `:quit`, preserves pending multiline input, reports incomplete input at EOF, and exits with failure in non-interactive mode when an error occurred [@cli-code]. The LSP command refuses to run without `--stdio` [@cli-code]. See [Husk Language Server](../../architecture/husk/language-server) for the editor architecture.

## Extension Commands

| Command | Purpose |
| --- | --- |
| `husk extension inspect BUNDLE` | Validates a bundle, compiles it for inspection, and prints package, module, world, digest, imports, and derived exports [@cli-code]. |
| `husk extension componentize --core-module PATH --output PATH` | Encodes a WIT-aware core Wasm module into a Component, verifies exports and imports, and writes a new output file [@cli-code]. |
| `husk extension pack --manifest PATH --component PATH --output PATH` | Assembles a directory bundle and validates it before reporting its digest [@cli-code]. |

The extension tests cover `pack`, `inspect`, and running a script against a packed bundle end to end, including the public export flattening rule for same-named component interfaces [@cli-tests]. Extension architecture and capability rules are covered in [Husk Extensions](../../architecture/husk/extensions).

## Crate Adapter Commands

| Command | Purpose |
| --- | --- |
| `husk add CRATE [OPTIONS]` | Generates, sandbox-builds, verifies, installs, vendors, and records a crate-backed extension adapter for a package [@cli-code]. |
| `husk crate inspect CRATE [--json] [OPTIONS]` | Resolves a crate request and reports whether adapter analysis can proceed [@cli-code]. |
| `husk crate interface CRATE --include PATH... [OPTIONS]` | Generates a reviewable WIT proposal for selected compatible public APIs [@cli-code]. |
| `husk crate adapter CRATE [--include PATH...] --output DIR [OPTIONS]` | Generates a deterministic Rust adapter crate without building it [@cli-code]. |
| `husk crate build-adapter DIR [--allow-network] [LIMITS]` | Builds a generated adapter inside the dedicated Cargo sandbox [@cli-code]. |

Crate requests accept `--version`, comma-delimited `--features`, repeatable `--specialize FUNCTION<T>`, `--no-default-features`, `--path`, and `--offline` [@cli-code]. Adapter build limits include timeout seconds, output byte limit, memory-byte budget, and process budget [@cli-code]. The language guide explains why this adapter workflow exists instead of loading Cargo crates directly at runtime [@language-guide].

## Related Reference

Use [Red Command](red-command) for the surrounding editor CLI and utility modes. Use [Husk Public Embedding API](../../architecture/husk/public-embedding-api) for the Rust embedding boundary that the CLI builds on.
