# Felix reference smoke test

Run the official Helix 25.07.1 editor through a two-buffer editing workflow. The runner verifies the release archive against its pinned SHA-256 digest, extracts its bundled runtime into a temporary directory, and launches Helix in an isolated tmux session with fresh user and project configuration.

On Apple Silicon, with Python 3.12+, tmux, and the GitHub CLI installed, run these commands from the Red repository root.

```sh
mkdir -p ~/.cache/red-felix/reference
gh release download 25.07.1 --repo helix-editor/helix \
  --pattern helix-25.07.1-aarch64-macos.tar.xz \
  --dir ~/.cache/red-felix/reference --skip-existing
python3 scripts/felix/reference_smoke.py \
  ~/.cache/red-felix/reference/helix-25.07.1-aarch64-macos.tar.xz > reference-trace.json
```

The trace selects two matches with the regex prompt, changes both to `bar`, switches documents and back, then undoes and redoes the change. At each checkpoint the runner waits for the expected visible line, mode, active document, and selection count. It reads tmux's pane without sending inspection commands to Helix. A mismatch times out with the last visible frame. A successful run writes the captured frames and archive identity to standard output.

Build the state observer once from the pinned checkout. Apply the patch to a clean checkout at that revision. Keep the observer separate from both production binaries.

```sh
git clone https://github.com/helix-editor/helix.git ~/.cache/red-felix/hx-source
git -C ~/.cache/red-felix/hx-source checkout --detach a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b
git -C ~/.cache/red-felix/hx-source apply "$PWD/scripts/felix/helix-observer.patch"
(cd ~/.cache/red-felix/hx-source && HELIX_DISABLE_AUTO_GRAMMAR_BUILD=1 RUSTUP_TOOLCHAIN=stable cargo build --locked -p helix-term --bin hx)
cargo build --locked --features helix --bin red
python3 scripts/felix/reference_smoke.py ~/.cache/red-felix/reference/helix-25.07.1-aarch64-macos.tar.xz \
  --observer ~/.cache/red-felix/hx-source/target/debug/hx --candidate target/debug/red > comparison.json
```

The runner compares the observer with tracing disabled and enabled against the release's visible frames, checks all 17 reference events against the fixture, and exercises Red at the same 11 terminal checkpoints. Separate traces compare five paging frames and eight repeat checkpoints, including undo after a counted repeat in another document opened in a split.

The search-prompt trace checks the bottom row and terminal caret in both editors while recalling a previous query, moving inside it, inserting a wide character, deleting it, and cancelling. It also checks the search-history menu, Tab and Shift-Tab wraparound, and the visible query and caret after narrowing and widening the terminal. The per-key fixtures also compare the prompt text and its UTF-8 cursor offset against the observer after every key, including bracketed paste and history traversal in a selection prompt.

Two split-view traces search the same document independently in each pane. One uses 100 numbered lines. The other uses a single line that wraps across 100 display rows. Each row contains a tab, a joined emoji, a CJK character, and a zero-width space, so the expected tags also check where both editors break the line. Separate per-key fixtures move onto adjacent zero-width spaces, extend a selection across rows, and replace the selected character. Both split traces check every visible row in each pane during preview, after accepting a search, and after canceling with Escape or Ctrl-C. The per-key wrapped paging and search fixtures also check the character offset at the top of the viewport, which can fall partway through a line.

The unwrapped horizontal scrolling trace compares both editors in a 24-cell document window. An ASCII ruler on the second line identifies the visible columns after each move. The other two lines check a joined emoji clipped at either edge, tabs, CJK text, and the actual insert caret after the emoji.

The clipboard trace uses an isolated OSC 52 transport in both editors. It checks multi-selection and primary-only yanks, paste before and after, replace, and undo. When another application appends to a previous Red yank, Red uses the complete external text. This approved difference from Helix 25.07.1 is exercised through the attached clipboard client. The completion-repeat trace accepts two distinct language-server candidates during one Insert session, switches documents, repeats twice, and checks undo and redo.

The completion trace exercises 45 checkpoints with two active selections. Up, Tab, and Ctrl-P cycle through candidates. The language server still sees the original document during preview. Ctrl-C discards the preview and keeps any filter text typed at both cursors. Typing or pressing Escape with a candidate selected accepts the preview before processing the key. One candidate requires deferred resolution.

The snippet-stops trace replaces four mirrored defaults across two selections, navigates to the two independent second stops, and verifies that replacing an enclosing placeholder skips its removed child.

The trace also accepts a snippet at both selections, replaces two successive placeholders, and undoes the changes. Undo after accepting a filtered completion restores the text from the time of the request. Dot repeats the saved completion at the primary cursor and replays ordinary typing at both cursors. The language server independently verifies the resulting document and counts completion requests during repeat. Helix 25.07.1 crashes when advancing from the second replaced placeholder to the final stop in this trace. A Red regression advances to that stop and types at both cursors.

The pending-input cases compare Normal and Select mode after every key. They cover modified character arguments, counts before and inside prefixes, canceled finds, bracketed paste while waiting for an argument, and counted motion repeats across multiple selections. Enter searches for a line ending across LF and CRLF while retaining the previous character motion for `Alt-.`.

A terminal trace also cancels a counted Space command, sends bracketed paste while the leader is pending, and checks the next edit in both panes after a counted window prefix.

The selection fixtures compare counts, range direction, and which selection stays primary after merging, trimming, filtering, or copying. Copies cover short lines, tabs and wide characters, selections spanning more than one line, and mixed LF and CRLF with a standalone carriage return or Unicode separator.

A terminal trace checks the messages shown when a regex or removal would leave no selections, then edits the preserved selection.

The whole-line selection fixtures cover counts, reversed selections, and files that mix CRLF and LF with a standalone carriage return or Unicode separator inside a line.

The text-object fixtures exercise inner and around selections for words, paragraphs, quotes, and nested delimiters. They include backward selections, Unicode delimiters, unmatched selections in a multi-cursor edit, counted searches, and CRLF paragraph separators.

The Red integration test compares directed selections, primary index, mode, register values, pending prefixes, text, and active document at each event. To replay the manual workflow in disposable files, run `sh scripts/felix/try-workflow.sh "$PWD/target/debug/red"`.

The [coverage inventory](coverage.toml) lists each active per-key case and the remaining work by command family. CI rejects a missing or duplicate case and an observation missing required state. The per-key suite uses a separate isolated session per case. Each includes its initial text and the input prefix leading up to every observation. It checks that Helix received the intended keys and includes CRLF, empty files, an EOF selection, combining marks, ZWJ emoji, counts and direction changes. Run the stored fixtures with `cargo test --locked -p felix-core` and `cargo test --locked -p red --features helix --lib editor::felix`. Paging and long-distance search cases also compare the top visible row. Repeat cases compare the recorded insert keys, including after undo and redo. To compare a fresh trace, extract the verified release runtime, run `scripts/felix/scenarios.py` with Python and its `--observer`, `--runtime`, `--seed` and `--steps` options, then pass the output file to the same tests through `FELIX_SCENARIOS`. The Felix conformance workflow does this on pull requests and runs a longer motion sequence nightly.

To exercise the real LSP delivery path, run the following with a Helix-enabled binary. A local test language server applies Red's incremental updates independently. The runner checks the server's final document hash and the bytes Red saved to disk after deleting eight selections in a Unicode file opened in a split.

```sh
python3 scripts/edit_replay_bench.py --binary target/debug/red --scenario felix-change \
  --lines 300 --block-rows 8 --lsp incremental --split --unicode
```

See the [milestone acceptance criteria](../../docs/HELIX_IMPLEMENTATION_PLAN.md) for the remaining command families and host workflows.

### Ordinary typing and cursor latency

Run from the repository root on an idle machine with Python 3.11 or newer. Build the same revision with the same dependencies in both profiles. On the reference Mac, set `CARGO_RESOLVER_LOCKFILE_PATH="$PWD/target/red-local/Cargo.lock"` in the environment of the Cargo commands. The local override is recorded in [input-baseline.json](input-baseline.json).

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked --features helix --bin red
CARGO_INCREMENTAL=0 cargo build --locked --release --features helix --bin red
sh scripts/felix/input-matrix.sh target/debug/red target/release/red target/felix-perf/results local
python3 scripts/felix/performance.py --binary target/debug/red
```

The matrix alternates editing-model order across five repetitions. Each run sends 100 warmup inputs followed by 1,000 measured inputs and writes its raw observations to a separate JSON file. A sample ends when a complete PTY frame shows the expected source position. Each run also saves and compares the entire disposable fixture. The gate compares the median of the five run percentiles to the [recorded calibration](input-baseline.json). Absolute timings are specific to the reference machine. CI checks behavior and redraw counts.

For a quick investigation, substitute a real file with `--source`. The runner copies it and edits only the copy. Use an unindented, nonempty line with text to its right for this workload. To include project discovery, pass `--project-root "$PWD"`. The scratch copy then lives under the project's existing `target` directory.

```sh
python3 scripts/felix/input_bench.py --binary target/debug/red --model helix \
  --source README.md --line 4 --scenario typing --cols 200 --rows 60 \
  --events 40 --warmup 4 --perf-mode trace
python3 scripts/felix/input_bench.py --binary target/release/red --model helix \
  --scenario horizontal --events 40 --warmup 4 --burst --burst-ms 10
```

The paced burst reports time through the last expected frame and time from the last write until that frame. Motion reverses direction and ends on a distinct row and column. The document marker verifies the destination. On macOS, add `--sample FILE` to a longer diagnostic run to collect a separate process sample. Compare gating runs with instrumentation disabled.
