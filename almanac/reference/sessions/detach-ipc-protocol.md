---
title: "Detach IPC Protocol"
summary: "Red detach IPC protocol version 3 is a newline-delimited JSON protocol for authenticated local clients, terminal input, render deltas, detach, and stop control."
topics: [sessions, detach, ipc, reference]
sources:
  - id: headless-ipc
    type: file
    path: src/headless/mod.rs
  - id: main-entry
    type: file
    path: src/main.rs
  - id: detach-doc
    type: file
    path: docs/DETACH.md
---

The detach IPC protocol connects a local terminal client to a running [detachable editor core](../../architecture/sessions/detachable-editor-core). Protocol version 3 uses newline-delimited JSON frames over an ordered local byte stream, authenticates named Unix sessions with a reconnect token, sends terminal-independent input, and returns render deltas capped by frame and terminal-size limits [@headless-ipc]. This page is lookup material for message shapes, limits, and expected responses; operational steps belong in [Detach and reattach](../../guides/sessions/detach-reattach).

## Version And Framing

`IPC_PROTOCOL_VERSION` is `3` [@headless-ipc]. The documented protocol version is also 3, and users are told to stop an older owner before attaching a version-3 client because protocol versions are intentionally not mixed across a running session [@detach-doc].

Every frame is one JSON value followed by a newline. Reads reject a frame that exceeds 2 MiB before the newline arrives, reject an EOF that occurs after a partial frame, and deserialize the complete frame as JSON [@headless-ipc]. Writes serialize JSON, require the encoded frame to be below the same 2 MiB limit, append a newline, flush, and time out after five seconds [@headless-ipc].

## Authentication And Rendezvous

Named sessions use three files derived from the session name: `<name>.sock`, `<name>.token`, and `<name>.pid` [@headless-ipc]. Session names must be non-empty single components containing only ASCII letters, numbers, dash, underscore, or dot [@headless-ipc]. On Unix, the owner creates a private socket, writes a UUID reconnect token and PID file with owner-only permissions, and removes rendezvous files when the bound session is dropped [@headless-ipc].

Interactive clients read the token file, connect to the Unix socket, and include the trimmed token in the `Connect` handshake [@headless-ipc]. Stop requests outside an attached client use `StopControl` with the same protocol version and reconnect token [@headless-ipc]. Sessions are local to the current OS user and do not expose a TCP port [@detach-doc].

## Client Messages

All client messages use a tagged JSON field named `type`, with snake-case variants [@headless-ipc].

| Message | Required fields | Meaning |
| --- | --- | --- |
| `connect` | `protocol_version`, `reconnect_token`, `last_revision`, `columns`, `rows`, `focused` | Authenticates and attaches a rendering client. `columns` and `rows` default to 80 by 24, and `focused` defaults to true when omitted [@headless-ipc]. |
| `stop_control` | `protocol_version`, `reconnect_token` | Authenticates a control-only stop request before an interactive attachment exists [@headless-ipc]. |
| `input` | `sequence`, `event` | Applies one ordered terminal-independent input event and expects a render response with the same sequence [@headless-ipc]. |
| `resize` | `columns`, `rows` | Updates viewport dimensions and returns a control render with sequence 0 [@headless-ipc]. |
| `focus` | `focused` | Updates interactive focus and returns a control render with sequence 0 [@headless-ipc]. |
| `heartbeat` | none | Renews the client lease and returns render changes newer than the client revision [@headless-ipc]. |
| `detach` | none | Closes the attachment while leaving the owner alive [@headless-ipc]. |
| `stop` | none | Stops the owner through an authenticated attached connection [@headless-ipc]. |

## Input Events

`InputEvent` uses a tagged JSON field named `kind`, with snake-case variants [@headless-ipc].

| Event | Fields | Meaning |
| --- | --- | --- |
| `key` | `code`, `modifiers` | Sends a normalized key. Codes include characters, enter, backspace, escape, tab, reverse tab, function keys, delete, arrows, home, end, page up, and page down. Modifiers are `control`, `alt`, and `shift` [@headless-ipc]. |
| `paste` | `text` | Sends a complete UTF-8 paste payload [@headless-ipc]. |
| `paste_chunk` | `text`, `final_chunk` | Appends to pending paste state and applies the paste only when `final_chunk` is true [@headless-ipc]. |
| `mouse` | `event` | Sends Crossterm's portable mouse event DTO, preserving native click and scroll data [@headless-ipc]. |

The main terminal client sends pastes up to 128 KiB as `paste`; larger pastes are split on character boundaries into `paste_chunk` frames [@main-entry]. The owner caps aggregate pending paste at 16 MiB and clears pending paste state on overflow or disconnect [@headless-ipc].

## Server Messages

All server messages use a tagged JSON field named `type`, with snake-case variants [@headless-ipc].

| Message | Fields | Meaning |
| --- | --- | --- |
| `connected` | `protocol_version`, `render` | Confirms handshake success and returns the initial render [@headless-ipc]. |
| `render` | `sequence`, `delta` | Returns a render delta. Input responses use the input sequence; resize, focus, and heartbeat use sequence 0 [@headless-ipc]. |
| `detached` | none | Confirms a clean detach [@headless-ipc]. |
| `stopped` | none | Confirms a stop request [@headless-ipc]. |
| `error` | `message` | Returns a protocol or editor error safe for the client to show [@headless-ipc]. |

`RenderDelta` contains a monotonic `revision`, a list of `LinePatch` entries, and a cursor tuple [@headless-ipc]. A reconnecting client can send `last_revision`; when it matches the owner's current revision, the owner may return an empty line set instead of a full repaint [@headless-ipc].

## Limits And Timeouts

| Contract | Value |
| --- | --- |
| Maximum frame size | 2 MiB [@headless-ipc] |
| Maximum pending paste | 16 MiB [@headless-ipc] |
| Client heartbeat lease | 15 seconds [@headless-ipc] |
| Handshake timeout | 5 seconds [@headless-ipc] |
| Write timeout | 5 seconds [@headless-ipc] |
| Input/control response timeout | 15 seconds [@headless-ipc] |
| Maximum columns | 4096 [@headless-ipc] |
| Maximum rows | 4096 [@headless-ipc] |
| Maximum cells | 12,288 [@headless-ipc] |

Terminal sizes must be non-zero and must satisfy all column, row, and total-cell limits before allocation [@headless-ipc].

## Error Cases

The first message on an interactive connection must be `connect`; otherwise the owner returns an error [@headless-ipc]. A protocol-version mismatch or reconnect-token mismatch returns `detach protocol version or reconnect token mismatch` [@headless-ipc]. After initialization, a second `connect` or `stop_control` on the same connection returns `connection is already initialized` [@headless-ipc].

Only one interactive client may attach. When a client is already attached, a new interactive connection receives `detach session already has an attached client`; an authenticated `stop_control` is still honored so `red --stop` can stop a busy owner [@headless-ipc].
