---
title: "Configuration Fail-Closed Recovery"
summary: "Red keeps editing available on whole-file configuration failure while disabling plugins, AI, LSP, logging, and plugin permissions."
topics: [decisions, configuration, diagnostics, safety]
sources:
  - id: config
    type: file
    path: src/config.rs
  - id: main
    type: file
    path: src/main.rs
  - id: defaults
    type: file
    path: default_config.toml
---

Red chooses fail-closed recovery for unreadable or malformed whole-file user configuration. Instead of aborting startup, the loader builds a usable editor config from embedded defaults, then disables high-risk surfaces: plugins, plugin permissions, AI, agent settings, LSP, language servers, and logging [@config]. Startup repeats the same restrictions after runtime validation before constructing the editor [@main]. The result is an editor that can still open files and show diagnostics, while code execution, external processes, and service integrations remain off until the user fixes configuration.

## Context

Configuration controls both ordinary editor behavior and surfaces that can launch processes or load plugin code. The complete default file enables bundled plugins, maps plugin commands into the default keymap, selects a theme, configures LSP defaults, and grants process permissions to selected bundled plugins such as project search and Git [@defaults]. A malformed whole file therefore cannot be treated as just a missing color or keymap setting.

Red already has a more precise recovery path for partial user errors. Unknown fields produce diagnostics and are ignored, invalid individual values keep previous valid defaults or disable only the affected plugin, plugin permission, language server, agent, or LSP unit, and command-line override fragments remain strict [@config]. Whole-file failure is different because Red cannot reliably identify independent user fields after an unreadable file or malformed TOML document [@config].

## Decision

When `Config::load_user_file` cannot read the user config or `Config::load_user_toml` sees malformed TOML, Red calls `safe_loaded_config` [@config]. That function starts from the embedded default configuration, then sets the theme to `red.json`, clears the log file, clears configured plugins, clears disabled plugins, clears plugin permissions, sets `disable_ai = true`, resets agent settings, disables LSP, and clears language servers [@config]. It returns a `LoadedConfig` with a `ConfigDiagnostic` at `<document>`, severity `Error`, recovery `WholeFileFallback`, and fallback text saying Red started with the fail-closed embedded profile [@config].

Startup preserves that decision after runtime checks. `finalize_runtime_config` performs missing-plugin, theme, and log validation, but if the recovery state is `WholeFileFallback`, it again disables AI, clears plugins and plugin permissions, disables LSP, clears language servers, and disables logging [@main]. This second pass prevents later runtime finalization from accidentally re-enabling a surface that whole-file recovery intentionally closed [@main].

## Status

This decision is active. `ConfigRecovery` has an explicit `WholeFileFallback` variant, `LoadedConfig` carries both the effective config and diagnostics, and startup passes diagnostics plus the recovery state into the editor with `editor.set_config_diagnostics` [@config] [@main]. `red --check-config` uses the same finalization path, prints sorted diagnostics, and fails when diagnostics remain [@main].

## Consequences

Editing remains available when configuration is broken. A user can still start Red, inspect files, and use the diagnostics path to understand what failed, rather than being locked out by a bad `config.toml` [@main]. The surrounding architecture is described in [Layered Config Recovery](../../architecture/configuration/layered-config-recovery), and the exact default fields are listed in [Default Config](../../reference/configuration/default-config).

Plugin and process surfaces must be re-enabled only through valid configuration. Whole-file fallback clears plugins and plugin permissions and sets `disable_ai = true`; because the process API depends on plugin permissions, configured plugin subprocess launches are not available in that profile [@config]. This is stricter than partial recovery, where Red can quarantine only the affected plugin or permission entry [@config].

The choice trades convenience for containment. A malformed TOML file disables LSP, logging, plugins, and AI even if the user's intent was unrelated to those features [@config] [@main]. That can surprise users, but the diagnostic explicitly reports the whole-document fallback, and the default editor core remains usable enough to repair the file.

Command-line overrides stay strict rather than recoverable. After a whole-file fallback, Red still applies CLI override fragments through the strict override path, and invalid override syntax or values fail the command instead of becoming diagnostics [@config]. This keeps recovery focused on durable user config files while preserving CLI overrides as deliberate, immediate instructions.
