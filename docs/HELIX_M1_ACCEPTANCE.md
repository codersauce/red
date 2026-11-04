# Felix milestone 1 acceptance

The candidate covers the agreed document-window commands against Helix 25.07.1, commit `a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b`. Ordinary Red builds exclude Felix. A feature-enabled build starts in Vim unless the user selects Helix.

| Gate | What to verify on the candidate |
| --- | --- |
| Coverage | The [inventory](../scripts/felix/coverage.toml) names 137 reference cases. The deterministic seed is 250701 with 4,044 key-by-key observations. The checker verifies the starting mode and count for each listed binding, including aliases delivered as distinct terminal events. The [native bindings](../src/editor/felix_bindings.toml) and [binding decisions](HELIX_IMPLEMENTATION_PLAN.md) define Red-owned actions. |
| Observer | The terminal suite verifies the release archive digest and compares observer-disabled and observer-enabled frames with the release. It checks the 17 intermediate states during a multi-selection edit, buffer switch and undo/redo. The coverage checker injects a dropped event and a reversed selection and requires both comparisons to fail. |
| Core parity | Replay the checked-in capture and a freshly generated 512-step seed through both the standalone engine and Red's production input dispatcher. Compare directed ranges and primary identity, document text, mode, pending keys, count and registers after every event. Paging, prompt and repeat cases include their additional observable state. |
| Host integration | Run the real RPC plugin, file reload, stale workspace edit, clipboard, language-server and save tests. The eight-selection LSP replay verifies the final document independently in the language server and on disk. A real-server signature-help test covers nested calls, retriggers, dismissal and completion in both modes. |
| Visible behavior | The paired terminal checkpoints cover horizontal scrolling, paging, search and completion in splits, snippets, clipboard and repeat. Red frame tests check the actual cells when a match is hidden inside a projected Markdown link and when a multiline completion shifts a table in two views. Detached mouse input checks drag, autoscroll and release. |
| Optionality and Vim | Run both configurations with warnings denied. Check the ordinary executable's default configuration and missing-feature diagnostic, inspect its dependency tree, and exercise both editing models in a feature-enabled executable. |
| Performance | Run five repetitions against the [frozen input budgets](../scripts/felix/input-baseline.json) and [frozen scaling budgets](../scripts/felix/performance-baseline.json). Each input run verifies cursor position and saved bytes. The README image regression verifies reuse of the displayed screenshot during typing and vertical motion. |
| Handoff | Match the workstation candidate to the Linux workflow's `candidate-commit.txt`. Both pinned Linux replays must pass with the repository lockfile. The artifact includes the generated captures, release and candidate frames, LSP receipt, signature-help reproduction, image receipts, reference identity and checksums. |

The [Felix conformance workflow](../.github/workflows/felix-conformance.yml) regenerates the deterministic capture and compares it byte for byte. On branch pushes the same job uses its run ID as the extended seed. Download the **felix-conformance** artifact for that push. Associate acceptance with the SHA inside the artifact.

The workstation run uses the ignored `target/red-local/Cargo.lock` to select versions served by the managed registry. Its raw outputs are in `target/felix-acceptance/` in the Felix worktree. The [runner instructions](../scripts/felix/README.md) reproduce the terminal comparisons and timing measurements. The [performance report](HELIX_PERFORMANCE_RESULTS.md) records the historical measurements and calibration.

## Workstation results

On November 4, 2026, both Felix and Red matched all 4,044 inputs for seed 250701 and all 4,672 for seed 20261104. The deterministic capture matched the checked-in fixture byte for byte. The paired terminal suite passed 170 Red checkpoints across 15 workflows.

Red's full test command passed 4,135 test executions with Helix and 4,102 with the feature excluded. Each run skipped the same three opt-in resize and file-picker benchmarks. Both Clippy configurations passed with warnings denied. The ordinary binary passed its default configuration check, reported the required build feature when asked for Helix, and had no Felix dependency in its normal Cargo tree.

The terminal signature test saw the parameter label in Red's output. The eight-selection edit produced one LSP notification carrying eight incremental changes, and the saved file matched the expected bytes. With the README image present, both typing and vertical movement reused the displayed image. See `m1-terminal.json`, `m1-lsp.json`, `m1-signature-painted.txt`, and `m1-image-*.json` in the workstation evidence directory.

All 24 input latency budgets and all five undo, paste and selection-scaling budgets passed on the Apple M4 Max workstation. Each latency below is the median of five run percentiles in milliseconds, measured from a single key to its completed terminal frame.

| Profile | Input | Helix p95 | Helix p99 | Vim p95 | Vim p99 |
| --- | --- | ---: | ---: | ---: | ---: |
| dev | typing | 3.137 | 3.274 | 4.363 | 4.876 |
| dev | horizontal | 0.594 | 0.643 | 1.156 | 1.232 |
| dev | vertical | 0.796 | 1.373 | 5.039 | 5.822 |
| release | typing | 0.408 | 0.482 | 0.608 | 0.674 |
| release | horizontal | 0.084 | 0.100 | 0.185 | 0.219 |
| release | vertical | 0.089 | 0.115 | 0.638 | 0.670 |

| Workload | Median (ms) | Frozen limit (ms) |
| --- | ---: | ---: |
| Undo | 4.060 | 5.281 |
| Paste | 5.749 | 7.401 |
| 2 selections | 4.049 | 4.486 |
| 16 selections | 6.162 | 7.051 |
| 128 selections | 14.826 | 18.789 |

The workstation evidence contains all 60 input traces in `timings/`, all 24 comparisons in `m1-input-limits.json`, and the 25 edit timings in `m1-scaling.json`. The measured dev and release binaries are identified in `m1-binaries.sha256`. The Linux workflow runs the behavior checks with the committed lockfile.

The agreed differences include Red's file pickers and NeoTree, configured save policy, and pasting the complete clipboard contents after another application edits them. Milestone 2 starts with named registers and Insert-mode register insertion after the candidate passes all eight gates.
