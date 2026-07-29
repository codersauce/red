# PR Replay implementation roadmap

Status: proposed implementation sequence based on the current Replay branch.
This document distinguishes existing behavior from future milestones; it is
not a claim that every target capability already exists.

## Architectural starting point

Current implementation boundaries:

- `src/replay/source.rs` verifies GitHub or local source identity and obtains a
  complete canonical base-to-head patch.
- `src/replay/session.rs` owns stable original hunks, same-file dependencies,
  review roles, notes, drafts, worktrees, and recovery-visible session state.
- `src/replay/plan.rs` projects exact independent hunks and currently expects
  raw file/hunk traversal order.
- `src/plugin/replay_panel.rs` renders the structured dedicated Replay pane.
- `src/plugin/panel.rs` and `src/editor.rs` own panel focus, source buffers,
  transactions, movement, resize, Codex sessions, and editor-visible effects.
- `plugins/replay.hk` orchestrates trusted host requests and reviewer UI state.
- `docs/PR_REPLAY.md` documents the behavior a person can actually use now.

Git, GitHub, worktree, and review-publication operations run in bounded
background workers. Only the editor event loop mutates source buffers or UI.
These ownership boundaries remain unchanged across every milestone.

## Milestone 0: publish the product specification

Outcome:

- A coherent, cross-linked product and engineering specification.
- A shared vocabulary for snapshots, scratch, findings, ordering, Codex,
  outcomes, and approval boundaries.
- Clear separation between current behavior and target behavior.

Acceptance:

- All documentation links resolve.
- Current operator instructions remain accurate.
- No product document presents a future safety boundary as implemented.

## Milestone 1: restore stable review surfaces

User-visible outcome:

- Replay remains a dedicated, stable original-change pane.
- Source remains a genuine editor window.
- Codex becomes an optional persistent companion instead of replacing the
  Replay guide with an answer screen.
- Questions stream into the companion while original source and diff stay
  visible.

Implementation:

- Reuse existing `PanelManager`, stable text-panel IDs, agent composer, and
  streaming primitives.
- Preserve the current guide/source divider, focus, scroll, selections, and
  source cursor when the Codex companion opens or closes.
- Add responsive horizontal docking when three columns would be unreadable.
- Retain separate read-only question and explicit draft/fix scopes.

Acceptance:

- Wide, standard, and short-terminal captures remain readable.
- `Ctrl-w h/j/k/l`, `Ctrl-w H/J/K/L`, generic resize, and mouse dragging
  preserve every pane's state.
- Asking a question never creates a finding, draft, patch, or GitHub review.
- Focus and status indicators identify the active surface correctly.

## Milestone 2: make findings first-class

User-visible outcome:

- Reviewers can record, inspect, investigate, dismiss, and promote findings.
- Existing notes migrate without losing their source anchor or recovery state.
- Codex findings remain provisional until explicitly accepted.

Implementation:

- Extend `ReplayNote` or introduce a versioned finding representation linked
  to the same original snapshot and hunk identity.
- Record human versus agent origin, evidence, confidence, category, severity,
  lifecycle state, and related artifacts.
- Add finding counters to the Replay guide and outbox without displacing the
  original diff.

Acceptance:

- Existing saved reviews and crash-recovery snapshots continue to load.
- A finding remains private until explicitly converted into another artifact.
- A stale snapshot cannot accept a finding anchored to a different head.
- Codex suggestions cannot silently create durable findings.

## Milestone 3: unify review outcomes and author fixes

User-visible outcome:

- One review session can produce private findings, GitHub feedback, and
  separately authorized original-PR patches when permitted.
- The outbox explains available actions without implying unavailable
  ownership or provider capabilities.

Implementation:

- Project findings, review drafts, verified receipts, and approved proposals
  into one coherent outcome view.
- Preserve current exact original diff anchors and verified publication flow.
- Reuse existing original-author worktree verification and Codex proposal
  machinery.
- Add independently previewed commit and push flows only after their exact
  target branch and remote are modeled safely.

Acceptance:

- A reviewer cannot modify someone else's PR branch.
- Original PR owners cannot self-approve or request changes from themselves.
- Unaccepted proposals do not touch buffers or disk.
- Commit and push remain separate explicit decisions.
- Ambiguous provider responses cannot produce duplicate review submissions.

## Milestone 4: make scratch explicitly on-demand

User-visible outcome:

- A reviewer can inspect original PR material before deciding to create a
  scratch worktree.
- Manual reconstruction and exact original-hunk application remain prominent,
  immediate to discover, and clearly labeled.

Implementation:

- Introduce a versioned representation for a review session without an
  existing `ReplayWorkspace`.
- Keep exact original source images available for read-only inspection.
- Prompt before materializing the approved durable sibling scratch worktree.
- Upgrade existing recovery and portable-review formats without invalidating
  already confirmed scratch sessions.

Acceptance:

- Reading the first change does not create a branch or worktree.
- Starting reconstruction previews and confirms the exact scratch identity.
- Existing manual reconstruction, automatic apply, validation, and undo pass
  unchanged after scratch exists.
- Denying worktree creation leaves findings and original source available.

## Milestone 5: decouple hunk identity from presentation order

User-visible outcome:

- Existing original order behaves exactly as before.
- Replay can safely represent an alternate order without regenerating hunk
  IDs or comment anchors.

Implementation:

- Introduce a versioned ordering overlay keyed by exact original hunk IDs.
- Stop assuming presentation index equals raw patch file/hunk traversal index.
- Preserve per-file original hunk order and exact scratch-source images.
- Add an identity-plan migration with no user-visible behavior change.

Acceptance:

- Identity-order snapshots match current presentation and existing fixtures.
- Every original hunk appears exactly once.
- Recovered sessions, portable review drafts, comments, and undo attribution
  retain stable original identities.
- Completing all hunks yields the exact original author source images.

## Milestone 6: expose dependency annotations

User-visible outcome:

- Original-order steps identify useful prerequisites and dependents.
- Reviewers can understand why another change is relevant without switching
  ordering profiles.

Implementation:

- Extract deterministic structural edges from patch paths, file creation, and
  existing same-file order.
- Add conservative Rust/TOML heuristics for modules, manifests, definitions,
  and references.
- Track confidence and ignore ambiguous symbol matches.

Acceptance:

- Wrong or ambiguous analysis cannot block a normal review.
- Parse failures fall back to raw behavior without losing original hunks.
- Dependency direction is always prerequisite to dependent.
- Module creation is shown before its registration.

## Milestone 7: add foundations-first reconstruction

User-visible outcome:

- Reviewers can switch between original and foundations-first profiles.
- Reconstruction can present definitions and new modules before their users.

Implementation:

- Condense required semantic dependency cycles into explicit logical groups.
- Topologically order groups with original ordinal as the stable tie-break.
- Display original hunk membership, group rationale, and cross-file source.
- Apply grouped hunks through safe composite editor transactions.

Acceptance:

- Same-file relative hunk order never changes.
- Applying every group reproduces the exact pinned original head.
- An incompatible plan falls back to original order and explains why.
- Atomic group undo refuses to discard newer unrelated manual edits.

## Milestone 8: offer trusted compile checkpoints

User-visible outcome:

- A reviewer can explicitly authorize checking meaningful completed groups.
- Checkpoints show honest pending, running, passed, failed, unavailable, or
  not-authorized states.

Implementation:

- Preview the exact untrusted-code execution risk, source identity, workspace,
  package selection, and command.
- Materialize only approved scratch images in an isolated review location.
- Prefer bounded, package-scoped, lockfile-preserving, offline checks.
- Collect bounded diagnostics and invalidate grants when the PR head changes.

Acceptance:

- A PR-controlled build script, macro, wrapper, or test never executes before
  explicit approval.
- Checks remain cancelable and do not block editor input.
- A failed base or original PR head is reported rather than hidden.
- No automatic checkpoint creates a commit, pushes a branch, saves unrelated
  source, or downloads dependencies.

## Validation strategy

Every milestone requires:

- Focused unit coverage for the new state transition or data invariant.
- Integration coverage through real `PluginRequest` and editor-event
  boundaries.
- Regression coverage for source editing, exact hunk application, validation,
  replay undo, session recovery, and portable review bundles.
- UI rendering and keyboard coverage at large and constrained terminal sizes.
- Real-PR dogfooding for multi-file and agent-generated review flows.
- Full workspace tests and the repository-required strict Clippy command when
  Rust changes are involved.
- The repository's Markdown link checker when documentation changes.

Suggested ordering-specific golden cases:

```text
Manifest dependency before its first import.
New module file before mod registration.
New definition before independent cross-file users.
Changed trait signature and implementation updates in one atomic group.
Removal after every changed caller no longer references the symbol.
Ambiguous duplicate symbol names with deterministic fallback.
Rename followed by edits under the new path.
Existing same-file hunks whose context shifts after earlier changes.
Untrusted build refusal, explicit approval, cancellation, and stale snapshots.
```

## Settled product decisions

- There is one review session with capability-driven outcomes.
- Hands-on scratch reconstruction remains a core feature.
- Scratch may be initialized on demand.
- The original PR snapshot and original hunk anchors remain immutable.
- Answers, findings, comments, and source patches are separate concepts.
- A finding can become private feedback, an original-source review comment,
  or an explicitly authorized source proposal.
- Reviewer-source and original-author-source worktrees remain separate.
- Original order and foundations-first reconstruction can coexist.
- Compilation is best-effort, opt-in, and treated as untrusted code execution.
- Commit, push, publication, and source proposal approval are separate gates.

## Open decisions

- Exact final shortcuts for the Codex companion, findings, and ordering switch.
- Which responsive layout thresholds choose a right-side panel versus a bottom
  drawer.
- Whether foundations-first becomes the default only after scratch begins or
  is always an explicit selection.
- Whether finding creation from a human-authored question needs a one-step
  shortcut or an explicit editor.
- How provider-compatible GitHub suggestion blocks are represented in the
  local outbox.
- Whether trusted compile grants are per snapshot, per repository, or per
  exact command.
- Whether package-scoped checks should include workspace feature profiles.
- How logical group undo should compose existing per-buffer editor undo trees.

## Explicitly deferred

- rust-analyzer-grade global name resolution.
- AI-authored ordering as an authoritative planner.
- Guaranteed compilation after every raw original hunk.
- Multi-language dependency extraction before Rust ordering is reliable.
- Arbitrary user drag-reordering of individual prerequisite-bearing hunks.
- Automatic network dependency downloads.
- Silent remote review retries, automatic commits, or automatic pushes.

See [the product vision](product.md),
[interaction design](interaction-design.md),
[domain model](domain-model.md), and
[reconstruction and ordering](reconstruction-and-ordering.md) for the
requirements behind these milestones.
