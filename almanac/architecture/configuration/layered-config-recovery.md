---
title: "Layered Config Recovery"
summary: "Red builds effective configuration from embedded defaults, recoverable user TOML, strict CLI overrides, and runtime diagnostics."
topics: [architecture, configuration, startup, diagnostics]
sources:
  - id: config-loader
    type: file
    path: src/config.rs
  - id: default-config
    type: file
    path: default_config.toml
  - id: startup-finalize
    type: file
    path: src/main.rs
---

# Layered Config Recovery

Layered config recovery is Red's startup contract for turning a possibly broken user `config.toml` into a usable editor configuration without silently accepting unsafe values. Embedded defaults provide the complete baseline, user TOML is applied with field-level diagnostics where recovery is possible, command-line override fragments are strict, and runtime validation appends missing-plugin, theme, and log-file diagnostics before the editor starts [@config-loader] [@startup-finalize]. The result is a `LoadedConfig` that carries both effective settings and the recovery record the editor can display.

## Embedded Defaults Are The Baseline

The effective configuration starts from `assets::DEFAULT_CONFIG`, which embeds `default_config.toml` in the binary through the runtime asset module [@config-loader]. The default file defines the default theme, editor behavior settings, LSP defaults, keymaps, plugin registrations, and bundled plugin command key bindings such as `ThemeBrowser`, `BufferPicker`, `NeoTree`, and agent commands [@default-config]. `embedded_config_value` parses that file and deserializes it before returning the TOML value, so an invalid bundled default is a startup error rather than a recoverable user problem [@config-loader].

This baseline lets Red start even when the user has no configuration file. `Config::load_user_file` treats a missing file as an empty user layer and calls the same recoverable loader used for present files [@config-loader]. The first-run onboarding path may write a commented starter config, but recovery itself does not depend on any generated user file; the embedded defaults remain the source of truth for unset values [@default-config].

## User TOML Recovers Field By Field

User configuration is applied over the embedded value by walking sorted top-level TOML entries and validating each known schema path [@config-loader]. Unknown top-level or nested fields produce `CFG101` warnings and are ignored, while invalid values that fail deserialization produce `CFG102` errors and keep the previous valid value or disable only the affected dynamic unit [@config-loader]. Dynamic plugin, plugin permission, language-server, matchit-language, and agent sections are treated as atomic enough that one bad entry can be quarantined without rejecting unrelated settings [@config-loader].

Keymaps use a specialized recovery path because nested key groups and leaf actions share the same TOML shape. `apply_keymap_value` descends into tables until it can deserialize a `KeyAction`, merges nested groups when both old and new values are groups, and emits `CFG201` when a key action or keymap group is invalid [@config-loader]. This preserves the embedded keymap around bad user bindings instead of replacing a whole mode's bindings with a partially parsed table.

Malformed or unreadable whole files use a different path. `safe_loaded_config` starts from embedded defaults but switches to a restricted profile: theme `red.json`, no log file, no plugins, no plugin permissions, disabled AI, default agent settings, disabled LSP, and no language servers [@config-loader]. The resulting `ConfigRecovery::WholeFileFallback` is fail-closed, and `finalize_runtime_config` repeats the AI/plugin/LSP/log restrictions after runtime validation so later code cannot accidentally re-enable those surfaces [@startup-finalize]. The design decision is documented in [Fail Closed Recovery](../../decisions/configuration/fail-closed-recovery).

## CLI Overrides Are Strict

Command-line config overrides are final ordered TOML fragments, not recoverable user hints. `apply_strict_overrides` parses each override, rejects the first unknown schema path, merges it into the current serialized config, and errors if the resulting value cannot deserialize into `Config` [@config-loader]. The error messages name the override index and do not create `ConfigDiagnostic` entries, because an invalid CLI override should fail the command that supplied it instead of falling back silently [@config-loader].

Overrides are still merged with the same nested-keymap rules used by user config. When a key path already contains a nested key group and the override supplies another nested group, Red merges those groups; when the override supplies a valid leaf action, it replaces the old value [@config-loader]. Language-server overrides also track an allowed server set so newly named servers can be added while previously recovered or removed servers are not resurrected accidentally [@config-loader].

## Runtime Validation Extends The Same Model

Startup performs checks that require filesystem and runtime asset access after the pure TOML load. `finalize_runtime_config` removes configured plugins that no longer resolve, records `CFG301`, tries to load the configured theme and falls back to the embedded `red.json` with `CFG302`, and resolves the log file path or disables logging with `CFG303` [@startup-finalize]. These diagnostics are appended through `LoadedConfig::add_runtime_diagnostic`, which changes a clean load into partial recovery when necessary [@config-loader].

The editor receives both the diagnostics vector and the coarse recovery state. `src/main.rs` takes diagnostics out of `LoadedConfig`, saves the `ConfigRecovery` value, builds the editor, and calls `editor.set_config_diagnostics(diagnostics, recovery)` before restoring session state or entering the event loop [@startup-finalize]. `red --check-config` uses the same finalization function, sorts diagnostics for stable terminal output, prints each formatted diagnostic, and fails if any remain [@startup-finalize].

## Boundaries With Assets And Themes

Configuration names runtime assets but does not directly load their contents during TOML recovery. Relative plugin paths are resolved later through the runtime asset system, which can return a filesystem path or a private bundled-plugin URI [@config-loader]. Theme loading also occurs at runtime finalization, where Red resolves the configured theme name and parses the VS Code-compatible JSON into the internal theme model [@startup-finalize]. Those boundaries are described in [Runtime Assets](../runtime/runtime-assets) and [Theme Import](../themes/theme-import).

The reference page [Default Config](../../reference/configuration/default-config) is the lookup companion for the exact fields and default values that this architecture page describes.
