# Clipboard delivery

Editor-buffer yanks, agent transcript selections (`v` / `V`, motions, `y`), and
whole-answer copying use the same register and clipboard path. A copy always keeps
the full text in Red's register, even when external delivery fails.

## Automatic selection

`clipboard.backend = "auto"` chooses from the current terminal's environment:

| Context | Delivery |
| --- | --- |
| No terminal or `TERM=dumb` | Internal register only |
| Inside tmux | tmux's clipboard command, targeting one attached client |
| SSH session | OSC 52 to the client terminal |
| GNU Screen | OSC 52 with conservative DCS wrapping |
| Local desktop | Native clipboard; terminal delivery if native initialization fails |

Native delivery supports macOS, Windows, X11, and Wayland compositors implementing
the data-control protocols used by `arboard`. Unsupported Wayland compositors may
require XWayland or terminal delivery. Native provider objects remain alive so Linux
clipboard ownership is retained while the frontend runs.

Environment detection is a hint, not proof of the entire terminal chain. SSH variables
can be absent or stale, and multiplexers can conceal the outer terminal. Force a
backend when necessary:

```toml
[clipboard]
enabled = true
backend = "osc52" # auto, native, osc52, tmux, command, none
sync_on_yank = true
sync_on_paste = true
max_bytes = 65536
passthrough = "none" # none, tmux, screen
```

`max_bytes` counts UTF-8 bytes before base64 encoding. The default is 64 KiB and the
hard safety limit is 256 KiB. A terminal may impose a lower limit. Oversized copies
are refused without truncating the internal register. GNU Screen wrapping is limited
to a 740-byte encoded sequence for compatibility with older DCS parsers. Do not split
a long copy into multiple independent OSC 52 writes: they replace one another.

## SSH and multiplexers

OSC 52 uses the terminal connection, so the SSH server's OS does not need to match
the client OS. Ghostty, for example, supports clipboard writes subject to its
`clipboard-write` permission. Red never changes terminal or multiplexer settings.

The tmux backend uses `tmux load-buffer -w -t CLIENT -`, with copied text on stdin.
It supports `set-clipboard on` and `external`, but honors `off`. The client must
advertise clipboard support, either through terminal features or its `Ms` capability.
If more than one client is attached to the session,
set `clipboard.tmux_client` to the intended client tty or detach the extra clients.
The target is resolved again for each copy because a tmux pane can outlive its client.
Other sessions' clients are never chosen. Tmux's paste buffer is not treated as a
live view of the desktop clipboard.

For ordinary OSC 52 emitted inside tmux, its relay needs `set-clipboard on` and a
working `Ms` capability. `passthrough = "tmux"` is an explicit alternative requiring
the user's `allow-passthrough` permission. It is not enabled automatically. Every
layer in a nested terminal/multiplexer chain must support the selected route.

Zellij normally relays OSC 52; a configured server-side `copy_command` can change its
destination. Screen wrapping, nested multiplexers, and terminal-specific limits must
be validated against the actual installed versions. Unknown or denied routes can
retain the register without updating the desktop clipboard.

References: [tmux clipboard](https://github.com/tmux/tmux/wiki/Clipboard),
[Ghostty OSC 52](https://ghostty.org/docs/vt/osc/52),
[Zellij copy options](https://zellij.dev/documentation/options.html#copy_command),
[arboard Linux support](https://docs.rs/crate/arboard/3.6.1).

## Paste and explicit command providers

Terminal paste (Command-V on macOS, or the terminal's paste binding elsewhere) sends
the client clipboard as bracketed paste. It does not require Red to query a clipboard.
With OSC 52 or tmux delivery, `p` uses Red's internal register. Red does not issue OSC
52 read queries or automatically read the SSH server's desktop clipboard.

For a trusted helper, including platform-specific or WSL integrations, configure argv:

```toml
[clipboard]
backend = "command"
copy_command = ["my-clipboard-helper", "copy"]
# Optional: explicitly permits a read when a paste action requests it.
paste_command = ["my-clipboard-helper", "paste"]
```

No shell is involved. Copy contents go to stdin; read contents must be UTF-8 on stdout.
Commands have a 750 ms deadline and bounded output. Their stderr and clipboard payloads
are not included in diagnostics. A helper runs on Red's host: remote users must select
a helper that actually reaches the intended client. OSC 52 reads are intentionally
unsupported; use terminal paste or an explicitly configured helper instead.

## Detached sessions

The attached frontend selects its backend using the owner's clipboard configuration
and its own current environment. The core keeps registers and sends transient copy
effects during an input transaction. Effects are separate from render snapshots,
chunked safely across IPC, consumed once, and discarded on reconnect.

Paste requests ask the current attachment for clipboard text only when needed. Native
or command read errors preserve Red's register. Typing, mouse movement, heartbeats,
and reconnecting do not read the clipboard. Reads time out after three seconds. The
protocol is version 4: stop an older owner before attaching the updated client.

## Diagnose and verify

Run these in the same terminal/multiplexer context as Red:

```fish
red clipboard
red clipboard --test-copy
```

The first command reports selection, read policy, wrapping, and size limits without
reading or changing clipboard contents. The second explicitly replaces the clipboard
with `red clipboard test (BACKEND) — café 👻`; paste it into a client-side application to verify.
Successful OSC 52, tmux, or custom-command delivery is reported as sent, not confirmed:
there is no portable acknowledgment that the desktop clipboard changed.

Tests cover backend selection, encoding, size/error handling, internal-register
preservation, and real detach IPC. Release verification must additionally exercise
actual desktop pastes on macOS, Windows, X11 and Wayland, direct SSH, tmux on either
side, and the nested/multiplexer combinations claimed for that release. A byte-level
test alone is not evidence that a particular terminal updated its clipboard.
