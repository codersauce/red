# ADR 0002: Detachable headless-core boundary

- Status: accepted and implemented for Linux/macOS
- Date: 2026-07-10
- Scope: ownership split, local IPC, reconnect, backpressure, cleanup, crash behavior,
  and the first-release Windows transport decision

## Decision

Detach uses a long-lived, terminal-independent Red owner and a replaceable thin TUI
client. The owner contains the production `Editor`, Husk runtime, LSP manager, directory
watchers, persistence store, proposal workspace, and ACP bridge/task. It processes the
same background pump as the ordinary editor loop, including timers, plugin processes,
hot reload, LSP responses, plugin requests, and agent events. Dropping the client does
not drop any of those objects.

The TUI owns raw mode, alternate-screen setup, crossterm input, and terminal painting. It
sends normalized key/paste, resize, focus, heartbeat, detach, and stop messages. The
owner returns logical styled row replacements and the authoritative cursor position. A
new session created by `red --detach [SESSION]` is placed in a new Unix process session
with null standard streams, so a controlling SSH terminal hangup cannot terminate it.

The first release supports exactly one attached client per session. TCP attach, remote
attach, collaboration, and simultaneous clients are non-goals.

## Commands and lifecycle

- `red --detach [SESSION] [files...]` starts the owner and attaches the current terminal.
  The optional session name defaults to `default`.
- `Ctrl-\` detaches the TUI without stopping the owner.
- `red --attach SESSION` reconnects from another terminal.
- `red --stop SESSION` performs authenticated administrative shutdown even while a client
  is attached.
- A normal editor quit also stops the owner. Otherwise the owner remains alive until an
  explicit stop; v1 has no idle timeout that could surprise a long-running agent task.
- `red --resume` is separate crash recovery. It remains available on every platform and
  is the fallback if the owner process itself dies.

## Ownership boundary

| Area | TUI client | Headless owner |
|------|------------|----------------|
| Terminal | raw/alternate screen, input polling, ANSI painting | logical size/focus and styled render frame |
| Editing | normalized events only | modes, counts, operators, macros, buffers, cursors, windows, undo |
| Services | none | LSP clients, Husk VM, plugin processes, timers, file watchers |
| Agents | none | ACP adapter process, bridge, transcript, proposals, permissions |
| Persistence | none | atomic session generations, preferences, attributed history |
| Workspace files | none | explicit saves and accepted proposal transactions |

`DetachedEditorCore` is not a second editor implementation. Both interactive paths use
`Editor::process_editor_event` and `Editor::service_background`, preserving one action,
plugin, LSP, and ACP execution boundary.

## Protocol and backpressure

Protocol version `2` is an explicitly versioned NDJSON stream with a 1 MiB frame limit.
The handshake fails closed on a version or reconnect-token mismatch. Client input has a
monotonic sequence and its render response echoes that sequence. Core render revisions
advance only when styled rows or the cursor change.

Each connection is deliberately request/response: at most one input/control operation is
unacknowledged, so input and output memory are bounded without an auxiliary unbounded
queue. Background work runs every 10 ms independently of connection state. The client
sends a heartbeat every five seconds; its response includes any newer render state. A
client that is silent for 15 seconds loses its lease. A reconnect at the current revision
avoids a full repaint; stale or unknown revisions receive a full logical frame.

## Authorization and cleanup

Session socket, reconnect token, and PID files live under Red's user-private `run`
directory. The directory is mode `0700`; socket, token, and PID are mode `0600`. The
unguessable token is checked on attach and stop. Startup only removes stale metadata
after the recorded PID is no longer alive. RAII cleanup removes all three files after a
clean owner exit.

The owner rejects a second live TUI explicitly. A separate authenticated `StopControl`
handshake remains available so operators can stop a wedged or still-attached session.
Malformed, oversized, or unsupported frames close only that client connection.

## Platform transport

Linux and macOS use Unix-domain sockets. Windows gets B.1 snapshot/resume but not B.2
detach in this release. Named pipes with equivalent same-user ACLs and framing remain the
required Windows design; TCP loopback is rejected because it introduces port discovery,
firewall, and authentication surface without enabling a supported product goal.

## Verification

The automated boundary includes:

- in-memory codec, sequence, revision, and reconnect tests;
- a real Unix-socket test;
- private permission, one-client, stale/drop, reattach, and administrative-stop coverage;
- production `Editor` input across a dropped connection; and
- a live ACP adapter test that records its PID, drops the client without a detach
  handshake, reattaches, and proves the original adapter process is still alive.

A real dropped-SSH run remains a release acceptance exercise because CI cannot reproduce
an external SSH daemon and terminal teardown faithfully. The implementation test covers
the ownership invariant underneath that exercise.

## Consequences

- B.1 persistence stays a prerequisite; detach is not crash recovery.
- The owner is authoritative. Clients never merge or replay mutable editor state.
- Styling and cursor decisions cross IPC as logical data; terminal escape sequences do
  not.
- The Unix-only limitation is explicit rather than hidden behind a nominal
  cross-platform claim.
- Multiple clients and remote transport require a new ADR and protocol version.
