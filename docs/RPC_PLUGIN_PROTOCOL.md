# JSON-RPC plugin protocol

This document defines the transport profile and bounded initial surface for
version 1 of Red's language-agnostic plugin protocol. Exact typed method schemas
land with their implementations. Package validation and classification are
implemented independently of process activation, which follows in later steps.

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
  "host_methods": ["red/DocumentSnapshot"]
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

Host calls use named parameters. Document revisions and request identifiers
that may exceed JavaScript's exact integer range are decimal strings on the
wire. Text positions use zero-based lines and Unicode-scalar indices; edit
ranges are half-open. They are neither UTF-8 byte offsets nor UTF-16 code-unit
offsets.

There is no generic host-action escape hatch. Intercept or veto hooks,
synchronous rendering callbacks, high-frequency cursor or viewport events, and
transparent process restart are outside version 1.

Statusline providers are planned as a later negotiated capability. Their data
will be pushed into a host-owned cache so rendering never waits for a plugin;
disconnecting the plugin clears its contribution. Version 1 assigns no wire
method or schema to that future surface.

The current host integration activates and executes commands. Event delivery,
document host calls, text panels, and statusline providers land as separate
typed steps; declaring RPC activation events before event delivery is available
fails activation with a diagnostic.

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
