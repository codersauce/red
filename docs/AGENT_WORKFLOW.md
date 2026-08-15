# Direct Codex workflow and safety contract

Red launches the installed Codex CLI as an app-server and speaks its JSONL
protocol directly. There is no ACP client, adapter, or companion executable.
The bundled Husk plugin owns the terminal UI; Rust core owns the Codex process,
thread and turn lifecycle, dynamic tools, paced following, saving, and attributed
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

## Inline assist

`Space i` is the limited-blast-radius path for code changes, explanations, and
inline reviews. In
Normal mode its target is exactly the current line; in Visual and Visual-line
mode its target is exactly the selection. Visual-block mode is intentionally
unsupported. The popup prefers the space below the target and moves above it
when needed. It remains inside the editor split where the request started and
never covers the rendered target. Long prompts soft-wrap and grow to six rows;
after that the prompt scrolls internally to keep the cursor visible.

Each invocation starts an ephemeral Codex thread with a read-only sandbox, no
native tools, and two submission tools. `submit_comments` leaves annotations
without editing code; `submit_replacement` submits a complete replacement and
optional annotations about the resulting code. Exactly one submission is
accepted per turn. Comment ranges are one-based and inclusive, relative to the
supplied target or replacement, and cannot escape it. Codex cannot choose a
file. Red supplies the immutable target plus at most 20 surrounding
lines on either side, rejects sensitive/ignored/out-of-workspace and binary
contexts, and accepts at most a 128 KiB complete replacement, 16 comments per
result, and 4 KiB of plain text per comment. An empty comment list is a valid
no-findings result. Follow-up
refinements reuse that ephemeral thread while the popup remains open.

Before applying a response, Red verifies the active buffer identity, revision,
range, and original text. A stale response fails without changing the buffer.
Successful output is applied only after a completed turn and full validation.
Code changes use one agent-attributed editor transaction and are deliberately
not saved. Comment-only results do not alter dirty state or text undo history.
The result controls are Enter/`k` to keep, `u` to undo the latest inline edit
and dismiss its comments (or just dismiss a comment-only result), `r` to
refine, and `A` to open the full Agent workflow. Refinement replaces only that
invocation's annotation group. Closing the popup destroys the ephemeral Codex
session; kept comments remain in the editor until dismissed or the editor
session ends.

Use `Space ] c` and `Space [ c` to navigate comments, `Space v` to read the full
message, `Space x` to dismiss one, and `Space X` to clear the current buffer.
Overlapping annotations are retained and collapsed into a numbered group; the
navigation commands select which range its gutter bracket shows. Comments
follow edits above them and are marked outdated when their referenced source
changes. They are not written into source files or persisted across restarts.

To use a Codex executable outside `PATH`:

```toml
[agent]
command = "/path/to/codex"
```

## Lifecycle

Open a workspace, press `Space A` (or run `:Agent`), type a request, and press
Enter. Red lazily starts `codex app-server --stdio`, initializes the connection,
checks the account, starts a persisted thread, and submits turns with
`turn/start`. Follow-up text and the busy indicator render before dispatch;
follow-ups submitted during an active turn appear immediately and remain queued
in FIFO order. Assistant deltas stream into the conversation footer. `Ctrl-c`
interrupts the active turn with `turn/interrupt`.

If Codex cannot start, Red preserves the prompt and offers a retry action.
Install or update Codex, run `codex login`, then retry without retyping.

The app-server process is owned by the detachable editor core, so disconnecting
and reattaching does not intentionally replace a healthy process.

Red snapshots the Codex thread ID together with a structured, clean projection
of the model-visible user and assistant messages. After an owner or machine
restart, the Agent composer remains disabled while Red starts a replacement
app-server and calls `thread/resume`. The returned turns reconcile the panel to
the history Codex actually loaded; Red's projection keeps injected editor
context out of the visible user message. If the persisted thread is missing or
cannot be loaded, Red marks the transcript as archived and makes the next prompt
start a new thread with bounded recovered context instead of pretending the old
session is live.

## Followed editing

Every Codex thread is started with:

- `sandbox = "read-only"`
- `approvalPolicy = "never"`
- no execution environments
- configured MCP servers disabled
- apps, connectors, plugins, orchestrator MCP, and notifications disabled
- hooks disabled unless the managed Codex policy requires them; when required,
  Codex may also load trusted user, workspace, or plugin hooks
- Red's bounded dynamic tools and live-edit instructions

Native command, file-change, and permission escalation requests are denied.
Red never asks Codex to edit the workspace directly.

Codex receives nine dynamic tools:

| Tool | Behavior |
| --- | --- |
| `list_files` | Lists at most 4,096 workspace files while respecting ignore files. |
| `search_files` | Searches bounded text content and returns at most 200 matches. |
| `read_file` | Reveals and reads the authoritative Red buffer, returning its revision. |
| `write_file` | Replaces revision-checked contents through Red and saves the buffer. |
| `get_editor_state` | Returns bounded active-file, cursor, selection, window, and diagnostic state. |
| `open_file` | Opens a safe workspace file in the requested split. |
| `select_text` | Creates a UTF-16-addressed editor selection. |
| `apply_edits` | Applies atomic, revision-checked UTF-16 edits and saves the buffer. |
| `run_editor_action` | Runs an allow-listed navigation or LSP action. |

Tool paths must remain below the physical workspace root. Reads and writes
reject parent traversal, symlink components, special files, unsafe roots,
oversized content, stale revisions, and overlapping edits. Reads always see the
latest visible editor contents, including unsaved user changes.

On Unix, content search uses descriptor-relative, nonblocking, no-follow reads
from the physical workspace root. It fails closed on symlinks and special files.
Content search is unavailable on platforms without that safe read boundary;
Codex must use `read_file` through Red instead.

Following is mandatory in this first iteration. Before a file tool runs, Red
opens the target, moves the cursor to the first affected range when available,
renders it, and waits briefly. The operation then passes through the editor's
transaction boundary with session/turn attribution and saves through the editor.
Tool calls remain serialized, which provides a natural future boundary for
pause, resume, and single-step controls.

## Limits and failure behavior

App-server frames are capped at 1 MiB and tool content at 960 KiB. Each turn is
limited to 32 dynamic-tool calls. File listing, search results, search bytes,
queues, and callback duration are bounded. Oversized or malformed frames stop
the Codex runtime without being rendered into the terminal.

App-server stderr is isolated from the TUI. Structured failures appear in the
conversation and status line. A stopped process is restarted and the persisted
thread is resumed when possible; otherwise the transcript becomes explicit
archived context and the submitted prompt remains available for retry.

Dynamic tools are part of Codex app-server's experimental capability surface.
Red pins a minimum tested CLI version and fails closed when the required
protocol is unavailable; it does not fall back to `codex exec` or native edits.

## Commands

| Command | Purpose |
| --- | --- |
| `Space i` | Edit the current line or visual selection in a bounded popup. |
| `:Agent` / `:AgentPrompt` | Open the prompt composer. |
| `:AgentOpen` | Show and focus the conversation pane without opening a prompt. |
| `:AgentCancel` | Interrupt the active Codex turn. |
| `:AgentClear` | Clear visible conversation while retaining current context. |
| `:AgentNew` | Close the current thread and start a new one. |
| `:AgentClose` | Hide the conversation panel without discarding state. |
| `:AgentHistory` | Inspect attributed agent transactions. |
