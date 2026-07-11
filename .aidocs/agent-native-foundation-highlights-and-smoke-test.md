# Agent-native foundation: highlights and smoke test

## Branch scope

- **Branch:** `feat/agent-native-foundation`
- **Comparison base:** `origin/master`
- **Pull request:** none at the time this guide was written
- **Scope:** the complete implementation of `docs/PROJECT_PLAN.md`

The implementation spans the editor transaction boundary, Vim compatibility, native ACP
support, proposal review, attributed history, crash recovery, detachable sessions, and
typed Husk plugin compatibility.

## Highlights

### Foundation and performance

- Editing now passes through a canonical `EditTransaction` boundary instead of allowing
  features to mutate buffers through unrelated paths.
- Transactions retain their origin, before/after payload, cursor state, and attribution.
- Repository inventory and reproducible performance baselines were added.
- CI now enforces release-mode performance gates and the expanded plugin checks.

### Vim credibility

- Semantic dot-repeat (`.`) replays completed changes through normal key resolution.
- Counts work with repeat, macros, find/till, and replace operations.
- Macro recording supports `q{register}`, `@{register}`, `@@`, uppercase append, and
  deterministic recursion/instruction limits.
- Local, global, last-change, last-visual, and previous-jump marks track edits, undo, and
  redo using Unicode character coordinates.
- Normal-mode `r{char}` supports counted grapheme replacement as one transaction.
- `:substitute` supports current, numeric, whole-file, and visual ranges; `g`, `i`, and
  explicit `c` confirmation; escaped delimiters; and Rust-regex captures.
- Undo retains sibling branches. `g-`/`g+` select branches and `:undotree` opens a visual
  navigator.
- The supported surface and intentional differences are versioned in
  `docs/VIM_COMPATIBILITY.md`.

### Agent-native workflow

- Red owns an ACP v1 client compiled against the official schema artifact.
- Agent reads are served from current Red buffers, including unsaved content.
- Agent writes become isolated proposals. They do not mutate buffers or disk.
- Proposed hunks render with decorations and gutter signs.
- The review workspace supports keyboard navigation and accept/reject by hunk or file.
- Accepted changes use normal attributed editor transactions containing the agent
  session and turn identifiers.
- Non-overlapping user and agent edits rebase; overlapping edits report a conflict.
- Permission requests expose only the choices supplied by the adapter and return the
  exact selected option.
- `red --agent-check` reports protocol, executable, authentication, and reviewability
  prerequisites without installing or authenticating anything.
- `disable_ai = true` removes the agent UI and prevents adapter processes and checks.

There is deliberately no production-supported adapter in this revision. A custom ACP
adapter may be configured, but it must already be installed and authenticated. An
adapter will be promoted only after it passes Red's client-filesystem conformance gate.

### Attributed history and recovery

- User, plugin, LSP, and agent changes carry explicit origins.
- `:AgentHistory` shows stable transaction IDs and before/after payloads.
- Selective revert creates a new user transaction only when the old transaction's
  post-image still matches. Stale inverses become review conflicts.
- Schema-v2 snapshots preserve dirty buffers, window layout, cursors, registers, marks,
  jumplist, undo trees, transcript, and pending proposals.
- Snapshots are owner-only, generation-based, atomically replaced, and fall back to the
  last valid generation after an interrupted write.
- `red --resume` restores state without writing recovered dirty content to disk.
- External disk divergence produces a unified recovery diff instead of silently choosing
  one version.

### Detach and reattach

- On Linux and macOS, `red --detach [SESSION]` starts a terminal-independent owner and
  attaches the current terminal.
- `Ctrl-\` leaves the TUI while the owner continues running.
- `red --attach SESSION` reconnects; `red --stop SESSION` shuts the owner down.
- The owner retains the production editor, buffers, LSP, Husk runtime, plugin processes,
  timers, directory watchers, snapshots, proposal workspace, and ACP adapter process.
- IPC is versioned, token-authenticated, frame-limited, heartbeat-driven, and carries
  styled logical row deltas plus the authoritative cursor.
- Only one TUI may attach at a time. TCP attach and collaboration are non-goals.
- The owner starts in a separate Unix process session so an SSH terminal hangup does not
  terminate it.
- Windows supports snapshot recovery but not detach/attach in this release.

### Typed plugin compatibility

- `src/plugin/host_api.json` is the canonical machine-readable host API.
- Husk plugins are parsed, resolved, typechecked, and checked against the host API before
  activation.
- Diagnostics use stable parsing, type, and API error families with source locations.
- Plugins declare semver host requirements and dependencies.
- One incompatible or broken plugin is quarantined without preventing editor startup.
- `red --self-check` reports active, disabled, quarantined, and reload-error states.
- Hot reload is transactional: a failed replacement leaves the old VM and callbacks
  active.
- Plugins can explicitly migrate state with `state_export()` and `state_import()`.
- The pinned example plugin and bundled corpus now use and validate the Husk boundary.

## Prerequisites

The commands below use Fish shell syntax. Use a fresh shell so the isolated configuration
does not affect your normal Red setup.

```fish
cargo build

set demo (mktemp -d)
set -gx XDG_CONFIG_HOME $demo/xdg

mkdir -p $XDG_CONFIG_HOME/red
mkdir -p $demo/project
touch $XDG_CONFIG_HOME/red/config.toml

printf 'foo foo\nalpha beta\nfoo gamma\n' > $demo/project/smoke.txt
```

Start the ordinary editor with:

```fish
target/debug/red $demo/project/smoke.txt
```

## Smoke test 1: Vim repeat, macros, marks, and substitution

1. Put the cursor on the first `foo`, execute `dw`, move to another word, and press `.`.

   **Expected:** the semantic delete is recomputed at the new cursor. `3.` repeats the
   completed change three times where possible.

2. Record a macro with `qa`, perform an edit and movement, and finish with `q`. One useful
   sequence is:

   ```text
   qaI// <Escape>jq
   ```

   Run `2@a` and then `@@`.

   **Expected:** the edit replays deterministically. `@@` repeats the last macro, and
   recursion limits prevent a self-referential macro from hanging Red.

3. Place a mark with `ma`, insert text before it, and jump with `` `a``. Undo and redo the
   insertion.

   **Expected:** the mark follows the original text through insertion, undo, and redo.

4. Run:

   ```text
   :%s/foo/bar/gc
   ```

   Answer `y`, `n`, and then `a`.

   **Expected:** confirmation applies per match. One `u` undoes every accepted
   replacement from the command as a single transaction.

5. Try `3rX`.

   **Expected:** three graphemes are replaced in one transaction. A count extending past
   the line is rejected without changing text.

## Smoke test 2: branching undo

1. Make an edit.
2. Press `u`.
3. Make a different edit.
4. Open `:undotree`.
5. Use `g-` and `g+` to select sibling branches, then redo.

**Expected:** the abandoned change remains available as a sibling branch instead of
being destroyed by the new edit.

## Smoke test 3: crash recovery

1. Save with `:w`.
2. Insert a unique unsaved line such as `RECOVER ME`.
3. Wait at least six seconds for a periodic snapshot.
4. From another terminal, find this exact Red process:

   ```fish
   pgrep -fl 'target/debug/red'
   ```

5. Kill only that PID:

   ```fish
   kill -9 <exact-pid>
   ```

6. Resume:

   ```fish
   target/debug/red --resume
   ```

**Expected:** the unsaved line, cursor/layout, marks, registers, and undo history return.
The original disk file still does not contain `RECOVER ME`.

For the conflict path, modify the disk file after the crash but before `--resume`.

**Expected:** Red reports a unified divergence diff and keeps the recovered contents
dirty rather than overwriting either version.

## Smoke test 4: detach and reattach

On Linux or macOS:

```fish
target/debug/red --detach spin $demo/project/smoke.txt
```

1. Make an unsaved edit.
2. Press `Ctrl-\`.

   **Expected:** the TUI exits while the owner remains alive.

3. Reattach:

   ```fish
   target/debug/red --attach spin
   ```

   **Expected:** the unsaved edit, cursor, undo history, plugins, and LSP state remain.

4. From a second terminal using the same `XDG_CONFIG_HOME`, attempt another attach while
   the first TUI is still attached.

   **Expected:** Red reports that the session already has an attached client.

5. For a closer SSH simulation, close the attached terminal window without pressing
   `Ctrl-\`, then attach from a new terminal.

   **Expected:** the owner survives the dropped terminal connection.

6. Stop the owner:

   ```fish
   target/debug/red --stop spin
   ```

   **Expected:** the socket, token, and PID metadata are removed.

## Smoke test 5: plugin compatibility and hot reload

Check the bundled runtime:

```fish
target/debug/red --self-check
```

**Expected:** bundled plugins report active status.

Create an isolated copy of the example plugin:

```fish
mkdir -p $demo/plugin
cp examples/example-plugin/index.hk $demo/plugin/
cp examples/example-plugin/package.json $demo/plugin/

set plugin_override "plugins.spin = \"$demo/plugin/index.hk\""

target/debug/red --config-override $plugin_override $demo/project/smoke.txt
```

Inside Red, run:

```text
:ExampleCommand
```

**Expected:** Red prints `Hello from the example Husk plugin!`.

While Red remains open, corrupt the plugin from another terminal:

```fish
printf 'pub fn activate( {' > $demo/plugin/index.hk
sleep 1
```

Run `:ExampleCommand` again and press `Space d p` to inspect plugin status.

**Expected:** the previous callback still works, and the plugin status reports an active
plugin with a reload diagnostic.

Restore it with changed behavior:

```fish
sed 's/Hello from the example Husk plugin!/Reloaded safely!/' examples/example-plugin/index.hk > $demo/plugin/index.hk
sleep 1
```

**Expected:** `:ExampleCommand` now prints `Reloaded safely!`.

For startup quarantine, exit Red, corrupt the file again, and relaunch with the same
override.

**Expected:** only the broken plugin is quarantined; Red and unrelated plugins still
start, and `--self-check` provides a source diagnostic.

## Smoke test 6: agent safety boundary

Run the read-only prerequisite check:

```fish
target/debug/red --agent-check
```

**Expected:** ACP schema artifact `1.4.0`, wire protocol `1`, an unconfigured adapter,
and reviewable-edit readiness `not ready`. Red does not install, authenticate, or make a
network request.

Verify the off switch:

```fish
target/debug/red --config-override 'disable_ai = true' --agent-check
```

**Expected:** agent support and adapter checks are disabled, and no process is spawned.

If a conforming custom adapter is already installed and authenticated, configure it in
the isolated `config.toml`, then try:

1. `:AgentStart`
2. `:AgentPrompt`
3. Ask it to modify several files, including an unsaved buffer.
4. Open `:AgentReview`.
5. Accept one hunk with `a` and reject another with `r`.
6. Confirm that nothing reaches disk until `:w`.
7. Open `:AgentHistory` and selectively revert an accepted transaction.

**Expected:** pending edits remain proposals, accepted changes retain session/turn
attribution, rejected changes never enter the buffer, and a stale selective inverse
becomes a conflict rather than changing text silently.

## Known boundaries worth checking

- Named registers exist for macros, but interactive named text-register selection is not
  yet implemented.
- Visual-mode `r` is not yet supported.
- A confirmed substitute is one undo transaction but is not a dot-repeat recipe.
- Search and substitute use Rust regex syntax, not Vim's regex dialect.
- Detach supports one local Unix client. Windows named pipes, TCP attach, collaboration,
  and simultaneous clients are not part of this release.
- A production ACP adapter has not yet passed the reviewable client-filesystem gate.

## Cleanup

```fish
target/debug/red --stop spin
rm -rf $demo
set -e XDG_CONFIG_HOME
```

If `spin` was already stopped, the cleanup command may report that no session exists; the
temporary directory removal is still sufficient.

## Related documentation

- `docs/PROJECT_PLAN_IMPLEMENTATION.md`
- `docs/VIM_COMPATIBILITY.md`
- `docs/VIM_DOGFOOD.md`
- `docs/AGENT_WORKFLOW.md`
- `docs/SESSION_RECOVERY.md`
- `docs/DETACH.md`
- `docs/PLUGIN_API.md`
- `docs/EDIT_PIPELINE.md`
