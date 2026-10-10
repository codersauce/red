---
title: "Configuration Architecture"
summary: "Configuration architecture routes Red's embedded defaults, recoverable user TOML, strict CLI overrides, fail-closed recovery, LSP defaults, theme loading, and AI completion settings."
topics: [architecture, configuration, navigation]
sources:
  - id: layered
    type: file
    path: almanac/architecture/configuration/layered-config-recovery.md
  - id: fail-closed
    type: file
    path: almanac/decisions/configuration/fail-closed-recovery.md
  - id: defaults
    type: file
    path: almanac/reference/configuration/default-config.md
  - id: lsp-config
    type: file
    path: almanac/reference/lsp/configuration.md
  - id: themes
    type: file
    path: almanac/architecture/themes/theme-import.md
  - id: copilot
    type: file
    path: almanac/guides/agent/copilot-completion.md
---

# Configuration Architecture

Configuration architecture is the reading hub for Red's effective settings.
Start here when a change touches embedded defaults, user `config.toml`
recovery, strict `-c` overrides, runtime diagnostics, LSP defaults, theme
loading, or optional Copilot completion settings. The core implementation is
[Layered Config Recovery](layered-config-recovery), and the fail-closed
decision explains why a malformed whole config disables plugins, AI, LSP,
logging, and plugin permissions while keeping basic editing available
[@layered] [@fail-closed].

## Loading And Recovery

Read [Layered Config Recovery](layered-config-recovery) when changing how Red
builds `LoadedConfig` from embedded defaults, recoverable user TOML, strict
command-line override fragments, and runtime validation [@layered]. It is the
architecture page for diagnostics such as unknown fields, invalid values,
missing plugins, missing themes, log-file failures, and the editor handoff.

Read [Configuration Fail-Closed Recovery](../../decisions/configuration/fail-closed-recovery)
when whole-file recovery behavior is the design question. That decision records
the active choice to preserve editing while closing process-launching and
service-integration surfaces after unreadable or malformed user config
[@fail-closed].

Use [Default Config](../../reference/configuration/default-config) for exact
fields, defaults, accepted top-level sections, bundled plugins, keymaps, AI
settings, formatting aliases, and permission defaults [@defaults]. It is the
lookup companion to the architecture, not the explanation of recovery flow.

## Config-Driven Subsystems

Use [LSP Configuration](../../reference/lsp/configuration) for built-in
language-server definitions, selector fields, root markers, initialization
options, and per-server command arguments [@lsp-config]. Use
[Theme Import](../themes/theme-import) when the setting being changed resolves
a VS Code-compatible theme file into Red's runtime theme model [@themes].

Use [Copilot Inline Completion](../../guides/agent/copilot-completion) when a
task concerns `[copilot]`, `[completion].inline_mode`, opt-in source
transmission, setup hints, or ghost-text acceptance [@copilot]. Those settings
live in the same default configuration surface, but the runtime behavior is a
completion-provider workflow rather than the Codex agent architecture.
