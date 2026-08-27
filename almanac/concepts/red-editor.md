---
title: "Red Editor"
summary: "Red is a modal terminal editor that combines Vim-style editing, embedded runtime assets, language tooling, Husk plugins, and followed agent edits."
topics: [concepts, red-editor, editor, runtime]
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
  - id: website-direction
    type: conversation
    path: /Users/fcoury/.claude/projects/-Users-fcoury-code-red/acaa6873-03ae-4f92-a325-63c9b68fef5b.jsonl
---

Red is a modal terminal editor built as a Rust application with embedded defaults, themes, plugins, language tooling, crash recovery, Unix detachable sessions, and a review-before-apply Codex workflow. The product documentation presents it as a fast Vim-inspired editor whose files remain under the user's control, while the entrypoint and editor coordinator show how the implementation divides startup lifecycle selection from the central input, rendering, LSP, plugin, recovery, and agent loop [@readme] [@main] [@editor].

## Editor Shape

Red's user model is modal. The README lists Normal, Insert, Visual, Visual Line, Visual Block, and Command modes, along with motions, text objects, splits, pickers, tree-sitter highlighting, language tools, Git UI, and command discovery [@readme]. The concept is therefore broader than a text buffer: Red is the terminal application that owns modes, windows, buffers, LSP clients, plugins, rendering, recovery, and optional agent state.

The executable is also intended to be self-contained. The README says the editor, default configuration, themes, and plugins are bundled into the executable, and the entrypoint exposes utility modes that list runtime assets, eject bundled assets, validate configuration, check Codex prerequisites, and run self-checks without entering the TUI [@readme] [@main]. That bundle model connects Red directly to [Husk language](husk-language) and plugin architecture.

## Runtime Lifecycle

`src/main.rs` chooses the top-level lifecycle before editor state runs. It forwards `red husk ...` to the Husk CLI, validates utility arguments, handles attach/stop/detach modes, services non-interactive utilities, optionally loads a crash-recovery snapshot, constructs LSP, buffers, configuration, theme, preferences, and the `Editor`, and only then enters `editor.run().await` [@main]. For the full startup path, read [Runtime lifecycle](../architecture/startup/runtime-lifecycle) and [Red command](../reference/cli/red-command).

Interactive behavior is coordinated by `src/editor.rs`. Its module comment identifies `Editor` as the owner of mutable application state on one async task, with terminal events resolving into actions, edits entering undo transactions, and background LSP, plugin, filesystem-watch, recovery, and Codex work polled between input batches rather than mutating state directly [@editor]. That ownership model is expanded in [Editor event loop](../architecture/editor/event-loop).

## Agent And Plugin Role

Red's agent feature is part of the editor model, not a separate file writer. The README says Red gives Codex editor context, including unsaved buffers, and follows each tool call by revealing the file and edit before it is applied and saved [@readme]. That user-facing rule is the core of [Agent-attributed edits](agent-attributed-edits).

Husk plugins are also first-class editor runtime assets. The README names bundled Husk plugins for the file tree, project search, Git workspace, progress, inlay hints, symbols, themes, and agent UI, and it describes typechecking against a versioned Husk host contract before activation [@readme]. For the language side, read [Husk language](husk-language); for runtime activation, read [Plugin lifecycle and reload](../architecture/plugins/lifecycle-and-reload).

## Website Positioning

The current external-site direction targets developers who already know code editors, especially Neovim users who need their Vim muscle memory to transfer [@website-direction]. The site should lead with Red's AI-native work model and batteries-included runtime, but production copy must verify the exact shipped shortcuts and names against repository docs: the current README documents `Space A` for the full Agent, `Space i` for inline assist, `Space t` for theme browsing, bundled plugins and themes, language packs, and detachable sessions [@readme].

Visual work for `getred.dev` should be evidence-first. The selected direction calls for screenshots and videos of real Red project workflows rather than placeholder editor mockups, and one prototype intentionally made the page behave like Red with Normal-mode navigation, live theme switching, and scripted Pair and Delegate demos [@website-direction]. Treat that prototype as design direction, not runtime truth: features or keys that are not supported by the current code or docs, such as a dedicated `Space D` Delegate entry point, need to stay marked as proposed until implementation catches up [@website-direction] [@readme].
