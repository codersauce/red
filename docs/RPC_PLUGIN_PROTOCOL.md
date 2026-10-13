# JSON-RPC plugin protocol

This document defines the transport profile and bounded initial surface for
version 1 of Red's language-agnostic plugin protocol. Exact typed method schemas
land with their implementations. Package validation and classification remain
independent of process activation.

## Package and process

A typical plugin keeps its manifest beside one executable or interpreter entry:

```text
document-tools/
├── red-plugin.toml
└── plugin.py
```

```toml
schema_version = 1

[plugin]
id = "document-tools"
name = "Document Tools"
version = "0.1.0"
red_api = "^0.19.0"

[rpc]
command = "python3"
args = ["plugin.py"]

[activation]
commands = ["DocumentTools"]
```

The `[rpc]` section selects the standalone process modality. It cannot be
combined with a Husk entrypoint or `[companion]`; `[languages]` remains an
independent declarative contribution. A bare command resolves through `PATH`.
A command containing `/` or `\\` resolves inside the package and must be an
existing file that cannot escape through traversal or symbolic links. Red
launches it with the package root as its working directory and passes `args`
directly without a shell.

## Transport profile

The transport is JSON-RPC 2.0 over UTF-8 standard input and output. Each compact
JSON message occupies one line. Standard output is reserved for protocol
messages; plugins write diagnostics to standard error. Batch arrays are not
supported. Requests receive exactly one result or error response. Notifications
receive no response.

Version 1 limits each frame to 8 MiB, queued data to 16 MiB in each direction
per connection, and outstanding requests to 64 in each direction. Red retains
at most the last 16 KiB of standard error when reporting a failure. Exceeding a
transport limit, writing malformed framing, or closing the stream disconnects
the plugin.
Request deadlines cover queue admission, writing, and the response.

## Lifecycle and negotiation

Red starts an activated plugin once and sends `initialize` with the application
protocol versions and supported host methods. The plugin selects protocol
version `1` and returns its command metadata and event subscriptions. Its
commands must exactly cover the commands declared by `[activation]`.

The version 1 initialization parameters are:

```json
{
  "protocol_versions": [1],
  "red_api_version": "0.19.0",
  "plugin_id": "document-tools",
  "host_methods": [
    "red/DocumentSnapshot",
    "red/DocumentApply",
    "red/DocumentUndo",
    "red/TextPanelCreate",
    "red/TextPanelSetContent",
    "red/TextPanelFocus",
    "red/TextPanelDispose",
    "red/StatuslineSet",
    "red/StatuslineClear"
  ]
}
```

`plugin_id` is the stable ID from `[plugin].id`, independent of any local
configuration alias used to register the package.

The response declares the selected version and the plugin's command metadata:

```json
{
  "protocol_version": 1,
  "commands": [
    {
      "name": "DocumentTools",
      "metadata": {
        "title": "Document tools",
        "arguments": true
      }
    }
  ],
  "events": []
}
```

Every command declared under `[activation].commands` must appear exactly once,
with no additional commands. Metadata uses Red's existing command fields:
`title`, `category`, `description`, `aliases`, `visible`, `scope`, `arguments`,
and `completions`. Omitted fields use the same defaults as Husk commands. If a
Husk and RPC plugin expose the same command name, the Husk command keeps
dispatch and discovery precedence.

The `events` array must likewise exactly match `[activation].events`. Protocol
version 1 accepts three low-frequency global editor events:

- `editor:ready` with an empty object payload;
- `file:opened` with `file` and `buffer_index` fields; and
- `file:saved` with `file`, `buffer_index`, and `document_id` fields.

Red lazily starts a plugin when one of its declared events occurs and includes
that triggering event. Each event is a one-way `event/notify` notification:

```json
{
  "name": "file:saved",
  "payload": {
    "file": "src/main.rs",
    "buffer_index": 0,
    "document_id": 42
  }
}
```

Undeclared events are not sent. Event notifications have no response, and a
write or transport failure quarantines only that plugin.

Red validates the full response before registering anything, then sends the
`initialized` notification. Plugins must not call host methods before that
notification. During shutdown Red sends `shutdown`, waits for its response,
sends `exit`, and terminates the process if it does not exit within the bounded
grace period.

The plugin must keep reading responses while a command or event handler is
running. A handler that waits for a host request while its message loop is
blocked will deadlock itself.

## Command execution

Red lazily starts the plugin when a declared command is first invoked, then
sends `command/execute`:

```json
{
  "name": "DocumentTools",
  "args": ["format", "selection"],
  "raw_args": "format selection"
}
```

Commands without `"arguments": true` receive empty `args` and `raw_args`.
Successful commands return `null` or `{ "message": "Done" }`; Red displays a
non-null message through its existing informational-message path. A JSON-RPC
error is shown as a warning and leaves the connection active. A transport or
result-schema failure quarantines the plugin.

## Document snapshots

An active plugin can read the current unsaved contents of an open document with
`red/DocumentSnapshot`. Pass an empty object to read the current document, or a
path to select another open document:

```json
{"jsonrpc":"2.0","id":"plugin-1","method":"red/DocumentSnapshot","params":{"path":"src/main.rs"}}
```

The result contains the editor path, exact revision as a decimal string, and
text:

```json
{"jsonrpc":"2.0","id":"plugin-1","result":{"path":"src/main.rs","revision":"42","text":"fn main() {}\n"}}
```

The optional `path` may be a string or `null`; additional fields are rejected
with JSON-RPC error `-32602`. A document that is not open in Red returns
application error `-32001`. These request errors leave the plugin active.

## Document transactions

`red/DocumentApply` applies one non-empty batch against an exact document
revision. Red validates every range and optional preimage before applying any
edit, then records the batch as one transaction attributed to the calling
plugin:

```json
{
  "jsonrpc": "2.0",
  "id": "plugin-2",
  "method": "red/DocumentApply",
  "params": {
    "path": "src/main.rs",
    "expected_revision": "42",
    "label": "Rename greeting",
    "edits": [{
      "range": {
        "start": {"line": 0, "character": 3},
        "end": {"line": 0, "character": 7}
      },
      "text": "welcome",
      "expected_text": "main"
    }]
  }
}
```

`path` is optional and defaults to the current document. `label` is optional
and defaults to `plugin edit`. The result contains the attributed transaction
ID and resulting revision as decimal strings:

```json
{"jsonrpc":"2.0","id":"plugin-2","result":{"transaction_id":"...","revision":"43"}}
```

`red/DocumentUndo` reverts that transaction only while it remains the latest
change and is owned by the calling plugin:

```json
{"jsonrpc":"2.0","id":"plugin-3","method":"red/DocumentUndo","params":{"path":"src/main.rs","transaction_id":"..."}}
```

Its result is `{"revision":"44"}`. Both calls preserve Red's ordinary dirty
tracking, undo history, rendering, plugin notifications, and LSP updates.
Invalid parameters return `-32602`, unavailable documents return `-32001`,
stale revisions return `-32002`, and rejected transactions or undo requests
return `-32003`. These application errors leave the plugin active.

## Text panels

An active plugin can create a host-rendered text panel with a connection-local
ID:

```json
{"jsonrpc":"2.0","id":"plugin-4","method":"red/TextPanelCreate","params":{"id":"preview","side":"right","width":36,"title":"Preview"}}
```

`side` is optional and accepts `left`, `right`, `top`, or `bottom`; `width`
defaults to `30`, and `title` is optional. Calling create again with the same ID
replaces that panel's configuration and content. A connection can own at most
16 text panels, and each ID must contain 1 to 128 UTF-8 bytes.

`red/TextPanelSetContent` replaces the panel's complete block list. Each block
has an `id`, `text`, optional semantic `kind` (`user`, `agent`, `error`,
`activity`, `action`, or `text`), and optional `format` (`plain` or `markdown`):

```json
{"jsonrpc":"2.0","id":"plugin-5","method":"red/TextPanelSetContent","params":{"id":"preview","blocks":[{"id":"result","format":"markdown","text":"# Result\nReady"}]}}
```

`red/TextPanelFocus` and `red/TextPanelDispose` each accept `{ "id": "preview"
}`. All four methods return `null` on success. Panel IDs are private to the
calling connection, so they cannot collide with Husk panels or panels owned by
another RPC plugin. Disconnect, quarantine, and successful hot reload dispose
all panels owned by the previous connection.

Unknown fields and invalid IDs or widths return `-32602`. A missing, disposed,
not-owned, or over-limit panel returns `-32004`. These errors leave the plugin
active.

## Statusline contributions

An active plugin can replace its single cached statusline contribution with
`red/StatuslineSet`:

```json
{"jsonrpc":"2.0","id":"plugin-6","method":"red/StatuslineSet","params":{"text":"Build ready","side":"right"}}
```

`side` is optional, defaults to `right`, and also accepts `left`. `text` must
contain a non-whitespace character, may not contain control characters, and is
limited to 128 UTF-8 bytes. The method returns `null`; calling it again replaces
the connection's previous text and side.

`red/StatuslineClear` accepts an empty object and returns `null`, including when
the connection has no contribution. Contributions render after configured
sections on the left and before configured sections on the right, toward the
center of the statusline. Red orders contributions by plugin name and drops or
truncates them before edge sections when terminal width is constrained.

Red renders only from its local cache. It never calls or waits for a plugin
while drawing a frame. Disconnect, quarantine, successful hot reload, and
editor deactivation clear the previous connection's contribution.

## Version 1 surface

Version 1 deliberately exposes a small set of typed operations:

- Commands declared in `[activation].commands`, with Red's existing argument
  and scope rules.
- Declared editor events with documented JSON payloads. Process-local callback
  events and raw keyboard input are excluded.
- `red/DocumentSnapshot`, `red/DocumentApply`, and `red/DocumentUndo`, preserving
  revisions, attributed undo transactions, dirty tracking, and LSP updates.
- Text-panel create, content replacement, focus, and disposal, with resources
  owned by the active plugin connection.
- One cached plain-text statusline contribution per active plugin connection.

Host calls use named parameters. Document revisions and request identifiers
that may exceed JavaScript's exact integer range are decimal strings on the
wire. Text positions use zero-based lines and Unicode-scalar indices; edit
ranges are half-open. They are neither UTF-8 byte offsets nor UTF-16 code-unit
offsets.

There is no generic host-action escape hatch. Intercept or veto hooks,
synchronous rendering callbacks, high-frequency cursor or viewport events, and
transparent process restart are outside version 1.

The current host integration activates and executes commands, delivers the
three editor events above, and serves the document and text-panel methods.
It also caches and renders one statusline contribution per active connection.

## Errors and failure semantics

Implementations use the standard JSON-RPC parse, invalid-request,
method-not-found, and invalid-params errors. Stable Red application errors
distinguish incompatible initialization, stale document revisions, unavailable
resources, deadline expiry, and revoked connections.

Invalid host-call arguments return an error without disconnecting the plugin.
Transport corruption, queue overflow, process exit, or a missed lifecycle
deadline disconnects and quarantines it. A timed-out mutation is never retried:
if its reply was lost, the plugin must request a fresh document snapshot to
reconcile the outcome.
