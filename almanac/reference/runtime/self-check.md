---
title: "Self Check"
summary: "`red --self-check` is Red's hidden packaging diagnostic for bundled themes, default plugins, production plugin snapshots, and quarantine status."
topics: [reference, runtime-assets, plugins, cli, validation]
sources:
  - id: self-check
    type: file
    path: src/self_check.rs
  - id: main
    type: file
    path: src/main.rs
  - id: cli
    type: file
    path: src/cli.rs
  - id: tests
    type: file
    path: tests/self_check.rs
  - id: debugging
    type: file
    path: docs/DEBUGGING.md
---

# Self Check

`red --self-check` is a hidden non-interactive validation mode for Red's packaged runtime. It parses bundled themes, verifies that default configured plugins resolve to bundled plugin assets, builds production-equivalent plugin snapshots, activates every configured bundled plugin, reports each plugin status, and fails if any bundled plugin is quarantined [@self-check] [@main]. It is a release and debugging diagnostic for the embedded runtime, not a user plugin validator [@self-check] [@debugging].

## Command Boundary

The flag is present in the CLI as `--self-check` but marked hidden [@cli]. Main treats it as a utility mode: it runs before configuration checks that enter the TUI, prints `report.format()`, prints `red self-check ok`, and returns without opening buffers interactively [@main]. Utility argument validation rejects combining this flag with files to edit [@cli].

Because the flag is hidden, user-facing docs normally mention it as a debugging validation command rather than as an editor workflow [@debugging].

## Validation Steps

The self-check starts from `Config::from_user_toml_with_overrides("", &[])`, which means it uses embedded defaults without reading a user configuration file [@self-check]. It then requires at least one bundled theme and parses every bundled theme through the VS Code theme parser [@self-check].

For plugins, the check iterates over the default `config.plugins` map and requires each configured plugin path to resolve to a bundled plugin specifier [@self-check]. It loads the configured default theme, constructs an `Editor` with an 80 by 24 terminal size, an empty buffer, default LSP manager, and the parsed theme, then creates a plugin runtime with the default plugin permissions [@self-check]. Before activation, it calls `editor.refresh_plugin_snapshots(&mut runtime, true, true, true)` so plugins see the same host snapshots used by production runtime activation [@self-check].

After `registry.initialize(&mut runtime).await`, the check collects plugin statuses into a sorted map [@self-check]. If any status is `Quarantined`, the command fails with `one or more bundled plugins were quarantined`; otherwise the formatted report is printed [@self-check].

## Status Output

`SelfCheckReport::format` prints one line per plugin in sorted order, using `plugin <name>: <status>` [@self-check]. Status labels are `pending`, `active`, `active (reload rejected)`, `disabled`, and `quarantined` [@self-check]. Main appends a final success line, `red self-check ok`, only after the report was produced successfully [@main].

The integration test requires self-check output to contain no ANSI escape sequences under `NO_COLOR`, end with `red self-check ok`, list at least two plugin status lines, and report the bundled plugins `agent`, `barbecue`, `buffer_picker`, `cool_search`, `fidget`, `git`, `indent_guides`, `inlay_hints`, `lsp_symbols`, `neotree`, `project_search`, and `theme_browser` as active [@tests]. The same test fails if any reported plugin status is not active [@tests].

## Debugging Use

The debugging guide recommends `red --self-check` when a plugin or bundled runtime problem is suspected because it parses themes, resolves bundled assets, seeds production-equivalent snapshots, and activates every bundled plugin without entering the terminal [@debugging]. A single invalid plugin should be quarantined and reported with its stage rather than preventing unrelated plugins from activating [@debugging].

For related lookup material, see [Red Command](../cli/red-command) for the CLI boundary and [Runtime Assets](../../architecture/runtime/runtime-assets) for the user, `RED_RUNTIME`, and embedded asset resolution order.
