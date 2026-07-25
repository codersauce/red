# Direct Codex workflow, modes, and safety contract

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

Agent defaults and the Codex executable are configurable:

```toml
[agent]
mode = "agent"              # agent, plan, or review
entry = "float"             # float or dock
position = "right"          # left, right, top, or bottom
width_percent = 38
height_percent = 35
responsive = true
persistent_threads = true
# model = "gpt-5.4"
# reasoning_effort = "high"
# command = "/path/to/codex"
```

## Lifecycle

Open a workspace, press `Space A` (or run `:Agent`), and type the first request
in the floating, buffer-backed composer. `Ctrl+Enter` sends from any composer mode;
`Alt+Enter` also sends when the terminal reports that key combination. In
Insert mode, `Enter`, `Shift+Enter`, and `Ctrl+J` insert a newline. In Normal
mode, `Enter` sends. `Esc` enters Normal mode without discarding the draft, so
`Esc`, then `Enter` is the universal send sequence when a terminal cannot
distinguish modified Enter. Set `entry = "dock"` to open and focus the
persistent dock immediately instead. Both composers use the editor's
configured `[cursor]` shapes for their own Insert, Normal, and Visual modes,
independently of the background editor.

`Ctrl+S` keeps its normal editor meaning: save the active file. It is not an
agent send shortcut.

Red lazily starts `codex app-server --stdio`, initializes the connection,
checks the account, resumes the saved workspace thread when possible, and
submits turns with `turn/start`. An expired saved thread falls back once to a
new thread while retaining the current mode and prompt. After the first
submission, the conversation opens in the configured dock. Follow-ups use the
same real modal composer and are held in a bounded FIFO queue; run
`:AgentSteer` to add instructions directly to an active turn.
Assistant deltas and real tool progress stream into the conversation. `Ctrl-c`
interrupts the active turn with `turn/interrupt` without discarding the thread.

The conversation has its own reading cursor and scroll position. In the
conversation body, use `j`/`k` or the arrow keys to read, `Ctrl+F`/`Ctrl+B`
to page, and `g`/`G` to reach the beginning or end. `Tab` and `Shift+Tab`
select links; `Enter` opens the selected link. Press `i` or `a`, or click the footer
to return to the composer. `Esc` inside the composer enters Normal mode;
`Ctrl+C` leaves the composer while preserving its draft. In conversation
reading mode, `Esc` returns focus to the editor.

With the conversation focused, use `Ctrl+W H`, `Ctrl+W J`, `Ctrl+W K`, or
`Ctrl+W L` to move it to the left, bottom, top, or right, just as with an
ordinary editor window. `:AgentLeft`, `:AgentBottom`, `:AgentTop`, and
`:AgentRight` remain available. Moving the conversation preserves its draft,
history, reading cursor, and focus; adaptive layout preserves usable editor
space on narrow terminals. The conversation and floating prompt use the
editor's background, with theme color confined to message text, separators,
status, and the shortcut strip.

If Codex cannot start, Red preserves the prompt and offers a retry action.
Install or update Codex, run `codex login`, then retry without retyping.

The app-server process is owned by the detachable editor core, so disconnecting
and reattaching does not intentionally replace a healthy process.

## Native Agent, Plan, and isolated Review modes

The default `mode = "agent"` uses the effective configuration of the installed
Codex CLI. Native commands, direct workspace edits, configured MCP servers,
apps, connectors, plugins, skills, and hooks are available only when the
user's Codex configuration and managed policy allow them. Red does not widen
the Codex sandbox or auto-accept an approval. Native command, file-change, and
permission requests are presented to the user with the exact choices supplied
by Codex; closing an approval without choosing denies it. Completed native file
changes reload clean open buffers and update their editor and LSP state. Dirty
buffers are never overwritten: Red retains the unsaved contents and reports the
file conflict in the conversation.

Set `mode = "plan"` to request Codex's planning collaboration mode. Set
`mode = "review"` to restore Red's strictly isolated editing contract. Review
mode starts each Codex thread with:

- `sandbox = "read-only"` and `approvalPolicy = "never"`;
- no native execution environments;
- configured MCP servers, apps, connectors, and plugins disabled;
- hooks disabled unless managed Codex policy requires trusted hooks; and
- Red's bounded dynamic tools and reviewable-edit instructions.

In Review mode, native command, file-change, and permission requests are
denied. Red's write tools stage proposals; they do not edit workspace files.

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
passes through the editor's transaction boundary and receives agent
attribution. Rejecting it discards only the selected proposal. Unaccepted
review-mode proposals never mutate a visible buffer or disk. Native Agent
mode can separately perform direct edits according to Codex's configured
sandbox and approval policy.

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
protocol is unavailable; it does not silently fall back to `codex exec`.

## Commands

| Command | Purpose |
| --- | --- |
| `:Agent` / `:AgentPrompt` | Open the prompt composer. |
| `:AgentOpen` | Show and focus the conversation pane without opening a prompt. |
| `:AgentLeft` / `:AgentRight` | Dock the conversation beside the editor. |
| `:AgentTop` / `:AgentBottom` | Dock the conversation above or below the editor. |
| `:AgentModels` | List models available to the active Codex session. |
| `:AgentSessions` | List resumable conversations for the workspace. |
| `:AgentSteer` | Add instructions directly to a running agent turn. |
| `:AgentCancel` | Interrupt the active Codex turn. |
| `:AgentClear` | Clear visible conversation while retaining current context. |
| `:AgentNew` | Close the current thread and start a new one. |
| `:AgentClose` | Hide the conversation panel without discarding state. |
| `:AgentReview` | Review pending proposal files and hunks. |
| `:AgentHistory` | Inspect attributed accepted/rejected transactions. |
