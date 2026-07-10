# Project plan implementation status

This file maps [`PROJECT_PLAN.md`](PROJECT_PLAN.md) to the shipped repository state. It
separates implementation from acceptance gates that require elapsed releases or external
people; those gates are not represented as completed by local tests.

| Phase | Implemented repository outcome | Operational gate still requiring external evidence |
|-------|--------------------------------|----------------------------------------------------|
| 0 — Foundation | Canonical `EditTransaction` boundary, current inventory/baselines, ACP proposal spike, and detachable-core ADR/vertical slice | None for the code milestone |
| 1 — Vim credibility | Semantic `.`, bounded macro record/replay, edit-aware marks, transactional `:substitute`, replace command, and a versioned compatibility matrix | Two Vim-native users completing one-week trials |
| 2 — Agent-native | ACP transport and live conformance fixture, buffer-backed proposal filesystem, pending-hunk UI, accept/reject transactions, permissions, prerequisite diagnostics, and inert `disable_ai` integration coverage | Fresh-profile, unaided external run over real SSH with a supported pre-authenticated adapter |
| 3 — Sessions and attribution | Session/turn edit origins, attributed undo tree and conflict-safe selective revert, atomic schema-v2 recovery, external-divergence review, Unix detach/attach/stop, styled IPC, and live adapter PID survival across reconnect | Release acceptance over a genuinely dropped SSH connection |
| 4 — Plugin compatibility | Machine-readable host API, semantic host declarations, load-time parse/type/compatibility checks, semver/dependency validation, quarantine, transactional stateful reload, self-check reporting, and pinned Husk example | Three consecutive releases following the published compatibility/cadence policy |

## Primary verification entry points

- `python3 scripts/repository_inventory.py`
- `cargo test --all-targets --all-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --test acp_conformance --all-features`
- `cargo test --test detach --all-features`
- `cargo test --lib session --all-features`
- `cargo test --lib plugin --all-features`

The manual detach procedure is in [`DETACH.md`](DETACH.md). Agent prerequisites and
review guarantees are in [`AGENT_WORKFLOW.md`](AGENT_WORKFLOW.md); plugin compatibility
and recovery contracts are in [`PLUGIN_API.md`](PLUGIN_API.md) and
[`SESSION_RECOVERY.md`](SESSION_RECOVERY.md).
