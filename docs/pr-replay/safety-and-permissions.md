# PR Replay safety and permissions

Status: many source, worktree, review-publication, and agent boundaries are
already implemented. Trusted build execution, dependency-planner approval, and
explicit PR commit/push actions are target behaviors.

## Security model

Replay handles three sources of untrusted input:

- Original PR metadata, author text, commit messages, and source files.
- Agent-generated explanations, findings, review drafts, and patch proposals.
- Imported portable review state and provider responses.

None of them is permission to change source, execute code, contact GitHub,
create a worktree, publish a review, commit, or push.

Prompt instructions, displayed author identity, an appealing explanation, and
a guessed GitHub handle are not authority checks.

## Authority is operation-specific

| Operation | Required boundary | Automatic? |
| --- | --- | --- |
| Read a locally available original snapshot | Verified repository, merge base, head, and bounded patch. | Yes, after choosing the source. |
| Fetch missing PR objects | Explicit preview of the exact source and Replay-owned refs. | No. |
| Create a scratch worktree | Explicit preview and acceptance of the exact sibling root, scratch branch, and merge base. | No. |
| Edit a scratch buffer | Selected verified review session and normal editor authority. | Yes, after entering scratch intentionally. |
| Add a private finding | Explicit human creation or acceptance. | No for agent suggestions. |
| Create a local review draft | Exact original head and, for inline drafts, original provider diff coordinates. | No for agent suggestions. |
| Submit a GitHub review | Exact provider preview, authenticated viewer, unchanged head, and explicit confirmation. | No. |
| Open writable original PR source | Verified original author, exact head repository, actual push permission, and separate worktree confirmation. | No. |
| Accept a Codex source proposal | Verified author workspace, matching source revision, and per-hunk human approval. | No. |
| Run a scratch build or test | Explicit authorization to execute code from the exact reviewed snapshot. | No. |
| Create a Git commit | Explicit preview of workspace, files, staged content, and commit message. | No. |
| Push a PR branch | Explicit preview of exact fork, remote, branch, original head, and commits. | No. |

Granting one action does not grant the next action. For example, authorizing a
scratch worktree does not authorize a build; authorizing a build does not
authorize a push.

## Immutable snapshot checks

Original review state is bound to:

```text
Provider host and repository identity.
Original PR number when applicable.
Verified merge-base object.
Complete original author-head object.
Digest of the exact canonical original patch.
Stable original hunk IDs, paths, source images, and changed-line ranges.
```

Short commit prefixes and display labels are informative only. Approval and
publication compare complete identities.

If the provider head changes, Replay marks the session stale. It does not
quietly replace original hunks, carry an old approval to a new commit, or
publish comments against a different diff.

## Scratch-worktree isolation

Scratch worktrees:

- Are durable sibling checkouts, not ephemeral directories.
- Start from the exact original merge base.
- Use a separate Replay-owned local branch.
- Never check out or reset the author's original PR branch.
- Require explicit creation or safe verified reuse.
- Reject unsafe paths, unrelated branches, mismatched repositories, and
  symlinked escapes.

Automatic reconstruction edits editor-owned scratch buffers and does not save
their content to disk without a separately authorized action.

If a build needs real filesystem images, the exact approved scratch state must
be materialized inside an isolated review location. It must not save an
unrelated dirty editor buffer or modify the original checkout.

Recovery never resets a dirty worktree, replaces an unrelated sibling, or
silently creates a missing checkout.

## Original-author worktree isolation

Opening real PR source requires all of:

1. The exact original PR belongs to the authenticated viewer.
2. The viewer can write to the exact original head repository.
3. The full original head, branch, fork, repository, and worktree path are
   reverified.
4. The person explicitly accepts the original-author worktree preview.

Write access to the base repository is not proof of PR ownership. A fork PR
must never silently redirect to the base repository's `origin`.

Existing unsaved buffers, untracked files, user-created commits, and local
changes in the authorized original-author worktree are preserved. A moved
remote or unrelated local branch causes a refusal rather than a reset.

## GitHub comment and review integrity

Inline comments bind to the original diff, not scratch:

```text
Full original PR head.
Original repository-relative path.
Original GitHub diff side: LEFT or RIGHT.
Exact one-based original changed-line range.
Original source-hunk digest.
```

Provider-specific suggestion blocks are permitted only when the selected
original line and replacement satisfy the provider's constraints. A scratch
line, inferred path, stale line number, or unverified renamed path is not a
valid anchor.

GitHub publication:

- Batches the explicitly selected comments and outcome into one provider
  review.
- Does not create an unapproved remote pending review.
- Persists the exact intended submission before contacting the provider.
- Rechecks reviewer identity, PR head, original anchors, and previewed body.
- Marks comments posted only after a verified provider response.
- Stores exact receipts for recovery and portability.
- Treats lost responses as uncertain, not as permission for a blind retry.

Self-approval and requesting changes on one's own PR are not offered. Local
branch reviews and demonstrations cannot publish fake GitHub reviews.

## Imported reviews and uncertain publication

Portable review files are not authentication or proof of publication.

Before importing, verify exact source identity and show any conflicting
findings or drafts. Imported provider receipts begin unverified until GitHub
confirms the original review, viewer, outcome, body, commit, and comments.

When a publication may have reached GitHub before a crash or network failure,
allow only an explicitly confirmed provider lookup. Verify an exact matching
review before marking it posted; retry only after the provider confirms that
the original request did not produce a matching review.

## Codex isolation

Codex sees only the context and capabilities explicitly granted to its current
turn.

Read-only explanation, investigation, comment, and summary scopes must reject
source proposals and mutating editor tools at the host boundary. A prompt that
claims write permission cannot override this policy.

Original-author fix scopes stage changes in Red's verified proposal workspace;
they cannot write source files, run shell commands, call GitHub, publish,
commit, or push directly.

PR descriptions, commit messages, code comments, and imported review text are
untrusted data. They cannot alter tool policy or authorize hidden actions.

See [Codex collaboration](codex-collaboration.md) and the existing
[agent workflow contract](../AGENT_WORKFLOW.md).

## Building untrusted pull requests

`cargo check`, tests, and build-related tooling can execute PR-controlled
code, including:

- `build.rs` scripts.
- Procedural macros.
- Compiler wrappers and workspace tool configuration.
- Cargo aliases, configuration, and dependency sources.
- Test executables and project-specific helper programs.

Therefore compiling an unfamiliar PR is not equivalent to parsing its source.
It is code execution.

Required behavior:

1. Checks are off by default.
2. Explain the exact command, repository, snapshot, and execution risk.
3. Require explicit per-snapshot or tightly scoped repository approval.
4. Use an isolated approved scratch location and avoid exposing unrelated
   editor state, secrets, or credentials.
5. Disable network access when possible; dependency downloads require a
   separate explicit permission.
6. Prefer lockfile-preserving, offline commands when they are valid.
7. Apply bounded time, output, concurrency, and cancellation limits.
8. Never interpret a compiler failure as permission to alter source.
9. Invalidate build permission if the reviewed original head changes.

A normal workstation process is not a security sandbox. If a trustworthy
isolation mechanism is unavailable, describe that limitation before asking
the user whether to run the code.

## Durable editor state

Editor recovery persists approved review state, buffer revisions, undo
attribution, private drafts, and verified receipts through the existing
versioned snapshot mechanism.

Recovery must not:

- Reuse a one-shot application approval token.
- Start a new Codex process or replay an agent turn.
- Save scratch or original-author files.
- Automatically retry an uncertain GitHub publication.
- Resume a build, commit, or push.
- Import a future unsupported snapshot schema.

The existing contract is described in
[session recovery](../SESSION_RECOVERY.md).
