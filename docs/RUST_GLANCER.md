# Rust Glancer

[Rust Glancer](https://github.com/rust-glancer/rust-glancer) is an experimental
Rust language server focused on low memory usage and reusing an index across
editor restarts. Red supports it as an opt-in alternative; rust-analyzer remains
the default.

## Install

Download the archive for your platform from the pinned
[v0.2.0 release](https://github.com/rust-glancer/rust-glancer/releases/tag/v0.2.0),
extract it, and put the `rust-glancer` executable on your `PATH`. For Apple
Silicon, use `rust-glancer-0.2.0-aarch64-apple-darwin.tar.gz`. An absolute executable
path in the configuration below also works; `~` is not expanded in `command`.

From the Rust project you want to edit, install the standard-library sources for
its active toolchain and verify the executable:

```sh
rustup component add rust-src
rust-glancer --version
```

See upstream's [installation guide](https://rust-glancer.github.io/docs/usage/INSTALL.html)
for other editors and distribution options.

## Configure Red

Copy the complete contents of
[`examples/rust-glancer.toml`](../examples/rust-glancer.toml) into
`~/.config/red/config.toml` (or `$XDG_CONFIG_HOME/red/config.toml` when set).
Replace any existing `[lsp.servers.rust]` section and its subtables rather than
appending a duplicate. Keep the rest of your configuration.

The example uses `command = "rust-glancer"` and `args = ["lsp"]`, which starts
the server over stdio. It keeps the server key and language ID as `rust`, so Rust
syntax highlighting and Cargo-workspace routing continue to work. Do not add a
second server with another name claiming the same `.rs` extension: Red selects
the first matching server by name.

This is a **user-file configuration**, not a command-only `red -c` override.
User-file server definitions replace the complete server entry; CLI overrides
merge into existing entries and can retain rust-analyzer's arguments or options.

Run `red --check-config`, then restart Red or run `:languages reload`. Open a Rust
file and try `K` for hover, `gd` for definition, and Space then `f` for formatting.
The configuration check validates TOML and runtime assets; the live editor check
also verifies that the executable and project can start successfully. Restart
Red after correcting a missing executable if its failed startup was cached.

The example explicitly enables `cargo check` diagnostics on save and leaves
startup diagnostics disabled. Glancer itself defaults both to disabled. Set
`onSave = false` to keep that upstream behavior. These options belong under
`initialization_options.diagnostics`, not `settings`, and take effect when the
server restarts. See upstream's
[configuration guide](https://rust-glancer.github.io/docs/usage/CONFIGURE.html).

Red's automatic rust-analyzer cache-priming and `.vscode/settings.json` rustfmt
settings apply only to recognized rust-analyzer launches, including
`rustup run <toolchain> rust-analyzer`. They are not added to Glancer or arbitrary
wrapper commands. Explicit server options are preserved.

## Switch back

Restore your previous complete Rust server section, or remove the Glancer section
and its `initialization_options` subtables to use Red's embedded rust-analyzer
defaults. Restart Red or run `:languages reload`; then verify hover or navigation
in a Rust file. Other language-server configurations are unaffected.

## Expectations

Glancer is not feature-equivalent to rust-analyzer. Its saved index means changes
to imports or other items may need a save before they are visible elsewhere.
Build-script analysis uses existing build artifacts; run `cargo check` or
`cargo build` when those outputs are needed. Initial indexing and warm restarts
have different performance characteristics, so measure both on your project.
See upstream's [limitations](https://rust-glancer.github.io/docs/usage/LIMITATIONS.html)
for details.
