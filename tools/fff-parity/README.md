# Checking Red's bundled Find in Files backend

Build the driver with the workspace lockfile, then run the differential corpus:

```sh
cargo build --release --locked -p red-fff-parity
python3 tools/fff-parity/compare.py target/release/red-fff-parity
```

The driver keeps a search engine alive across input queries. The comparison
waits for its index to become available, then checks paths, line numbers, every
byte range, regex errors, and truncation against the installed rg. It creates
disposable ordinary directories, Git repositories, and linked worktrees.
`--raw` runs the same comparison against the released FFF FilePicker in an
isolated driver process. Differences are expected in that mode. The default
comparison exercises the code Red ships.

For timings, supply a real checkout and queries with at most 500 matching lines:

```sh
python3 tools/fff-parity/bench.py target/release/red-fff-parity . OpenCallbackPicker ZzNoFffSymbol6381
```

The first query includes process launch and can overlap background indexing.
The subsequent 12 samples use a ready index. Both completion times include
transfer and decoding of results. First-match timings compare a callback inside
the Red worker with receipt of the first matching JSON line from the rg
process. Picker debounce, event dispatch, and painting are outside these
timings. RSS is a whole-driver sample after warm queries.

## Backend

Red links FFF 0.11.0 with its default Rust walker feature. Upstream's high-level
grep changes raw patterns, visibility rules, Unicode matching, and offsets after
512 bytes. Red uses the pinned library's bigram filter on owned input bytes and
uses the Rust `grep-regex` and `grep-searcher` libraries to search candidates.
The opt-in backend requires no external executable.

Each query walks current ignore rules and follows the requested symlink policy.
Only indexed ASCII files with a matching identity and change timestamp qualify
for a negative prefilter result. New files, changed files, non-ASCII input,
large files, and general regexes go through the streaming matcher. This also
keeps external truncation safe while a search is running. The background index
reads up to roughly 64 MiB across the first 8,192 traversal entries, with a
512 KiB per-file indexing limit. These limits affect acceleration. All eligible
files remain searchable.

The editor serializes active searches and cancels superseded work, including
reads in large files. It sends the first matching row to the owning plugin,
then a bounded completion. The result includes up to 500 lines, 256 highlight
spans per line, 2 KiB of source context, and a 2 MiB total item payload.
Highlights use original file byte ranges for navigation and character ranges
for the displayed excerpt. Overflow sets the truncation flag.

## Measurements on 2026-11-06

The macOS run used the Red worktree at base `5b15a726` and a local Codex
checkout at `aa0bd300ed0`. Linux used tracked snapshots on the container's
native filesystem. The Red queries were `OpenCallbackPicker` on macOS and
`ProjectSearchState` on Linux. Both Codex runs searched for `thread_fork`.
Times are milliseconds, rounded.

| Host and checkout | First match, FFF / rg p50 | Complete, FFF / rg p50 | Complete, FFF / rg p95 | First query complete, FFF | Warm driver RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| macOS arm64, Red | 2 / 11 | 11 / 22 | 15 / 52 | 29 | 14 MiB |
| macOS arm64, Codex | 8 / 12 | 85 / 135 | 120 / 207 | 371 | 18 MiB |
| Linux arm64, Red | 2 / 3 | 8 / 9 | 12 / 18 | 17 | 10 MiB |
| Linux arm64, Codex | 2 / 3 | 18 / 33 | 20 / 37 | 31 | 18 MiB |

macOS and Linux each passed 75 parity cases. Red's macOS release executable
also exercised search, hidden files, invalid regex recovery, Unicode navigation,
and export with an empty process allowlist and no rg on PATH. The Linux arm64
Red executable passed configuration validation in an offline container.
The Windows x86-64 GNU driver passed 50 Git and non-Git cases against native
rg 15.2.0. On a synthetic 2,048-file corpus its worker produced the first row in
16 ms median and completed searches in 49 ms median (72 ms p95). Windows rg
completed in 90 ms median (101 ms p95). The Windows Red executable also loaded
the FFF configuration. The release workflow builds Windows with MSVC.

The configured default remains `rg`. Before switching it, run the packaged
editor on Windows MSVC, measure search latency and memory on representative
Windows checkouts, and evaluate first-query latency on large workspaces. The CI
matrix runs the differential corpus when this branch is published.
