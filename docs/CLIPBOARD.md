# Clipboard delivery

Buffer yanks, agent transcript copies, and plugin `red/ClipboardWrite` requests use
one frontend-owned clipboard route. Buffer copies retain the full text in Red's
register even if external delivery fails.

## Automatic routing

`clipboard.backend = "auto"` selects the destination using the active frontend:

| Context | Delivery |
| --- | --- |
| No terminal or `TERM=dumb` | Internal register only |
| Herdr, SSH, or tmux | OSC 52 through the terminal connection |
| Local desktop | Native clipboard; OSC 52 if native initialization fails |

Herdr panes can persist without `SSH_*` variables. Red recognizes `HERDR_ENV=1`
and `TERM_PROGRAM=herdr` so copying still reaches the attached client rather than
the server's desktop clipboard. Remote routes never construct a native provider.

To override detection:

```toml
[clipboard]
enabled = true
backend = "osc52" # auto, native, osc52, none
sync_on_yank = true
sync_on_paste = true
max_bytes = 65536
```

`max_bytes` limits OSC 52 payloads in UTF-8 bytes before base64 encoding. The default
is 64 KiB and the maximum is 256 KiB. Oversized copies are refused rather than
truncated; Red's internal register remains intact. Native clipboard copies and
reads keep their existing behavior and do not use this terminal limit.

OSC 52 is a write request, not a desktop acknowledgment. The terminal and every
multiplexer in the connection must allow clipboard forwarding. Herdr must be
attached using `herdr --remote` for its remote-client forwarding path. For tmux,
application OSC 52 relay requires `set-clipboard on` and the outer terminal's
clipboard capability. Red does not change terminal or multiplexer permissions,
select a tmux client, or launch clipboard helper commands.

## Paste

With native delivery, `p` can read the native clipboard when `sync_on_paste` is
true. With OSC 52, `p` uses Red's internal register. Red never queries the terminal
clipboard or reads the SSH server's clipboard in this mode. Use the terminal's
paste shortcut to insert client clipboard text through bracketed paste.

## Detached sessions

The attached frontend chooses its route using its own environment and the owner's
clipboard configuration. The owner sends transient copy effects separately from
render snapshots. Input copies are delivered with their response; asynchronous
plugin copies are delivered on the next heartbeat. Only the final pending copy
is retained, and disconnect discards it so reconnect cannot replay old copies.

Native clipboard reads are requested only during paste actions, never on input
polling or reconnect. Detach IPC bounds clipboard text to 1 MiB and chunks writes
to fit frame limits, preserving the existing plugin write limit. OSC 52 retains
its smaller terminal-specific limit. IPC protocol version 4 requires restarting
older detached owners before attaching the updated frontend.

## Diagnose and verify

Run these from the same terminal context as Red:

```sh
red clipboard
red clipboard --test-copy
```

The first command reports routing without reading or replacing clipboard text.
The second copies `red clipboard test (BACKEND) — café 👻`. Paste into an application
on the client machine to confirm delivery. In Herdr, expect `Clipboard backend:
Osc52`, even when `SSH_CONNECTION` and `SSH_TTY` are absent.

For an editor check, yank a line with `yy`, paste into a client application, and
check its exact text. Then set `clipboard.max_bytes` below the line's byte length:
the copy should report a limit error, emit no clipboard sequence, and still paste
inside Red with `p`. Repeat through `red --detach` and reconnect to confirm that
old copy effects are not replayed.
