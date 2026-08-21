# Direct Codex workflow and safety contract

Red launches the installed Codex CLI as an app-server and speaks its JSONL
protocol directly. There is no ACP client, adapter, or companion executable.
The bundled Husk plugin owns the terminal UI; Rust core owns the Codex process,
thread and turn lifecycle, dynamic tools, optional followed playback, saving, and attributed
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
Normal mode its target is the enclosing function when syntax information is
available, otherwise the current line. The popup identifies the chosen range.
In Visual and Visual-line mode its target is exactly the selection. Visual-block mode is intentionally
unsupported. The popup prefers the space below the target and moves above it
when needed. It remains inside the editor split where the request started and
avoids the rendered target when space permits. For a function that fills the
viewport, the popup remains usable near its source anchor. Long prompts use the
Agent composer's word-aware wrapping and grow to six rows; after that the prompt
scrolls internally to keep the cursor visible. Up/Down move through visual rows,
and clicking prompt text places the cursor there. Wrapping and resizing never
change the submitted text.

Each invocation starts an ephemeral Codex thread with a read-only sandbox, no
native tools, four bounded project-reading tools, and four submission tools.
`list_files`, `search_files`, and `read_file` inspect the same workspace without
opening files or moving focus. Unsaved buffers take precedence over disk.
`read_git_diff` compares one tracked file at `HEAD` with its current buffer,
including unsaved changes. The response identifies the exact base commit.
Reading more context does not expand the editable target. File reads are limited
to 200 lines and 32 KiB per response, and files larger than 512 KiB are omitted.
Results report truncation.
Ignored, binary, symlinked, and out-of-workspace files are excluded. Secret-like
filenames are excluded by default and can be included explicitly with
`[agent] allow_sensitive_paths = true`.
On platforms without the safe on-disk reader, open-buffer reads remain available
and other disk reads fail closed.

`submit_comments` leaves annotations
without editing code; `submit_replacement` submits a complete replacement and
optional annotations about the resulting code. `request_agent` returns an
actionable broader-scope result without editing code or replacing earlier
annotations. Exactly one submission is
accepted per turn. Comment ranges are one-based and inclusive, relative to the
supplied target or replacement, and cannot escape it. Codex cannot choose a
file. Red supplies the immutable target plus at most 20 surrounding
lines on either side and a bounded recap of the current Agent conversation
when it belongs to the same workspace. Current buffer contents remain
authoritative. Red rejects disallowed sensitive, ignored, out-of-workspace, and binary
contexts, and accepts at most a 128 KiB complete replacement, 16 comments per
result, and 4 KiB of plain text per comment. An empty comment list is a valid
no-findings result. Follow-up
refinements reuse that ephemeral thread while its job remains available.

### Wider same-file proposals

An invocation started from a cursor may use `propose_expanded_replacement`
when a local refactor needs a larger range in the same file. An explicit visual
selection remains exact, including after continuation or recovery. The proposal
must contain and extend the original target, identify the editor revision, and
include the exact original text. It cannot choose another file. Red verifies
the source and retains at most 64 KiB of original text and a 128 KiB replacement.

A wider proposal is never applied automatically, even with its popup open.
The source marker says **Review wider edit**. Enter or `v` opens the full diff,
with the original target, proposed range, and reason. `a` in that review
approves one unsaved, undoable editor transaction. Enter does not approve. `d` in the review or result popup
declines the proposal; Esc hides it. Both keep the discussion in InlineHistory.
Changed source disables approval and requires a recheck. Rechecking an
unapproved proposal starts from the original target, not the proposed range.
Pending proposals survive normal recovery, but must still pass exact-source
checks and explicit review. Multi-file work continues through Agent.
The review renders explanatory Markdown separately from the exact source diff.
Added and removed rows use Red's Git diff colors; code uses the file's installed
language highlighter. The applied-change view uses the same rendering, including
verbatim indentation and readable wrapping in narrow windows.

### Applying inline results

Before applying a response, Red verifies the active buffer identity, revision,
range, and original text. A stale response fails without changing the buffer.
By default, a code-changing result confined to the exact requested target
applies immediately when its original popup is still in the foreground. Set
`[agent] auto_apply_inline_edits = false` to require review for those results.
Background, stale, and wider-scope results always wait for review. Enter opens a
staged diff; `a` applies that reviewed request, `d` declines it, and Esc returns
or hides it. Enter in the diff does not apply it. Successful output is applied
only after a completed turn and full validation.
Code changes use one agent-attributed editor transaction and are deliberately
not saved. Comment-only results do not alter dirty state or text undo history.
After a code edit, Red retains an editor-generated **Applied** summary even when
the assistant supplied no comments. It shows the number of changed locations and
whether the buffer is unsaved. Click it, use `Space v`, or open the request in
InlineHistory to see that edit's exact before/after diff. `[` and `]` move between
changed locations while the retained source still matches. `u` undoes the edit
only when its transaction is still the latest change; later work is never silently
discarded. `Space x` hides the summary, and `p` in InlineHistory restores it.
Applying and declining also produce actionable bottom-line notices. Completed
edits remain available after recovery; no provider session is needed.

The result controls are Esc to close (Enter/`k` are compatibility aliases), `u` to undo the latest inline edit
and dismiss its comments (or just dismiss a comment-only result), `r` to
refine, `v` to read the full answer or applied diff, `p` to pin its annotations,
and `A` to prepare a full Agent
draft containing the latest request, source location, and earlier inline
discussion. Nothing is sent automatically; replacing an unsent Agent draft
requires confirmation and is undoable. Refinement replaces only that
invocation's visible annotation group. Earlier turns remain in Inline History.
Clicking away or pressing Esc closes an empty inline prompt immediately. If it
contains text, choose Delete (the default), Edit, or Save draft. Edit returns to
the same prompt; Delete affects only unsent text, not earlier results. Closing a
working popup still hides it without cancelling the job. A source-anchored activity marker shows
an animated working spinner, ready, or stopped status. Click it, or use `Space v` on its source line,
to reopen. `Ctrl-c` opens the same draft choices or cancels a running
request. Several requests can run independently. Explanations and comment-only
results appear automatically when their source still matches, without changing
source or focus. Proposed code edits completed while hidden are retained as
**ready**; reopen, use Enter or `v` to inspect, and `a` in the diff to apply. Changed source disables application and offers a
fresh recheck or Agent handoff. Kept comments remain until hidden or resolved.

`Space H` opens the unified bottom-docked InlineHistory panel. It includes
running jobs, ready edits, saved drafts, and completed discussions. Unsubmitted
drafts live for this editor session; submitted questions and completed results
use normal history recovery. `:InlineActivity` remains a compatibility alias
for `:InlineHistory`; there is no separate activity picker.

When a hidden request finishes, the bottom message line shows its outcome and
a clickable file/range. Click that location, press `Space N` in normal mode,
or run `:InlineLast` to reopen it. The notice lasts 12 seconds once shown; the shortcut
still opens the latest completion afterward. Existing errors and command input
take precedence. Notifications never move focus or apply a proposed edit.

Use `Space ] c` and `Space [ c` to navigate comments across the file,
`Space v` to read the full message, `Space x` to dismiss
one, and `Space X` to clear the current buffer. Overlapping annotations are
retained and collapsed into a group. `‹ 2/4 ›` means the second of four
overlapping items is current, not a progress count; `✓ Done` is that request's
completion status. Click `‹` / `›`, or use `[ i` / `] i`, to cycle
only that group. Click the count to choose from the group's titles. Clicking
ordinary card text focuses a compact action view; Enter expands it and Esc
returns to the editor. Left/Right or `h`/`l` cycle that location's items in the
focused card, full viewer, or result popup; `[` / `]` remain aliases. These keys
remain ordinary text/cursor input while composing a reply. `i` asks a new inline
question about that exact comment, `A` asks Agent, `r` refines its discussion,
`x` dismisses the comment, and `d` resolves its discussion. Each pane remembers
its current item separately, highlights its card and connector, and advances
to the next overlapping item after dismissal. Hovering does not change it.
Opening an item from History makes its annotations current. Comments
follow edits above them and are marked outdated when their referenced source
changes. They are never written into source files. Hiding or clearing comments
does not delete the question or answer.

### Inline history

Run `:InlineHistory` or press `Space H` for retained questions, answers, edits,
and outcomes. The panel starts with the workspace; `w` switches to the current
file. Running jobs and ready edits come first, with live status and answer
updates. Rows include their file/range and a source snippet. `j`/`k` or the
mouse previews items, `l` expands earlier turns, `h` collapses them, and `/`
fuzzy-searches their text. Enter reopens the conversation or draft; `g` keeps
the selected source location without opening a dialog. Esc restores the original
location and comment visibility. Browsing never applies an edit.
The embedded detail pane renders Markdown, syntax-highlighted source, and the
same colored diffs as the full review popup. `v` cycles Conversation, Reviewed
code, Before, Compare, and Changes. Applied/unsaved/source-state labels use
semantic colors. Click the underlined workspace-relative location (or use `g`)
to jump to its tracked range; detached ranges are not presented as live links.
File references in the answer are also clickable. The mouse wheel scrolls the
detail under the pointer, while `j`/`k` continue browsing requests. Narrow panes
show one selected request and its position in the list, leaving room for details.
The conversation view also lists successful context reads, including editor
revisions and Git base commits, so the answer's sources are inspectable.

Press `p` to pin the previewed turn's annotations and return to
the source. This also reopens a resolved discussion. It restores only annotations,
never reruns the agent or reapplies a code edit. Changed source is marked outdated;
deleted or ambiguous ranges remain in history. The displayed turn is remembered
across normal recovery, including when you choose an older answer.

In an inline prompt, Ctrl-P/Ctrl-N recall submitted prompts from this
workspace. Moving forward past the newest entry restores the unsent draft.
Prompt recall uses retained inline history and survives normal session recovery.

“Continue in Agent” explicitly reveals the conversation pane, leaves the editor
in normal mode, and loads a reviewable draft. A saved hidden pane or editor zoom
does not block it. Replacing an unsent Agent draft still requires confirmation;
nothing is submitted automatically.

When that draft is sent, its `Red inline history reference` links the Agent turn
back to the original inline request. Keep that reference line if you edit the
draft. `Space H` then shows the turn's actual editor writes grouped by file,
with saved/unsaved or changed-since status and clickable locations. Enter opens
the retained, syntax-colored review: `[` / `]` move between changed locations,
and `f` / `F` move between files. The Changes view shows all retained diffs.
Each affected open file gets a change marker; `Space x` or `Space X` hides it,
and History `p` restores it without applying anything. A completion notification
and `Space N` reopen the review even after navigating away.

These receipts compare the exact text before and after Agent editor-tool writes,
not the whole Git diff. Consecutive writes may be combined; interleaved user
edits remain separate. Agent writes retain their existing save-to-disk behavior.
Cancellation or failure does not roll back already-applied writes, and the
receipt says so. Historical review never applies, saves, or bulk-undoes files.

Within history, `v` cycles through the conversation, reviewed source, before-edit
source, and a reviewed/current comparison. `Ctrl-d`/`Ctrl-u` or Page Down/Up
scroll the detail. `r` continues the selected discussion, while `R` prepares a
recheck against current source. Existing running or ready jobs are reopened;
otherwise a new ephemeral provider thread uses bounded recovered conversation
context and the usual fresh-target guards.
Detached source must be selected again explicitly in the editor.

`d` toggles resolved state. `D` asks before forgetting the whole conversation.
`:InlineHistoryExport path.json` writes a new local JSON export without
overwriting an existing file. Exports contain prompts and reviewed source;
treat them as private workspace data.

History belongs to the editor core and is included in normal recovery snapshots.
Set top-level `persist_inline_history = false` to keep it in memory only. Pending
requests become cancelled on recovery; ready results remain available for
inspection and guarded application. Red never pretends an ephemeral provider
thread survived a restart. The store is bounded to 32 MiB and refuses new turns
before silently dropping old ones. Large repeated annotation source snapshots
are content-addressed. User-visible assistant prose is retained up to 64 KiB
per turn, with an explicit truncation marker.

Each turn and each comment has its own tracked source location. History labels
source as unchanged, changed, or detached; unchanged means matching source text,
not that an old answer is semantically guaranteed correct. Deleted or ambiguous
targets remain readable in history without an arrow on unrelated code.

To use a Codex executable outside `PATH`:

```toml
[agent]
command = "/path/to/codex"
```

## Lifecycle

In the Agent prompt and conversation footer, Enter sends. Alt+Enter,
Shift+Enter, and Ctrl+J insert a newline; Ctrl+Enter remains a send alias.
See [keyboard compatibility](KEYBOARD.md) if a terminal does not report a
modified Enter distinctly.

The docked composer starts with three input rows and grows with its wrapped
draft, up to roughly 70% of the Agent pane. Longer drafts scroll within that
space. It shrinks again when text is removed or sent, and short panes always
retain room for the conversation.

Open a workspace, press `Space A` (or run `:Agent`), type a request, and press
Enter. Red lazily starts `codex app-server --stdio`, initializes the connection,
checks the account, starts a persisted thread, and submits turns with
`turn/start`. Follow-up text and the busy indicator render before dispatch;
follow-ups submitted during an active turn appear immediately and remain queued
in FIFO order. Assistant deltas stream into the conversation footer. `Ctrl-c`
interrupts the active turn with `turn/interrupt`.

`Tab` or `Shift-Tab` switches between the composer and transcript, preserving
the draft, composer editing mode, and transcript reading position. Switching
away from an unfinished transcript search cancels it and restores its starting
position. In transcript Normal mode, `]l` jumps to the next link and `[l` to the
previous one, wrapping at the ends. Enter opens the link under the cursor.

In the conversation transcript's Normal mode, `[p` jumps backward to a user
prompt and `]p` jumps forward. From an answer, the first backward jump returns
to that turn's prompt; repeating it visits earlier prompts. Jumps reveal the
prompt card, update its accent, and pause automatic scrolling without changing
the composer draft. `G` returns to the latest output and resumes following it.

While a turn runs, the status row names the current operation and shows elapsed
time, with a blank row separating it from the transcript. Assistant messages
stream without repeated role headings. Tool calls are grouped into one compact
`Activity · N actions · N issues` disclosure per turn. On completion, that
summary stays above the final answer and shows an issue count instead of full
tool errors. A quiet `Worked for …` footer appears below the answer. Errors that
stop the request still appear separately. Click a summary (or move to it with
`[l` / `]l` and press Enter) to expand that turn's five most recent actions.
Select **View all details** to inspect full paths and bounded error text.
**Activity** in the pane header and `:AgentActivity` toggle the latest turn.
**New** starts a fresh Codex session and focuses the pane composer directly,
without opening another ask popup. Details are
bounded and retained for the current conversation view; restored conversations
may contain only their saved summaries. Raw file contents are not shown in the
activity log. Scrolling back continues to pause automatic tail-following.

`/` searches forward through visible prompt and answer text; `?` searches
backward. Search is literal and case-sensitive, previews matches as you type,
and shows a result count. Enter keeps the result, while Escape cancels an
unfinished search and restores its starting position. `n` repeats the search
direction and `N` reverses it, wrapping at the ends. After a completed search,
Escape hides its highlights; `n` or `N` brings them back. Transcript search is
independent of the composer's draft and prompt-local search.

Press `m` in transcript Normal mode for the selected turn's actions: copy its
prompt, copy its answer, or reuse the prompt in the composer. Reuse only loads
text for editing; it never submits. If an unsent draft would be replaced, Red
asks first and defaults to keeping it. An approved replacement is one undoable
composer edit, so Escape followed by `u` restores the previous draft. `y` copies
the selected turn's answer; `Y` still copies the whole conversation.

With a transcript selection (`v`, `V`, or a mouse drag), `y` copies and keeps
your reading position. Enter copies the same selection, returns to the latest
output, resumes tail-following, and focuses the composer without changing or
sending its draft.

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
- configured MCP servers disabled unless named in `agent.enabled_mcp_servers`
- apps, connectors, plugins, remote plugins, skill MCP dependency installation,
  and orchestrator MCP disabled unless named in `agent.enabled_codex_features`
- notifications disabled
- hooks disabled unless the managed Codex policy requires them; when required,
  Codex may also load trusted user, workspace, or plugin hooks
- Red's bounded dynamic tools and live-edit instructions

Native command, file-change, and permission escalation requests are denied.
Red never asks Codex to edit the workspace directly.

Codex receives twelve dynamic tools:

| Tool | Behavior |
| --- | --- |
| `list_files` | Lists sorted workspace files in pages of up to 4,096 while respecting ignore and sensitive-path policy; `next_offset` continues the walk result. |
| `search_files` | Searches bounded text content, reports truncation, and returns at most 200 matches. |
| `read_file` | Reads an authoritative Red-buffer page of up to 1,000 lines and 256 KiB, returning its revision and `next_line`. Continuations must pass the first page's revision and restart if it changes; a single line over 256 KiB returns an explicit error rather than partial source. |
| `write_file` | Replaces revision-checked contents through Red, creates missing parent directories, and saves the buffer. |
| `create_directory` | Creates a workspace directory and missing parents; an existing directory is a successful no-op. |
| `get_editor_state` | Returns bounded active-file, cursor, selection, window, diagnostic, and current-annotation state. |
| `open_file` | Opens a safe workspace file in the requested split. |
| `select_text` | Creates a UTF-16-addressed editor selection. |
| `apply_edits` | Applies atomic, revision-checked UTF-16 edits, creates missing parent directories, and saves the buffer. |
| `add_annotations` | Adds up to 16 revision-checked source annotation cards without editing or saving the file. |
| `dismiss_annotations` | Hides visible annotation cards by stable ID without deleting source or conversation history. |
| `run_editor_action` | Runs an allow-listed navigation or LSP action, including annotation traversal and overlap cycling. |

Agent annotations use zero-based, inclusive file line ranges. They share inline
assist's source anchors, overlap projection, stale-source indicator, cards, and
keyboard controls. The first added annotation becomes current. The
`annotations` object returned by `get_editor_state` reports the visible count
and the current card's stable ID, line range, message, provenance, and stale
state. `next_annotation` and `previous_annotation` walk the active file;
`next_overlapping_annotation` and `previous_overlapping_annotation` cycle the
cards at the current source location. Agent-created cards survive normal session
recovery with their IDs and tracked locations. They do not change buffer dirty
state, text revision, undo history, or files on disk.

Each item returned by `add_annotations` includes a canonical
`red://annotation/<uuid>` `href`. Agent responses can use that exact value as a
Markdown destination to connect explanatory prose to a particular card. Link
activation is resolved as a typed internal target: Red switches to the card's
live buffer, moves to its tracked source anchor, and opens the card. Dismissed
or otherwise unavailable annotations report a quiet status message and never
fall through to file-path or external-URL handling.

Tool paths must remain below the physical workspace root. Reads and writes
reject parent traversal, symlink components, special files, unsafe roots,
oversized content, stale revisions, and overlapping edits. Secret-like paths
also require `[agent] allow_sensitive_paths = true`. Reads always see the latest
visible editor contents, including unsaved user changes.

Directory creation is confined to the same workspace and rejects ignored,
protected, symlinked, and non-directory paths. It creates at most 64 path
components, leaves existing directories untouched, and does not switch buffers.
On Unix, creation uses directory-relative, no-follow operations. Platforms
without that secure boundary return an explicit unsupported-operation error
when new directories are needed. Directory creation does not grant shell access,
and directories are not removed automatically if a later file save fails.

On Unix, content search uses descriptor-relative, nonblocking, no-follow reads
from the physical workspace root. It fails closed on symlinks and special files.
Content search is unavailable on platforms without that safe read boundary;
Codex must use `read_file` through Red instead.

Tool calls remain serialized. By default they run without deliberate playback
pauses, and incidental file tools restore the user's active buffer after they
finish. Explicit navigation tools still change the active location. Set
`[agent] follow_tool_calls = true` to reveal each file target, move to the first
affected range when available, render it, and wait briefly before the operation.
Mutations always pass through the editor's transaction boundary with
session/turn attribution and save through the editor.

## Limits and failure behavior

App-server frames are capped at 1 MiB and tool content at 960 KiB. File-list
pages, workspace-walk work, search results, search bytes, queues, and callback
duration are bounded. File listings and reads expose continuation metadata;
search reports when its bounded scan was truncated. There is no per-turn tool
call count limit.
Oversized or malformed frames stop the Codex runtime without being rendered into
the terminal.

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
| `:AgentModel` / `Alt+m` in the Agent pane | Choose this conversation’s model and reasoning effort. |
| `:AgentCancel` | Interrupt the active Codex turn. |
| `:AgentClear` | Clear visible conversation while retaining current context. |
| `:AgentNew` | Close the current thread and start a new one. |
| `:AgentClose` | Hide the conversation panel without discarding state. |
| `:AgentHistory` | Inspect attributed agent transactions. |

### Choosing a model

Opening the Agent pane reads the workspace's configured Codex model without
starting a conversation. Once a thread starts or resumes, its confirmed model
and reasoning effort replace that preview. Click the model, press `Alt+m` from the pane's composer or transcript, or run
`:AgentModel` from anywhere. Model names take priority over descriptions when
space is limited, and a checkmark identifies the current choice. Search the
model list, select with Enter, then choose a supported reasoning effort. Red
preloads the catalog when the pane opens; if it is still loading, the picker
shows a spinner and preserves anything you type while waiting. Escape leaves the previous settings and
draft intact. Changes affect only this conversation; a running turn finishes
with its existing model, and the header shows the accepted next-message choice.
New conversations use Codex's default unless a model was explicitly selected
before their first message. Red never changes global Codex configuration.
