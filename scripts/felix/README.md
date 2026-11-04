# Felix reference smoke test

Run the official Helix 25.07.1 editor through a two-buffer editing workflow. The runner verifies the release archive against its pinned SHA-256 digest, extracts its bundled runtime into a temporary directory, and launches Helix in an isolated 80 × 24 tmux session with fresh user and project configuration.

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

The runner cross-checks the observer with tracing disabled and enabled against the release's visible frames, compares all 17 reference events to the checked fixture, and checks the Red binary at the same 11 terminal checkpoints. Separate traces compare five paging frames and eight repeat checkpoints, including an undo after a counted repeat in a second document opened in a split. The Red integration test compares directed selections, primary index, mode, register values, pending prefixes, text, and active document at each event. To replay the manual workflow in disposable files, run `sh scripts/felix/try-workflow.sh "$PWD/target/debug/red"`.

The [coverage inventory](coverage.toml) lists each active per-key case and the remaining work by command family. CI rejects a missing or duplicate case and an observation missing required state. The per-key suite uses a separate isolated session per case. Each includes its initial text and the input prefix leading up to every observation. It checks that Helix received the intended keys and includes CRLF, empty files, an EOF selection, combining marks, ZWJ emoji, counts and direction changes. Run the stored fixtures with `cargo test --locked -p felix-core` and `cargo test --locked -p red --features helix --lib editor::felix`. Paging and long-distance search cases also compare the top visible row. Repeat cases compare the recorded insert keys, including after undo and redo. To compare a fresh trace, extract the verified release runtime, run `scripts/felix/scenarios.py` with Python and its `--observer`, `--runtime`, `--seed` and `--steps` options, then pass the output file to the same tests through `FELIX_SCENARIOS`. The Felix conformance workflow does this on pull requests and runs a longer motion sequence nightly.

To exercise the real LSP delivery path, run the following with a Helix-enabled binary. A local test language server applies Red's incremental updates independently. The runner checks the server's final document hash and the bytes Red saved to disk after deleting eight selections in a Unicode file opened in a split.

```sh
python3 scripts/edit_replay_bench.py --binary target/debug/red --scenario felix-change \
  --lines 300 --block-rows 8 --lsp incremental --split --unicode
```

See the [milestone acceptance criteria](../../docs/HELIX_IMPLEMENTATION_PLAN.md) for the remaining command families and host workflows.
