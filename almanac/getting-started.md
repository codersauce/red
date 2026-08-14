---
title: "Getting Started"
summary: "Front door for the Red wiki, routing readers through the editor, agent, Husk, sessions, and development areas."
topics: [red-editor, onboarding, navigation]
sources:
  - id: readme
    type: file
    path: README.md
  - id: getting-started-doc
    type: file
    path: docs/GETTING_STARTED.md
  - id: cargo
    type: file
    path: Cargo.toml
---

Red is a modal terminal editor with a self-contained Rust binary, embedded runtime assets, optional Codex agent integration, Husk plugins, language tooling, crash recovery, and Unix detachable sessions. The quickest path through this wiki is to start with the [Red editor concept](concepts/red-editor), then follow the architecture page for the subsystem you need to change. The repository README describes Red as "the modal editor for the agent era" and highlights its bundled defaults, safer agent workflow, Husk runtime, recovery, and detach support [@readme].

For page-type browsing, use [Concepts](concepts) for repo-specific mental models, [Architecture](architecture) for subsystem maps, [Guides](guides) for task procedures, [Decisions](decisions) for accepted constraints, and [Reference](reference) for exact command, configuration, API, and protocol lookup.

## Start With The Product Model

Use [Red editor](concepts/red-editor) to understand the basic mental model before reading subsystem pages. Red combines Vim-inspired modes and motions with tree-sitter highlighting, language servers, command discovery, file and buffer pickers, Git tooling, embedded Husk plugins, and optional Codex support [@readme]. The user guide shows the everyday workflow: launch with `red path/to/file`, use Normal, Insert, Visual, Visual Line, Visual Block, and Command modes, and discover commands with `Space ?`, `F1`, `Alt-x`, or `Ctrl-Shift-p` [@getting-started-doc].

For startup behavior, read [Runtime lifecycle](architecture/startup/runtime-lifecycle) and [Red command](reference/cli/red-command). The command-line contract includes ordinary file opening, `-r` workspace roots, inline config overrides, runtime asset listing, asset ejection, Codex setup checks, crash resume, and Unix detach/attach modes [@getting-started-doc].

## Editing, Runtime, And Plugins

Read [Editor architecture](architecture/editor) when changing input handling, editing actions, rendering, LSP polling, plugin dispatch, recovery snapshots, or agent event processing. For language-server work, start with [LSP architecture](architecture/lsp) before moving into transport, document sync, completion, workspace edits, or configuration. The current user documentation treats editing, selecting, searching, windows, command mode, Git, LSP, configuration, plugins, and troubleshooting as one day-to-day editor surface [@getting-started-doc].

Read [Plugin architecture](architecture/plugins) when changing bundled plugins or Husk host behavior, then use [Plugin lifecycle and reload](architecture/plugins/lifecycle-and-reload) for discovery, activation, quarantine, and reload details. Red ships embedded plugins and themes, lets users list visible runtime files with `red --runtime-files`, and can eject bundled assets into the user config directory where they shadow embedded copies [@readme].

## Agent Work

Read [Reviewable agent edits](concepts/reviewable-agent-edits) before changing agent behavior. The README states the user-facing contract: Codex receives editor context, but every suggested write is staged as an isolated proposal until explicit review [@readme]. The deeper reading order starts at [Agent architecture](architecture/agent), then continues through the Codex app-server flow, proposal workspace, dynamic tools, and review guide.

## Husk Work

Read [Husk language](concepts/husk-language) before touching the scripting language or plugin runtime. The workspace includes Husk crates for the public facade, CLI, parser, runtime, package handling, LSP, semantic analysis, extension support, standard library, and WebAssembly support [@cargo]. The deeper reading path starts at [Husk architecture](architecture/husk), continues through the embedding, package, extension, and language-server pages, and uses [Husk command](reference/cli/husk-command) for command behavior.

## Sessions And Recovery

Read [Detach versus recovery](concepts/sessions/detach-vs-recovery) when deciding whether a problem concerns a live owner process or a persisted snapshot. The README distinguishes Unix detach/attach sessions, which preserve live editor state across terminal or SSH disconnects, from atomic crash recovery, which restores persisted work after an editor crash or restart [@readme].

## Development Path

For local work, start with [Build, test, and validate](guides/development/build-test-and-validate). The README lists the expected development loop as `cargo build`, `cargo test --all-targets --all-features`, and `cargo clippy --all-targets --all-features -- -D warnings` after cloning the repository [@readme].
