---
title: "Decisions"
summary: "Decision pages record accepted architectural choices for agent integration, configuration recovery, Husk semantics and extension boundaries, plugin language-pack distribution, runtime assets, and sessions."
topics: [decisions, navigation]
sources:
  - id: topics
    type: file
    path: almanac/topics.yaml
  - id: agent-decision
    type: file
    path: almanac/decisions/agent/direct-codex-app-server.md
  - id: config-decision
    type: file
    path: almanac/decisions/configuration/fail-closed-recovery.md
  - id: engine-ownership
    type: file
    path: almanac/decisions/husk/engine-instance-ownership.md
  - id: scripts-modules
    type: file
    path: almanac/decisions/husk/scripts-and-modules.md
  - id: extension-tiers
    type: file
    path: almanac/decisions/husk/extension-tiers.md
  - id: semantic-profiles
    type: file
    path: almanac/decisions/husk/semantic-profiles.md
  - id: value-semantics
    type: file
    path: almanac/decisions/husk/value-semantics.md
  - id: husk-decisions
    type: file
    path: almanac/decisions/husk/README.md
  - id: language-pack
    type: file
    path: almanac/decisions/plugins/language-pack-distribution.md
  - id: arborium
    type: file
    path: almanac/decisions/plugins/arborium-language-source.md
  - id: assets
    type: file
    path: almanac/decisions/runtime/embedded-assets-with-user-ejection.md
  - id: sessions
    type: file
    path: almanac/decisions/sessions/detachable-core-boundary.md
---

# Decisions

Decision pages record choices future maintainers must respect when changing subsystem boundaries, provider integrations, safety gates, runtime assets, language-pack distribution, or session ownership. The topic graph groups these pages under the `decisions` root and connects individual decisions to architecture neighborhoods such as agent, configuration, Husk, plugins, runtime, and sessions [@topics].

## Integration And Safety Decisions

Read [Direct Codex app-server](agent/direct-codex-app-server) before changing Red's agent process model, dynamic tool contract, or editor-owned edit flow. That decision supersedes the earlier ACP foundation and keeps Codex behind Red-owned app-server process control and dynamic tools [@agent-decision].

Read [Fail closed recovery](configuration/fail-closed-recovery) before loosening configuration recovery. Whole-file configuration failure intentionally disables AI, plugins, plugin permissions, LSP, language servers, and logging, while field-level user errors can recover more narrowly [@config-decision].

Read [Embedded assets with user ejection](runtime/embedded-assets-with-user-ejection) before changing bundled plugin/theme distribution or `red --eject`. The decision keeps Red self-contained while making user shadowing explicit rather than silently materializing stale local copies [@assets].

## Husk Decisions

Read [Husk decisions](husk) before changing the scripting language, package execution, embedding boundary, or extension model. That hub orders the cluster by language semantics, standalone package shape, engine ownership, and extension tiers, then links to the individual decisions for [engine instance ownership](husk/engine-instance-ownership), [scripts and modules](husk/scripts-and-modules), [extension tiers](husk/extension-tiers), [semantic profiles](husk/semantic-profiles), and [value semantics](husk/value-semantics) [@husk-decisions] [@engine-ownership] [@scripts-modules] [@extension-tiers] [@semantic-profiles] [@value-semantics].

## Plugin And Language-Pack Decisions

Read [Official language pack distribution](plugins/language-pack-distribution) before changing catalog-backed language packs. The accepted boundary is per-package lifecycle, update, install record, release artifact, and native grammar trust, even when packs are authored in a shared repository [@language-pack].

Read [Arborium language pack source](plugins/arborium-language-source) when importing Tree-sitter grammar inventories. Arborium is a build-time source for independent Red packages, not a runtime aggregate package or a managed LSP tool catalog [@arborium].

## Session Decisions

Read [Detachable core boundary](sessions/detachable-core-boundary) before changing detach behavior. The accepted model keeps mutable editor ownership in a long-lived headless owner process, while terminal clients collect input and paint rendered state [@sessions].
