---
title: "Command Discovery"
summary: "Red discovers commands through colon parsing, effective keymaps, command-palette metadata, and registered plugin command records."
topics: [architecture, commands, cli, plugins]
sources:
  - id: command-parser
    type: file
    path: src/command.rs
  - id: command-palette
    type: file
    path: src/command_palette.rs
  - id: editor-dispatch
    type: file
    path: src/editor.rs
  - id: default-config
    type: file
    path: default_config.toml
  - id: plugin-runtime
    type: file
    path: src/plugin/runtime.rs
---

# Command Discovery

Command discovery is the set of paths that let Red users find and execute editor actions: colon commands, configured keymaps, the command palette, and plugin-registered commands. Built-in colon input is parsed against an explicit command-name list, palette rows are built from built-in command metadata plus active plugin command metadata, and shortcut labels come from the effective configured keymaps rather than hard-coded defaults [@command-parser] [@command-palette]. The editor then dispatches selected built-in actions itself and routes plugin commands through the plugin registry [@editor-dispatch].

## Colon Command Parsing

The low-level parser in `src/command.rs` receives the authoritative built-in command list from its caller. It parses a trailing `!` as `CommandFlag::Force`, splits arguments on spaces without shell quoting, resolves exact command names, resolves conventional initial-based abbreviations such as `bn` for `buffer-next`, and supports chains such as `wq` by mapping each character to the first matching command [@command-parser]. If any command in a chain is unknown, parsing returns `None` for the whole input so a partially recognized chain cannot execute unintended commands [@command-parser].

The editor adds command-specific handling around that parser. It special-cases register, join, and syntax commands before calling the built-in parser, then maps parsed command names to semantic `Action` values such as save, quit, buffer movement, deletion, edit/reload, split, wrap, syntax, and config diagnostics [@editor-dispatch]. If built-in parsing fails but the runtime has a registered plugin command with the exact input name, the editor returns `Action::PluginCommand`; otherwise it records an unknown-command error [@editor-dispatch].

## Completion Names

Colon completion uses the palette module's command-name inventory rather than the parser alone. `colon_completion_names` starts with built-in colon commands, adds special built-ins such as `commands`, `command-palette`, debug commands, registers, undotree, `j`, and `join`, and then appends plugin command names that do not collide with built-in colon names [@command-palette]. The editor uses that list for command-line completion when the current command fragment has no whitespace and is not in a file or syntax completion context [@editor-dispatch].

Built-in precedence is intentional. `colon_name_is_builtin` treats special names and anything the built-in parser can resolve as reserved, so plugin commands with those names can still be active internally but do not create alternate colon command entries in discovery surfaces [@command-palette]. This matches the plugin API boundary described in [Red Host API](../plugins/red-host-api).

## Palette Entries

The command palette is opened by `Action::CommandPalette`. The editor asks the plugin runtime for `registered_commands`, passes those commands plus `self.config.keys` to `command_palette::entries`, converts entries into structured picker rows, and builds a picker titled `Commands` with the placeholder `Type a command, keymap, or :command` [@editor-dispatch]. Choosing a row returns the entry's stored `Action`, so built-ins and plugin commands share the same picker selection path after discovery [@editor-dispatch].

Built-in palette entries are declared with stable ids, titles, categories, descriptions, optional colon forms, aliases, and semantic actions [@command-palette]. For plugin commands, the palette creates `Action::PluginCommand(name)`, derives shortcuts from effective keymaps, uses plugin-provided title/category/description/aliases when present, humanizes the command name when the title is absent, and gives non-colliding plugin commands a `:<name>` colon display [@command-palette]. Entries are sorted by category, title, and id for stable presentation [@command-palette].

## Effective Keymaps And Hints

Shortcut discovery reads the current `Keys` configuration, so user overrides and embedded defaults affect palette output. `shortcuts_for_action` walks configured keymaps to find bindings for each entry's action, and `picker_items` displays the primary shortcut beside the colon form when one exists [@command-palette]. The embedded default config maps normal-mode command-palette entrypoints to `Ctrl-Shift-p`, `Alt-x`, `F1`, and leader-space `?`, and it maps many bundled plugin commands under normal-mode, window, leader, and visual bindings [@default-config].

The same module produces delayed keymap hints for nested key prefixes. `keymap_hints` takes the active prefix and immediate mapping table, labels nested groups as more keymaps, labels leaf actions with human-readable action names, and sorts groups and keys deterministically [@command-palette]. The editor maintains the active prefix, deadline, and visibility flags, shows hints after the configured delay, and clears them after a continuation or cancellation [@editor-dispatch]. The `key_hints` defaults in `default_config.toml` enable this guide with a 250 ms delay [@default-config].

## Plugin Command Metadata

Plugins register command discovery data through the runtime's `CommandMetadata`, which carries optional title, category, description, and alias fields [@plugin-runtime]. `Runtime::registered_commands` returns active command records with command name, owning plugin, callback, and metadata sorted by command name for stable discovery UI [@plugin-runtime]. Because palette entries retain the owning plugin in their ids, duplicate command names have deterministic runtime behavior before discovery surfaces display them [@plugin-runtime].

This command system does not make colon syntax a general shell. Arguments are split simply, command names are case-sensitive where plugin names are involved, and plugin command execution stays behind the plugin host boundary [@command-parser] [@editor-dispatch]. The CLI-level command surface is documented in [Red Command](../../reference/cli/red-command), and exact default keymaps and plugin command bindings belong in [Default Config](../../reference/configuration/default-config).
