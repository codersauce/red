# Neo-tree unbounded directory performance — 2026-08-21

## Benchmark

`scripts/neotree_bench.py` creates real directories, starts the optimized editor
in a PTY, opens Neo-tree, jumps to its final row, and measures opening latency,
navigation latency, resident memory, and idle CPU. A run is complete only when
the alphabetically final fixture file is reachable. Benchmark parsing and
completeness aggregation are covered by `tests/test_neotree_bench.py`.

Run the same three-sample matrix used below:

```sh
cargo build --locked --release --bin red
python3 -B scripts/neotree_bench.py \
  --sizes 128 512 2048 8192 \
  --samples 3 \
  --navigation-presses 50 \
  --require-complete
```

Add `--active-target` to open the final file before the tree and exercise the
active-file reveal path. Process CPU values come from `ps` and have 10 ms
resolution; RSS deltas are whole-process observations, not allocator-level
measurements.

## Matched results

Clean baseline: `7380cba`. Both runs use optimized release builds, a 120 × 45
PTY, syntax and bundled plugins enabled, LSP disabled, 50 navigation presses,
and three samples per directory size. Values are medians.

| Directory entries | Baseline complete | Updated complete | Open, baseline → updated | Navigation p95, baseline → updated | RSS increase, baseline → updated |
| ---: | :---: | :---: | ---: | ---: | ---: |
| 128 | Yes | Yes | 59.47 → 21.55 ms | 2.796 → 0.555 ms | 7,152 → 2,368 KiB |
| 512 | No | Yes | 36.59 → 23.94 ms | 1.115 → 0.566 ms | 8,288 → 3,616 KiB |
| 2,048 | No | Yes | 35.55 → 24.32 ms | 0.565 → 0.662 ms | 8,352 → 11,264 KiB |
| 8,192 | No | Yes | 37.30 → 47.17 ms | 0.704 → 0.566 ms | 8,240 → 41,104 KiB |

Baseline timings and memory above 128 entries are not full-directory costs:
the previous host discarded every entry after its first 160 filesystem results,
and the Husk presentation layer separately stopped at 200 tree rows. The
updated 8,192-entry run indexes the complete directory, keeps its final file
reachable, and retains a 0 ms median idle-CPU sample during the 350 ms measured
idle window.

A separate single-sample stress run reached the final file in a directory with
32,768 entries. It opened in 119.88 ms, used approximately 163.5 MiB of
additional whole-process RSS, and maintained 0.694 ms p95 navigation.

## Implementation

- Directory enumeration is uncapped, sorts every entry, and runs on Tokio's
  blocking pool instead of the editor thread.
- Expanded directories retain the Husk entry arrays through shared `Arc`
  ownership. The Rust tree indexes entries by compact directory/entry
  coordinates instead of allocating a decorated `PanelRow` per file.
- Rich icons, theme styles, Git badges, branch guides, and paths are
  materialized only for visible rows and explicit selection events.
- Small trees continue using ordinary row panels; large trees switch to the
  virtual tree without changing keyboard navigation, mouse handling,
  active-file reveal, or persisted panel selection.
- Nonrecursive filesystem watchers retain only directory and ignore-file
  metadata fingerprints. They construct a complete listing only after a
  fingerprint changes.
