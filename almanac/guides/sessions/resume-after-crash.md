---
title: "Resume After Crash"
summary: "Use `red --resume` to recover the newest useful crash snapshot, inspect divergence warnings, and save recovered buffers deliberately."
topics: [guides, sessions, recovery, operations]
sources:
  - id: recovery-doc
    type: file
    path: docs/SESSION_RECOVERY.md
  - id: main-entry
    type: file
    path: src/main.rs
  - id: session-store
    type: file
    path: src/session.rs
  - id: editor-session
    type: file
    path: src/editor.rs
---

Use this guide when Red or the machine crashed and the editor owner is no longer running. The expected result is an interactive editor restored from the newest valid crash snapshot, with dirty buffers in memory, any disk divergence printed for review, and no recovered text written to disk until you explicitly save [@recovery-doc]. If the owner is still alive and only the terminal disappeared, use [Detach and reattach](detach-reattach) instead.

## Before Running Resume

Confirm the failure mode first. Recovery is for owner loss or restart; a detached session that is still running should be reached with `red --attach SESSION` because it keeps the live editor and agent process alive [@recovery-doc]. The recovery documentation warns not to resume an editor that is still running because interactive owners do not currently hold an exclusive recovery lock [@recovery-doc].

Run:

```shell
red --resume
```

The CLI makes `--resume` conflict with positional files and `--root`, so resume is not mixed with opening a new file set or overriding the working directory from the command line [@main-entry].

## What Resume Selects

`red --resume` loads from the configuration directory's `sessions` root [@main-entry]. The store scans owner namespaces and chooses recoverable snapshots with dirty buffers ahead of clean snapshots, then chooses by saved time and generation [@session-store]. Legacy root snapshots are still considered because the scan includes the root store as well as owner directories [@session-store].

After loading, Red changes to the snapshot's saved working directory when present [@main-entry]. It reconstructs buffers directly from the snapshot, creates the editor, restores the full session state, and reuses the store that supplied the snapshot so later clean saves replace the stale recovery point in that namespace [@main-entry].

## Inspect Recovery Warnings

If a recovered file changed on disk after the snapshot's trusted base was captured, Red prints:

```text
Recovered <path> with external disk changes:
<unified diff>
```

The diff compares `snapshot disk base` to `current disk` [@session-store]. Unreadable current files are reported as divergences with a marker rather than treated as empty or unchanged, and the divergence check does not modify disk or recovered buffers [@session-store].

Inside the editor, Red can also show `Recovered unsaved state; N file(s) changed on disk (see recovery report)` when divergences were found [@editor-session]. If an agent transcript was restored but cannot be continued, Red reports archived context and tells the user to start a new session [@editor-session].

## Save Deliberately

Recovered dirty text is editor memory, not an automatic disk write. The recovery documentation states that restored dirty contents remain in memory and Red never writes them to disk until an explicit save [@recovery-doc]. Review the divergence report before saving any buffer whose backing file changed externally.

For agent work, distinguish structured conversations from archived transcript text. New snapshots can store an `agent_conversation` with the Codex thread binding and clean message projection; on restore, Red loads that conversation into `AgentManager` so the replacement app-server can resume and reconcile it when possible [@session-store] [@editor-session]. Legacy flat transcript text without a resumable conversation is restored as archived context, and Red tells the user to start a new session instead of pretending the old process survived [@editor-session].

## If Resume Fails

If Red reports that no recoverable snapshot exists, the snapshot root may not contain a valid root or owner snapshot [@session-store]. If it reports that a latest snapshot is invalid and the last known-good snapshot is unavailable, both `latest.json` and `previous.json` failed validation for that store [@session-store].

Do not repair a snapshot by editing recovered buffers onto disk blindly. The recovery store intentionally falls back from invalid `latest.json` to `previous.json`, rejects future schema versions, and preserves the last known-good generation during failed writes [@session-store]. Use [Crash recovery snapshots](../../architecture/sessions/crash-recovery-snapshots) for the storage rules behind those errors.
