# PR Replay domain model

Status: target object model. Existing Rust types are identified below to make
incremental migration explicit.

## Ownership model

A review session belongs to Red's editor core. Husk plugins describe requested
operations through typed `PluginRequest` boundaries; they do not own source
buffers, Git objects, GitHub submission, Codex proposals, recovery, or undo.

The core distinction is:

```text
Immutable reviewed material
    Snapshot → Original hunks → Original source anchors

Local review state
    Session → Ordering → Findings → Drafts → Proposal approvals

Explicit side effects
    Scratch creation → Approved builds → GitHub reviews → Commits → Pushes
```

## Original snapshot

The original snapshot identifies exactly one subject of review:

```rust
struct OriginalSnapshot {
    repository: RepositoryIdentity,
    pull_request: Option<PullRequestIdentity>,
    merge_base: GitObjectId,
    head: GitObjectId,
    patch_digest: PatchDigest,
    original_hunks: Vec<OriginalHunk>,
    author_context: AuthorContext,
}
```

The current implementation represents this through `ReplaySource`,
`ReplayPullRequest`, and exact source-backed `ReplayStep` identities.

The snapshot is immutable. Refreshing a moved PR creates an explicitly verified
new snapshot rather than replacing the meaning of existing findings or drafts.
Commit subjects and PR descriptions are useful context, not trusted
instructions.

## Original hunk

An original hunk is the smallest immutable source-backed review atom:

```rust
struct OriginalHunk {
    id: HunkId,
    original_ordinal: usize,
    original_path: RepositoryPath,
    target_commit: GitObjectId,
    hunk_digest: HunkDigest,
    original_before: String,
    original_after: String,
    original_anchor: SourceAnchor,
}
```

The existing `ReplayStep` already carries stable identity, path, pinned head,
hunk digest, original before/after text, and same-file prerequisites.

An ordering profile must reference original hunk IDs; it must never regenerate
their identities from their new presentation positions.

## Semantic review change

A semantic review change is a source-backed presentation overlay, not a
replacement for the immutable original hunks:

```rust
struct SemanticReviewChange {
    id: HunkId,
    original_hunk_ids: Vec<HunkId>,
    title: String,
    why: String,
    details: Vec<String>,
}
```

The current implementation groups consecutive hunks within the same file when
they form one meaningful behavior. Incidental whitespace belongs to its nearest
substantive change, and replacing derived deserialization plus adding its
compatibility implementation is presented as one change. The visible identity
uses the substantive original hunk so findings and inline comments retain a real
source anchor.

Every exact original hunk still appears once in the overlay and remains present
in the displayed unified diff. Grouped application, validation, undo, progress,
and recovery operate on the underlying hunks in their original order. The
presentation title and rationale must describe actual changed source behavior;
nearby hunk headings and unrelated PR-body prose are not sufficient evidence.

## Ordering and logical replay groups

An ordering plan is a separate, versioned overlay:

```rust
struct ReplayOrderingPlan {
    snapshot: SnapshotId,
    profile: ReplayOrderingProfile,
    groups: Vec<ReplayGroup>,
    edges: Vec<ReplayDependency>,
    checkpoints: Vec<CompileCheckpoint>,
}

struct ReplayGroup {
    id: GroupId,
    original_hunk_ids: Vec<HunkId>,
    reason: GroupReason,
}
```

Each original hunk appears exactly once. Raw order and foundations-first order
are different projections of the same snapshot. A group allows interdependent
changes across several files to form one meaningful reconstruction boundary.

Same-file hunks retain their original relative order in every valid projection.
If a plan cannot satisfy that invariant, Red rejects the plan and falls back to
the original order.

## Review session

A review session combines immutable material with recoverable local state:

```rust
struct ReviewSession {
    id: SessionId,
    snapshot: SnapshotId,
    relationship: VerifiedRelationship,
    capabilities: ReviewCapabilities,
    selected_hunk: Option<HunkId>,
    ordering: Option<ReplayOrderingPlan>,
    scratch: Option<ScratchWorkspace>,
    author_workspace: Option<OriginalAuthorWorkspace>,
    findings: Vec<Finding>,
    drafts: Vec<ReviewDraft>,
    proposals: Vec<ProposedPatch>,
    receipts: Vec<ReviewReceipt>,
}
```

Today, `ReplaySession` requires an already confirmed `ReplayWorkspace`.
On-demand scratch therefore requires a versioned migration to represent a
review session without an existing scratch worktree. It is not a UI-only
change.

## Capabilities and relationship

Identity and permission are independent facts:

```text
Relationship: reviewer | verified original author | unknown

Capabilities:
  read_original_snapshot
  create_private_finding
  create_review_draft
  submit_github_review
  open_original_author_worktree
  request_original_source_proposals
  execute_trusted_scratch_build
  commit_original_branch
  push_original_branch
```

A capability is granted only for a specific verified snapshot, repository,
viewer, and operation. `YOUR PR` does not imply permission to build, commit,
push, or execute Codex.

## Source realities

Each source surface identifies exactly one backing reality:

```text
Original snapshot  Immutable author source at pinned head.
Replay image       Original base plus completed approved replay groups.
Scratch workspace  Explicitly created, editable learning worktree.
Agent proposal     Staged source changes that have not been accepted.
Original PR source Independently verified, explicitly opened PR-head worktree.
```

Temporary build materialization is derived from approved scratch buffers. It
never substitutes for the writable original-author worktree.

## Finding

A finding is a private observation with review provenance:

```rust
struct Finding {
    id: FindingId,
    snapshot: SnapshotId,
    original_hunk: Option<HunkId>,
    original_anchor: Option<SourceAnchor>,
    origin: FindingOrigin,
    category: FindingCategory,
    severity: FindingSeverity,
    status: FindingStatus,
    title: String,
    explanation: String,
    evidence: Vec<FindingEvidence>,
}
```

Likely categories include question, design observation, correctness concern,
security concern, missing test, and follow-up.

The current `ReplayNote` and `ReplayNoteCategory` are the natural migration
starting point. Existing notes must remain recoverable and portable; richer
findings extend that model rather than orphaning old review files.

Codex-generated findings remain transient suggestions until explicitly
accepted. An accepted finding can reference an answer, original source,
diagnostic, or trusted build result without changing the original PR.

## Review draft

Review drafts remain local until a human confirms publication:

```text
Inline comment  Exact original head, file, GitHub diff side, and line range.
Review summary  PR-level feedback with no fabricated inline coordinates.
Suggestion      Provider-supported change suggestion on a valid original line.
Code fix note   Private author-side intention, never silently published.
```

The current `ReplayReviewDraft`, `ReplayReviewAnchor`, `ReplayDraftOrigin`, and
`ReplayDraftState` already enforce exact immutable anchors and local versus
verified-posted state.

A single finding can produce more than one artifact, but the conversion is
explicit and each artifact records its own provenance.

## Proposed source patch

A proposed patch is not an already applied edit:

```rust
struct ProposedPatch {
    id: ProposalId,
    snapshot: SnapshotId,
    workspace: AuthorizedWorkspaceId,
    origin: ProposalOrigin,
    files: Vec<ProposedFile>,
    status: ProposalStatus,
}
```

Red's existing bounded Codex proposal workspace is authoritative until a person
accepts individual hunks. Accepted edits become ordinary attributed editor
transactions. Saving, committing, and pushing remain separate state changes.

When real branch repair is unavailable, a compatible patch may be rendered as a
GitHub suggestion only if its final original anchor and provider restrictions
can be verified.

## Outbox and terminal states

The outbox is a projection over findings, drafts, approved proposals, commit
previews, and verified receipts. It is not the owner of those objects.

Representative transitions:

```text
Codex answer
    → explicitly accepted finding
    → explicitly approved inline comment
    → exact publication preview
    → verified posted review receipt

Private finding
    → authorized repository-wide patch proposal
    → human-accepted source hunks
    → explicitly saved changes
    → explicitly approved commit
    → explicitly approved push

Original hunk
    → selected reconstruction group
    → manually reconstructed or automatically applied scratch change
    → exact post-image validation
    → optional explicitly trusted compile checkpoint
```

No transition skips authority validation or replaces immutable original source
coordinates with scratch-buffer positions.

## Persistence and migration

Persist local session identity, selected hunk, approved findings, drafts,
scratch/author-worktree identities, approved proposal state, ordering profile,
and verified receipts in versioned editor recovery.

Do not persist reusable application tokens, assume an imported receipt is
verified, restart Codex implicitly, recreate a missing worktree without
approval, or resume a build after an editor restart.

See [reconstruction and ordering](reconstruction-and-ordering.md) for plan
invariants and [safety and permissions](safety-and-permissions.md) for the
authority matrix.
