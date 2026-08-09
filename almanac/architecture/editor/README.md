---
title: "Editor Architecture"
summary: "Editor architecture routes readers through Red's single-owner event loop, buffer and window model, mutation boundary, rendering pipeline, LSP sync, plugin request handling, and syntax services."
topics: [architecture, editor, red-editor]
sources:
  - id: editor
    type: file
    path: src/editor.rs
  - id: buffer
    type: file
    path: src/buffer.rs
  - id: window
    type: file
    path: src/window.rs
  - id: rendering
    type: file
    path: src/editor/rendering.rs
  - id: highlighter
    type: file
    path: src/highlighter.rs
---

# Editor Architecture

Red's editor core is the single-owner runtime for interactive state. `Editor` owns buffer selection, window layout, session snapshots, LSP coordination, agent state, plugin registry state, syntax highlighting, render caches, terminal output, dialogs, overlays, and background service polling on one async task [@editor]. The surrounding pages in this folder split that large owner into the boundaries a maintainer normally needs to change: input and background service order, text mutation, view state, rendering, LSP sync, plugin requests, and syntax services.

## Reading Order

Start with [Editor Event Loop](event-loop) when work touches startup, terminal input, background service ticks, rendering cadence, session persistence, detached core ticking, or shutdown. That page explains why LSP, plugins, sessions, and Codex return to the editor before mutating interactive state [@editor].

Use [Command Discovery](../commands/command-discovery) when changing colon commands, command-palette rows, effective keymap shortcuts, nested key hints, or whether a command can run while plugin panels have focus.

Read [Text Mutation Boundary](text-mutation-boundary) before adding any user, plugin, LSP, or agent path that changes buffer text. `Buffer` owns Ropey text, revision, dirty state, cursor fallback, and undo history, but raw buffer replacement does not perform the editor-owned undo, mark, dirty-state, LSP, plugin, and render updates required by production edits [@buffer].

Use [Buffers And Windows](buffers-and-windows) for the split between editable text identity and visible split-tree presentation. Buffers own process-local text identity, while windows own stable window ids, viewport offsets, wrapping state, cursor position, active state, and split layout [@buffer] [@window].

Use [Rendering Pipeline](rendering-pipeline) when changing terminal output, gutters, panels, overlays, dialogs, plugin paint, diagnostics, cursor drawing, detached frame serialization, or motion fast paths. The renderer turns editor and window state into terminal-cell frames and diffs changed cells or rows before flushing output [@rendering].

Read [LSP Document Sync](lsp-document-sync) when a change concerns editor-side document open/close state, change delivery, diagnostics URI identity, stale LSP responses, or workspace edits reaching open buffers. For server routing and process transport, continue to [LSP Architecture](../lsp).

Read [Plugin Host Requests](plugin-host-requests) when Husk plugin calls need editor effects. Plugin requests enter the editor's serialized queue, and editor state remains the owner for buffer, window, UI, LSP, agent, and filesystem reconciliation work [@editor].

Use [Syntax Services](syntax-services) for language selection, viewport highlight caching, Tree-sitter and Husk highlighting, Markdown injections, and matchit navigation. `Highlighter` maps filenames, extensions, language names, queries, and Husk lexer tokens into byte-range style spans, while the editor decides which buffer language and viewport slice to render [@highlighter] [@editor].

Use [Vim Compatibility](../../reference/vim/vim-compatibility) as the lookup page for supported Vim-inspired behavior, intentional differences, and unsupported surface before changing modal editing semantics.

## Boundaries To Preserve

Do not bypass the editor loop to mutate buffers, windows, plugin resources, LSP state, agent state, or session state. The `Editor` struct groups those domains under one owner and delegates narrow storage to subcontrollers rather than making those controllers independent mutators [@editor].

Do not confuse buffer identity with window identity. Buffer ids and revisions track text; window ids and split snapshots track views over that text [@buffer] [@window]. Rendering and plugin snapshots depend on both identities staying separate.

Do not treat rendering as a side effect available to every subsystem. The rendering code consumes already-updated editor state, composes a frame, and diffs output after the state transition [@rendering]. Subsystems should request editor actions or plugin host requests, then let the event loop decide when a frame is needed.
