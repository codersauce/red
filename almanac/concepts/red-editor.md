---
title: "Red Editor"
summary: "Red is a modal terminal editor that combines Vim-style editing, embedded runtime assets, language tooling, Husk plugins, and reviewable agent edits."
topics: [red-editor, editor, runtime]
sources:
  - id: readme
    type: file
    path: README.md
  - id: main
    type: file
    path: src/main.rs
  - id: editor
    type: file
    path: src/editor.rs
---

Red is a modal terminal editor built as a Rust application with embedded defaults, themes, plugins, language tooling, crash recovery, Unix detachable sessions, and a review-before-apply Codex workflow. The product documentation presents it as a fast Vim-inspired editor whose files remain under the user's control, while the entrypoint and editor coordinator show how the implementation divides startup lifecycle selection from the central input, rendering, LSP, plugin, recovery, and agent loop [@readme] [@main] [@editor].

## Editor Shape

Red's user model is modal. The README lists Normal, Insert, Visual, Visual Line, Visual Block, and Command modes, along with motions, text objects, splits, pickers, tree-sitter highlighting, language tools, Git UI, and command discovery [@readme]. The concept is therefore broader than a text buffer: Red is the terminal application that owns modes, windows, buffers, LSP clients, plugins, rendering, recovery, and optional agent state.

The executable is also intended to be self-contained. The README says the editor, default configuration, themes, and plugins are bundled into the executable, and the entrypoint exposes utility modes that list runtime assets, eject bundled assets, validate configuration, check Codex prerequisites, and run self-checks without entering the TUI [@readme] [@main]. That bundle model connects Red directly to [Husk language](husk-language) and plugin architecture.

## Runtime Lifecycle

`src/main.rs` chooses the top-level lifecycle before editor state runs. It forwards `red husk ...` to the Husk CLI, validates utility arguments, handles attach/stop/detach modes, services non-interactive utilities, optionally loads a crash-recovery snapshot, constructs LSP, buffers, configuration, theme, preferences, and the `Editor`, and only then enters `editor.run().await` [@main]. For the full startup path, read [Runtime lifecycle](../architecture/startup/runtime-lifecycle) and [Red command](../reference/cli/red-command).

Interactive behavior is coordinated by `src/editor.rs`. Its module comment identifies `Editor` as the owner of mutable application state on one async task, with terminal events resolving into actions, edits entering undo transactions, and background LSP, plugin, filesystem-watch, recovery, and Codex work polled between input batches rather than mutating state directly [@editor]. That ownership model is expanded in [Editor event loop](../architecture/editor/event-loop).

## Agent And Plugin Role

Red's agent feature is part of the editor model, not a separate file writer. The README says Red sends Codex editor context, including unsaved buffers, while staging every suggested write as an isolated proposal for explicit review [@readme]. That user-facing rule is the core of [Reviewable agent edits](reviewable-agent-edits).

Husk plugins are also first-class editor runtime assets. The README names bundled Husk plugins for the file tree, project search, Git workspace, progress, inlay hints, symbols, themes, and agent UI, and it describes typechecking against a versioned Husk host contract before activation [@readme]. For the language side, read [Husk language](husk-language); for runtime activation, read [Plugin lifecycle and reload](../architecture/plugins/lifecycle-and-reload).
