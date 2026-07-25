# Husk plugin compatibility

Red host API version `0.4.1` is defined by
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

Load runs parse, name resolution, and type checking against Red's host declarations
before activation. Diagnostics retain source spans and use stable families:
`HUSK-P0001` for parsing, `HUSK-T0001` for semantic/type errors, and `HUSK-A0001` for a
literal host call absent from the canonical schema. Literal host calls also check
required/optional arity (`HUSK-A0002`) and obvious literal argument types
(`HUSK-A0003`) against the machine-readable signature. `--no-typecheck` is an unsupported
development escape hatch; compatibility guarantees do not apply while it is enabled.

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
`UpdatePickerQuery`, `UpdatePickerStatus`, and `ClosePicker`. Plugins
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

Callbacks are retained by the runtime and delivered only to the plugin that opened the
picker. They do not use global `picker:*:<id>` subscriptions. Picker items and callback
payloads use the declared `PickerItem`, `PickerCancelled`, and `PickerActionEvent` records;
the `PickerItem.data` field remains `Json` so a plugin can attach its own payload.

`OpenPicker` was added in host API `0.3.0`. Plugins targeting this Red release should
declare `"red_api_version": "^0.4.0"`. The numeric-ID `OpenDynamicPicker` API remains
available for compatibility, but new plugins should not use it.

## Agent composer

Plugins that collect a multiline request should call `OpenComposer(title: String, query: String, history: [String], handlers: ComposerHandlers)`. `submitted` receives the complete prompt as a `String`; `cancelled` receives a `ComposerCancelled` record. Both are terminal one-shot callbacks scoped to the plugin that opened the composer, so plugins do not allocate IDs or register synthetic event names.

```husk
red::execute("OpenComposer", "Agent prompt", draft, history, ComposerHandlers {
    submitted: prompt_submitted,
    cancelled: prompt_cancelled,
});
```

The host owns a real, in-memory editor buffer, Vim-style normal/insert/visual
modes, multiline editing, wrapping, cursor movement, undo/redo, operators, text
objects, and history navigation; it does not send a callback for each
keystroke. Input is limited to 128 KiB so an escaping-heavy prompt remains
within the Codex app-server frame limit; an oversized paste leaves the current
draft intact and shows a validation message. `Ctrl+Enter` submits in any
composer mode. `Alt+Enter` also submits when the terminal reports that key
combination. In insert mode, `Enter`, `Shift+Enter`, and `Ctrl+J` insert a
newline; in normal mode, `Enter` submits. `Escape` switches from insert or
visual mode to normal mode, so `Escape`, then `Enter` provides a universal
send sequence. `Ctrl+C` cancels the floating composer. `Ctrl+P` and `Ctrl+N`
move through the supplied history while preserving the current draft.
`Ctrl+S` remains the editor's save shortcut; it is not an agent submission
binding.

`OpenComposer` was introduced in host API `0.3.0`. The numeric-ID `OpenAgentComposer` API and its `composer:submitted:<id>` / `composer:cancelled:<id>` events remain available for compatibility with `0.2.0` plugins.

`AgentArchiveSession(session_id: String)` was also introduced in host API `0.2.0`. Use it when Codex app-server has already stopped: pending proposals remain reviewable, and the host does not send an interrupt to a replacement process that may reuse the same session ID. Use `AgentCloseSession(session_id: String)` for a live session that should be closed normally.

`AgentPrompt` automatically attaches bounded editor context containing the active visual selection or a roughly 80-line cursor excerpt, unsaved-state metadata, cursor/range, and intersecting diagnostics. Files outside the workspace, ignored paths, common credential/secret filenames, and binary buffers are omitted. Plugins that need to inspect or explicitly override this context can call `GetAgentContext(callback)` and `AgentPromptWithContext(session_id: String, text: String, context: Json)`; the context object accepts `uri` and `text` fields and is included in the direct Codex turn.

## Native agent conversations

Host API `0.4.1` adds typed, bounded operations for native Codex conversation
management:

- `AgentResumeSession(session_id: String, cwd: String)` resumes an existing
  workspace-scoped Codex thread.
- `AgentSteer(session_id: String, text: String)` submits additional instructions
  to an existing active turn.
- `AgentListModels(session_id: String)` requests the available model catalog.
- `AgentListSessions(session_id: String, cwd: String)` requests up to 50 saved
  conversations in the current workspace.
- `AgentSetModel(session_id: String, model: String, reasoning_effort?: String)`
  changes the model for future turns without restarting the Codex bridge,
  discarding the current thread, or overriding the user's approval policy.
- `AgentSetReasoningEffort(effort: String)` configures the reasoning effort
  used by the next Codex session.
- `SetAgentPosition(position: String)` moves the existing agent panel to
  `left`, `right`, `top`, or `bottom` without replacing its source blocks,
  composer, draft, or focus.

Catalog results arrive as session-scoped `agent:activity` events with
`session_update: "models"` or `session_update: "sessions"`; successful model
selection arrives as `session_update: "model_selected"`, and token usage
arrives as `session_update: "token_usage"`. Completed native file changes
reload clean open buffers. A change that overlaps unsaved editor contents
preserves the buffer and emits a session-scoped `agent:file_conflict` event.
User approval requests retain the exact app-server choices and are denied when
the session is inactive, the request is cancelled, or the response cannot be
delivered. Red's `disable_ai` switch remains authoritative over all
conversation and process-launch operations.

## Text panels

`CreateTextPanel`, `UpdateTextPanel`, and `AppendTextPanel` provide a source-backed conversation surface. `TextPanelBlock` accepts an `id`, `kind` (`user`, `agent`, `error`, or `text`), `format` (`plain` or `markdown`), and `text`; the host preserves the source while wrapping and rendering it for the current panel width. These calls were introduced in host API `0.2.0`.

`PanelConfig.side` accepts `left`, `right`, `top`, and `bottom`. Its existing
`width` field describes the dock's thickness: columns for a left/right panel
and rows for a top/bottom panel. This preserves compatibility with existing
panel plugins and configuration records. `PanelConfig` may include
`composer: Json { placeholder: String, rows: i32 }` for a persistent footer
composer and `header_actions: [Json { id: String, label: String,
compact_label?: String }]` for clickable, right-aligned header controls. Row
panels can also set `surface: ThemeStyleSpec` and `border: ThemeStyleSpec` to
resolve theme-aware panel foreground, background, and separator colors without
affecting other panels.

Header actions emit `panel:event:<id>` using their configured `id`; compact
labels are selected automatically on narrow panels, with the rightmost actions
retained when space is especially limited. Focus the footer with
`FocusTextPanelComposer(id)`, update its enabled/status state with
`SetTextPanelComposerState(id, enabled, status?)`, and clear its draft with
`ClearTextPanelComposer(id)`. The focused composer shares the floating
composer's real modal editor, Unicode-safe editing, undo, paste, wrapping,
click-to-position cursor, history, and mode-aware send/newline controls. It
emits `panel:event:<id>` with `action: "submit"` and the complete `text`;
other footer actions include `composer_focus`, `composer_blur`, `interrupt`,
`clear`, `new`, `history`, and `close`. When the conversation body has focus,
its independent reading cursor supports `j`/`k`, arrow keys,
`Ctrl+F`/`Ctrl+B`, `g`/`G`, link selection with `Tab`/`Shift+Tab`, and link
activation with `Enter`. Press `i` or `a`, click the footer, or call
`FocusTextPanelComposer(id)` to enter the composer. `Escape` inside the
composer enters normal mode; `Ctrl+C` blurs the composer without discarding
its draft. `Escape` while reading returns focus to the editor.
`SetPanelVisible(id, visible)` hides or restores a panel without
discarding its blocks, scroll position, or draft. Replacing text-panel blocks
with an empty list resets scrolling and restores tail-following. Responsive
agent panels preserve the editor viewport and fall back to a bottom dock when
a requested left or right panel cannot fit safely.

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
