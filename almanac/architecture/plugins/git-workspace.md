---
title: "Git Workspace"
summary: "The bundled Git workspace is a Husk plugin workflow for status polling, dashboard rendering, structured diffs, hunk actions, commits, push previews, and guarded Git operations."
topics: [architecture, plugins, ui, safety]
sources:
  - id: git-plugin
    type: file
    path: plugins/git.hk
  - id: git-core
    type: file
    path: plugins/git_core/
  - id: runtime-tests
    type: file
    path: src/plugin/runtime.rs
  - id: host-api-doc
    type: file
    path: docs/PLUGIN_API.md
  - id: default-config
    type: file
    path: default_config.toml
---

# Git Workspace

The bundled Git workspace is Red's source-control UI implemented as a Husk plugin. `plugins/git.hk` owns the editor-facing workflow: background status polling, gutter signs, the full-screen Git dashboard, structured diff detail documents, hunk actions, commit buffers, push previews, and guarded operations such as pull, merge, rebase, revert, stash, branch, tag, remote, and worktree actions [@git-plugin]. Pure parsing and command construction live in `plugins/git_core/`, so the plugin shell can stay focused on Red events, host calls, process orchestration, and user interaction [@git-plugin] [@git-core].

## Status And Refresh Ownership

The Git plugin activates by requesting editor information and scheduling a status poll. It listens for filesystem changes, buffer changes, file opens and saves, window-buffer changes, and focus changes, then debounces those signals into a refresh instead of spawning Git for every event [@git-plugin]. A status refresh runs `git status` through `SpawnProcess` with pager and prompt disabled, `GIT_OPTIONAL_LOCKS=0`, and `LC_ALL=C`; the process is rooted at the plugin's current workspace root rather than at an arbitrary shell directory [@git-plugin].

Refreshes are serialized. If a scan is in flight, another refresh request only sets `refresh_pending`; the follow-up starts after the current scan finishes [@git-plugin]. The host API guide records the same policy for the bundled Git plugin: idle status polls wait at least five seconds and nine times the previous scan duration, target roughly a 10 percent background duty cycle, coalesce requests received during a scan, and retain the last successful status after failed scans [@host-api-doc].

## Dashboard And Detail Documents

`GitDashboard` is a global plugin command that opens the `git-dashboard` workspace with a two-pane layout and then forces a refresh [@git-plugin]. The dashboard model has header, row, detail, footer, action, status, and detail-title fields; row data records the section, path, and Git entry, while detail documents contain structured diff lines with old-line, new-line, hunk, path, section, and patch-index metadata [@git-plugin]. Runtime tests cover bounded pathological porcelain output, large tracked-file statistics, committed-file inspection, root-commit and merge-commit views, untracked-file detail rendering, structured diffs, and debounced detail selection [@runtime-tests].

The detail pane is not plain text. `show_detail` cancels any pending detail work, starts a process for the selected row or commit-file view, and renders a loading state only when requested [@git-plugin]. `detail_document` then converts Git output into a `GitWorkspaceDocument`, preserving enough line metadata for row actions and hunk selection to operate on the same structured patch that the user is inspecting [@git-plugin]. Tests assert that a structured Git diff can render and stage one selected line, which is the regression boundary for keeping visual detail selection tied to patch application [@runtime-tests].

## Mutating Operations

The plugin keeps destructive or history-changing operations behind explicit UI state. It stores the active operation, confirmation mode, operation candidate, row-action process, Git task process, push-preview state, and operation-specific arguments in plugin state before it starts processes or opens confirmation prompts [@git-plugin]. Pushes are previewed before execution: the plugin opens a confirmation that tells the user to review outgoing commits, stores remote, target, arguments, count, process, and error state, and only proceeds through the push workflow after that preview boundary [@git-plugin].

Commits use an editor-owned scratch buffer named `[Git Commit].gitcommit` with `GitSubmitMessage` and `GitCancelMessage` commands. The scratch text contains a marker line, and only text above the marker becomes the commit message; generated context below the marker is ignored before `git commit` receives stdin [@git-plugin]. Default keymaps bind the commit neighborhood to submit and cancel commands, so the scratch buffer participates in ordinary command discovery rather than requiring a hidden plugin-only path [@default-config].

## Hunk Actions And Unsaved Text

Hunk navigation and hunk mutation are plugin commands: `GitHunkPrevious`, `GitHunkNext`, `GitHunkStage`, `GitHunkUnstage`, and `GitHunkReset` all enter the same hunk-action workflow [@git-plugin]. The workflow asks Red for open windows, buffer text, cursor or selection information, and then prepares a patch through Git subprocesses before staging, unstaging, resetting, or moving the cursor [@git-plugin]. That matters because unsaved-buffer work must use editor text, not only disk contents, when deciding which hunk the user is operating on [@git-plugin].

Runtime tests cover hunk navigation targets, end-of-range warnings, and the "no hunk under cursor" failure path for stage, unstage, and reset commands [@runtime-tests]. Keep those tests in view when changing gutter signs, structured diff line metadata, or hunk-command routing, because the visible dashboard and the buffer-local hunk commands share the same patch model.

## Cleanup And Boundaries

The plugin's opening comment states the boundary that future changes should preserve: Git commands run through the plugin process permission boundary without a shell; destructive and history-changing actions require explicit confirmation; operation responses are correlated by plugin-owned process state; unsaved hunk work uses editor text; and deactivation cancels watchers, processes, timers, panels, workspace state, and gutter namespaces [@git-plugin]. This places the Git workspace inside the broader [process and filesystem boundaries](process-and-filesystem-boundaries) and [resource ownership](resource-ownership) contracts rather than making it a privileged editor subsystem.

Read [Bundled Husk Plugins](../../concepts/plugins/bundled-husk-plugins) for the shell-versus-core-package split, [Red Host API](red-host-api) for the calls that let the plugin request editor-owned resources, and [Command Discovery And Dispatch](../commands/command-discovery) for how `GitDashboard` and hunk commands appear in command surfaces.
