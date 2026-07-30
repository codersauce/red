# PR Replay product vision

Status: agreed target product. Existing behavior is identified separately in
the [specification index](README.md) and the
[current user guide](../PR_REPLAY.md).

## Problem

Reading a finished PR diff often reveals what changed without revealing how the
implementation fits together. Large agent-generated changes are particularly
difficult to review because related edits span files, intermediate reasoning is
missing, and the final diff can obscure foundational decisions.

Reconstructing an implementation in a separate scratch workspace forces the
reviewer to identify prerequisites, understand the surrounding source, and
discover whether the original change is coherent. Replay turns that effective
manual review practice into a repeatable editor workflow.

## Primary outcome

After a Replay session, the reviewer should understand the implementation well
enough to do the appropriate next thing:

- Submit useful, correctly anchored feedback on someone else's PR.
- Approve a PR or request changes after reviewing its actual behavior.
- Correct an agent-generated PR they own without confusing scratch experiments
  with real PR edits.
- Record private findings or carry an unfinished review to another computer.
- Explain the change, its dependencies, its tradeoffs, and its remaining risks.

Understanding is the primary outcome. Comments, approval, proposed patches,
commits, and pushes are possible consequences rather than the definition of the
review itself.

## One session, independent capabilities

A review is not permanently divided into separate reviewer and author products.
One session can involve learning, questioning, investigation, feedback, and
repair. Three independent dimensions determine the available actions:

1. **Authority:** the verified relationship between the current person and the
   exact original PR branch.
2. **Intent:** what the person wants to do at this moment, such as understand,
   investigate, comment, or fix.
3. **Destination:** where an approved artifact belongs: private review state,
   GitHub, or an authorized original-PR worktree.

Authenticated ownership and push permission can suggest useful defaults, but
they are not substitutes for explicit authorization. A maintainer with write
access is still a reviewer until a separately verified repair workflow grants
authority over the exact PR branch.

The existing `Reviewer` and `Author` roles remain honest identity labels. They
must not become hidden permission to publish, modify a worktree, execute PR
code, or start an agent.

## Product principles

### Reconstruction remains central

The ability to rebuild the original implementation by hand is not optional
product scope. Reviewers can also apply an exact original hunk automatically,
validate their own implementation, and undo Replay-attributed changes.

Creating a real scratch worktree can be deferred until reconstruction, testing,
or another filesystem-backed operation requires it. The product must still
make reconstruction visible and easy to start.

### The reviewed snapshot never moves silently

Original PR identity, merge base, head commit, patch digest, and source anchors
are pinned. A force-push creates a stale review that requires an explicit
refresh or replacement session; it never silently changes the subject of an
existing finding or draft.

### Findings bridge understanding and action

A finding is a private, source-linked observation. A person may dismiss it,
investigate it, leave it private, turn it into a review comment, include it in a
summary, or request a patch. Codex can suggest findings, but durable findings
and their external consequences require explicit human acceptance.

### Every source surface identifies its reality

The original PR snapshot, progressive replay, scratch experiments, staged agent
proposals, and writable PR branch are distinct. Their labels must explain both
where the source came from and whether the current surface can be edited.

### AI supports the review without taking ownership

Codex can explain, search, connect changes, investigate a finding, draft
feedback, and propose changes within its authorized scope. It does not decide
that a question should become a GitHub comment, apply a patch without review,
publish feedback, commit, push, or execute an unapproved build.

### External actions have explicit gates

GitHub fetches, new worktrees, untrusted builds, original-PR changes, GitHub
publication, commits, and pushes remain independently confirmed. An action
authorized in one category does not imply authorization in another.

## In scope

- GitHub PRs, local branches, and a safe in-memory demonstration.
- Original unified diffs, source context, rationale, and completion tracking.
- On-demand scratch reconstruction and optional dependency-aware ordering.
- A persistent, bounded review conversation with Codex.
- Private findings, local drafts, portable review state, and recoverable
  publication receipts.
- Original-source comments and reviewer-selected GitHub outcomes.
- Explicitly reviewed patch proposals for authorized original PR branches.

## Out of scope

- Automatically publishing comments or creating a remote pending review.
- Automatically saving, committing, pushing, or changing the original PR.
- Treating an inferred explanation as the original author's stated intent.
- Replacing genuine editor buffers or native panels with editable Markdown
  documents pretending to be panes.
- Guaranteeing that every individual raw diff hunk compiles on its own.
- Executing code from an untrusted PR merely to improve its suggested ordering.

## Measures of success

- Reviewers can describe the implementation and its dependencies after a
  session.
- Scratch reconstruction clearly improves understanding of nontrivial PRs.
- People can distinguish original, scratch, proposed, and writable source at a
  glance.
- Review comments remain anchored to the exact original diff.
- No user is surprised by a network request, branch change, build, published
  review, commit, or push.
- Large or entangled PRs remain navigable through grouping and honest
  dependency explanations.

Implementation sequencing and open choices belong in the
[implementation roadmap](implementation-roadmap.md).
