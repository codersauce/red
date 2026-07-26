---
title: "Red Command"
summary: "The `red` command exposes editor startup, utility checks, runtime asset operations, detach control, config overrides, and internal hidden boundaries."
topics: [reference, cli, startup]
sources:
  - id: cli
    type: file
    path: src/cli.rs
  - id: main
    type: file
    path: src/main.rs
  - id: readme
    type: file
    path: README.md
  - id: getting-started
    type: file
    path: docs/GETTING_STARTED.md
---

# Red Command

The `red` command is the public entrypoint for opening the editor, forwarding Husk subcommands, running non-interactive diagnostics, managing Unix detachable sessions, and copying runtime assets into user configuration [@cli] [@main]. Its argument parser is defined in `src/cli.rs`, while `src/main.rs` selects the lifecycle branch for each parsed mode before editor state is constructed [@cli] [@main].

## Invocation Forms

| Form | Behavior |
| --- | --- |
| `red [OPTIONS] [FILES]...` | Starts the interactive editor unless a utility or detach-control flag exits earlier [@cli] [@main]. |
| `red husk ...` | Forwards arguments to the bundled Husk CLI by rewriting the program name to `red husk` and returning through `husk_cli::run_from` [@main]. |
| `red --version` | Uses Clap's generated version output [@cli]. |

The README and getting-started guide present `red path/to/file`, multiple files, and `red -r path/to/project src/main.rs` as ordinary editor startup examples [@readme] [@getting-started].

## General Editor Options

| Option | Meaning |
| --- | --- |
| `-r, --root <ROOT>` | Changes the current directory before opening editor buffers, except where conflicts prevent a lifecycle from accepting a root [@cli] [@main]. |
| `-c, --config-override <TOML>` | Applies an inline TOML config override. The flag can appear multiple times and is passed to configuration loading in order [@cli] [@main]. |
| `--resume` | Restores the latest core-owned crash-safe session snapshot and conflicts with file arguments and `--root` [@cli] [@main]. |
| `--no-typecheck` | Sets the runtime-only `disable_plugin_typecheck` escape hatch after configuration finalization [@cli] [@main]. |
| `FILES...` | Opens the listed files as buffers; without files, Red starts with an empty buffer [@cli] [@main]. |

`--resume` changes to the snapshot working directory when one is present and reconstructs buffers from the recovered snapshot rather than opening command-line files [@main].

## Utility Modes

| Option | Output or effect |
| --- | --- |
| `--runtime-files` | Prints runtime plugins and themes from user config, `RED_RUNTIME`, and embedded assets [@cli] [@main]. |
| `--check-config` | Loads and finalizes the effective user configuration, prints sorted diagnostics, prints `config ok` when clean, and exits non-zero when diagnostics remain [@cli] [@main]. |
| `--agent-check` | Runs the offline Codex prerequisite report after requiring a clean configuration [@cli] [@main]. See [Agent Check](../agent/agent-check). |
| `--agent-check --strict` | Makes an unready Codex report a command failure [@cli] [@main]. |
| `--eject <ASSET>` | Copies a bundled or runtime asset into the user config directory without overwriting an existing user file [@cli] [@main]. |
| `--eject-force <ASSET>` | Copies the asset and allows overwrite of an existing user file [@cli] [@main]. |

`Args::utility_requested` includes `--runtime-files`, `--check-config`, `--agent-check`, hidden `--self-check`, `--eject`, `--eject-force`, and hidden process-editor replacement [@cli]. `Args::validate_utility_args` rejects utility modes combined with files to edit, except the hidden process-editor replacement mode, which requires exactly one target file [@cli].

## Detach And Attach

| Option | Behavior |
| --- | --- |
| `--detach[=SESSION] [FILES]...` | Starts a detachable editor owner and attaches this terminal. Without an explicit value, the session name is `default`; with a value, the syntax is `--detach=work` [@cli] [@main]. |
| `--attach <SESSION>` | Attaches this terminal to an existing local editor session [@cli] [@main]. |
| `--stop <SESSION>` | Requests shutdown of an existing local editor session [@cli] [@main]. |

`--detach` conflicts with `--attach`, `--stop`, hidden `--core-session`, and `--resume`, but it can carry root, config override, typecheck, and file arguments into the detached owner [@cli] [@main]. `--attach` and `--stop` conflict with file arguments, `--root`, hidden `--core-session`, and `--resume` [@cli]. The README documents `red --detach path/to/file`, `red --detach=work path/to/project`, and `red --attach work` as the user-facing Unix workflow [@readme].

## Hidden Boundaries

| Hidden option | Internal role |
| --- | --- |
| `--self-check` | Validates the embedded runtime and bundled assets, prints plugin status lines plus `red self-check ok`, and exits [@cli] [@main]. See [Self Check](../runtime/self-check). |
| `--core-session <SESSION>` | Starts the detached owner process that serves the headless editor session [@cli] [@main]. |
| `--process-editor-replace <FILE>` | Replaces one editor target with `RED_PROCESS_EDITOR_CONTENT` and exits [@cli] [@main]. |

These flags are marked hidden in the Clap definition because they are process boundaries for packaging, detach, and editor integration rather than supported interactive workflows [@cli].

The lifecycle implementation behind this command surface is described in [Runtime Lifecycle](../../architecture/startup/runtime-lifecycle).
