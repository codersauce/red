---
title: "Detach And Reattach"
summary: "Use `red --detach`, `red --attach`, and `red --stop` to keep a Unix editor owner alive across terminal disconnects."
topics: [guides, sessions, detach, operations]
sources:
  - id: detach-doc
    type: file
    path: docs/DETACH.md
  - id: cli
    type: file
    path: src/cli.rs
  - id: main-entry
    type: file
    path: src/main.rs
  - id: detach-tests
    type: file
    path: tests/detach.rs
---

Use this guide when you want Red to survive a terminal or SSH disconnect on Linux or macOS. The expected result is a named background owner that keeps editor state, LSP servers, plugins, unsaved buffers, and the running Codex app-server process alive while terminals attach and detach [@detach-doc]. If the owner crashed or the machine restarted, use [Resume after crash](resume-after-crash) because detach cannot reconnect to a dead process.

## Start A Detached Session

Start a named session with the equals form:

```shell
red --detach=refactor src/main.rs
```

The equals form is intentional. The CLI defines `--detach` with an optional value that requires `=`, and `--detach` without a value uses the `default` session, so `red --detach src/main.rs` opens `src/main.rs` in the default session rather than treating the file as a session name [@cli]. The detach documentation recommends `--detach=SESSION` for named sessions for the same reason [@detach-doc].

On Unix, Red spawns a hidden `--core-session SESSION` owner in a new process, forwards supported root/config/typecheck options and files, waits for the owner socket, token, and PID file, and then attaches the current terminal [@main-entry]. On non-Unix platforms, detach commands fail with a message that detach is available on Linux and macOS and that Windows users should use `--resume` [@main-entry].

## Detach The Terminal

While attached, press `Ctrl-\` to leave the TUI and keep the owner running [@detach-doc]. The terminal client recognizes both raw `Ctrl-\` and raw `Ctrl-4` as the detach key, sends a protocol detach message, and restores terminal modes with its guard when the attachment ends [@main-entry].

Only one TUI may attach to a session at a time [@detach-doc]. If another terminal is already attached, wait for it to detach or stop the owner deliberately.

## Reattach

Reconnect from another terminal on the same machine:

```shell
red --attach refactor
```

`red --attach` connects to the session's private Unix socket, reads the reconnect token, performs the attach handshake, enables raw terminal features, paints the initial render, and then sends keyboard, mouse, paste, resize, focus, and heartbeat messages to the owner [@main-entry]. Large pastes are chunked before sending so the owner can apply them as a single completed paste operation [@main-entry].

The integration test for detach verifies the main operational guarantee: after the first client drops, the original mock Codex process remains alive, a second client reconnects, and reattach does not restart the app-server process [@detach-tests]. For the underlying owner/client split, read [Detachable editor core](../../architecture/sessions/detachable-editor-core); for message details, read [Detach IPC protocol](../../reference/sessions/detach-ipc-protocol).

## Stop The Owner

When finished, stop the session explicitly:

```shell
red --stop refactor
```

`red --stop` opens a control connection to the session and asks the authenticated owner to shut down [@main-entry]. The detach documentation lists this as the normal cleanup command after a detached workflow [@detach-doc].

## Choose Detach Or Recovery

Use detach when the owner is still alive and the connection disappeared. Use [recovery](../../concepts/sessions/detach-vs-recovery) through `red --resume` when the owner crashed or the machine restarted [@detach-doc]. The documentation is explicit that restored transcript context after a crash does not imply a Codex process survived, while detach keeps the live owner and agent process running [@detach-doc].
