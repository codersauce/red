# PR Replay and Codex collaboration

Status: direct read-only questions, explicit local draft acceptance, and
author-only source proposals already exist. A persistent companion pane,
first-class findings, richer intent routing, and unified proposal presentation
are target behaviors.

## Collaboration goal

Codex is a review partner, not an autonomous reviewer. It helps the person
understand an implementation, investigate concerns, and prepare possible
actions. The person decides whether an answer becomes a finding, whether a
finding becomes feedback, and whether a proposed source edit is applied.

## Persistent review conversation

The target interaction is one PR-scoped, persistent conversation in a genuine
editor-owned companion pane. Opening it preserves the Replay guide and source
editor rather than replacing the current review surface.

The conversation retains:

- Original PR repository, title, pinned head, and author identity.
- The selected original hunk or logical replay group.
- Relevant original source and nearby files.
- Current review findings and explicitly shared local drafts.
- Verified role and the capabilities authorized for the current operation.
- Prior messages and source references within bounded context limits.

A new selected change updates context for the next turn without discarding the
previous conversation.

## Intent is explicit

Different user intentions require different prompts and side-effect policies:

```text
Explain       Answer a question directly in prose.
Investigate   Search the original repository and provide evidence.
Review        Suggest possible findings for human triage.
Draft comment Propose an original-source inline review comment.
Draft summary Propose PR-level feedback.
Propose fix   Stage reviewable source edits in an authorized PR worktree.
```

An explanation must not return a JSON review comment. A question must not
automatically create a finding, add a draft, change source, or publish a
review.

Conversely, when a person explicitly asks for a draft comment, Codex may
produce a reviewable suggestion, but that suggestion is not a durable local
draft until it has been inspected and accepted.

## Scope and authority

Scopes are separately verified:

```text
Current change       Read-only original source and selected hunk.
Whole pull request   Read-only pinned original PR context.
Inline review draft  Read-only source, exact original diff anchor.
PR review summary    Read-only whole-PR source.
Authorized PR fix    Verified original-author worktree; staged proposals only.
```

Current Rust types already distinguish `CurrentChange`, `PullRequest`,
`InlineComment`, `ReviewSummary`, and `AuthorFix`.

Read-only sessions cannot stage source changes through dynamic editor tools.
Author-fix sessions require an independently verified exact PR head and a
separately authorized original-author worktree. Scratch access does not imply
original-branch editing authority.

## Answers and streaming

A Codex question should immediately display:

- The exact submitted question.
- The selected source context.
- A visible busy indicator.
- Streamed response text as it arrives.
- Clear cancellation, completion, and failure states.

The target companion retains the response alongside the original guide and
editor. The current answer-only Replay view is an interim implementation and
must not be mistaken for the final conversation design.

Answers remain private. They can be copied, revisited, dismissed, or used as
input to an explicitly chosen next action.

## Findings from AI review

An AI review pass can suggest observations such as:

```text
Possible correctness issue
    Thread resume restores history but does not restore token usage on forks.

Evidence
    src/app.rs:441 calls restore_history without the fork-specific state.

Confidence
    Medium: the relevant integration test does not cover the fork path.
```

A suggested finding is provisional. The human can:

- Ask a follow-up question.
- Inspect the original source.
- Accept it as a private finding.
- Dismiss it.
- Request a comment draft.
- Request an authorized patch proposal.

Codex must distinguish evidence from inference. An inferred rationale cannot be
attributed to the PR author as if it were stated in a commit, comment, or PR
description.

## Comment and summary promotion

A person can explicitly promote an answer or finding into an editable local
comment or PR-level summary.

Inline comments use the exact pinned original PR head, path, diff side, and
changed-line range. The currently visible scratch cursor is never accepted as a
replacement for original GitHub coordinates.

The human can revise the suggested text and then explicitly accept it into the
local outbox. Posting still requires its own exact publication preview and
confirmation.

When branch editing is unavailable, a supported source patch may become a
GitHub suggestion block only after Red validates that the provider permits a
suggestion at the selected original anchor.

## Original PR source proposals

For a verified original PR owner:

1. Preview and explicitly open the exact original-author worktree.
2. Ask Codex for a selected-change or repository-wide fix.
3. Let Codex inspect bounded repository context and stage proposed edits.
4. Show each affected file and hunk without changing the PR branch.
5. Accept, reject, or revise individual hunks.
6. Apply accepted hunks as attributed, undoable editor transactions.
7. Save, commit, and push only through independent later actions.

Codex can propose changes outside the currently selected replay hunk when the
fix genuinely spans the repository. That broad read scope is not permission to
write files, run arbitrary commands, create network requests, or publish.

## Process and tool isolation

Red uses its existing Codex app-server bridge and bounded dynamic editor
tools. The agent does not receive unrestricted shell, filesystem, GitHub,
network, or native editing authority.

Existing policy includes a read-only app-server sandbox, no execution
environments, bounded tool calls, descriptor-safe source access, and
editor-owned staged proposals. Replay-specific session scope is enforced
independently of whatever a prompt says.

The detailed underlying contract is documented in
[Direct Codex workflow and safety contract](../AGENT_WORKFLOW.md).

## Failure and cancellation

- Startup failure retains the submitted question and offers an honest retry.
- Streaming failures preserve partial visible text without inventing an
  answer.
- Cancellation interrupts the active turn and ignores late response deltas.
- A changed PR head invalidates stale context and prevents draft promotion.
- A rejected draft leaves the original answer available when practical.
- Session recovery restores safe transcript context but never silently starts a
  new Codex process or reissues a request.
- A source proposal that conflicts with newer edits remains pending and does
  not overwrite the user's work.

See [interaction design](interaction-design.md) for the companion layout and
[safety and permissions](safety-and-permissions.md) for approval gates.
