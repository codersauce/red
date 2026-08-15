---
title: "Runtime Lifecycle"
summary: "Red's process entrypoint selects utility, detachable, recovery, onboarding, and interactive editor lifecycles before editor state starts running."
topics: [architecture, startup, cli, sessions]
sources:
  - id: main-entry
    type: file
    path: src/main.rs
  - id: cli-args
    type: file
    path: src/cli.rs
  - id: onboarding
    type: file
    path: src/onboarding.rs
  - id: sessions
    type: file
    path: src/session.rs
  - id: headless
    type: file
    path: src/headless/mod.rs
---

# Runtime Lifecycle

Red's runtime lifecycle is chosen in `src/main.rs` before the editor event loop owns the terminal. The binary first forwards `red husk` to the Husk CLI, then handles mutually exclusive utility modes, detachable-session control, first-run onboarding, crash recovery, and finally the normal interactive editor path [@main-entry]. That ordering matters because utility and detachable client commands must exit without constructing a full editor, while resumed and interactive runs must load configuration, runtime assets, buffers, session storage, LSP, preferences, and theme state before `Editor::run` starts [@main-entry].

## Public Argument Boundary

The CLI surface is defined by `Args` and Clap conflict rules. Public lifecycle selectors include `--runtime-files`, `--check-config`, `--agent-check`, `--resume`, `--detach`, `--attach`, `--stop`, `--eject`, and file arguments; hidden flags such as `--core-session`, `--self-check`, and `--process-editor-replace` are internal process boundaries [@cli-args]. `Args::validate_utility_args` adds a runtime rule that utility modes cannot be combined with edit targets, except for the hidden process-editor replacement mode, which requires exactly one target file [@cli-args].

The process also has a subcommand escape hatch: if the first argument is `husk`, `main` rewrites the argv vector to use `red husk` as the program name and returns through `husk_cli::run_from` instead of entering Red's own lifecycle selection [@main-entry]. This keeps Husk package, REPL, and language-server commands in the same binary without making them editor startup modes.

## Utility Lifecycles

Utility modes run before onboarding and before the interactive terminal is entered. `--self-check` prints the self-check report, `--check-config` loads and sorts diagnostics for the effective configuration, `--agent-check` validates Codex readiness against a clean configuration, `--runtime-files` prints the runtime asset inventory, and `--eject` or `--eject-force` copies a resolved runtime asset into the user config directory [@main-entry]. These flows return immediately after printing or writing their result, so they do not open buffers, initialize LSP, or install the panic hook used by the interactive terminal [@main-entry].

Configuration utility mode shares the same loader used by editor startup but stops at validation. `--check-config` calls `finalize_runtime_config` after loading the user file so runtime failures such as missing plugins, missing themes, or unusable log files are reported through the same diagnostic model as interactive startup [@main-entry]. `--agent-check` is stricter: it requires `LoadedConfig::is_clean` before running the readiness report, and `--strict` turns an unready report into a non-zero exit [@main-entry].

## Detachable Sessions

Detach and attach are selected before the normal editor path. `--attach` connects the terminal to an existing local session, `--stop` sends a stop request, and `--detach[=SESSION]` starts a detached owner process and then attaches the current terminal to it [@main-entry]. The public CLI allows `--detach` to carry file and root arguments into the owner, while `--attach` and `--stop` conflict with edit targets and root changes [@cli-args].

On Unix, `start_detached_owner` launches the current executable with the hidden `--core-session` flag, copies selected root/config/typecheck/file arguments, detaches with `setsid`, and waits for the owner to create a socket, token, and matching pid file [@main-entry]. The hidden owner then creates a session-scoped `SessionStore`, wraps the initialized editor in `DetachedEditorCore`, binds the session under the runtime `run` directory, and serves the headless IPC protocol [@main-entry]. The IPC layer uses versioned messages, reconnect tokens, normalized input events, render deltas, heartbeats, detach, and stop control messages [@headless].

The terminal-side attach lifecycle owns raw mode and terminal painting only while connected. It enables bracketed paste, focus, mouse capture, keyboard enhancement flags, the alternate screen, and line wrapping control, then sends normalized key, mouse, paste, resize, focus, and heartbeat messages to the headless client [@main-entry]. `Ctrl-\` or `Ctrl-4` detaches the terminal without stopping the owner [@main-entry]. The related long-running owner architecture is covered in [Detachable Editor Core](../sessions/detachable-editor-core).

## Interactive And Recovery Startup

When no utility or detach branch exits, startup creates a user config path and runs onboarding only if `config.toml` is missing [@main-entry]. Onboarding is deliberately optional: non-interactive stdin skips file creation, while an interactive user may write a commented starter config built from embedded defaults; themes and plugins remain embedded until explicitly ejected [@onboarding].

After onboarding, startup loads and finalizes configuration, applies `--no-typecheck`, initializes logging, loads [Preferences Store](../preferences/preferences-store), records the number of startup files, and optionally changes to `--root` [@main-entry]. `--resume` loads the latest crash-safe session snapshot, changes back to the snapshot working directory when present, and later reconstructs buffers from the snapshot instead of opening CLI file arguments [@main-entry]. Session snapshots are durable, versioned records of buffers, window layout, registers, marks, jump state, undo history, working directory, plugin extensions, and agent conversation state, with readers rejecting unsupported schema versions rather than attempting partial recovery [@sessions].

The editor is constructed only after startup has selected the correct session store and buffer list. Detached owners use a store named `detached-<session>`, resumed editors reuse the loaded store, and fresh interactive editors use an `editor-<uuid>` owner namespace [@main-entry]. Once `Editor::new_with_preferences` succeeds, startup attaches any configuration diagnostics to the editor, restores session state if needed, installs the session store, and either enters the detached owner server or installs the interactive panic hook and calls `editor.run().await` [@main-entry].

## Shutdown Boundary

The normal interactive lifecycle treats terminal cleanup, editor result handling, and LSP shutdown as separate obligations. The panic hook resets cursor color, bracketed paste, focus change, the alternate screen, and raw mode before printing panic information [@main-entry]. After `Editor::run` returns, startup calls `editor.cleanup`, then asks the LSP manager to shut down, and only then propagates cleanup and editor errors [@main-entry]. This keeps terminal restoration and language-server teardown outside the editor's internal command dispatch and separates lifecycle policy from editor behavior.

See also [Red Command](../../reference/cli/red-command) for the public CLI shape, [Layered Config Recovery](../configuration/layered-config-recovery) for startup configuration loading, and [Runtime Assets](../runtime/runtime-assets) for the embedded/user asset boundary.
