# Session recovery

Red's core writes a versioned recovery snapshot to `sessions/latest.json` under the
configuration directory. Run `red --resume` to restore it. Recovery is deliberately
separate from opening files: restored dirty contents remain in memory and Red never
writes them to disk until an explicit save.

The snapshot includes open buffers and unsaved contents, the window tree and cursors,
registers, marks, jumplist, per-buffer undo trees with attribution, the persisted agent
transcript, and pending proposal files. Agent transcripts are restored as archived
context unless the negotiated ACP capabilities explicitly support session load or
resume; Red does not silently start a replacement agent.

Snapshots use schema version 2. Older supported versions migrate through explicit
defaults, while unknown future versions are rejected without replacement. Each write
uses a unique, create-new, owner-only temporary file, flushes it, rotates a valid latest
generation, atomically renames the new generation, and flushes the containing directory
on Unix. A corrupt latest generation is never rotated over the last known-good
snapshot; Red falls back safely and preserves that recovery point while repairing the
latest slot. Failed writes remove their temporary file. The editor writes at most once
every five seconds while active and once on clean exit; `RED_PERF=1` reports the work as
`session:snapshot`.

For file-backed buffers, the snapshot also records the disk contents seen at snapshot
time. On resume, Red compares that base with the current file. Any divergence is printed
as a unified recovery diff while the recovered buffer remains untouched and unsaved.
This prevents crash recovery from overwriting work performed by another process.

The fault-injection suite covers failures after temporary-file sync and after generation
rotation. Both leave a loadable previous generation. Snapshot directories use mode
`0700` and files use mode `0600` on Unix.
