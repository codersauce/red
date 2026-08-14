---
title: "Default Config"
summary: "Red's default configuration defines the embedded baseline for editor behavior, keymaps, bundled plugins, LSP routing, diagnostics, completion, search, picker UI, cursor shapes, AI, and permissions."
topics: [reference, configuration, defaults]
sources:
  - id: defaults
    type: file
    path: default_config.toml
  - id: config
    type: file
    path: src/config.rs
---

# Default Config

`default_config.toml` is Red's embedded baseline configuration. The configuration loader parses this file into the complete default `Config`, layers recoverable user TOML and strict CLI override fragments on top of it, and treats absent user settings as requests to keep these defaults [@defaults] [@config]. Use this page as a lookup companion to [Layered Config Recovery](../../architecture/configuration/layered-config-recovery), which explains how the defaults are applied and recovered.

## Location And Scope

The sample/default file says user configuration is read from `$XDG_CONFIG_HOME/red/config.toml` or `$HOME/.config/red/config.toml` [@defaults]. The code computes the same config directory, preferring a non-empty `XDG_CONFIG_HOME` and otherwise using `$HOME/.config/red` [@config]. Runtime paths such as logs, preferences, sessions, plugins, and themes are resolved below that directory unless a setting explicitly uses an absolute or expanded path [@config].

The top-level schema accepts keys for editor behavior, keymaps, theme, cursor, plugins, disabled plugins, plugin permissions, plugin config, logging, search, completion, picker, statusline, key hints, clipboard, LSP, commenting, matchit, AI, diagnostics, and ASCII window borders [@config]. Unknown top-level fields are ignored with diagnostics during recoverable user loading rather than becoming part of the effective configuration [@config].

## Top-Level Defaults

| Key | Default | Meaning |
| --- | --- | --- |
| `theme` | `"red.json"` | VS Code-compatible theme name [@defaults]. |
| `mouse_scroll_lines` | `3` | Terminal mouse-wheel scroll amount [@defaults]. |
| `scrolloff` | `3` | Minimum visible lines above and below the cursor when possible [@defaults]. |
| `wrap` | `true` | Wrap long lines at the window edge [@defaults]. |
| `breakindent` | `true` | Indent wrapped continuation rows to leading whitespace, keeping at least 20 text columns [@defaults]. |
| `sidescroll` | `1` | Horizontal scroll step when wrapping is off [@defaults]. |
| `sidescrolloff` | `0` | Preferred visible columns beside the cursor [@defaults]. |
| `splash` | `true` | Show the startup splash when Red opens without file arguments [@defaults]. |
| `log_file` | `"red.log"` | Default log file name under the config directory [@defaults]. |
| `disabled_plugins` | `[]` | Plugin IDs removed from activation [@defaults] [@config]. |
| `disable_ai` | `false` | When true, removes the agent plugin and rejects Codex launches [@defaults] [@config]. |

`show_diagnostics` defaults to true in code and in the shipped default file, while `window_borders_ascii` defaults to false in code [@defaults] [@config]. These fields are accepted user-facing top-level fields [@config].

## Search, Completion, Picker, Statusline, Key Hints, Clipboard, And Cursor

| Section | Defaults |
| --- | --- |
| `[search]` | `incsearch = true`, `hlsearch = true`, `wrapscan = true`, `ignorecase = false`, `smartcase = false` [@defaults] [@config]. |
| `[completion]` | `auto_trigger = true`, `min_prefix_length = 1`, `debounce_ms = 120`, `buffer_words = true`, `max_buffer_words = 100` [@defaults] [@config]. |
| `[picker]` | `input_position = "bottom"` [@defaults] [@config]. |
| `[picker.icons]` | `style = "nerd_font"`, `color = true`; code also accepts `unicode`, `ascii`, and `none` icon styles [@defaults] [@config]. |
| `[diagnostics]` | `gutter_signs = true`, `icon_style = "nerd_font"`; code also accepts `unicode`, `ascii`, and `none` icon styles for diagnostic gutter signs [@defaults] [@config]. |
| `[statusline]` | `left = ["mode", "diagnostics", "git_branch", "filename"]`, `right = ["position", "syntax"]`; the configuration schema lets all 25 statusline sections move between sides [@defaults] [@config]. |
| `[statusline.icons]` | `style = "nerd_font"`, `color = true`; the same `unicode`, `ascii`, and `none` icon styles are accepted for statusline icons [@defaults] [@config]. |
| `[key_hints]` | `enabled = true`, `delay_ms = 250` [@defaults] [@config]. |
| `[clipboard]` | Defaults are enabled, sync on yank, and sync on paste when omitted [@config]. |
| `[cursor]` | normal, command, search, visual, visual-line, and visual-block use `default`; insert uses `steady_bar`; waiting uses `steady_underscore` [@defaults] [@config]. |

Supported cursor shapes are `default`, `blinking_block`, `steady_block`, `blinking_underscore`, `steady_underscore`, `blinking_bar`, and `steady_bar` [@defaults] [@config].

## Keys And Plugin Commands

Every editor mode has its own key table under `[keys.<mode>]` [@defaults]. A binding can be an action string, a list of actions, a nested chord table, or a plugin command such as `{ PluginCommand = "BufferPicker" }` [@defaults]. The default normal-mode map includes Vim-style motion and editing keys, command/search entry through `:` and `/`, file and command pickers through `Ctrl-p`, `Ctrl-Shift-p`, `Alt-x`, and `F1`, and plugin commands for buffers, project search, theme browser, Git, LSP symbols, and the agent [@defaults].

The default Space and `Ctrl-w` prefixes are dense command neighborhoods. Space opens buffer, plugin listing, project search, theme, LSP, Git, hunk, statusline, code-action, rename, and agent workflows; `Ctrl-w` owns window focus, movement, splits, closing, balancing, maximizing, and only-window behavior [@defaults].

## Plugins, Plugin Config, And Permissions

The default `[plugins]` table enables bundled Husk plugins: `agent`, `barbecue`, `buffer_picker`, `cool_search`, `fidget`, `git`, `indent_guides`, `inlay_hints`, `lsp_symbols`, `neotree`, `project_search`, and `theme_browser` [@defaults]. Relative plugin paths are resolved through the runtime asset system or the config directory's `plugins` folder; absolute plugin paths are kept absolute [@config].

`[plugin_permissions.<plugin>]` currently grants process execution allowlists. The default config allows `project_search` to launch `rg` and `git` to launch `git` [@defaults] [@config]. The process API matches allowed commands exactly and does not invoke a shell [@config].

`[plugin_config]` stores plugin-specific JSON-compatible settings [@config]. The shipped defaults configure barbecue display behavior, Git gutter signs for unstaged and staged states, and symbol icon visibility/overrides for `lsp_symbols` [@defaults].

## LSP And Commenting

`[lsp]` defaults to enabled, with `format_on_save = false` and built-in server definitions for Rust, Husk, TypeScript/JavaScript, Python, Markdown, JSON, TOML, YAML, Lua, and Fish [@config]. The default file documents both legacy single-language server configuration and preferred multi-document selectors [@defaults]. User server tables merge into the built-in server map rather than replacing the entire map [@config]. Use [LSP Configuration](../lsp/configuration) for the exact built-in server table and per-server fields.

Comment templates are keyed by language or extension and use a single `%s` placeholder [@defaults] [@config]. The shipped defaults cover Bash, C-family extensions, CSS/SCSS, Go, HTML/XML, Husk, Java, JavaScript/TypeScript/JSX/TSX, JSONC, Lua, Markdown, PowerShell, Python, Rust, SQL, TOML, and YAML [@defaults] [@config].

## Agent Configuration

`[agent]` contains an optional `command` override, plus optional `args` and `env` fields in the code schema [@config]. When `command` is absent, the agent check and runtime Codex integration use `codex` from `PATH` [@config]. Setting `disable_ai = true` removes the bundled agent plugin, resets agent configuration during recovered loading paths, and prevents Red from launching Codex [@defaults] [@config].

See [Agent Check](../agent/agent-check) for the command-line readiness report that reads these agent settings.
