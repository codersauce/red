# PR Replay review workflows

Status: target journeys grounded in existing Replay capabilities. The
[implementation roadmap](implementation-roadmap.md) distinguishes available
behavior from future work.

## Shared session lifecycle

Every review follows the same broad lifecycle:

```text
Select a review source
        |
Pin the original repository, merge base, head, and diff
        |
Inspect changes and rationale
        |
Optionally materialize scratch and reconstruct original changes
        |
Ask questions, investigate, and collect private findings
        |
Promote selected findings into comments or proposed fixes
        |
Inspect the outbox and explicitly perform authorized final actions
```

Reading a source, browsing a step, asking a question, creating a worktree,
executing code, posting a review, and changing a branch remain separate
operations with separate authority.

## Reviewing someone else's GitHub pull request

### Open and identify the PR

The reviewer starts Replay from the relevant repository and enters a PR number
or canonical GitHub URL. Red verifies the repository, authenticated viewer,
original merge base, exact original head, and read permissions.

Missing Git objects are fetched only after a separate confirmation. The review
initially shows the original snapshot and the first change without requiring
the reviewer's current checkout to switch branches.

### Understand the change

The dedicated Replay pane shows original PR identity, selected change,
completion, exact original diff, and author-grounded rationale. The real source
editor shows the selected source reality with an explicit provenance label.

The reviewer can browse changes without claiming that a later dependent change
has been reconstructed. Dependency annotations explain when a definition,
module, manifest entry, or prerequisite appears elsewhere in the PR.

### Reconstruct when useful

Choosing manual reconstruction or automatic hunk application previews the exact
merge-base scratch worktree. The reviewer explicitly authorizes its creation.
The scratch branch is distinct from the original PR branch.

The reviewer may:

- Rebuild a change manually and validate the result.
- Apply one exact original hunk with an undoable editor transaction.
- Choose foundations-first reconstruction if dependency analysis is available.
- Inspect an atomic multi-file group before applying its constituent hunks.
- Explicitly authorize a trusted scratch build or test when appropriate.

Scratch changes never become edits to the original author's branch.

### Investigate and capture findings

The reviewer can open the optional Codex companion and ask a direct question
about the selected change or the entire PR. Answers remain private and do not
automatically create findings or review comments.

The reviewer can record a finding manually or explicitly accept a
Codex-proposed finding. Each finding identifies its source, confidence,
original change, and snapshot when that information is available.

### Prepare and submit feedback

Selected findings become original-source inline comments, a PR-level summary,
or a provider-compatible suggestion block. Drafts stay in the local outbox.

The reviewer chooses one supported outcome:

- Comment without approving or requesting changes.
- Approve the original PR head.
- Request changes, with an explanatory summary when required.

Red previews the exact PR, head, reviewer, outcome, summary, comments, and
anchors. Nothing is posted until the reviewer explicitly accepts that preview.
A verified receipt, not a local assumption, marks submitted comments as posted.

## Reviewing an agent-generated pull request you own

### Establish identity and branch authority

Red verifies that the authenticated viewer is the original PR author and checks
actual access to the exact head repository. `YOUR PR` identifies the verified
relationship; it does not automatically open or modify the branch.

The same Replay guide, original snapshot, optional scratch reconstruction,
findings, and Codex conversation remain available. The person can still leave
a comment-only GitHub review, but self-approval and requesting changes on
their own PR are unavailable.

### Learn before changing anything

The owner can replay an agent's work in scratch just as a reviewer would. The
original PR head and scratch base remain independently labeled:

```text
SCRATCH SOURCE · merge base · change 08/49
PR SOURCE · original head 15c4957 · read-only until authorized
```

Reconstructing a suspicious change does not silently switch the source editor
to the writable PR branch.

### Open the real PR source explicitly

If a real fix is needed, Red previews the original head repository, fork,
branch, pinned commit, proposed durable sibling worktree, and authenticated
viewer. Opening the original-PR worktree requires explicit confirmation.

The existing checkout, learning scratch, and unrelated dirty buffers remain
unchanged. The writable source surface is labeled `PR SOURCE` or `PR BRANCH`.

### Propose, inspect, and accept fixes

The owner may edit the authorized PR worktree manually or ask Codex to propose
a repository-wide fix. Codex stages its edits through Red's existing bounded
proposal machinery; the original source does not change until the person
accepts individual hunks.

Accepted changes use ordinary editor transactions and remain undoable. Saving,
staging, committing, and pushing are separate actions; none is implied by
accepting a proposal.

### Finish the PR update

The outbox identifies approved local edits, unresolved findings, optional
comment-only feedback, and the actual target repository and branch. The owner
explicitly chooses whether to save, commit, and push.

Commit creation requires a preview of staged content and commit message. Push
requires a separate preview of the exact fork, branch, expected remote head,
and commits to be transmitted. A changed remote head blocks an unsafe push.

## Reviewing a local branch

The reviewer selects a locally available head and explicit or safely inferred
base. Red resolves the real merge base and pins the local source objects.

Local-branch reviews support understanding, findings, reconstruction, optional
trusted checks, and portable local review state. They do not pretend that a
GitHub PR exists, create fake provider anchors, or offer GitHub publication.

If a local branch later becomes a GitHub PR, linking it requires independently
verified repository, base, head, and diff identity; matching names alone are
insufficient.

## Resuming an existing review

Replay discovers recoverable sessions without changing branches. If exactly one
safe review exists it can reopen directly; multiple reviews require an
explicit selection.

Before restoring a session, Red verifies:

- Original repository and full pinned head.
- Source patch digest and original hunk identities.
- Any existing scratch-worktree root, branch, and merge-base identity.
- Original-author worktree identity, when present.
- Relevant undo attribution, unsaved editor buffers, and review receipts.

Recovery restores private findings, drafts, progress, and visible source
surfaces without saving files, recreating a worktree, rerunning Codex, or
replaying an external publication.

## Moving a review to another computer

A person can explicitly save the local findings, drafts, original anchors,
snapshot identity, and known review receipts to a private review file.

Loading requires a matching repository, PR, merge base, full head, and patch
digest. Red previews new or conflicting records before importing them.

Imported GitHub receipts are not automatically trusted: a local file cannot
prove that GitHub actually accepted a review. Red verifies them against the
provider before showing an imported comment as posted.

## Exceptional outcomes

- **Original PR force-pushed:** mark the current session stale and offer a new
  verified snapshot; never silently re-anchor old findings.
- **Original file changed later in the PR:** explain when a draft anchor no
  longer refers to a line visible in the final provider diff.
- **Missing push permission:** disable real PR edits and offer a comment or
  supported suggestion block instead.
- **Build permission denied:** keep reconstruction available and label compile
  checkpoints as not authorized.
- **Uncertain GitHub submission:** verify the original request with the
  provider before allowing a retry.
- **Unrecoverable scratch:** preserve findings and drafts; do not overwrite an
  unrelated worktree or dirty buffer.

Detailed interface behavior belongs in
[interaction design](interaction-design.md); authority and failure handling
belong in [safety and permissions](safety-and-permissions.md).
