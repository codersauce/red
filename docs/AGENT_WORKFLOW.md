# Agent workflow and safety contract

Red owns an ACP v1 client compiled against the official schema artifact 1.4.0. The
bundled Husk plugin owns the terminal UI; the Rust core owns adapter lifecycle,
filesystem policy, proposal state, review, and attributed application.

## Prerequisite check

Run this before opening the agent UI:

```shell
red --agent-check
```

The check is read-only. It reports the configured or registered adapter command,
whether the executable exists, protocol versions, authentication expectations, and
whether the adapter has passed Red's reviewable-filesystem gate. It never installs,
downloads, authenticates, or exposes secrets.

The built-in registry currently contains only the development conformance fixture.
There is deliberately no production-supported adapter in this revision: the audited
Codex ACP adapter writes through its own process rather than Red's ACP client filesystem,
so presenting it as reviewable would violate the product contract. A production adapter
will be promoted only after the same live `fs/read_text_file` and `fs/write_text_file`
conformance suite passes.

Custom adapters remain available explicitly:

```toml
[agent]
command = "my-acp-adapter"
args = ["--acp"]

[agent.env]
EXAMPLE_NON_SECRET_SETTING = "value"
```

The command must already be installed and authenticated. Red does not silently fall
back to a different agent.

## Review-before-apply filesystem

For every session and file, Red retains the visible buffer revision and contents used as
the proposal base, the agent's proposed contents, pending hunks, and the originating
session/turn.

- The first read after a prompt is synchronized from Red's buffers, including unsaved
  changes.
- ACP writes update proposal state and return success. Later reads in that session see
  the proposal.
- ACP writes never mutate a visible buffer or disk.
- Saving before acceptance writes the user-visible buffer only.
- Review rebases non-overlapping user and agent edits. Overlap produces an explicit
  conflict payload and never changes text.
- Partial acceptance rebases remaining hunks. Rejecting all resets the agent-visible
  file to the current Red buffer.
- Accepted contents enter the canonical edit boundary as one transaction tagged
  `Agent { session_id, turn_id }`. Disk changes only on a later explicit save.
- Absolute, lexically normalized paths inside the session workspace are required.
  Symlink components and paths outside the workspace are rejected.
- New files remain in-memory proposals until accepted and then become unsaved buffers.
  ACP v1's current client filesystem has no delete/rename request; Red does not emulate
  those operations through writes.

## Commands and review keys

| Command | Behavior |
|---|---|
| `:AgentStart` | Start the configured adapter and create a session. |
| `:AgentPrompt` | Submit the bundled plugin's current prompt action. |
| `:AgentCancel` | Send ACP cancellation for the active session. |
| `:AgentReview` | Open the full-screen pending-proposal workspace. |
| `:AgentHistory` | Open attributed user/agent/plugin/LSP transaction history. |

`:AgentPrompt` opens a focused, wrapping picker-style composer. Enter submits the current
query, Escape cancels without prompting, and submitted prompts are retained in bounded
plugin history. Streamed updates render in a right-side conversation panel whose live
text is capped at 20,000 characters; Phase 3 persistence retains the durable transcript
separately from this bounded view model.

The review workspace is keyboard-only capable:

| Key | Behavior |
|---|---|
| `j` / `k`, arrows, Page Up/Down | Navigate files and hunks. |
| `a` / `A` | Accept selected hunk / whole file. |
| `r` / `R` | Reject selected hunk / whole file. |
| `q` / Escape | Close review without changing proposals. |

ACP permission requests open a focused chooser containing only the option IDs and labels
provided by the agent. Red returns the exact selected ID. Closing the chooser or
cancelling the prompt returns ACP `cancelled`; no plugin process allowlist is interpreted
as agent authorization.

The attributed-history workspace shows each transaction's stable ID, origin, and
before/after edit payload. `r` creates a new user-attributed revert transaction only when
the selected transaction is on the current undo branch and its post-image still matches.
Otherwise Red reports a conflict and leaves the buffer unchanged. Ordinary undo now
retains sibling branches; `g-`/`g+` choose a branch and redo traverses it.

## Off switch

```toml
disable_ai = true
```

This removes the bundled agent plugin before activation, prevents adapter startup, and
makes `red --agent-check` skip executable, authentication, and network checks. The
normal editor, LSP, and unrelated plugins remain available.
