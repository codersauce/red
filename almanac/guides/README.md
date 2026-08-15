---
title: "Guides"
summary: "Guide pages give maintainers task-oriented procedures for validation, plugins, LSP debugging, sessions, performance checks, releases, installers, and agent history inspection."
topics: [guides, navigation]
sources:
  - id: topics
    type: file
    path: almanac/topics.yaml
  - id: validation-guide
    type: file
    path: almanac/guides/development/build-test-and-validate.md
  - id: plugin-guide
    type: file
    path: almanac/guides/plugins/write-a-husk-plugin.md
  - id: lsp-guide
    type: file
    path: almanac/guides/lsp/debugging-lsp-failures.md
  - id: agent-guide
    type: file
    path: almanac/guides/agent/inspect-agent-history.md
  - id: detach-guide
    type: file
    path: almanac/guides/sessions/detach-reattach.md
  - id: recovery-guide
    type: file
    path: almanac/guides/sessions/resume-after-crash.md
  - id: release-guide
    type: file
    path: almanac/guides/releases/release-red.md
  - id: installers-guide
    type: file
    path: almanac/guides/installers/release-installers.md
  - id: performance-guide
    type: file
    path: almanac/guides/performance/performance-checks.md
---

# Guides

Guide pages are the task-oriented shelf for Red maintainers. The topic graph keeps them under the `guides` root and connects individual guides to validation, plugins, LSP, sessions, release, installers, performance, agent, and operations topics [@topics]. Use this hub when you know the work you need to finish and want the procedure before reading subsystem architecture.

## Everyday Development

Start with [Build, Test, And Validate](development/build-test-and-validate) when preparing ordinary code changes for review, push, or CI parity. It orders local Rust tests, clippy, formatting, self-checks, plugin checks, performance gates, and release-adjacent validation by changed area [@validation-guide].

Use [Write A Husk Plugin](plugins/write-a-husk-plugin) when adding or updating a plugin package, host API usage, resource registration, permissions, or plugin validation [@plugin-guide]. Use [Debugging LSP Failures](lsp/debugging-lsp-failures) when a language-server issue may involve startup, routing, transport, diagnostics, completion, or workspace edits [@lsp-guide].

## Agent And Session Operations

[Inspect Agent History](agent/inspect-agent-history) is the operational guide for inspecting and safely reverting Codex-origin transactions after followed edits or inline assist have entered Red's undo history [@agent-guide].

For live sessions, use [Detach And Reattach](sessions/detach-reattach) when the owner process should stay alive across terminal disconnects [@detach-guide]. Use [Resume After Crash](sessions/resume-after-crash) when the owner is gone and the task is to recover the newest useful snapshot deliberately [@recovery-guide].

## Release And Performance Work

Use [Performance Checks](performance/performance-checks) for deterministic CI performance gates and workstation benchmarks that catch editor, detach, interaction, and Git workspace regressions [@performance-guide].

For publication work, [Release Red](releases/release-red) covers the release flow from prepare-release through tag publishing, archive smoke tests, Homebrew update, installer verification, and Discord announcement [@release-guide]. [Release Installers](installers/release-installers) narrows that to Unix and Windows installer verification, checksum handling, self-check execution, fixture tests, and latest-release smoke tests [@installers-guide].
