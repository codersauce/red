---
title: "Concepts"
summary: "Concept pages define the durable mental models behind Red's editor, Husk runtime, plugins, LSP, sessions, and agent edit boundaries."
topics: [concepts, navigation]
sources:
  - id: topics
    type: file
    path: almanac/topics.yaml
  - id: red-editor
    type: file
    path: almanac/concepts/red-editor.md
  - id: coordinates
    type: file
    path: almanac/concepts/editor/coordinate-systems.md
  - id: display
    type: file
    path: almanac/concepts/editor/display-layout.md
  - id: undo
    type: file
    path: almanac/concepts/editor/undo-tree.md
  - id: husk
    type: file
    path: almanac/concepts/husk-language.md
  - id: lsp
    type: file
    path: almanac/concepts/lsp/capabilities.md
  - id: bundled-plugins
    type: file
    path: almanac/concepts/plugins/bundled-husk-plugins.md
  - id: dialogs
    type: file
    path: almanac/concepts/plugins/callback-scoped-dialogs.md
  - id: agent-edits
    type: file
    path: almanac/concepts/agent-attributed-edits.md
  - id: detach-recovery
    type: file
    path: almanac/concepts/sessions/detach-vs-recovery.md
---

# Concepts

Concept pages define the repo-specific vocabulary and mental models that make
the rest of the Red wiki easier to read. The topic graph keeps them under the
`concepts` root, with links into editor core, Husk, plugins, LSP, agent safety,
and sessions neighborhoods [@topics]. Use this hub when you need the meaning of
a system boundary before reading architecture, guides, decisions, or reference
pages.

## Product And Editor Models

Start with [Red editor](red-editor) for the product-level model: Red combines a
modal terminal editor, embedded runtime assets, language tooling, Husk plugins,
crash recovery, detachable sessions, and followed Codex editing
[@red-editor].

For editor internals, read [Editor coordinate systems](editor/coordinate-systems)
before changing byte, scalar, grapheme, terminal-column, or UTF-16 boundaries
[@coordinates]. Use [Display layout](editor/display-layout) for the mapping
from logical buffer lines to viewport rows, and [Undo tree](editor/undo-tree)
for buffer-local branching history, dirty revisions, and edit replay contracts
[@display] [@undo].

## Husk, Plugins, And LSP

[Husk language](husk-language) explains the embedded scripting language, its
standalone CLI, packages, runtime, language server, semantic tooling, and
extension boundary [@husk]. [Bundled Husk plugins](plugins/bundled-husk-plugins)
then narrows that model to Red's embedded plugin assets and the native packages
that supply pure logic for some plugins [@bundled-plugins].

Use [Callback-scoped dialogs](plugins/callback-scoped-dialogs) when plugin UI
work needs to preserve host-owned picker and composer callback handles instead
of global event names [@dialogs]. Use [LSP capabilities](lsp/capabilities) when
the question is what Red promises to language servers and why the advertised
capability set stays conservative [@lsp].

## Agent And Session Boundaries

[Agent-attributed edits](agent-attributed-edits) is the core concept for Red's
Codex integration: agent writes enter editor-owned transactions with session and
turn attribution instead of using native Codex workspace writes [@agent-edits].
Read it before the agent architecture or history guide.

[Detach versus recovery](sessions/detach-vs-recovery) separates live Unix owner
sessions from persisted crash-recovery snapshots [@detach-recovery]. That
distinction is the starting point for the sessions architecture, detach guide,
resume guide, and detachable-core decision.
