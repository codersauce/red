---
title: "Husk Scripts And Modules"
summary: "Husk chose an explicit `main` entrypoint, local package manifests, deterministic module resolution, and lock-file-backed extension inputs for standalone execution."
topics: [decisions, husk, cli, packages]
sources:
  - id: adr-0007
    type: file
    path: docs/adr/0007-husk-scripts-and-modules.md
  - id: cli
    type: file
    path: crates/husk-cli/src/lib.rs
  - id: runtime
    type: file
    path: crates/husk-runtime/src/lib.rs
  - id: package
    type: file
    path: crates/husk-package/src/lib.rs
---

Husk's accepted standalone execution decision is to run scripts through an explicit `main` entrypoint and to resolve multi-file packages from local, deterministic package inputs [@adr-0007]. The CLI implements `husk run <file.hk> [-- <args>...]`, strips only a first-line shebang while preserving byte locations, checks the compiled program's `main` signature, passes arguments only to `fn main(args: [String])`, and maps `()`, `i32`, and `Result<(), E>` returns to process status or failure [@cli] [@runtime]. Package resolution is filesystem-only: `Husk.toml` names the package entry, modules come from explicit `mod` declarations, local paths are canonicalized under the source root, ambiguous module layouts and cycles are errors, and `Husk.lock` records extension provenance [@package].

## Status

This decision is accepted by ADR 0007, dated 2026-07-19 [@adr-0007]. The CLI, runtime entrypoint model, and package resolver already implement the initial standalone contract [@cli] [@runtime] [@package].

## Context

Standalone Husk needed a first execution shape that would not blur scripts, packages, and future top-level statements. ADR 0007 defers top-level executable statements until the parser can lower them to a synthetic `main` without corrupting spans [@adr-0007]. It also keeps remote dependency resolution out of version 1, so local scripts and packages can be reproducible without a registry trust model [@adr-0007].

The current command surface matches that constraint. The `Run` command accepts a script or package path and treats values after clap's trailing argument boundary as script arguments [@cli]. The same CLI also exposes `check`, `test`, `repl`, `lsp`, package creation, package installation, and extension inspection, but `run` remains the entrypoint that executes a compiled `main` [@cli].

## Decision

Execution requires `main`. The runtime exposes `MainArguments`, `MainResult`, and `MainSignature` as the checked standalone entrypoint contract [@runtime]. `run_compiled` rejects scripts without `main`, rejects extra command-line arguments when `main` has no parameter, wraps argument strings in a Husk list for argument-taking mains, validates numeric exit codes into the `0..=255` process range, treats `Ok(())` as success, and renders `Err` as a runtime failure [@cli].

Single-file input and package input use different compilation paths. `compile_path` reads one UTF-8 file, enforces the CLI source-size limit, replaces a first-line shebang with spaces to preserve locations, and compiles the source through the engine [@cli]. Package input uses `ResolvedPackage::open` and `Engine::compile_package`, which connects this decision to [packages and locks](../../architecture/husk/packages-and-locks) [@cli] [@package].

Packages resolve explicit modules under a source root. `PackageManifest` requires a package name, version, and entry path; manifest and extension paths must be normalized relative paths [@package]. The module resolver canonicalizes source files under the package source root, rejects symlinks, rejects duplicate canonical files, reports cycles with the active module chain, and resolves each `mod child;` to exactly one of `child.hk` or `child/mod.hk` [@package].

## Consequences

The [Husk command](../../reference/cli/husk-command) has unambiguous script behavior. There is one executable function, argument passing is explicit, and process status is derived from the declared `main` return shape rather than from arbitrary top-level statements [@cli] [@runtime].

Module loading cannot escape the selected source root through aliases, absolute paths, parent traversal, or symlinked files. That makes package compilation deterministic and gives diagnostics stable display paths relative to the source root [@package].

Version 1 package reproducibility does not require a network registry. `Husk.lock` records schema version, package identity, extension module, version, source path, digest, and optional crate provenance; lock validation rejects manifest and lock mismatches before resolving crate extensions [@package]. Remote dependency resolution and top-level executable statements remain future features that need their own design work [@adr-0007].
