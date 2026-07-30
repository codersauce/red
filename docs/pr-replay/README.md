# PR Replay product and architecture specification

PR Replay helps a person understand a pull request by reconstructing the
original implementation, investigating its decisions, and turning review
findings into explicitly approved feedback or code changes.

This directory describes the agreed target product. The existing
[PR Replay Coach guide](../PR_REPLAY.md) documents commands and behavior that
are available today. A target behavior in this directory must not be presented
as already implemented unless its document explicitly says so.

## Product in one paragraph

One review session owns an immutable snapshot of a GitHub pull request or local
branch. A dedicated Replay pane navigates the original changes beside a real
source editor. Reviewers can start an isolated scratch reconstruction when they
want to implement changes themselves or apply original hunks progressively.
Codex can explain the code and suggest findings, comments, or source patches,
but authority and explicit human approval determine which outcomes are
possible. Findings become private notes, original-source review comments, or
approved changes to an independently verified PR worktree. Nothing is posted,
saved, committed, pushed, or executed implicitly.

## Specification map

| Document | Owns |
| --- | --- |
| [Product vision](product.md) | Goals, principles, product boundaries, and success criteria. |
| [Review workflows](review-workflows.md) | Reviewer, PR-owner, local-branch, resume, and portable-review journeys. |
| [Interaction design](interaction-design.md) | Panes, focus, responsive layouts, commands, keyboard behavior, and notices. |
| [Domain model](domain-model.md) | Immutable snapshots, original hunks, findings, workspaces, drafts, and state transitions. |
| [Reconstruction and ordering](reconstruction-and-ordering.md) | Scratch learning, dependency-aware plans, grouped steps, and compilation checkpoints. |
| [Codex collaboration](codex-collaboration.md) | Persistent conversations, intent, scope, findings, draft promotion, and proposed fixes. |
| [Safety and permissions](safety-and-permissions.md) | Authority, approval boundaries, filesystem isolation, provider publication, and untrusted builds. |
| [Implementation roadmap](implementation-roadmap.md) | Current state, milestones, migration constraints, validation, and open decisions. |

## Current and target behavior

Already available on the Replay branch:

- Exact GitHub PR, local-branch, and safe-demo source selection.
- A pinned original commit, original per-hunk diffs, and a dedicated Replay
  pane beside real editable scratch-source buffers.
- Manual reconstruction, exact hunk application, validation, attributed undo,
  review recovery, and portable private review bundles.
- Locally persisted inline comments, summaries, notes, explicit GitHub-review
  publication, and independently verified original-author PR worktrees.
- Scoped Codex answers, explicitly approved local drafts, and original-author
  proposal review.

Specified but not yet available as the target product:

- A persistent Codex companion pane that does not replace the Replay guide.
- A first-class finding lifecycle linking observations, comments, and patches.
- Opening a review without immediately materializing its scratch worktree.
- A dependency graph, foundations-first ordering, atomic multi-file replay
  groups, and optional trusted compilation checkpoints.
- A unified outcome model for suggestions, branch fixes, explicit commits, and
  pushes.

## Nonnegotiable invariants

1. Original PR identity, head commit, original hunks, and GitHub anchors remain
   immutable throughout a review.
2. Scratch reconstruction is essential and can be initialized on demand.
3. Scratch, original PR source, proposed fixes, and writable PR source are
   always visibly distinct.
4. Findings are not GitHub comments, and answers are not findings or comments
   unless a human explicitly promotes them.
5. Only the editor event loop mutates buffers, UI state, or undo history.
6. Network access, worktree creation, builds, GitHub publication, commits, and
   pushes have independent and explicit authorization boundaries.
7. A reviewer cannot modify someone else's PR merely because a shared
   repository happens to grant write access.
8. A failed or interrupted operation never claims a side effect it cannot
   verify.

## Related existing documentation

- [Current PR Replay user guide](../PR_REPLAY.md).
- [Husk plugin compatibility and Replay host calls](../PLUGIN_API.md).
- [Terminal UI ownership and surface boundaries](../UI_ARCHITECTURE.md).
- [Direct Codex workflow and proposal safety](../AGENT_WORKFLOW.md).
- [Crash-safe editor session recovery](../SESSION_RECOVERY.md).
