---
title: "LSP Workspace Edits"
summary: "Red parses workspace edits fail-closed, validates every target before mutation, applies resource operations transactionally when supported, and lets the editor own buffer changes."
topics: [architecture, lsp, editing, workspace-edits, safety]
sources:
  - id: edit
    type: file
    path: src/lsp/edit.rs
  - id: workspace-edit
    type: file
    path: src/lsp/workspace_edit.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: lazy-tests
    type: file
    path: tests/lsp_lazy.rs
---

LSP workspace edits are a two-stage boundary in Red. The LSP parser converts protocol JSON into ordered document and resource operations, while the workspace-edit preparer validates the whole edit against open buffers, disk snapshots, revisions, versions, workspace confinement, protected paths, and size budgets before any mutation occurs [@edit] [@workspace-edit]. The editor then applies resource operations, routes prepared buffer contents through the [Text Mutation Boundary](../editor/text-mutation-boundary), responds to `workspace/applyEdit`, and sends follow-up [LSP Document Sync](../editor/lsp-document-sync) notifications [@editor].

## Parsing And Text Conversion

`workspace_edit_operations` accepts either `changes` or `documentChanges`, but rejects payloads that contain both forms because their ordering would be ambiguous [@edit]. It also rejects change annotations that require confirmation, unknown resource operation kinds, missing URIs, invalid options, and non-file URIs [@edit]. The older `workspace_edits` helper only returns document edits and rejects resource operations because those require ordered application [@edit].

Text edits are converted from LSP UTF-16 positions to byte and character ranges before application. `apply_text_edits` builds line starts, rejects positions outside a document, rejects positions that split a UTF-16 character, rejects ranges whose end precedes their start, sorts edits by range, rejects overlaps, and applies replacements from the end of the document backward [@edit]. `text_edit_char_range` exposes the same UTF-16 boundary checks for editor completion and cursor-oriented application paths [@edit].

## Preparation Before Mutation

`prepare_workspace_edit` receives the ordered operations, request-time expected revisions, the currently open documents touched by the edit, and an optional workspace root [@workspace-edit]. It rejects edits with more than 1024 operations, duplicate open buffers, stale expected revisions, stale LSP document versions, deleted targets, missing files, non-UTF-8 unopened files, recursive deletes, and edits whose total prepared content and snapshots exceed the configured byte budget [@workspace-edit].

Workspace confinement is part of preparation. Paths are normalized, must remain under the workspace root when a root is required, must not pass through symlinks, and must not target protected control or secret paths such as `.git`, `.ssh`, `.red`, `red.toml`, `.env*`, `.vscode/tasks.json`, or `.vscode/launch.json` [@workspace-edit]. Unopened-file and resource operations require a workspace root; server-originated apply-edit requests fail closed when their originating root cannot be recovered [@editor] [@lazy-tests].

## Resource Operations And Rollback

On Unix platforms, the resource layer pins the workspace root, opens paths through handle-relative no-follow operations, snapshots contents and file metadata, and verifies snapshots before applying each create, rename, or delete [@workspace-edit]. Create uses exclusive create unless overwrite is requested, remove refuses non-regular files, and no-replace rename is only available on platforms that provide an atomic primitive [@workspace-edit].

If a resource operation fails after preparation, Red verifies that targets have not changed concurrently before attempting rollback from the original snapshots [@workspace-edit]. If a target changed concurrently, rollback is refused instead of overwriting outside changes; if rollback itself fails, the error reports both the original failure and rollback failure [@workspace-edit].

## Editor-Owned Application

The editor gathers touched open buffers, checks their total byte budget, builds `OpenWorkspaceDocument` records with buffer revisions and LSP document versions, and calls `prepare_workspace_edit` before any resource operation or buffer replacement [@editor]. Resource operations are applied before buffer text replacement. Prepared documents then update existing buffers or open new buffers, whole-buffer replacements are recorded as LSP-origin undo transactions, renamed documents close the old LSP identity and open the new one, changed buffers send `didChange`, and server-initiated requests receive success or failure responses [@editor]. This is the multi-file counterpart to [LSP Completion](completion), which reuses the text-edit conversion path for a single accepted item.

The lazy LSP tests cover the important behavior: dirty open buffers are updated without writing disk, unopened documents can be opened and synced after a valid server edit, invalid edits fail without opening or mutating targets, resource-only rename updates LSP document identity while preserving unsaved buffer text, and unsupported no-follow resource operations fail closed on non-Unix platforms [@lazy-tests].
