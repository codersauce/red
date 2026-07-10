# ADR 0002: Detachable headless-core boundary

- Status: accepted for the Phase 0 feasibility spike
- Date: 2026-07-10
- Scope: ownership split, local IPC, reconnect, backpressure, cleanup, crash behavior,
  Windows transport, and the Phase 3 B.2 re-estimate

## Decision

Detach will use a long-lived headless Red process and a replaceable terminal client. The
headless process owns authoritative editor state. The TUI client owns the terminal and
has no independent copy of mutable buffers, plugin state, LSP state, or agent state.

The protocol is an explicitly versioned, same-machine, ordered message stream. The first
release supports one attached client per session. TCP attach, collaboration, and
simultaneous clients are non-goals.

`src/headless/mod.rs` is the executable feasibility proof. It accepts a normalized key or
paste event over a byte stream, mutates state in a persistent owner, and returns a
correlated logical render delta. Tests run the same framing over Tokio's in-memory duplex
stream and a real Unix-domain socket. Reconnect includes the client's last render
revision; a current client avoids a full repaint, while an unknown/stale revision receives
a complete logical frame.

The spike does not wrap the production `Editor`. That is the central result: today's
`Editor` is not already a client/server boundary, and `RenderBuffer` is a terminal-frame
optimization rather than a transferable ownership seam.

## Current ownership inventory

`Editor` currently owns all of the following in one object and drives them from one loop:

| Area | Current concrete state | Detach owner |
|------|------------------------|--------------|
| Terminal | stdout buffer, terminal enablement, focus/click suppression, terminal size | TUI owns setup/stdout; core owns logical focus and size sent by control messages |
| Input | crossterm polling, pending event queue, count/operator/key-prefix state, command/search input | TUI normalizes OS events; core owns every editing/input state machine |
| Documents | buffers, revisions, dirty state, current buffer, undo transactions | Core |
| Layout | windows, cursors, viewports, wrap/skip columns, selections, jumplist | Core |
| Rendering | theme, highlighter, layout/highlight caches, render generation, last cursor surface, plugin render commands | Core computes logical styled deltas; TUI translates deltas to terminal escape sequences |
| Plugins | registry, Husk VM lifecycle, overlays, decorations, gutters, panels, workspaces, window bars, timers/processes | Core |
| Language services | LSP process/client, opened-document set, diagnostics, pending requests | Core |
| Agents | ACP bridge, adapter process task, sessions and future proposal state | Core |
| Filesystem | directory watchers, file load/save, workspace identity | Core; TUI never writes workspace files |
| Persistence | preferences, histories, future crash snapshots and reconnect metadata | Core |
| Local OS integration | system clipboard | Split RPC: registers remain core-owned; clipboard read/write executes in the attached TUI |

The extraction must first split `Editor` into a terminal-independent `EditorCore` and a
`TerminalClient`. Moving the existing struct unchanged into another process would still
leave stdout, crossterm polling, clipboard, and rendering coupled to the server.

## Protocol and lifecycle

- Protocol version starts at `1`; handshake mismatch fails closed with an actionable
  error. There is no best-effort deserialization across incompatible versions.
- Client input carries a monotonically increasing sequence. Render responses echo that
  sequence and carry a monotonically increasing core revision.
- Frames are NDJSON with a 1 MiB hard limit in the spike. Production may move to a
  length-prefixed codec, but limits and typed messages remain mandatory.
- The production path uses bounded input/control queues. A client may have only a small
  number of unacknowledged inputs. On render backpressure, the core discards superseded
  deltas and retains one latest full-frame snapshot; it never accumulates an unbounded
  frame history.
- A reconnect token identifies the session but is not an authentication secret. Socket
  ownership and filesystem permissions provide same-user authorization. The core rejects
  a second live client in v1 and may evict a stale client only after its heartbeat lease
  expires.
- Resize and focus are core control messages. Reconnect always sends current size/focus,
  and the core produces a fresh full frame when geometry differs.
- The TUI restores terminal modes on normal exit, protocol error, and panic. A TUI crash
  cannot terminate the core. A core crash closes IPC; the TUI restores the terminal and
  offers the most recent B.1 crash snapshot rather than silently starting empty.
- Socket files live in a user-private runtime directory, are mode `0600`, and include a
  separately validated owner PID/session record. Clean shutdown removes them. Startup
  removes a stale socket only after proving that its recorded owner process is gone.
- An unattached core remains alive while an agent/task is active. With no client and no
  work, it exits after a documented idle lease. `red --stop <session>` is the explicit
  cleanup path.

## Platform transport

Linux and macOS use Unix-domain sockets. Windows requires named pipes with an equivalent
same-user ACL and message/framing behavior. The first B.2 release is explicitly limited
to Linux and macOS; Windows receives B.1 crash recovery but not detach. Windows named-pipe
support is a follow-up release gate before Red again describes detach itself as
cross-platform. TCP loopback is rejected as the shortcut because it creates unnecessary
port discovery, firewall, and authentication surface.

## Re-estimate

Phase 3 B.2 is revised from `4–7` to `6–9` focused engineer-weeks for Linux/macOS:

- 2–3 weeks: extract terminal-independent core/input ownership from `Editor`;
- 1.5–2 weeks: production IPC actor, bounded delta stream, reconnect/heartbeat, and
  lifecycle commands;
- 1–1.5 weeks: thin terminal client, terminal restoration, resize/focus, and clipboard
  RPC;
- 1–1.5 weeks: crash/kill/SSH fault tests, permissions, stale cleanup, packaging, and
  observability;
- 0.5–1 week contingency for plugin/LSP callbacks that currently assume loop-local
  access.

Windows named pipes add an estimated 2–3 weeks and are not included in that first release.
The overall Phase 3 estimate changes from `12–19` to `14–21` weeks.

## Consequences

- Phase B.1 persistence remains a prerequisite; detach must not be the only recovery
  mechanism.
- Core extraction is real architecture work, not wiring around `RenderBuffer`.
- Logical render deltas keep terminal escape details out of the core, but styles, layout,
  panels, overlays, and cursor decisions remain authoritative core state.
- The spike's codec and input/render correlation can be reused in B.2; its toy document
  model will be deleted when `EditorCore` owns the production action/edit pipeline.
