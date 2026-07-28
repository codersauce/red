# Husk plugin compatibility

Red host API version `0.5.1` is defined by
[`src/plugin/host_api.json`](../src/plugin/host_api.json). That file is the canonical,
machine-readable list of execute actions, request actions, signatures, and introduction
versions. Runtime dispatch and the bundled-plugin corpus are checked against it in tests.

Plugin packages may declare a semver range in `red_api_version`. Red checks that range
before activation. A malformed or incompatible range quarantines that plugin and reports
the source path, stage, current API version, and this migration guide; editor startup and
unrelated plugins continue. While Red is pre-1.0:

- patch API releases fix behavior without intentionally changing signatures;
- minor API releases may add calls and fields, and may deprecate calls;
- removing or incompatibly changing a call requires a host-API minor bump, a change
  manifest entry, and a migration note.

Host API `0.5.1` preserves the complete `0.4.0` contract. Existing filesystem
plugins declaring `"red_api_version": "^0.4.0"` therefore continue to load
without editing their metadata; plugins that require the new Replay host calls
must explicitly declare the `0.5.x` version that introduced those calls. Older
unsupported minor ranges and future incompatible versions remain quarantined.

Load runs parse, name resolution, and type checking against Red's host declarations
before activation. Diagnostics retain source spans and use stable families:
`HUSK-P0001` for parsing, `HUSK-T0001` for semantic/type errors, and `HUSK-A0001` for a
literal host call absent from the canonical schema. Literal host calls also check
required/optional arity (`HUSK-A0002`) and obvious literal argument types
(`HUSK-A0003`) against the machine-readable signature. `--no-typecheck` is an unsupported
development escape hatch; compatibility guarantees do not apply while it is enabled.

## Pull request replay preview

Host API `0.5.0` introduces an editor-owned, in-memory PR Replay preview.
`ReplayDemoPlan(callback)` returns the original mock PR metadata and complete,
source-linked unified hunks. `ReplayDemoOpenWorkspace(callback)` opens only the
Rust-owned, editable, fileless scratch source. The bundled replay plugin uses the
existing `CreateTextPanel`, `UpdateTextPanel`, `FocusPanel`, and
`SetPanelVisible` host calls to render a separate read-only Replay coach. The
coach is a real plugin panel, never a Markdown file or an editable editor
buffer. `Ctrl-w H`, `Ctrl-w J`, `Ctrl-w K`, and `Ctrl-w L` move a focused text
panel to the left, bottom, top, or right dock while preserving its content,
scroll state, and stable identity.

The coach uses the structured `replay` text-panel block format. Its JSON model
retains the complete original unified patch, PR metadata, step progress,
learning mode, local observations, and completion state. The editor validates
the model and its source path before rendering the hunk. Removed and added source
are independently Tree-sitter highlighted against the old and new file
projections, then combined with theme-derived Git colors and source line
numbers. PR context, reconstruction steps, and responsive actions remain pinned;
only the actual source hunk scrolls.

`ReplayDemoFocusSource(workspace_id)` restores the original scratch-source
window. Hiding the coach preserves both the panel and replay session.

`ReplayValidateStep(callback, workspace_id, step_id)` checks the actual
in-memory scratch source against the Rust-owned original hunk.
`ReplayApplyStep(callback, workspace_id, step_id, revision)` rejects a stale
workspace, changed source, nested user transaction, or nonmatching pre-image.
Its `revision` is a nonnegative, full-width `i64`; it is never narrowed to a
32-bit integer before reaching the editor's checked buffer revision.
Successful application becomes exactly one attributed, undoable editor
transaction. These preview calls never create files or branches, fetch GitHub,
stage changes, save buffers, or submit reviews.

The original `ReplayDemoValidateStep` and `ReplayDemoApplyStep` names remain
supported as backwards-compatible aliases. The production names apply equally
to the safe in-memory demo and real source-backed Replay workspaces.

`ReplayReconcileReview(callback, workspace_id)` performs a bounded, read-only
lookup for one previously approved uncertain review or imported unverified
receipt. The editor compares the original PR, reviewer, commit, outcome, body,
and inline diff coordinates before returning a verified receipt. It never
submits a review, starts an agent, mutates a Git ref, or grants a plugin shell
or GitHub credentials.

Plugins declaring a host API requirement for these calls should use
`"red_api_version": "^0.5.0"`.

## Source-backed pull request replay

Host API `0.5.1` adds real, editor-owned GitHub and local-branch review without
changing or invalidating the `0.5.0` preview contract. Existing plugins that
declare `^0.5.0` remain compatible.

`ReplayResolvePullRequest(callback, input)` accepts a positive PR number or a
canonical HTTPS pull-request URL for the current repository. The editor reads
bounded `gh pr view` metadata, validates the repository and original author
head, pins both Git object identities, and reports missing objects without
implicitly fetching. `ReplayFetchPullRequestObjects(callback, source_id,
confirmed)` fetches only verified, Replay-namespaced refs and refuses to run
without explicit user confirmation. The real PR diff is produced from the
original pinned merge base and target commit.

`ReplayResolveLocalBranch(callback, head, base)` resolves a locally present
feature branch without checking it out. Pass an empty base to prefer the
locally present `origin/HEAD` target, followed by `origin/main`,
`origin/master`, `main`, and `master`. Resolution pins both references and
computes their actual merge base; unrelated later changes on the default
branch are excluded. Neither source-resolution call creates a worktree.

Both resolution calls provide a durable sibling-worktree preview.
`ReplayCreateWorkspace(callback, source_id, confirmed)` creates the displayed
local scratch branch only after the reviewer explicitly confirms. Its response
contains a bounded presentation plan, complete original per-step unified
hunks, hunk-local original source images, and editable scratch-file identities.
Full scratch-file images remain in editor-owned buffers; a large file is never
copied into every plugin-visible step. `ReplayFocusStepSource(workspace_id,
step_id)` switches the existing source window to the exact scratch file for a
multi-file step without turning the dedicated guide into an editor buffer.
`ReplayToggleZoom(workspace_id)` temporarily enlarges the focused Replay guide
or its verified scratch-source window, then restores the exact original pane
geometry when called again. It does not change source buffers, create a split,
or write to a review workspace.

`ReplayActiveSession(callback)` returns the authoritative recovered source,
bounded original presentation, selected hunk, learning mode, completed
exercises, private observations, authenticated review role, exact original PR
head, and local review outbox. A GitHub review is classified as `author` only
when a separate, bounded, read-only GraphQL response verifies that the
authenticated viewer matches the author of the exact same repository, pull
request, head branch, and immutable head commit. Otherwise it remains
`reviewer`; repository write permission alone never grants author authority.
The bundled coach requests this snapshot on `editor:ready`, so `--resume`
restores the dedicated guide and local drafts without fetching, creating
another worktree, or exposing reusable application tokens.
`ReplayListReviews(callback)` discovers editor-owned and safely identified
legacy scratch reviews in a bounded background worker, returning provenance,
completion, note counts, and unsaved or active state without exposing source
buffers. `ReplayResumeReview(callback, review_id)` rechecks the selected
snapshot, immutable source, and exact original scratch worktree before opening
its guide. Reopening never creates a branch or discards unrelated dirty buffers.
`ReplayAddNote(callback, workspace_id, step_id, category, text)` validates and
stores a reviewer observation against the exact original author commit and
source hunk.

`ReplayAddDraft(callback, workspace_id, step_id, kind, text)` creates a durable
local review outcome. An `inline_comment` is anchored to the original changed
path, full head commit, hunk digest, original GitHub `left` or `right` diff
side, and exact one-based changed-line range. A `code_fix` receives the same
source anchor and is accepted only for the verified original author; it records
a proposal and does not modify the PR branch. A `review_summary` uses an empty
`step_id` and never claims inline coordinates. `ReplayUpdateDraft(callback,
workspace_id, draft_id, text)` preserves the original anchor while editing a
local draft. `ReplayRemoveDraft(callback, workspace_id, draft_id)` removes only
the specified local draft. All draft mutations advance recoverable editor
state, enforce bounded reviewer text, and reject foreign or stale original
hunks.

`ReplayAgentStart(workspace_id, step_id, scope, prompt)` starts an isolated
Codex turn owned by the exact Replay session. The `current_change` and
`pull_request` scopes are enforced as read-only: their dynamic-tool host
rejects source proposals and editor mutations, and generated text remains a
transient suggestion. Only the reviewer's explicit acceptance may call
`ReplayAcceptAgentDraft(callback, workspace_id, step_id, kind, text)`, which
creates an original-source-anchored local draft marked with `agent` provenance.
PR-level summaries pass an empty step identity. Agent-generated source fixes
cannot enter the comment outbox.

The `author_fix` scope is available only to the verified original GitHub PR
author after the exact, separately confirmed original-head worktree has been
opened. Codex may inspect the whole repository and stage normal reviewable
source proposals in that worktree, but it cannot write source files directly.
`ReplayAgentOpenProposals(workspace_id, session_id)` verifies the same original
head and opens Red's existing per-hunk agent approval surface without creating
a conversation pane. Accepting a hunk creates an ordinary undoable editor
transaction; saving, committing, pushing, and GitHub review submission never
happen automatically.

`ReplaySetMode(callback, workspace_id, mode)` records the selected Challenge or
Snippet mode in the same editor-owned session. These additive calls belong to
the unreleased source-backed `0.5.1` contract.

Automatic step application remains a single revision- and pre-image-checked
editor transaction; `a` requires no additional modal, and Replay undo refuses
to discard newer reviewer-authored changes. After a successful Replay undo, the
editor sends `replay:undone` with the original workspace and step identities,
allowing the coach to distinguish restored source from a new manual edit. No
Replay host call automatically
saves, commits, pushes, posts a comment, creates a GitHub pending review, or
submits a GitHub review. Cross-computer draft saving, GitHub review publication,
and agent-generated suggestions each have their own explicit human approval
boundary; committing or pushing an original author worktree is never an
implicit effect of the local outbox or proposal acceptance.

Plugins requiring these additive source-backed calls should declare
`"red_api_version": "^0.5.1"`.

## Workspace file operations

`FileOperation(callback: fn(Json), operation: Json)` applies a structured filesystem
operation inside the active workspace. Supported `kind` values are `create`,
`create_file`, `create_directory`, `rename`, `move`, `copy`, `delete`, `trash`,
`restore`, `undo_trash`, and `stat`. Mutation paths must be workspace-relative; the host
rejects absolute paths, parent traversal, workspace-root mutation, symlink escapes,
self/descendant copies, and implicit overwrites.

Create requests accept `path`; `create` treats a trailing slash as a directory and
supports bounded Bash-style list and range brace expansion. Rename, move, and copy
accept `source` and `destination`. Delete, trash, restore, and undo accept `paths:
[String]`. Results contain `ok`, an optional `error`, and operation-specific path data.
Trash restoration is available only on platforms whose system trash API exposes stable
item identities.

`FileOperation` was introduced in host API `0.4.0`.

## Compact plugin dialogs

`OpenInput(title: String, initial: String, handlers: ComposerHandlers)` opens the same
compact, single-line input used by LSP rename. It submits through
`ComposerHandlers.submitted` and cancels through `ComposerHandlers.cancelled`.

`OpenConfirm(title: String, message: String, handlers: PickerHandlers)` opens a compact
Accept/Cancel dialog. Cancel is selected by default; Left or `y` selects Accept, Right or
`n` selects Cancel, Enter confirms the selection, and Escape cancels. Accept invokes
`PickerHandlers.selected` with an item whose `id` is `accept`; cancellation invokes
`PickerHandlers.cancelled`.

Both calls were introduced in host API `0.4.0`, and their callback handles remain owned
and released by the calling plugin.

## Command discovery metadata

`red::add_command(name, callback[, metadata])` accepts an optional `Json` object
with `title`, `category`, `description`, and `aliases: [String]`. Red uses these
fields to populate the command palette; aliases are search terms and do not
create alternate colon commands. The palette shows the exact, case-sensitive
`:Name` invocation when it is available and resolves keymaps from the user's
effective configuration. Existing two-argument registrations continue to work.

## Callback-scoped pickers

New pickers should use
`OpenPicker(title: String, items: [PickerItem], options: PickerOptions, handlers: PickerHandlers)`.
The host returns an opaque integer handle that may be passed to `UpdatePickerItems`,
`UpdatePickerQuery`, `UpdatePickerStatus`, `UpdatePickerBusy`, and `ClosePicker`. Plugins
must not assign or interpret this handle.

```husk
red::execute("OpenPicker", "Themes", items, PickerOptions {
    placeholder: "Filter themes",
}, PickerHandlers {
    changed: theme_changed,
    cancelled: theme_cancelled,
    selected: theme_selected,
});
```

`PickerHandlers` accepts `selected`, `cancelled`, `changed`, `query`, and `action`
callbacks; unused handlers may be omitted. `changed`, `query`, and `action` can run
repeatedly. Selection and cancellation are terminal: the host consumes every handler for
that picker before invoking the terminal callback. Closing or replacing the dialog,
reloading its plugin, or unloading its plugin also releases the handlers. Stale handles
are ignored.

Set `busy: true` in `PickerOptions` to display an animated Braille spinner before the
picker status. Call `UpdatePickerBusy(handle, false)` when the asynchronous operation
finishes; the editor owns spinner timing and redraws, so plugins do not need timers.

Callbacks are retained by the runtime and delivered only to the plugin that opened the
picker. They do not use global `picker:*:<id>` subscriptions. Picker items and callback
payloads use the declared `PickerItem`, `PickerCancelled`, and `PickerActionEvent` records;
the `PickerItem.data` field remains `Json` so a plugin can attach its own payload.

`OpenPicker` was added in host API `0.3.0`. Plugins targeting this Red release should
declare `"red_api_version": "^0.5.0"`. The numeric-ID `OpenDynamicPicker` API remains
available for compatibility, but new plugins should not use it.

## Agent composer

Plugins that collect a multiline request should call `OpenComposer(title: String, query: String, history: [String], handlers: ComposerHandlers)`. `submitted` receives the complete prompt as a `String`; `cancelled` receives a `ComposerCancelled` record. Both are terminal one-shot callbacks scoped to the plugin that opened the composer, so plugins do not allocate IDs or register synthetic event names.

```husk
red::execute("OpenComposer", "Agent prompt", draft, history, ComposerHandlers {
    submitted: prompt_submitted,
    cancelled: prompt_cancelled,
});
```

The host owns multiline editing, wrapping, cursor movement, and history navigation; it does not send a callback for each keystroke. Input is limited to 128 KiB so an escaping-heavy prompt remains within the Codex app-server frame limit; an oversized paste leaves the current draft intact and shows a validation message. Enter submits, `Ctrl-j` or Shift-Enter inserts a newline, Escape or `Ctrl-c` cancels, and `Ctrl-p` / `Ctrl-n` moves through the supplied history while preserving the current draft.

`OpenComposer` was introduced in host API `0.3.0`. The numeric-ID `OpenAgentComposer` API and its `composer:submitted:<id>` / `composer:cancelled:<id>` events remain available for compatibility with `0.2.0` plugins.

`AgentArchiveSession(session_id: String)` was also introduced in host API `0.2.0`. Use it when Codex app-server has already stopped: pending proposals remain reviewable, and the host does not send an interrupt to a replacement process that may reuse the same session ID. Use `AgentCloseSession(session_id: String)` for a live session that should be closed normally.

`AgentPrompt` automatically attaches bounded editor context containing the active visual selection or a roughly 80-line cursor excerpt, unsaved-state metadata, cursor/range, and intersecting diagnostics. Files outside the workspace, ignored paths, common credential/secret filenames, and binary buffers are omitted. Plugins that need to inspect or explicitly override this context can call `GetAgentContext(callback)` and `AgentPromptWithContext(session_id: String, text: String, context: Json)`; the context object accepts `uri` and `text` fields and is included in the direct Codex turn.

## Text panels

`CreateTextPanel`, `UpdateTextPanel`, and `AppendTextPanel` provide a source-backed conversation surface. `TextPanelBlock` accepts an `id`, `kind` (`user`, `agent`, `error`, or `text`), `format` (`plain` or `markdown`), and `text`; the host preserves the source while wrapping and rendering it for the current panel width. These calls were introduced in host API `0.2.0`.

Both row and text panels accept `side: "left"`, `"right"`, `"top"`, or
`"bottom"` in `PanelConfig`. `width` is measured in columns for left/right panes
and rows for top/bottom panes. Users can move a focused pane using
`Ctrl-w H/J/K/L`, resize it using `Ctrl-w <` / `>` or `Ctrl-w -` / `+`, or drag
its divider. The host remembers widths and heights independently and preserves
the pane's stable ID, source blocks, scroll state, focus, and composer draft.
While dragging, the host highlights only the captured divider. It prefers the
theme's `sash.hoverBorder`, `panelTitle.activeBorder`, or `focusBorder`, then
derives a readable accent when those colors are absent or insufficiently
contrasted. Plugins do not need to configure a border to make resizing visible.

`PanelConfig` may include `composer: Json { placeholder: String, rows: i32 }` for a persistent footer composer and `header_actions: [Json { id: String, label: String, compact_label?: String }]` for clickable, right-aligned header controls. Row panels can also set `surface: ThemeStyleSpec` and `border: ThemeStyleSpec` to resolve theme-aware panel foreground, background, and separator colors without affecting other panels. Header actions emit `panel:event:<id>` using their configured `id`; compact labels are selected automatically on narrow panels, with the rightmost actions retained when space is especially limited. Focus the footer with `FocusTextPanelComposer(id)`, update its enabled/status state with `SetTextPanelComposerState(id, enabled, status?)`, and clear its draft with `ClearTextPanelComposer(id)`. A focused composer supports Unicode-safe editing, paste, wrapping, click-to-position cursor movement, `Ctrl-p`/`Ctrl-n` local history, Enter to submit, and `Ctrl-j` or Shift-Enter for a newline. It emits `panel:event:<id>` with `action: "submit"` and the complete `text`; other footer actions include `composer_focus`, `composer_blur`, `interrupt`, `clear`, `new`, `history`, and `close`. `SetPanelVisible(id, visible)` hides or restores a panel without discarding its blocks, scroll position, or draft. Replacing text-panel blocks with an empty list resets scrolling and restores tail-following. Footer panels shrink on narrow terminals while preserving an editor viewport.

Codex app-server updates other than assistant text chunks are forwarded to plugins as `agent:activity` with the normalized `update` payload. Core editor-tool calls also emit this event with `session_update: "editor_tool"`, `status: "in_progress"`, and a concise `title` such as `Opening src/main.rs` or `Proposing 2 edit(s) in src/main.rs`. This allows status/tool/plan progress to be displayed without treating it as transcript text.

## Quarantine and self-check

Plugins load independently. Source, version, dependency, compile, activation, and runtime
failures quarantine only their owner. `red --self-check` prints every bundled plugin's
status. Required plugin dependencies must be active or the dependent plugin is
quarantined with the dependency chain.

Plugin subprocesses inherit only the standard execution, locale, temporary-directory,
platform, and SSH-agent environment keys. Explicit environment overrides remain
allowlisted. Process stdin is limited to 16 MiB, raw output to 2 MiB, individual
streaming lines to 256 KiB, and pending process events to 16 (at most roughly 32 MiB
of payload); oversized output is
reported without letting an untrusted process grow editor memory indefinitely.

## Dynamic JSON boundary

`Json` remains intentional for persisted plugin state, arbitrary user configuration,
external process data, and plugin-defined payloads such as `PickerItem.data`. Values with
a host-defined shape should use nominal records instead. Picker callbacks are the first
migrated slice; request results, editor events, styles, panel values, and the remaining
bundled-plugin helpers will move incrementally as their host schemas become canonical.

## Transactional reload and state

User plugin files are polled with a 250 ms debounce. A replacement VM is parsed,
typechecked, activated, and migrated before it replaces the live program. A bad save
leaves the previous callbacks and program active and records an `active_with_reload_error`
status. Host requests, editor actions, logs, and timers produced while staging are
published only after a successful swap. Starting or killing a process from reload-time
`activate`, `state_import`, or `deactivate` is rejected so a failed reload cannot leak
or terminate a subprocess; manage processes from an event or command callback instead.

State is intentionally explicit. A plugin that wants state carried across a successful
reload implements:

```husk
fn state_export() -> Json { /* return versioned state */ }
fn state_import(saved: Json) { /* validate or migrate saved state */ }
```

If either hook fails, the replacement is discarded. Successful replacement removes old
commands, event callbacks, pending requests, and VM state before the new registry becomes
authoritative. Plugins should clean up host-owned panels, timers, watchers, and processes
from `deactivate`.
