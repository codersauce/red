---
title: "Command Discovery"
summary: "Red discovers commands through colon parsing, effective keymaps, command-palette metadata, and registered plugin command records."
topics: [architecture, commands, cli, plugins]
sources:
  - id: command-parser
    type: file
    path: src/command.rs
  - id: command-completion
    type: file
    path: src/command_completion.rs
  - id: command-palette
    type: file
    path: src/command_palette.rs
  - id: editor-dispatch
    type: file
    path: src/editor.rs
  - id: buffer-do
    type: file
    path: src/editor/buffer_do.rs
  - id: default-config
    type: file
    path: default_config.toml
  - id: plugin-runtime
    type: file
    path: src/plugin/runtime.rs
  - id: plugin-api
    type: file
    path: src/plugin/api.rs
  - id: neotree-plugin
    type: file
    path: plugins/neotree.hk
---

# Command Discovery

Command discovery is the set of paths that let Red users find and execute editor actions: colon commands, configured keymaps, the command palette, and plugin-registered commands. Built-in colon input is parsed against an explicit command-name list, palette rows are built from built-in command metadata plus active plugin command metadata, and shortcut labels come from the effective configured keymaps rather than hard-coded defaults [@command-parser] [@command-palette]. The editor then dispatches selected built-in actions itself and routes plugin commands through the plugin registry [@editor-dispatch].

## Colon Command Parsing

The low-level parser in `src/command.rs` receives authoritative built-in command specifications, including each command's stable minimum abbreviation length. It parses a trailing `!` as `CommandFlag::Force`, splits arguments on spaces without shell quoting, prioritizes exact command names and existing initial-based aliases such as `bn` for `buffer-next`, explicitly recognizes the `wq` save-and-quit chain, and resolves unambiguous Vim-style prefixes such as `ene` for `enew` [@command-parser]. Ambiguous, too-short, and unknown names are rejected instead of being decomposed into unrelated commands, preventing accidental buffer deletion, reloads, or writes [@command-parser]. The command inventory has a regression test that rejects duplicate names and conflicting executable prefixes [@command-palette].

The editor adds command-specific handling around that parser. It special-cases register and join commands, uses parsed canonical names for syntax, set, and messages commands, and maps the remaining parsed command names to semantic `Action` values such as save, quit, buffer movement, deletion, edit/reload, split, and wrap [@editor-dispatch]. If built-in parsing fails but the runtime has a registered plugin command with the exact input name, the editor returns `Action::PluginCommand`; otherwise it records an unknown-command error [@editor-dispatch].

`:bufdo` parses its optional range as stable buffer ids rather than line numbers, snapshots matching open-buffer ids, and reparses its nested command after activating each target so line ranges such as `%` resolve against that buffer. Traversal stops on parse, substitution, write, or reload errors and rejects nested actions that could invalidate the snapshot or open interactive UI. The outer force flag is accepted for Vim compatibility; Red already preserves dirty buffers during ordinary buffer switches [@editor-dispatch] [@buffer-do].

`:wa[ll]` snapshots modified buffers, writes every named target through the ordinary save pipeline, and restores the active buffer and view. Clean buffers are skipped. Dirty unnamed buffers remain open and produce an error after the named targets have been processed [@editor-dispatch] [@buffer-do].

Advertising a built-in colon form is a parser and dispatch change, not only palette metadata. A built-in command that appears in docs or `CommandPaletteEntry::colon` must also be present in the built-in command list passed to `command::parse` and must map to an `Action` in `Editor::handle_command`; otherwise the palette can describe a `:<name>` form that direct colon input still reports as unknown [@command-palette] [@editor-dispatch].

## Completion Names

Colon completion uses the palette module's command-name inventory rather than the parser alone. `colon_completion_names` starts with built-in colon commands, adds special built-ins such as `commands`, `command-palette`, debug commands, registers, undotree, `j`, and `join`, and then appends plugin command names that do not collide with built-in colon names [@command-palette]. The editor uses that list for command-name completion. `src/command_completion.rs` selects argument providers after a space: fixed choices for commands such as `set`, `languages`, and `Copilot`, file paths for file commands, and language ids for syntax commands. After `bufdo`, completion restarts at the nested command, including optional stable-ID ranges, and excludes recursive `bufdo` completion. `Tab` cycles forward and `Shift-Tab` cycles backward through the original matches; typing resets the cycle. Positional choices replace only the current argument; file completion retains its existing whole-path replacement behavior. Completion never executes a command or plugin callback [@command-completion] [@editor-dispatch].

Built-in precedence is intentional. `colon_name_is_builtin` treats special names and anything the built-in parser can resolve as reserved, so plugin commands with those names can still be active internally but do not create alternate colon command entries in discovery surfaces [@command-palette]. This matches the plugin API boundary described in [Red Host API](../plugins/red-host-api).

## Palette Entries

The command palette is opened by `Action::CommandPalette`. The editor asks the plugin runtime for `registered_commands`, passes those commands plus `self.config.keys` to `command_palette::entries`, converts entries into structured picker rows, and builds a picker titled `Commands` with the placeholder `Type a command, keymap, or :command` [@editor-dispatch]. Choosing a row returns the entry's stored `Action`, so built-ins and plugin commands share the same picker selection path after discovery [@editor-dispatch].

Built-in palette entries are declared with stable ids, titles, categories, descriptions, optional colon forms, aliases, and semantic actions [@command-palette]. For plugin commands, the palette creates `Action::PluginCommand(name)`, derives shortcuts from effective keymaps, uses plugin-provided title/category/description/aliases when present, humanizes the command name when the title is absent, and gives non-colliding plugin commands a `:<name>` colon display [@command-palette]. Entries are sorted by category, title, and id for stable presentation [@command-palette].

## Effective Keymaps And Hints

Shortcut discovery reads the current `Keys` configuration, so user overrides and embedded defaults affect palette output. `shortcuts_for_action` walks configured keymaps to find bindings for each entry's action, and `picker_items` displays the primary shortcut beside the colon form when one exists [@command-palette]. The embedded default config maps normal-mode command-palette entrypoints to `Ctrl-Shift-p`, `Alt-x`, `F1`, and leader-space `?`, and it maps many bundled plugin commands under normal-mode, window, leader, and visual bindings [@default-config].

The same module produces delayed keymap hints for nested key prefixes. `keymap_hints` takes the active prefix and immediate mapping table, labels nested groups as more keymaps, labels leaf actions with human-readable action names, and sorts groups and keys deterministically [@command-palette]. The editor maintains the active prefix, deadline, and visibility flags, shows hints after the configured delay, and clears them after a continuation or cancellation [@editor-dispatch]. The `key_hints` defaults in `default_config.toml` enable this guide with a 250 ms delay [@default-config].

## Panel-Focused Key Dispatch

Configured normal-mode bindings are not automatically global when a plugin panel owns focus. After dialogs, workspace mode, command mode, and search mode have had a chance to handle input, `process_editor_event` gives focused panels their own event path and only falls back to `panel_global_key_action` for actions admitted by `action_runs_from_panel` [@editor-dispatch]. This lets panel-local keys such as row navigation, expansion, activation, toggles, and close stay local while selected editor-level commands still work from a focused panel [@editor-dispatch].

Panel-local handling comes first. Row panels prefer unmodified character keys other than `:` and `;`, so a user mapping such as normal-mode `x = FilePicker` does not steal a row-panel `x` action [@editor-dispatch]. The editor also reserves explicit panel chords such as row-panel `Ctrl-r` before trying the global normal-mode map, which lets Neo-tree clear its clipboard even if the user maps `Ctrl-r` globally [@editor-dispatch] [@default-config] [@neotree-plugin]. Other modified keys can fall through to `panel_global_key_action`, so global defaults such as `Ctrl-p` for `FilePicker`, `Ctrl-z` for `Suspend`, and `Alt-x` for the command palette still work from a focused panel [@default-config] [@editor-dispatch].

The panel-global allowlist currently admits command/search entry, file picker, command palette, statusline manager, configuration diagnostics, suspend, logs, plugin listing, window and split management, and nested mappings whose descendants include an admitted action [@editor-dispatch]. Plugin commands are admitted only when the active runtime reports `CommandScope::Global` for that command; an unscoped or missing plugin command remains editor-scoped even if it has a normal-mode key binding [@editor-dispatch] [@plugin-runtime]. A future binding that should work from panels therefore needs both a normal-mode keymap entry and either an allowlisted editor action or plugin command metadata with `scope = "global"` [@editor-dispatch] [@plugin-runtime].

## Keyboard-Shortcut Explorer

`F1` opens contextual keyboard help above the active editor, panel, picker, or
workspace. `:keys` and the **Keyboard shortcuts** command open the same surface;
`Space ?`, `Alt-x`, and `Ctrl-Shift-p` still open the command palette. The explorer
does not replace the underlying dialog, so its selection and unsent input survive.
Its global view enumerates effective keymap leaves, including nested sequences,
action chains, and plugin bindings, alongside shared surface-action references.
The action bar emits its mouse hit target from its actual responsive layout.
Reference-only and other-context bindings are readable but not executable.

## Plugin Command Metadata

Plugins register command discovery data through the runtime's `CommandMetadata`, which carries optional title, category, description, aliases, visibility, panel key-dispatch scope, and opt-in argument/completion metadata [@plugin-runtime]. Declarative `#[red::command]` metadata accepts `scope = "editor"` or `scope = "global"` and defaults to editor scope, while imperative `red::add_command` metadata is deserialized into the same runtime structure [@plugin-api] [@plugin-runtime]. `Runtime::registered_commands` returns active command records with command name, owning plugin, callback, and metadata sorted by command name for stable discovery UI, and `Runtime::command_scope` is the editor's lookup path when deciding whether a plugin command may run from focused panels [@plugin-runtime] [@editor-dispatch]. Because palette entries retain the owning plugin in their ids, duplicate command names have deterministic runtime behavior before discovery surfaces display them [@plugin-runtime].

This command system does not make colon syntax a general shell. Arguments are split simply, command names are case-sensitive where plugin names are involved, and plugin command execution stays behind the plugin host boundary [@command-parser] [@editor-dispatch]. The CLI-level command surface is documented in [Red Command](../../reference/cli/red-command), and exact default keymaps and plugin command bindings belong in [Default Config](../../reference/configuration/default-config).

Plugin commands may opt in with `arguments = true` and positional `completions = [["enable", "disable"]]`. They receive one `CommandInvocation` record containing the registered name, split arguments, and raw argument text. Legacy callbacks still receive no arguments; exact registered names and built-in precedence are preserved. See [Plugin Host API](../../reference/plugins/host-api) for compatibility requirements [@plugin-api] [@plugin-runtime].
