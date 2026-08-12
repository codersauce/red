---
title: "Reference"
summary: "Reference pages provide exact lookup material for Red and Husk commands, configuration, host APIs, runtime self-checks, validation, editor UI contracts, session IPC, and Vim compatibility."
topics: [reference, navigation]
sources:
  - id: topics
    type: file
    path: almanac/topics.yaml
  - id: red-command
    type: file
    path: almanac/reference/cli/red-command.md
  - id: husk-command
    type: file
    path: almanac/reference/cli/husk-command.md
  - id: default-config
    type: file
    path: almanac/reference/configuration/default-config.md
  - id: host-api
    type: file
    path: almanac/reference/plugins/host-api.md
  - id: self-check
    type: file
    path: almanac/reference/runtime/self-check.md
  - id: validation
    type: file
    path: almanac/reference/validation/ci-and-validation.md
---

# Reference

Reference pages are exact lookup material for commands, configuration, schemas, protocol states, validation gates, and compatibility surfaces. The topic graph keeps reference as a root neighborhood and connects individual reference pages to CLI, configuration, plugins, runtime assets, sessions, validation, LSP, editor, and Vim topics [@topics].

## Commands And Configuration

Use [Red command](cli/red-command) for Red's public and hidden CLI flags, argument conflicts, lifecycle branches, utility modes, detach and resume options, and runtime asset commands [@red-command]. Use [Husk command](cli/husk-command) for the standalone Husk CLI surface: `run`, `check`, `fmt`, `repl`, package commands, extension commands, `lsp --stdio`, and examples [@husk-command].

Use [Default config](configuration/default-config) for the top-level config schema, default plugin permissions, bundled plugin mapping, keymaps, LSP defaults, language definitions, comments, cursor shapes, statusline sections, picker settings, and failure behavior [@default-config]. Use [LSP configuration](lsp/configuration) when the lookup is specifically language-server defaults or per-server selector fields.

## Runtime And Plugin Contracts

Use [Host API](plugins/host-api) for the Red host API schema, versioning, request and execute forms, compatibility ranges, validator diagnostics, and schema/code generation checks [@host-api]. Use [Self check](runtime/self-check) for `red --self-check` output, bundled plugin health states, production-equivalent plugin snapshots, and release packaging diagnostics [@self-check].

Use [UI components](editor/ui-components) for modal component behavior, picker and file picker handling, composer/input dialogs, plugin callback ownership, and sensitive input reporting. Use [Registers, clipboard, and macros](editor/registers-clipboard-and-macros) for exact register, clipboard, macro, and command-line history behavior.

## Sessions, Validation, And Compatibility

Use [Detach IPC protocol](sessions/detach-ipc-protocol) for attach authentication, client and server message shapes, render deltas, limits, heartbeat behavior, and stop control. Use [CI and validation](validation/ci-and-validation) for local clippy policy, GitHub Actions jobs, plugin checks, nightly Rust, release archive smoke tests, and Homebrew publication behavior [@validation].

Use [Agent check](agent/agent-check) for `red --agent-check` report fields and Codex readiness rules. Use [Vim compatibility](vim/vim-compatibility) for supported motions, modes, commands, intentional differences, and not-yet-supported behavior.
