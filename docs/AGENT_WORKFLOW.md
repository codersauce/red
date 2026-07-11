# Agent workflow and safety contract

Red owns an ACP v1 client compiled against the official schema artifact 1.4.0. The bundled Husk plugin owns the terminal UI; the Rust core owns adapter lifecycle, filesystem policy, proposal state, review, and attributed application.

## Quick start

Open a workspace, press `Space A` (or run `:Agent`), type a request, and press Enter. Red starts the configured adapter when the first prompt is submitted, streams the response, and announces when proposed changes are ready. Run `:AgentReview`, accept or reject the changes, and save accepted buffers when ready.

If the adapter cannot start, Red preserves the prompt and opens a compact setup chooser. It can open a masked, session-only OpenAI API-key prompt, select the bundled reviewable Codex bridge, retry, or cancel without losing the prompt. The Codex bridge uses an installed, authenticated `codex` CLI (`codex login`), reads Red's unsaved buffers, and stages writes as proposals. API keys entered in this flow are never written to configuration, transcripts, or logs. `red --agent-check` remains available for an offline prerequisite report.

## Prerequisite check

Release archives include the first-party `red_openai_acp` and `red_codex_acp` executables. The embedded default configuration selects `adapter = "openai"`; the setup chooser can select the Codex bridge for the current session. Red discovers companions beside its own executable before falling back to `PATH`, so an extracted archive works without changing the shell path. To check the configured adapter before opening the agent UI:

```shell
export OPENAI_API_KEY='...'
red --agent-check
red --agent-check --strict # optional non-zero readiness gate for scripts
```

The check reports the configured or registered adapter command, whether the executable exists and is marked runnable, protocol versions, authentication expectations, and whether the adapter has passed Red's reviewable-filesystem gate. It is entirely offline: it checks only that `OPENAI_API_KEY` is non-empty and does not validate the credential, model entitlement, or network connectivity. It never prints, persists, authenticates, installs, or downloads credentials. `--strict` makes a not-ready result exit non-zero while retaining the full diagnostic output. Prefer the environment or the masked session-only prompt to placing credentials in plaintext configuration.

The first-party OpenAI adapter uses the Responses API and defaults to `gpt-5.6-terra`; set `RED_OPENAI_MODEL` to choose another Responses-compatible model. `RED_OPENAI_BASE_URL` defaults to `https://api.openai.com/v1`; other HTTPS hosts require the explicit `RED_OPENAI_ALLOW_CUSTOM_HOST=true` opt-in because credentials and workspace context will be sent there. Embedded URL credentials/query/fragment are rejected, and HTTP is permitted only for loopback test servers. Redirects and environment-configured proxies are disabled so an API key cannot be forwarded to an unexpected host.

The bundled `red_codex_acp` bridge starts the installed Codex app-server without native execution or patch tools and exposes bounded workspace tools that route reads and writes through Red's ACP filesystem callbacks. It requires `codex` on `PATH` and a completed `codex login`; unsaved buffers remain visible and every write remains a reviewable proposal. Set `[agent] adapter = "codex"` to make this backend the default.

The adapter exposes four bounded tools. `list_files` respects ignore files, does not follow links, rejects a starting root that is a symlink or non-directory, stops after 65,536 entries or five seconds, and returns at most 4,096 paths. Listing is path-based, so replacing the workspace root concurrently can affect returned names; it never returns file contents. On Unix, `search_files` is read-only, scans at most 32 MiB of small text files off the editor thread using component-wise no-follow bounded reads, and returns at most 200 matching lines; on other platforms content search fails closed because portable reparse-point-safe reads are unavailable. Search results reflect saved disk contents; the model is instructed to call `read_file` before reasoning about or editing a file so unsaved buffers and proposals remain authoritative. `read_file` and `write_file` always call `fs/read_text_file` and `fs/write_text_file`; they reject parent traversal, out-of-workspace paths, symlink components, and contents above 960 KiB. Writes become proposals and never fall back to direct disk access, including when the editor rejects a write. Follow-up reads see the proposal through Red's ACP host.

Responses requests and bodies are capped at 2 MiB, ACP frames at 1 MiB (including JSON escaping and newline), tool arguments/content at 960 KiB, conversation history at 256 KiB, active sessions and callbacks at 64, and each turn at 12 tool rounds or 32 calls. Complete response, tool, and encrypted-reasoning items are retained across turns; history trims only whole turns so a function call or reasoning item is never orphaned, and starts a fresh context if a single turn cannot fit the cap. An in-flight API request, client callback, or workspace search is interrupted by ACP cancellation. The adapter does not log prompts, file paths or contents, tool arguments/results, raw API bodies, or credentials.

The separate Codex adapter in the official ACP registry and other third-party bridges remain usable only as explicitly configured custom adapters. Unlike Red's bundled Codex bridge, they are not marked reviewable: adapters that write directly, approve an underlying patch after staging, or fall back to local file I/O when a client callback fails can bypass Red's proposal boundary. The built-in conformance fixture remains available for development and is not a production agent.

Custom adapters remain available explicitly:

```toml
[agent]
command = "my-acp-adapter"
args = ["--acp"]

[agent.env]
EXAMPLE_NON_SECRET_SETTING = "value"
```

The command must already be installed and authenticated. Red does not silently fall back to a different agent, and overriding the built-in command makes `--agent-check` report that reviewable-edit readiness must be independently validated.

Adapter stderr is isolated from the terminal UI so a failed backend cannot overwrite the editor screen or leak raw diagnostics into an active session; structured ACP errors remain visible in the conversation and status line.

ACP transport is newline-delimited JSON with a 1 MiB maximum message size, including
the terminating newline, enforced in both directions. In-flight requests are bounded
by the configured queue capacity (32 by default). Setup and control requests time out
after 30 seconds by default; prompt responses have no turn deadline and remain active
until the adapter responds or the user explicitly cancels. Each write to adapter stdin
is bounded to five seconds, and shutdown flushes are bounded to two seconds, so a
non-reading adapter cannot stall the editor. Proposal-file content is limited to
960 KiB so ACP envelopes retain headroom and oversized on-disk files are rejected
before loading or serialization.

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
- Absolute, lexically normalized paths inside the session workspace are required. On
  Unix, unopened files are read through a stable workspace-directory handle with
  component-wise no-follow opens; on other platforms unopened disk reads fail closed
  until the file is opened in Red. Symlink components and paths outside the workspace
  are rejected.
- New files remain in-memory proposals until accepted and then become unsaved buffers.
  ACP v1's current client filesystem has no delete/rename request; Red does not emulate
  those operations through writes.

## Commands and review keys

| Command | Behavior |
|---|---|
| `:Agent` / `Space A` | Open the composer; the first submitted prompt starts the configured adapter automatically. |
| `:AgentStart` | Start the configured adapter and create a session. |
| `:AgentPrompt` | Open the composer and start the adapter automatically on first submit. |
| `:AgentCancel` | Send ACP cancellation for the active session. |
| `:AgentReview` | Open the full-screen pending-proposal workspace. |
| `:AgentHistory` | Open attributed user/agent/plugin/LSP transaction history. |

`:Agent` and `:AgentPrompt` open a focused, wrapping multiline composer. Enter submits the current prompt, `Ctrl-j` or Shift-Enter inserts a newline, and Escape or `Ctrl-c` cancels without prompting. `Ctrl-p` and `Ctrl-n` recall the previous and next prompt while preserving the current draft; the plugin persists a deduplicated, 50-entry prompt history. Prompts are limited to 128 KiB to stay within the ACP frame limit, and an oversized paste preserves the current draft with a clear validation message. Composer input is excluded from performance traces, macro recording, and unrelated plugins. Streamed chunks are coalesced before rendering in the right-side conversation panel, whose live text is capped at 20,000 characters; Phase 3 persistence retains the durable transcript separately from this bounded view model. When the adapter proposes an edit while review is closed, Red prints the pending file/hunk count and an actionable `:AgentReview` reminder; proposals remain isolated from buffers and disk until explicitly accepted.

The conversation keeps each user, agent, and error turn as a source-backed text block. Agent turns render GitHub-flavored Markdown with readable headings, paragraphs, nested and task lists, quotes, links, inline and fenced code, and responsive tables. Content wraps to the actual panel width and reflows when the terminal is resized; wide tables use aligned columns and narrow panels fall back to labeled records instead of clipping values. New output follows the tail until the user scrolls away, so reading earlier content is stable while an answer streams. Successful `end_turn` completions quietly finish the turn; interruptions and other stop reasons remain visible.

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

Core session snapshots preserve the transcript and pending proposal workspace. After
`red --resume`, the transcript is archived context unless the adapter negotiated an ACP
session load/resume capability; Red never claims to have resumed a process it could not
resume. See [`SESSION_RECOVERY.md`](SESSION_RECOVERY.md).

## Off switch

```toml
disable_ai = true
```

This removes the bundled agent plugin before activation, prevents adapter startup, and
makes `red --agent-check` skip executable, authentication, and network checks. The
normal editor, LSP, and unrelated plugins remain available.
