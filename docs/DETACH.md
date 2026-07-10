# Detachable sessions

On Linux and macOS, Red can keep the editor, unsaved buffers, LSP servers, plugins, and
running ACP agent process alive after a terminal or SSH connection disappears.

Start a named session and open files normally:

```shell
red --detach refactor src/main.rs
```

The current terminal attaches immediately. Press `Ctrl-\` to leave the TUI while the
owner continues in the background. Reconnect from any terminal on the same machine:

```shell
red --attach refactor
```

Stop the owner explicitly when finished:

```shell
red --stop refactor
```

Omit the name after `--detach` to use the `default` session. Only one TUI may attach to a
session at a time. Sessions are local to the current OS user: Red uses a private Unix
socket and reconnect token, and does not expose a TCP port.

Detach and crash recovery solve different failures. A client or SSH disconnect leaves
the live owner and agent process running. If the owner itself crashes or the machine
restarts, use `red --resume` to load the latest atomic snapshot; see
[`SESSION_RECOVERY.md`](SESSION_RECOVERY.md). Restored transcript context does not imply
that an adapter process survived a machine or owner crash.

Windows supports `red --resume` but not detach/attach in this release. Named-pipe support
is deferred; Red reports this limitation directly instead of silently falling back to an
insecure or unsupported transport.

## SSH acceptance check

For release verification on a real host:

1. SSH to the host and run `red --detach ssh-check <file>`.
2. Start an agent session and a task long enough to outlive the connection.
3. Terminate the SSH transport without using `Ctrl-\`.
4. Open a new SSH connection and run `red --attach ssh-check`.
5. Confirm the agent output continued, accepted edits retain session/turn attribution,
   and a clean older transaction can be selectively reverted.
6. Run `red --stop ssh-check`.

The automated `tests/detach.rs` companion records the live adapter PID and verifies that
the same process survives a dropped client and reattach. The manual check adds the real
SSH daemon and terminal-hangup boundary.
