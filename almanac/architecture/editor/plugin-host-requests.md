---
title: "Plugin Host Requests"
summary: "Plugin host requests are the typed message boundary that lets Husk plugins ask for editor effects while the editor remains the only mutator of buffers, windows, UI resources, LSP state, agent state, and filesystem reconciliation."
topics: [architecture, editor, plugins, host-api]
sources:
  - id: dispatcher
    type: file
    path: src/dispatcher.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
---

Plugin host requests are Red's typed bridge from Husk plugin code into the editor event loop. Each `Runtime` owns a `Dispatcher<PluginRequest, PluginResponse>`; the Husk runtime translates `red::execute`, `red::request`, timers, callbacks, and resource APIs into `PluginRequest` values, sends them through that runtime dispatcher, and stores callbacks for requests that need a response [@runtime]. The editor drains those requests inside its background service tick, applies effects through editor-owned state, resolves plugin callbacks when needed, and requests rendering only after the state transition is complete [@editor].

## Dispatcher Boundary

The low-level dispatcher is a pair of synchronous request and response channels behind cloneable endpoints and a shared receiver lock [@dispatcher]. Its current process-lifetime assumptions are strict: sending to a disconnected receiver, blocking receive failures, and poisoned receiver locks panic rather than becoming ordinary plugin errors [@dispatcher]. `Runtime::try_new_with_permissions` creates a fresh dispatcher, stores it on the runtime, and gives the Red host a clone, so production request ownership is runtime-scoped rather than a process-global queue [@runtime].

`ACTION_DISPATCHER` remains as a compatibility facade for tests and external harnesses that inject plugin requests directly. It is thread-local and binds to the current runtime dispatcher, which prevents independently running tests from draining or enqueueing requests for a runtime owned by another test thread [@editor].

The request enum is the Rust side of the plugin host contract. Its documentation states that the runtime may construct requests, but only the editor event loop mutates buffers, windows, UI resources, agent state, LSP state, or the filesystem [@editor]. Variants cover semantic editor actions, agent session operations, permission responses, edit-history requests, picker and composer resources, buffer edits, cursor queries, buffer text, selections, plugin storage, editor snapshots, LSP-backed symbol requests, decorations, gutters, overlays, panels, workspaces, window bars, directory listing, git status, filesystem operations, and directory watchers [@editor].

## Runtime Translation

`Runtime` wraps the Red-agnostic Husk VM with a Red host. Its module documentation states that the host translates Husk calls into `PluginRequest` values, while the editor consumes those requests and remains the sole mutator of buffers and UI state [@runtime]. Direct host calls such as `red::execute("SetCursorPosition", ...)`, `red::execute("SetDecorations", ...)`, and `red::execute("OpenPicker", ...)` allocate handles when needed and enqueue typed requests through `send_request` [@runtime].

Request/response APIs use `RequestId`. `red::request` allocates a request id, records the plugin callback in `pending_requests`, builds a typed `PluginRequest`, and removes the callback again if request construction fails [@runtime]. When the editor later calls `plugin_registry.resolve_request`, `Runtime::resolve_request` removes the pending callback and invokes it with the JSON response [@runtime]. This keeps plugin code asynchronous from the editor loop's perspective even though the request queue itself is synchronous.

## Editor Drain And Mutation

The [event loop](event-loop) drains plugin requests from the active runtime up to a per-tick budget [@editor]. Each request is matched in `service_background`, and the editor decides whether it needs a full render, a motion render, a callback response, or no immediate paint [@editor]. This batching is important because a startup or plugin-refresh chain can issue many small requests; the editor drains a bounded batch so each operation does not wait for a separate 10 ms loop tick [@editor].

Text edits demonstrate the ownership boundary. `BufferInsert`, `BufferDelete`, and `BufferReplace` open plugin-labeled transactions, call the same `replace_range` helper as user edits, commit, notify LSP and plugins through `notify_change`, and mark the frame for render [@editor]. That means plugin text changes follow the [text mutation boundary](text-mutation-boundary) instead of directly calling raw buffer methods.

## Resources And Ownership Checks

Callback-scoped UI resources carry owner information. The runtime allocates picker and composer handles and records the plugin that owns each handle before sending open requests [@runtime]. The editor checks callback ownership before opening callback pickers, confirmations, composers, or inputs; if the handle no longer belongs to the requesting owner, it releases the handle, reports an error, and renders instead of invoking a stale callback path [@editor].

The runtime also enforces owner checks for mutating picker actions such as `UpdatePickerItems`, `UpdatePickerQuery`, `UpdatePickerStatus`, and `ClosePicker` [@runtime]. Plugin reload staging preserves the same boundary: host effects are staged during replacement and teardown, committed only after the replacement activates, and rolled back entirely on failure [@runtime]. This prevents a failed reload from leaking partial editor effects into the live event loop.

## Filesystem, LSP, And Agent Requests

Requests that touch external systems still return to the editor for reconciliation. LSP-backed requests such as document symbols, workspace symbols, references, and inlay hints ensure the current or requested file is open in LSP before issuing the language-server request, then map the language-server response back to the plugin request id later [@editor]. Filesystem operations are applied through plugin filesystem helpers, then the editor reconciles the outcome before resolving the plugin request and requesting render [@editor].

Agent requests share the same queue. Runtime actions enqueue `AgentNewSession`, prompts, cancellation, session close/archive/forget, permission responses, transaction reverts, and `EditHistory` requests [@runtime]. The editor owns the `AgentManager`, validates bridge and workspace state, starts Codex app-server tasks when needed, applies agent tool edits through editor transactions, and emits agent plugin notifications from the serialized loop [@editor].
