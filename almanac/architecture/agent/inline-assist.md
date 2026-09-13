---
title: "Inline Assist"
summary: "Inline assist is Red's bounded Codex workflow for source-anchored questions, annotations, exact-target edits, wider same-file proposals, and handoff into the full Agent path."
topics: [architecture, agent, agent-edits, inline-assist, safety]
sources:
  - id: workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: codex
    type: file
    path: src/codex/mod.rs
  - id: actions
    type: file
    path: src/editor/inline_actions.rs
  - id: assist
    type: file
    path: src/inline_assist.rs
  - id: context
    type: file
    path: src/inline_context.rs
  - id: expansion
    type: file
    path: src/editor/inline_expansion.rs
  - id: history
    type: file
    path: src/inline_history.rs
---

# Inline Assist

Inline assist is Red's source-anchored Codex path for local questions, reviews,
annotations, and bounded code edits. It starts from an editor-selected target,
opens an ephemeral Codex thread with read-only project tools, accepts exactly
one submission result, and routes every possible text change back through the
editor rather than letting Codex write files directly [@workflow] [@codex].
Use this page when changing `Space i`, inline result validation, wider
same-file proposals, retained inline history, or the handoff from an inline
discussion into the full [Agent architecture](../agent) path.

## Request Boundary

`Action::InlineAssist` records the active window, buffer id, buffer revision,
target range, original target text, scope label, and a fresh annotation group
before it opens the inline prompt [@actions]. Normal mode can allow later scope
expansion, Visual and Visual Line requests pin the exact selection, and Visual
Block requests are rejected before a session is created [@workflow] [@actions].
Submitting the prompt rechecks the target, starts or reuses an ephemeral inline
Codex session, records an `InlineHistoryTurn`, and sends either
`CodexCommand::InlineAssist` or `CodexCommand::InlineAssistFollowup` through
the existing agent bridge [@actions].

Codex receives inline instructions, the immutable editor-owned target and
context, and the inline tool set [@codex]. The tool set is not the full Agent
surface. Tests assert that inline sessions expose `submit_replacement`,
`submit_comments`, `request_agent`, `propose_expanded_replacement`,
`list_files`, `search_files`, `read_file`, and `read_git_diff`, and do not
expose editor writes, directory creation, navigation, annotation mutation, or
LSP edit tools [@codex].

## Read-Only Context

Inline context tools are bounded project inspection tools. `InlineContextCall`
allows listing files, literal search, reading up to 200 lines of one file, and
diffing one tracked file against `HEAD` [@context]. The snapshot gives unsaved
editor buffers priority over disk, masks oversized open buffers instead of
falling back to stale disk, excludes `.git`, sensitive paths unless explicitly
allowed, binary or unavailable files, oversized files, and out-of-workspace
paths [@context]. This is why reading more files can inform the answer without
expanding the editable target.

## Submission And Edit Validation

Inline submissions are represented by `InlineAssistResult`. A turn can return
comments, one complete replacement, a wider same-file proposal, or an Agent
handoff reason; the result parser rejects unknown fields and invalid mixtures
such as a handoff that also edits code [@assist]. Limits are enforced at the
result boundary: replacements are capped at 128 KiB, wider-source snapshots at
64 KiB, comments at 16 items, and comment text at 4 KiB [@assist].

Before applying a replacement, Red verifies the current buffer identity,
revision, target range, and original text that were captured when the request
started [@workflow] [@actions]. Exact-scope foreground edits can auto-apply
depending on configuration, but background, stale, and wider-scope results wait
for review [@workflow]. Applied inline edits use one agent-attributed editor
transaction and deliberately remain unsaved, which keeps them separate from
the full [Followed Editing](followed-editing) contract where successful
full-agent file writes are saved through Red [@workflow].

## Wider Proposals

Wider proposals are same-file only and exist for cursor-started requests, not
explicit visual selections. `validate_inline_expansion` requires the proposed
line range to contain and extend the original target, match the captured
revision, match the current source text, and include the exact source before
the replacement can become `Ready` [@expansion]. Approval is a separate review
step: `prepare_reviewed_inline_expansion` re-resolves the retained source,
updates the active inline session to the wider range, and only then lets the
normal inline apply path run [@expansion].

## History Is The Durable Record

Provider threads are disposable. `InlineHistory` keeps recoverable
conversations, prompts, bounded answers, context-read labels, original and
expanded locations, retained before/reviewed text, state, disposition,
transaction ids, change summaries, comment locations, and agent handoff
outcomes [@history]. It enforces a 32 MiB retained-history ceiling before
dispatch and stores content-addressed comment source snapshots for overlapping
or historical annotations [@history]. That makes [Inspect Agent History](../../guides/agent/inspect-agent-history)
the operational route after inline changes, even when the Codex provider
session no longer exists.
