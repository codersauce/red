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

Release packages may omit `command` and provide a native executable for each
supported Rust target triple. Target-specific local commands and release
artifacts require `red_api = "^0.20.0"` or later:

```toml
[plugin]
red_api = "^0.20.0"

[rpc]
args = []

[rpc.artifacts.aarch64-apple-darwin]
url = "https://github.com/example/document-tools/releases/download/v0.1.0/document-tools-aarch64-apple-darwin"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
```

For the running target, Red selects `[rpc.commands]`, then `rpc.command`, then
`[rpc.artifacts]`. GitHub installs and updates download a selected artifact
into the staged package, reject executables larger than 128 MiB, verify the
declared SHA-256 digest, and set executable permissions before atomically
replacing the installed package. A failed download or verification leaves the
previous installation active. Local path installs do not download release
artifacts.

RPC processes start with a cleared environment. Red then inherits its baseline
home, `PATH`, user, shell, locale, terminal, temporary-directory, XDG,
`SSH_AUTH_SOCK`, and Windows process variables. Grant other variables by the
plugin's configured owner name; package manifests cannot request them:

```toml
[plugin_permissions.document_tools]
inherit_env = ["DOCUMENT_TOOLS_TOKEN"]
```

Values are never written to the log. This filtering limits accidental ambient
environment exposure; it does not sandbox the native plugin process.

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
the plugin. When a successful host response would exceed the frame limit, Red
returns application error `-32006` instead and keeps the connection active.
Request deadlines begin after bounded queue admission and cover the response.
A stalled writer therefore fails through the write watchdog, its request
deadline, or the shared outbound byte limit, whichever is reached first.

The portable [envelope conformance fixture](../tests/fixtures/rpc_v1_envelopes.jsonl)
contains one JSON object per line. Each `message` is a wire-envelope value;
`expected` is `request`, `notification`, `response`, or `invalid`. Run
`cargo test --lib protocol_v1_envelope_conformance` to check Red's parser, or
feed the same cases into another implementation. The fixture checks envelope
classification, not transport framing, lifecycle, or individual method schemas.

## Lifecycle and negotiation

Red starts an activated plugin once and sends `initialize` with the application
protocol versions and supported host methods. The plugin selects protocol
version `1` and returns its command metadata and event subscriptions. Its
commands must exactly cover the commands declared by `[activation]`.

The version 1 initialization parameters are:

```json
{
  "protocol_versions": [1],
  "red_api_version": "0.23.0",
  "plugin_id": "document-tools",
  "host_methods": [
    "red/DocumentCreate",
    "red/DocumentShow",
    "red/DocumentWatch",
    "red/DocumentUnwatch",
    "red/DocumentSnapshot",
    "red/DocumentApply",
    "red/DocumentUndo",
    "red/ConfigGet",
    "red/StorageGet",
    "red/StorageSet",
    "red/StorageDelete",
    "red/TextPanelCreate",
    "red/TextPanelSetContent",
    "red/TextPanelFocus",
    "red/TextPanelDispose",
    "red/StatuslineSet",
    "red/StatuslineClear",
    "red/PickerOpen"
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

After activation, the editor waits for command responses in the background,
so input, resizing, other plugins, and incoming host calls continue. Each
connection runs at most one command at a time; another invocation reports
that the plugin is busy without disconnecting it. The 30-second command
deadline still applies. Closing or replacing a connection retires its pending
command; late completion cannot affect the replacement.

An event-triggered activation queues its triggering events in arrival order.
Commands report busy until that activation finishes. When a command triggers
activation first, declared events that arrive during initialization are queued
behind the command.

Dependent actions in a key binding, repeated command, macro, or dot replay
pause until that command settles. Macro depth and instruction limits carry
across the wait. New user key, paste, or mouse-action input cancels the paused
actions, while the plugin command itself continues. Resize, focus, mouse
movement, and key-release events do not cancel them. Retiring the connection
also discards its paused actions.

Lazy and event-triggered initialization, replacement validation, and retired
process shutdown run in the background. The previous connection remains active
until its replacement validates, then its resources are cleared during the
swap. Notifications and host responses enter the bounded writer queue without
waiting for a child pipe acknowledgement. Eager plugins still initialize before
the editor first opens.

## Configuration and storage

`red/ConfigGet` accepts an empty object and returns the calling plugin's value
from `[plugin_config.<owner>]`, or `null` when it has no configuration. The
owner is the local name Red used to register the plugin; the process cannot
read another plugin's configuration by supplying an ID:

```json
{"jsonrpc":"2.0","id":"plugin-config","method":"red/ConfigGet","params":{}}
```

Plugins can persist JSON under that same owner with `red/StorageGet`,
`red/StorageSet`, and `red/StorageDelete`:

```json
{"jsonrpc":"2.0","id":"plugin-get","method":"red/StorageGet","params":{"key":"settings"}}
{"jsonrpc":"2.0","id":"plugin-set","method":"red/StorageSet","params":{"key":"settings","value":{"enabled":true}}}
{"jsonrpc":"2.0","id":"plugin-delete","method":"red/StorageDelete","params":{"key":"settings"}}
```

Get returns the stored value or `null` when the key is missing. Set and delete
return `null`; deleting a missing key succeeds. Values survive editor restarts
through Red's existing preferences store. Husk and RPC plugins use the same
owner-scoped store and lookup rules, so changing a plugin's local registration
name selects a different configuration and storage namespace.

Keys must contain 1 to 128 UTF-8 bytes and may not contain `:` or control
characters. Storage is unavailable when the configured owner name contains
`:` because existing preference keys use that delimiter. A configuration or
stored value may use at most 1 MiB after compact JSON serialization. Unknown
fields, invalid keys, and oversized writes return `-32602`; an unavailable
owner or oversized read returns `-32005`, and a persistence failure returns
`-32603`. These request errors leave the connection active.

## Editable documents

Host API `0.22.0` adds ordinary editable documents for explicit user workflows.
`red/DocumentCreate` accepts initial text of at most 1 MiB and an optional
supported syntax name:

```json
{"jsonrpc":"2.0","id":"plugin-create","method":"red/DocumentCreate","params":{"text":"{\"ok\":true}\n","syntax":"json"}}
```

The result contains a stable decimal-string document ID:

```json
{"jsonrpc":"2.0","id":"plugin-create","result":{"document_id":"42"}}
```

The document is an ordinary unnamed buffer, outside connection-owned cleanup.
Initial text that differs from Red's normal blank-buffer baseline is one
attributed undo transaction, so generated content is dirty, prompts before
discard, can be undone independently, participates in crash recovery, and can
be saved with the usual Save As flow. Closing, disabling, quarantining, or
reloading the plugin leaves the document open. `syntax` must contain 1 to 128
UTF-8 bytes and resolve to an installed syntax; invalid or unknown values
return `-32602`.

`red/DocumentShow` reveals a live document by ID. The optional `target` is
`current` by default and also accepts `horizontal` or `vertical` to use Red's
native split routing:

```json
{"jsonrpc":"2.0","id":"plugin-show","method":"red/DocumentShow","params":{"document_id":"42","target":"current"}}
```

The result is `null`. A closed ID returns `-32001`; malformed IDs or targets
return `-32602`; a split or render failure returns `-32003`. Only this explicit
call changes focus, so background computation cannot reveal a document by
creating it. Plugins using either method must require `red_api = "^0.22.0"` or
later.

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

The optional `path` may be a string or `null`; unknown fields are rejected
with JSON-RPC error `-32602`. A document that is not open in Red returns
application error `-32001`. A snapshot that cannot fit in one frame returns
`-32006`. These request errors leave the plugin active.

Host API `0.20.0` adds `include_document_id: true` to request a `document_id`
field in the snapshot result. It is a decimal string that identifies the open
buffer, including an unnamed buffer, independently of its position in the
buffer list. It remains stable until that buffer closes and must not be saved
across editor restarts. Omitting the option preserves the original result shape.

Snapshot, apply, and undo requests accept this ID as an optional `document_id`
selector. A closed ID returns `-32001`; malformed IDs or conflicting `path` and
ID selectors return `-32602`. Plugins using these fields must require
`red_api = "^0.20.0"` or later. Older RPC and Husk packages continue to load.

## Document watches

Host API `0.23.0` adds owner-local invalidation notifications for live document
workflows. Establish a watch by stable document ID before taking the first
snapshot:

```json
{"jsonrpc":"2.0","id":"plugin-watch","method":"red/DocumentWatch","params":{"document_id":"42"}}
{"jsonrpc":"2.0","id":"plugin-watch","result":{"revision":"7"}}
```

The result is the document revision observed when the watch was installed. A
plugin should then snapshot the document and treat any notification received
during setup as a request to read the latest state again. This ordering ensures
that changes during setup are either present in the snapshot or reported as an
invalidation.

Each connection may watch at most two documents. Repeating a watch is
idempotent and does not consume another slot. Red coalesces changes for 50 ms
before enqueueing one owner-local `document/event` notification containing the
latest revision:

```json
{"jsonrpc":"2.0","method":"document/event","params":{"document_id":"42","event":"changed","revision":"9"}}
```

The event is an invalidation, not an edit log or document snapshot. Plugins
must read the current document when they need its text. A close removes the
watch, drops any pending change, and sends one terminal event without a
revision:

```json
{"jsonrpc":"2.0","method":"document/event","params":{"document_id":"42","event":"closed"}}
```

Stop watching with `red/DocumentUnwatch`; it returns `null` and is idempotent,
including after the document closes. Disconnect, quarantine, successful reload,
and editor deactivation release every remaining watch. Malformed or closed IDs
return `-32602` or `-32001` when installing a watch; exceeding the cap returns
`-32004`. These application errors leave the connection active.

Notifications are coalesced before they enter the existing 16 MiB outbound
queue and remain subject to the 8 MiB frame and transport watchdog limits.
Transport failure or queue overflow quarantines a slow connection. Plugins
using watches must require
`red_api = "^0.23.0"` or later.

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

For deferred work, request a snapshot with `include_document_id: true` and
pass its `document_id` with the revision to apply, then with the transaction ID
to undo. ID-selected mutations currently require that document to be active;
otherwise they return `-32003` without switching focus or changing contents.
Select the document again before retrying, and refresh its snapshot if its
revision has changed. An ID never silently falls back to another open buffer.
Explicit path mutations preserve the active buffer, view, and user insertion
transaction when editing another open document. Plugin edits and user edits
remain separate undo transactions; undo checks include uncommitted user edits.
During a command, a legacy mutation with neither path nor ID is rejected if
the current document differs from the one at invocation. The host never
silently redirects such a request to that earlier document. Outside a pending
command, omitted selectors still mean the current document; use an explicit
selector for work that can outlive a buffer switch, since a revision alone
does not identify a document.

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

## Pickers

Host API `0.21.0` adds a host-rendered, locally filtered picker. Opening it is a
bounded host request; `command/execute` does not wait for user selection:

```json
{"jsonrpc":"2.0","id":"plugin-7","method":"red/PickerOpen","params":{"id":"targets","title":"Choose a target","items":[{"id":"debug","label":"Debug"},{"id":"release","label":"Release","detail":"optimized","data":{"profile":"release"}}]}}
```

`id` identifies the picker within the calling connection and must contain 1 to
128 UTF-8 bytes. `title` is optional. `items` may contain at most 4096 entries;
each entry requires string `id` and `label` fields and may include string
`detail` and arbitrary JSON `data`. Unknown fields are rejected. The method
returns `null` after Red installs and renders the picker. Red owns filtering,
selection, keyboard handling, and drawing, so rendering never waits on the
plugin.

Selection closes the picker and sends only its owner a `picker/event`
notification containing the connection-local ID and selected item:

```json
{"jsonrpc":"2.0","method":"picker/event","params":{"id":"targets","event":"selected","item":{"id":"release","label":"Release","detail":"optimized","data":{"profile":"release"}}}}
```

Escape or Ctrl-C sends `{"id":"targets","event":"cancelled"}` through the
same notification method. Local query and highlight changes are not sent to
the plugin. Replacing the dialog, disconnecting, quarantine, successful hot
reload, and editor deactivation release the picker without waiting on the
process. Picker IDs are private to the connection, and RPC handles occupy a
separate range from Husk callback handles, so both plugin modalities continue
to use the same native picker UI without sharing callback ownership.

Invalid parameters return `-32602` and leave the plugin active. Transport
failure while sending a terminal notification quarantines the owning plugin.

## Version 1 surface

Version 1 deliberately exposes a small set of typed operations:

- Commands declared in `[activation].commands`, with Red's existing argument
  and scope rules.
- Declared editor events with documented JSON payloads. Process-local callback
  events and raw keyboard input are excluded.
- Owner-scoped configuration and bounded persistent JSON storage.
- Ordinary editable document creation and explicit ID-based reveal through
  `red/DocumentCreate` and `red/DocumentShow`.
- `red/DocumentSnapshot`, `red/DocumentApply`, and `red/DocumentUndo`,
  preserving revisions, attributed undo transactions, dirty tracking, and LSP
  updates.
- Owner-local, coalesced document invalidations through `red/DocumentWatch`,
  `red/DocumentUnwatch`, and `document/event`.
- Text-panel create, content replacement, focus, and disposal, with resources
  owned by the active plugin connection.
- One cached plain-text statusline contribution per active plugin connection.
- Host-rendered, locally filtered pickers with owner-local terminal events.

Host calls use named parameters. Document revisions and request identifiers
that may exceed JavaScript's exact integer range are decimal strings on the
wire. Text positions use zero-based lines and Unicode-scalar indices; edit
ranges are half-open. They are neither UTF-8 byte offsets nor UTF-16 code-unit
offsets.

There is no generic host-action escape hatch. Intercept or veto hooks,
synchronous rendering callbacks, high-frequency cursor or viewport events, and
transparent process restart are outside version 1.

The current host integration activates and executes commands, delivers the
three editor events above, serves configuration, storage, document,
text-panel, statusline, and picker methods, and keeps UI resources owned by the
active connection.

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
