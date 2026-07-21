# Direct Codex workflow and safety contract

Red launches the installed Codex CLI as an app-server and speaks its JSONL
protocol directly. There is no ACP client, adapter, or companion executable.
The bundled Husk plugin owns the terminal UI; Rust core owns the Codex process,
thread and turn lifecycle, dynamic tools, proposal state, review, and attributed
application.

## Prerequisites

Install Codex CLI 0.144.1 or newer and authenticate it:

```shell
codex login
red --agent-check --strict
```

The check is offline. It locates `codex`, reads `codex --version`, and reports
whether the installed version supports Red's app-server contract.
Authentication is verified by `account/read` when the first session starts.

To use a Codex executable outside `PATH`:

```toml
[agent]
command = "/path/to/codex"
```

## Lifecycle

Open a workspace, press `Space A` (or run `:Agent`), type a request, and press
Enter. Red lazily starts `codex app-server --stdio`, initializes the connection,
checks the account, starts an ephemeral thread, and submits turns with
`turn/start`. Follow-up text and the busy indicator render before dispatch;
follow-ups submitted during an active turn appear immediately and remain queued
in FIFO order. Assistant deltas stream into the conversation footer. `Ctrl-c`
interrupts the active turn with `turn/interrupt`.

If Codex cannot start, Red preserves the prompt and offers a retry action.
Install or update Codex, run `codex login`, then retry without retyping.

The app-server process is owned by the detachable editor core, so disconnecting
and reattaching does not intentionally replace a healthy process.

## Reviewable editing

Every Codex thread is started with:

- `sandbox = "read-only"`
- `approvalPolicy = "never"`
- no execution environments
- configured MCP servers disabled
- apps, connectors, plugins, orchestrator MCP, and notifications disabled
- hooks disabled unless the managed Codex policy requires them; when required,
  Codex may also load trusted user, workspace, or plugin hooks
- Red's bounded dynamic tools and reviewable-edit instructions

Native command, file-change, and permission escalation requests are denied.
Red never asks Codex to edit the workspace directly.

Codex receives nine dynamic tools:

| Tool | Behavior |
| --- | --- |
| `list_files` | Lists at most 4,096 workspace files while respecting ignore files. |
| `search_files` | Searches bounded text content and returns at most 200 matches. |
| `read_file` | Reads through Red so unsaved buffers and staged proposals are authoritative. |
| `write_file` | Stages complete contents in the proposal workspace without touching disk. |
| `get_editor_state` | Returns bounded active-file, cursor, selection, window, and diagnostic state. |
| `open_file` | Opens a safe workspace file in the requested split. |
| `select_text` | Creates a UTF-16-addressed editor selection. |
| `apply_edits` | Stages atomic, revision-checked UTF-16 edits as a proposal. |
| `run_editor_action` | Runs an allow-listed navigation or LSP action. |

Tool paths must remain below the physical workspace root. Proposal reads and
writes reject parent traversal, symlink components, special files, unsafe roots,
oversized content, stale revisions, and overlapping edits. Later reads in the
same session see staged proposal contents.

On Unix, content search uses descriptor-relative, nonblocking, no-follow reads
from the physical workspace root. It fails closed on symlinks and special files.
Content search is unavailable on platforms without that safe read boundary;
Codex must use `read_file` through Red instead.

Run `:AgentReview` to inspect pending files and hunks. Accepting a proposal
passes through the editor's transaction boundary and receives agent attribution.
Rejecting it discards only the selected proposal. Unaccepted proposals never
mutate a visible buffer or disk.

## Limits and failure behavior

App-server frames are capped at 1 MiB and tool content at 960 KiB. Each turn is
limited to 32 dynamic-tool calls. File listing, search results, search bytes,
queues, and callback duration are bounded. Oversized or malformed frames stop
the Codex runtime without being rendered into the terminal.

App-server stderr is isolated from the TUI. Structured failures appear in the
conversation and status line. A stopped process archives pending proposals and
preserves the submitted prompt for retry.

Dynamic tools are part of Codex app-server's experimental capability surface.
Red pins a minimum tested CLI version and fails closed when the required
protocol is unavailable; it does not fall back to `codex exec` or native edits.

## Commands

| Command | Purpose |
| --- | --- |
| `:Agent` / `:AgentPrompt` | Open the prompt composer. |
| `:AgentOpen` | Show and focus the conversation pane without opening a prompt. |
| `:AgentCancel` | Interrupt the active Codex turn. |
| `:AgentClear` | Clear visible conversation while retaining current context. |
| `:AgentNew` | Close the current thread and start a new one. |
| `:AgentClose` | Hide the conversation panel without discarding state. |
| `:AgentReview` | Review pending proposal files and hunks. |
| `:AgentHistory` | Inspect attributed accepted/rejected transactions. |
