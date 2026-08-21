# Husk plugin compatibility

Red host API version `0.15.0` is defined by
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
(`HUSK-A0003`) against the machine-readable signature. Invalid static plugin
annotations use `HUSK-A0004`. `--no-typecheck` is an unsupported development
escape hatch; compatibility guarantees do not apply while it is enabled.

Red `0.15.0` retains the complete `0.4.0`, `0.6.0`, `0.7.0`, `0.8.0`, `0.9.0`,
`0.10.0`, `0.11.0`, `0.12.0`, and `0.14.0` contracts, so existing packages that declare those
minors continue to load. New packages should target `"red_api_version": "^0.15.0"`.

## Document-symbol breadcrumbs

Host API `0.13.0` adds `document_id` to `GetWindows` entries and successful
`DocumentSymbols` results, and to `file:saved` events. This identifies one live
buffer and survives buffer-index changes. Cache symbols by this ID and `revision`, not by the display or navigation
path. The optional argument to `DocumentSymbols` remains a buffer index. Responses
for closed, renamed, or changed documents return an error.

`GetWindows.breadcrumb_components` contains display-only path labels: relative to
the working directory when possible, otherwise home-abbreviated with `~`, otherwise
absolute. Native roots, drive letters, and UNC shares are preserved. Never use these
labels for file access or LSP requests.

`red::document_symbol_chain(symbols, position, file)` returns the containing symbol
ancestry from normalized document symbols. `position` uses zero-based UTF-16 LSP
coordinates; `file` is the response's navigation path. The native lookup preserves
the original records and works beyond the bundled plugin's former 512-symbol limit.

## Language-pack indentation

Host API `0.12.0` adds `languages.<id>.grammar.indents`, an ordered list of
package-relative query files. Packs using it must require `^0.12.0` or later.
See [the indentation query contract](LANGUAGES.md#indentation-queries) for captures,
fallback behavior, and portable fixtures. Older packs remain supported.

## Command arguments and completion

Host API `0.11.0` adds opt-in arguments to plugin commands:

```husk
#[red::command(
    name = "Service",
    arguments = true,
    completions = [["enable", "disable", "status"], ["local", "workspace"]],
)]
fn service(command: CommandInvocation) {
    red::execute("Print", command.raw_args);
}
```

`CommandInvocation` contains the exact registered `name`, whitespace-separated
`args: [String]`, and unexpanded `raw_args: String`. A callback may also accept
`Json` when it does not need the typed record. A palette or keymap invocation
without arguments receives an empty argument list. Existing commands continue to
take no parameters unless they opt in. The same `arguments` and `completions`
fields are accepted by `red::add_command` metadata.

Each inner completion array describes one argument position. Choices must be
nonempty strings without whitespace; an empty array leaves that position without
suggestions. Choices are hints, not validation: the callback must validate its
arguments. `Tab` and `Shift-Tab` cycle matching choices without invoking the
callback. Hidden commands stay out of completion, and built-in colon commands
retain precedence. There is no shell quoting, expansion, or completion callback.
Packages using these fields must target `^0.11.0` or later.

## Language-pack formatters

Host API `0.10.0` adds an optional `formatter` table to each language definition:

```toml
[languages.python.formatter]
name = "Black"
command = "black"
args = ["--quiet", "--stdin-filename", "{file}", "-"]
root_markers = ["pyproject.toml", ".git"]
```

Red launches the command directly, sends the document on standard input, and replaces
the document with UTF-8 standard output. `{file}` and `{workspace}` placeholders are
expanded in arguments and environment values. Project-local executables under
`node_modules/.bin`, `.venv/bin`, `venv/bin`, and `vendor/bin` take precedence over
`PATH`.

The global `[formatting]` table supports `on_save` (default `true`) and a `provider` of `auto`,
`external`, or `lsp`. `auto` prefers an installed language-pack formatter and falls
back to LSP when the formatter is absent; a formatter that starts and fails does not
silently switch engines. Set `formatting.on_save = false` to disable automatic
formatting without disabling the explicit Format Document action. The legacy
`lsp.format_on_save` flag remains accepted as an alias for either boolean value.
The modern key wins when both appear in the same config layer; later command-line
overrides still take precedence.

## Scratch-buffer workflows

`OpenScratchBuffer(callback, name, text, commands?)` accepts an optional `syntax`
language name plus optional `submit` and `cancel` plugin command names. The syntax
selection is local to the scratch buffer and falls back to automatic filename
detection when omitted or unknown. In a managed scratch buffer, `:w` and `:wq` invoke
the submit command without writing the display name to disk, while `:q` and `:q!`
invoke the cancel command without quitting Red. `Save`, `Quit`, and configured key
bindings follow the same routing. The options were added in host API `0.8.0`; calls
using the original three required arguments remain compatible.

## External package primitives

`CompanionCall(callback, method, params, timeout_ms?)` lazily starts the calling
package's declared native companion and exchanges bounded JSON-lines RPC
messages. Red owns the process, matches responses to requests, enforces the
timeout, and stops the process when the package or editor shuts down.

`DocumentSnapshot(callback, path?)` returns the current text and revision for an
open document. `DocumentApply(callback, options)` atomically applies non-overlapping
edits after checking the expected revision and optional preimages. The result
contains an attributed transaction ID. `DocumentUndo(callback, options)` reverts
that exact transaction only while it is still the most recent transaction from
the same plugin. Red remains responsible for buffer state, dirty tracking, LSP
notifications, and undo history.

Plugin-owned storage is captured as an opaque namespaced extension in Red session
snapshots. Unknown extensions survive load/save cycles, allowing a package to
restore its workflow without Red understanding the payload.

These calls were introduced in host API `0.6.0`.

## Row-panel search and workspace path discovery

Host API `0.15.0` supports an inline search prompt owned by an existing row
panel. `OpenPanelSearch(id, initial, prefix)` focuses that prompt,
`UpdatePanelSearch(id, status)` replaces its compact result indicator,
`KeepPanelSearch(id)` preserves the query while returning focus to row
navigation, and `ClosePanelSearch(id)` removes it.

Input emits ordinary `panel:event:<id>` events with actions `search_query`,
`search_move`, `search_submit`, `search_reveal`, `search_keep`, and
`search_cancel`. The current query is provided in `text`, while `row` identifies
the current selection. Up/Down and `Ctrl-p`/`Ctrl-n` move the selection; Enter
submits, `Ctrl-Enter` requests reveal, `Shift-Enter` keeps the prompt, and
Escape cancels.

`SearchWorkspacePaths(callback, path, query, directories_only)` performs a
bounded, asynchronous, ignore-aware recursive filesystem search. Its result
contains the original `query`, `directories_only`, ranked `matches`, synthetic
directory `children`, expanded ancestor paths, the total match count,
`truncated`, and an optional `error`. Paths are normalized relative to the
requested workspace root. `InvalidateWorkspacePaths(path)` drops its cached
index after filesystem changes. Neither call grants subprocess permissions.

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

`OpenConfirm(title: String, message: String, handlers: PickerHandlers, options?: Json)`
opens a compact Accept/Cancel dialog. Cancel is selected by default; Left or `y` selects
Accept, Right or `n` selects Cancel, Enter confirms the selection, and Escape cancels.
Accept invokes `PickerHandlers.selected` with an item whose `id` is `accept`;
cancellation invokes `PickerHandlers.cancelled`.

Both calls were introduced in host API `0.4.0`, and their callback handles remain owned
and released by the calling plugin.

## Rich confirmations and busy overlays

The optional `OpenConfirm` options object accepts `accept_label`, `cancel_label`, and
`rows`. Each row is an array of `{ text, style }` segments, allowing a plugin to present
bounded, theme-aware details while retaining the host dialog's safe default and input
behavior. Calls without options keep the original compact presentation.

`UpdateOverlayBusy(id: String, busy: bool)` adds host-driven spinner animation before
the first visible line of an existing overlay. The host owns frame timing and redraws;
plugins only toggle busy state and replace the overlay content when work completes.

These additions were introduced in host API `0.9.0`.

## Declarative plugin authoring

Top-level functions can declare commands, event listeners, state, configuration,
and lifecycle hooks directly:

```husk
struct SymbolsState {
    request: i32,
    enabled: bool,
}

#[red::state]
fn initial_state() -> SymbolsState {
    return SymbolsState { request: -1, enabled: true };
}

#[red::config("plugin_config")]
fn configuration_loaded(event: Json) {
    red::state_patch(SymbolsState {
        enabled: event.value.symbols.enabled,
    });
}

#[red::command(
    name = "LspDocumentSymbols",
    title = "Show document symbols",
    category = "LSP",
    description = "Find symbols in the current document",
    aliases = ["outline", "symbols"],
    scope = "global",
)]
fn document_symbols() {
    red::request("DocumentSymbols", show_document_symbols);
}

#[red::on("timeout:callback")]
fn symbol_batch_timeout(event: TimerEvent) {
    // Handle a host-owned timer notification.
}

#[red::lifecycle("deactivate")]
fn stop_background_work() {
    // Cancel timers or release plugin-owned resources.
}
```

`#[red::command]` requires a nonempty `name`, a zero-argument function by default, and
optional string `title`, `category`, and `description` fields plus an optional
string-array `aliases` field. `visible = false` hides a command from the command
palette and colon completion without disabling direct invocation or keymaps.
The optional `scope` is `"editor"` by default. Set it to `"global"` only for
commands that are safe and meaningful when a plugin panel owns focus, such as
workspace pickers or pane toggles. Packages using command scope must declare
`"red_api_version": "^0.7.0"`.
`#[red::on]` takes exactly one nonempty event-name string and requires a
one-argument function. A function may subscribe to multiple distinct events by
repeating `#[red::on(...)]`.

`#[red::state]` marks one zero-argument initializer with an explicit named record
return type. Red runs it after static registration and before activation, stores
its concrete record privately for that plugin, and makes it available through
`red::state()`. Update individual named fields with a sparse record literal:

```husk
red::state_patch(SymbolsState { enabled: false });
```

Patches retain every omitted field and reject unknown fields or records that do
not belong to the current plugin. They avoid rebuilding unrelated collections,
which makes them preferable for state containing picker results, transcript
blocks, or other larger values. Use `red::state_set(state)` only when replacing
the complete record intentionally. Existing `red::state("key")` and
`red::state_set("key", value)` calls remain supported and independent of the
typed record; reserve them for explicit compatibility or high-volume payload
boundaries that cannot safely live inside the typed state snapshot.

`#[red::config("key")]` binds a one-argument callback to an initial `GetConfig`
request. `#[red::config]` requests the complete configuration. Configuration
requests are staged and delivered only after state initialization and activation
succeed. Each key may be bound once per plugin.

`#[red::lifecycle("hook")]` binds a function to `activate`, `deactivate`,
`before_exit`, `state_export`, or `state_import`; activation, deactivation, and
export callbacks take no arguments, while exit and import callbacks take one.
An explicitly annotated hook takes precedence over a conventionally named
function, which remains supported for compatibility.

Red validates these annotations before activation, retains source diagnostics,
and registers their generation-safe callbacks in source order. State
initialization, activation, configuration requests, and lifecycle registration
participate in the same transactional reload as imperative commands and
listeners: failed validation, ownership conflicts, initializer failures, or
activation failures cannot replace or leak the previous plugin's wiring.
Annotations are supported on top-level functions only, including functions in
package source modules. Other attribute namespaces are ignored by Red.

`red::add_command(name, callback[, metadata])` accepts an optional `Json` object
with `title`, `category`, `description`, `aliases: [String]`, `visible: bool`,
and `scope`. Red uses visible commands to populate the command palette; aliases
are search terms and do not create alternate colon commands. The palette shows
the exact, case-sensitive `:Name` invocation when it is available and resolves
keymaps from the user's effective configuration.

The optional scope is `"editor"` by default. Set it to `"global"` only for
commands that are safe and meaningful when a plugin panel owns focus, such as
workspace pickers or pane toggles. Global commands use the configured
normal-mode binding from focused panels, subject to keys reserved by that
panel's own input handling. Existing two-argument registrations and metadata
without a scope continue to work. Imperative `red::add_command` and `red::on`
remain supported for dynamic or conditional registrations, including
process-specific and filesystem-watch-specific event names.

## Callback-scoped pickers

New pickers should use
`OpenPicker(title: String, items: [PickerItem], options: PickerOptions, handlers: PickerHandlers)`.
The host returns an opaque integer handle that may be passed to `UpdatePickerItems`,
`UpdatePickerQuery`, `UpdatePickerSelection`, `UpdatePickerStatus`, and `ClosePicker`. Plugins
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

`OpenPicker` was added in host API `0.3.0`. New plugins targeting this Red release should
declare `"red_api_version": "^0.7.0"`. The numeric-ID `OpenDynamicPicker` API remains
available for compatibility, but new plugins should not use it.

## Agent composer

Plugins that collect a multiline request should call `OpenComposer(title: String, query: String, history: [String], handlers: ComposerHandlers)`. `submitted` receives the complete prompt as a `String`; `cancelled` receives a `ComposerCancelled` record. Both are terminal one-shot callbacks scoped to the plugin that opened the composer, so plugins do not allocate IDs or register synthetic event names.

```husk
red::execute("OpenComposer", "Agent prompt", draft, history, ComposerHandlers {
    submitted: prompt_submitted,
    cancelled: prompt_cancelled,
});
```

The host owns multiline editing, wrapping, cursor movement, and history navigation; it does not send a callback for each keystroke. Input is limited to 128 KiB so an escaping-heavy prompt remains within the Codex app-server frame limit; an oversized paste leaves the current draft intact and shows a validation message. The prompt starts in Insert mode. Enter or `Ctrl-Enter` submits; `Alt-Enter`, `Shift-Enter`, or `Ctrl-j` inserts a newline; Escape enters Normal mode. Normal mode uses the same Unicode-aware word motions, character searches, text objects, and transactional edits as file-backed editor buffers: counts, `d`/`c`/`y` operators, `f`/`t`/`F`/`T`, `;`/`,`, Visual selections, `p`/`P`, `u`/`Ctrl-r`, dot-repeat, and local macros operate on the prompt itself. `/` and `?` search only the prompt; `:` and application commands are not exposed. In idle Normal mode, Enter submits and Escape cancels. Enter finishes an active search without submitting, and unfinished operators or visual selections never accidentally send the prompt. Escape leaves Visual or Search mode without closing the composer. `Ctrl-c` cancels from either mode. `Ctrl-p` / `Ctrl-n` moves through the supplied history while preserving the current draft. See [keyboard compatibility](KEYBOARD.md) for terminal-specific limitations and diagnostics.

`OpenComposer` was introduced in host API `0.3.0`. The numeric-ID `OpenAgentComposer` API and its `composer:submitted:<id>` / `composer:cancelled:<id>` events remain available for compatibility with `0.2.0` plugins.

`AgentArchiveSession(session_id: String)` was also introduced in host API `0.2.0`. Use it when Codex app-server has already stopped and the host must not send an interrupt to a replacement process that may reuse the same session ID. Use `AgentCloseSession(session_id: String)` for a live session that should be closed normally.

Host API `0.9.0` adds `AgentResumeSession(cwd, session_id)` for rejoining a
persisted Codex thread and `AgentForgetSession(session_id)` for an explicit new
conversation. The bundled Agent plugin renders
`agent:conversation_restore_pending` with its composer disabled, then replaces
the cached projection from `agent:session_restored`; a
`agent:session_restore_failed` event converts the cached transcript to clearly
archived context.

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

`PanelConfig` may include `composer: Json { placeholder: String, rows: i32 }` for a persistent footer composer and `header_actions: [Json { id: String, label: String, compact_label?: String }]` for clickable, right-aligned header controls. Row panels can also set `surface: ThemeStyleSpec` and `border: ThemeStyleSpec` to resolve theme-aware panel foreground, background, and separator colors without affecting other panels. Header actions emit `panel:event:<id>` using their configured `id`; compact labels are selected automatically on narrow panels, with the rightmost actions retained when space is especially limited. Focus the footer with `FocusTextPanelComposer(id)`, update its enabled/status state with `SetTextPanelComposerState(id, enabled, status?)`, replace its bounded prompt history with `SetTextPanelComposerHistory(id, history)`, and clear its draft with `ClearTextPanelComposer(id)`. `SetTextPanelComposerHistory` was introduced in host API `0.15.0`; it normalizes, deduplicates, and limits supplied entries without changing the current draft.

A focused footer is a host-owned modal text area, not an editor file buffer. It supports Unicode-safe editing, wrapping, click-to-position cursor movement, counts, Vim word and character motions, `d`/`c`/`y` operators, inner/around text objects, Visual/Visual-line/Visual-block selections, transactional undo/redo, local yank/paste registers, dot-repeat, local macro recording, and prompt-local `/` or `?` searches. Its mode, pending operator, selection, undo tree, register, and search never alter the main editor. File writes, Ex commands, window operations, LSP requests, and plugin callbacks cannot be invoked through textarea keys.

The footer uses the same Enter policy as `OpenComposer`: Enter or `Ctrl-Enter` submits; `Alt-Enter`, `Shift-Enter`, or `Ctrl-j` inserts a newline. While the footer is focused, `Ctrl-j` edits the prompt rather than scrolling the conversation. Escape enters Normal mode. In idle Normal mode, Enter submits and Escape emits `composer_blur`; Escape from Visual or Search mode first returns to Normal. `Ctrl-p`/`Ctrl-n` navigate local prompt history. In Insert mode, Up moves within a multiline draft until the first visual row, then recalls older prompts; once history navigation starts, Up/Down browse entries and Down past the newest entry restores the exact draft and cursor. `Ctrl-r` opens an empty, editable `Search prompts:` field with matching history entries previewed separately; repeated `Ctrl-r` selects older matches, Enter loads the selected match without submitting it, and Escape or `Ctrl-g` cancels. Normal-mode `Ctrl-r` remains redo, and `/` or `?` remains prompt-local text search. `Ctrl-c` remains a pane-level shortcut that emits `interrupt` without changing the composer draft or focus. Successful submission emits `panel:event:<id>` with `action: "submit"` and the complete `text`; other footer actions include `composer_focus`, `composer_blur`, `interrupt`, `clear`, `new`, `history`, and `close`. `SetPanelVisible(id, visible)` hides or restores a panel without discarding its blocks, scroll position, or draft. Replacing text-panel blocks with an empty list resets scrolling and restores tail-following. Footer panels shrink on narrow terminals while preserving an editor viewport.

Codex app-server updates other than assistant text chunks are forwarded to plugins as `agent:activity` with the normalized `update` payload. Core editor-tool calls also emit this event with `session_update: "editor_tool"`, `status: "in_progress"`, and a concise `title` such as `Opening src/main.rs` or `Editing src/main.rs (2 changes)`. This allows status/tool/plan progress to be displayed without treating it as transcript text. Agent recovery uses `agent:conversation_restore_pending`, `AgentResumeSession`, `agent:session_restored`, and `agent:session_restore_failed`; plugins must not enable follow-up input while restoration is pending.

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

## Edit notification boundaries

`buffer:changed`, `cursor:moved`, and `viewport:changed` describe the latest
editor state. Dot-repeat, macros, counted edits, and visual-block replay may
coalesce these notifications within a bounded group of source-local actions.
Do not use their delivery count as an edit log; use the buffer revision and
read the current document instead. Mode transitions remain observable in order.
Explicit plugin commands, LSP requests, saves, and document switches flush
pending changes before they execute. A long replay publishes intermediate
states and can be interrupted with Ctrl-C without dropping other queued keys.

## Dynamic JSON boundary

Event listeners and request callbacks decode host payloads according to their declared
parameter types. Named records, arrays, tuples, and nested `Option<T>` values are
decoded recursively. Both JSON `null` and omitted optional fields become `None`; a
present value becomes `Some(value)`. Existing callbacks declared as `Json` retain their
dynamic behavior, and additional object fields are preserved for compatibility.

Tagged JSON objects also decode into nominal Husk enum variants. The host recognizes
`type`, `session_update`, `kind`, and `$case` discriminators and maps snake-case wire tags to
PascalCase variants. Declare an `Unknown(Json)` case to preserve forward-compatible
payloads when the host adds a new variant. Plugin process events share this host enum:

```husk
fn process_finished(event: ProcessEvent) {
    match event {
        ProcessEvent::Exit { process_id, code, plugin_name } => {
            if let Some(exit_code) = code {
                red::execute("Print", process_id + " exited with " + exit_code);
            }
        }
        ProcessEvent::Stdout { line, process_id, plugin_name } => {}
        ProcessEvent::Stderr { line, process_id, plugin_name } => {}
        ProcessEvent::Error { message, process_id, plugin_name } => {}
    }
}
```

When typed records cross back into host actions or plugin storage, `None` serializes to
JSON `null` and `Some(value)` serializes to the value itself, including inside arrays and
nested records. `Json` remains intentional for persisted user-defined state, arbitrary
configuration, genuinely open-ended process output, and plugin-defined payloads such as
`PickerItem.data`. Prefer nominal records and enums whenever the host owns the shape.

## Transactional reload and state

User plugin files are polled with a 250 ms debounce. A replacement VM is parsed,
typechecked, statically registered, activated, and migrated before it replaces
the live program. A bad save
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


### Keyboard-shortcut discovery

Shared `UiAction` records can supply `group` and `description` for contextual
keyboard help. Use `priority: "reference"` for a supported binding that belongs
in the complete reference but not in the compact action strip. Disabled actions
are omitted. The strip and its clickable `F1 shortcuts` affordance use the same
records, including the current mode and enabled state. `F1` opens help above the
active surface; closing help does not replace that surface or its draft.

## Agent model selection

Host API `0.14.0` adds `AgentReadDefaultModel(callback)`, which returns
`{ model_info, error }` for the current workspace without creating a thread.
It reads Codex's effective configuration, falling back to the catalog default
when no model is configured. This is a preview; thread settings remain authoritative.

`AgentListModels(callback)` and
`AgentSetModel(callback, session_id, selection)`. The catalog callback returns
`{ models, error }`; model entries use Codex's `model/list` shape. Selection is
`{ model: String, effort?: String }`. An empty session ID stores the choice for
the next conversation; an existing ID updates only that conversation's next-turn
settings. The callback reports `{ accepted, error }`. No global configuration is
written. `agent:model_changed` carries `{ session_id, model_info }`, where
`model_info` contains the effective `model`, optional `provider`, and optional
`effort`. Treat that event as authoritative, and ignore foreign session IDs.
`agent:model_rerouted` reports `{ session_id, model }` for the running turn only.

For name-oriented lists, `PickerOptions.item_layout: "label_first"` reserves the
longest filtered label, then aligns annotations and descriptions in shared columns.
Secondary fields disappear before labels are shortened; the default layout is unchanged.
`UpdatePickerSelection(handle, item_id)` selects a currently visible item without
resetting the query. This is useful after populating a loading picker in place.

`SetTextPanelHeaderDetail(id, detail?)` updates header metadata in place. Detail
is `{ text: String, secondary?: String, compact_text?: String, action?: String, shortcut?: String }`.
The header compacts its buttons before dropping secondary text. An optional
`compact_text` preserves essential state in the shortened label. Clicking the visible
detail emits the configured panel action; the shortcut works while the panel is focused and is included in contextual
help. Omitting detail clears it. Draft, focus, scroll, and panel layout survive.
