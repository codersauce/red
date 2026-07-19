# ADR 0007: Husk scripts, entry points, and modules

- Status: accepted
- Date: 2026-07-19
- Scope: initial standalone CLI and local package resolution

## Decision

The first standalone command is `husk run <file.hk> [-- <args>...]`. It strips
a UTF-8 shebang only when that shebang is the first line and passes arguments
after `--` without reinterpretation.

Execution requires `main` in one of these forms:

- `fn main()`;
- `fn main(args: [String])`;
- either form returning `()`, `i32`, or `Result<(), E>`.

An `i32` becomes the validated process exit status. `Err` is rendered as a
runtime failure. Other signatures are compile errors. Top-level executable
statements are deferred until the parser can lower them to a synthetic `main`
without corrupting spans.

Multi-file packages use explicit `mod` and `use`. `mod util;` resolves exactly
one of `util.hk` and `util/mod.hk`; finding both is an error. Resolution
canonicalizes files under the package source root, rejects duplicate canonical
files, detects cycles with their chain, and enforces visibility.

`Husk.toml` is optional for a single file and required for a package. Version 1
resolves local paths only. `Husk.lock` records extension identity, version,
digest, and source. There is no registry or network resolver in version 1, and
the external root name `std` is reserved.

The complete contract is in the
[standalone scripts section](../HUSK_LANGUAGE_EXTRACTION_PLAN.md#standalone-scripts-and-packages).

## Consequences

- The first CLI milestone has an unambiguous entry point and exit behavior.
- Module loading cannot escape the selected package root through aliases or
  symlinks.
- Package reproducibility does not initially depend on a network service.
- Top-level statements and remote dependency resolution remain explicit future
  features.

## Revisit when

Revisit when top-level statements have a span-preserving parser design, a
registry has a signed and reproducible trust model, or real packages require a
module-layout extension that cannot be expressed by these two canonical forms.
