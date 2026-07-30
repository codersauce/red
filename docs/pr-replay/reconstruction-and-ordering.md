# PR Replay reconstruction and dependency-aware ordering

Status: target design. Exact per-hunk reconstruction, same-file prerequisites,
validation, and undo already exist. On-demand scratch, alternate orderings,
atomic multi-file groups, and compilation checkpoints remain proposed.

## Why reconstruction is central

Reading the finished PR shows the final answer. Reconstructing the original
change requires the reviewer to understand the surrounding source, identify
its prerequisites, and decide how the implementation should be written.

Replay supports two equally legitimate reconstruction actions:

- **Manual reconstruction:** edit scratch source to reproduce the original
  author change and validate the exact result.
- **Automatic application:** apply the original unified hunk as an attributed,
  revision-checked, undoable editor transaction.

The original diff remains visible in either mode. Automatic application is
immediate and undoable; no additional modal confirmation interrupts each
learning step.

## Existing ordering behavior

The current implementation obtains one canonical `git diff` from pinned merge
base to pinned head. It iterates files in Git-emitted order and hunks in
top-to-bottom order within each file.

Each original hunk becomes one `ReplayStep`. Later hunks in the same file
depend on earlier hunks through `prior_by_path`. No current dependency connects
different files.

A separate semantic presentation overlay can group consecutive same-file
original hunks without changing their identities, relative order, source
anchors, or exact patches. Formatting-only hunks attach to the next meaningful
change when possible. Related compatibility changes, such as removing derived
deserialization and adding an explicit backwards-compatible implementation,
form one reviewable unit. Applying or undoing that unit remains one ordinary
editor transaction while each original hunk retains independent completion and
recovery metadata.

For example, Git path order can produce:

```text
1. src/lib.rs         Add mod pagination;
2. src/pagination.rs  Create the pagination module.
```

The intermediate scratch repository cannot compile after step 1 because the
referenced module does not yet exist.

The presentation compiler still retains every raw parsed hunk and verifies that
each semantic group references consecutive hunks in its original file. The
current overlay groups changes but does not reorder them; changing the visible
list order independently would still violate same-file prerequisites and
source-image assumptions.

## Separate original identity from presentation order

Original hunk identity belongs to the pinned snapshot:

```text
Original Hunk A  src/lib.rs         original ordinal 01
Original Hunk B  src/pagination.rs  original ordinal 02
```

A reconstruction profile is an overlay:

```text
Original order:          A → B
Foundations-first order: B → A
```

Both profiles reference exactly the same hunk IDs, source digests, original
paths, changed lines, before/after text, and GitHub anchors.

The presentation position of a hunk must not become its immutable identity.
Recovery, notes, findings, comments, and Codex context remain anchored to the
original hunk ID rather than an order-dependent row number.

## Ordering profiles

### Original order

Preserve the current Git-emitted file and hunk order. This profile remains
stable, matches familiar provider diff presentation, and is useful when a
reviewer wants to begin from an entry point or high-level behavior.

The dependency graph can still annotate the original order:

```text
01  Update app.rs
    Requires PaginationState, introduced in change 07.

07  Add PaginationState
    Used by changes 01, 03, and 11.
```

### Foundations-first order

Order prerequisites before dependents where a safe relationship can be
established. The same example becomes:

```text
01  Create src/pagination.rs.
02  Add PaginationState.
03  Register mod pagination;.
04  Update app.rs to use PaginationState.
05  Add regression coverage.
```

Foundations-first is especially appropriate when the reviewer enters scratch
reconstruction. The product can offer or select it at that point without
forcing it on someone who only wants to read the PR.

### Future narrative profiles

Author-commit order, intent-first order, and test-first order are possible
future projections. Commit chronology is useful only when the author commits
are independently meaningful; a squashed PR cannot supply a narrative it does
not contain.

These profiles are not required for the first dependency-aware milestone.

## Dependency graph

Dependency edges are directed from prerequisite to dependent:

```text
Create module file        → Register module declaration
Add Cargo dependency      → Import the new dependency
Define a type             → Use the type in another file
Define a trait            → Add its implementation
Earlier same-file hunk    → Later same-file hunk
Update a caller           → Remove the old called function
```

The direction must always be explicit. In particular, a Rust module file must
exist before the `mod name;` declaration that causes the compiler to load it.

### Hard structural dependencies

Hard edges preserve patch applicability and source identity:

- An earlier hunk in the same file precedes a later hunk in that file.
- Creating a file precedes later edits to that newly created file.
- A verified rename or move is grouped with edits that require its new path.
- A file cannot be removed before hunks that still require its original image.

Every valid profile preserves the raw relative order of hunks within each file.
Therefore the reconstructed state of any individual file is always a contiguous
prefix of its original hunk sequence.

This prefix invariant lets existing scratch images, hunk context, line-delta
calculation, revision checks, and unique pre-image validation remain valid
while independent files interleave differently.

### Semantic dependencies

Semantic edges describe likely definition-before-use relationships:

- New module file before its registration.
- Manifest dependency before imports from the new crate.
- Struct, enum, trait, function, constant, macro, or re-export before another
  changed file references it.
- Trait definition before an added implementation.
- A changed API and its verified call-site updates.
- Relevant implementation changes before related tests.
- Removal of callers before removing a provider they still reference.

Use confidence levels. A relationship established from exact path, module, and
symbol evidence can influence reconstruction. Ambiguous same-name symbols,
macros, conditional compilation, generated code, and external crates must not
be treated as facts merely because identifiers match.

Uncertain relationships should be shown as suggestions or dropped; they must
not invalidate an otherwise safe review.

## Extracting dependencies

Start with existing patch metadata and cheap deterministic file classification:

```text
Cargo.toml / Cargo.lock
New modules and new source files
Changed definitions and exports
Call sites and configuration wiring
Tests
Documentation and generated artifacts
Safe removals after their consumers
```

For Rust, Red already includes Tree-sitter Rust and TOML grammars. Parse the
original base and original head images and inspect changed regions for:

- `mod`, `use`, `pub use`, and qualified paths.
- Function, type, trait, implementation, macro, and constant definitions.
- Added function calls and changed type references.
- `[dependencies]`, workspace dependencies, and feature changes.
- `#[cfg(test)]`, integration-test paths, and test attributes.

Prefer same-module and same-crate evidence. If several possible definitions
remain, do not guess. A parse failure degrades only the affected file to raw
structural ordering.

Do not initially depend on a running language server, rust-analyzer name
resolution, a Codex model call, or an unapproved repository build to produce a
usable ordering plan.

## Atomic multi-file groups

Some final states cannot be reached through individually compiling hunks.

For example:

```text
src/retry.rs   Change RetryPolicy::next_delay signature.
src/client.rs  Update the first implementation.
src/worker.rs  Update the second implementation.
tests/retry.rs Update the associated call.
```

No single hunk can guarantee a compiling intermediate repository. These hunks
can become one logical replay group:

```text
Group 08 · Update the retry interface · 4 original hunks
```

Group behavior:

- Preserve every exact original hunk and its GitHub anchor.
- Show all affected files before application.
- Allow manual inspection or reconstruction of each member.
- Treat automatic group application as an explicit composite operation.
- Validate completion only after all required members are present.
- Run optional compilation only after the group is complete.
- Undo a composite application safely without skipping newer manual edits.

Use strongly connected components only for required or high-confidence
semantic cycles. Low-confidence soft cycles should be discarded or weakened;
they must not collapse half a PR into one opaque group.

A structural hard-edge cycle is invalid. Fall back to original order rather
than manufacturing a false dependency resolution.

## Stable topological ordering

After grouping necessary cycles, topologically order the resulting group graph.
If several groups are ready, prefer the one containing the lowest original
hunk ordinal. This stable tie-break preserves the existing narrative wherever
dependency analysis does not justify a change.

Planner failure conditions include:

- Original-hunk coverage differs from the pinned snapshot.
- A hunk appears in multiple groups or no group.
- A same-file relative ordering changes.
- A required edge points backward in the final result.
- Parsing exceeds its bounded time or memory budget.
- The original snapshot moves or cannot be verified.

Failure falls back to original order and explains why. It never rewrites the
original review or destroys reconstruction progress.

## Scratch lifecycle

The target session can show original source before creating a worktree. Scratch
materializes only when the person explicitly chooses a filesystem-backed
action, such as:

- Reconstruct this change manually.
- Apply the original hunk or group.
- Run an approved build or test.
- Experiment with an alternative implementation.

Creation previews the exact durable sibling worktree, scratch branch, original
repository, and merge base. The original checkout and PR branch remain
unchanged.

Automatic hunk application changes editor-owned scratch buffers. It does not
silently save them. A trusted build that requires real files must explicitly
materialize the exact approved scratch images in an isolated review workspace;
it must never save unrelated original-PR buffers as a side effect.

Scratch remains reconstructable from its pinned base and approved replay
history. Temporary compile checkpoints do not create throwaway Git commits,
rewrite the reviewer branch, or publish anything.

## Compilation checkpoints

The achievable promise is:

> Keep scratch code compilable at meaningful replay-group boundaries when the
> original repository and environment allow it.

Never claim that every raw hunk compiles. An unchanged base may already fail,
the original PR head may fail, features may differ, generated sources may be
missing, or a change may require an indivisible cross-file group.

A checkpoint records:

```text
Snapshot and scratch identity.
Completed original hunk IDs.
Exact command and feature configuration.
Packages examined.
Permission and isolation policy.
Result, bounded diagnostics, duration, and cancellation state.
```

Suggested states:

```text
NOT RUN        The reviewer did not request compilation.
NOT AUTHORIZED Untrusted code execution was not approved.
RUNNING        An approved, bounded background build is active.
PASSED         The recorded approved command succeeded.
FAILED         The recorded command failed; diagnostics are inspectable.
UNAVAILABLE    No safe command or reproducible build configuration exists.
```

Package-scoped `cargo check -p <package>` can reduce cost when changed paths map
unambiguously to workspace members. Workspace manifests, shared features,
cross-crate changes, build scripts, generated code, and uncertain mappings may
require an explicitly selected wider command.

Use offline and lockfile-preserving execution where possible. Dependency
downloads require a separate network decision. Checks are rate-limited,
cancelable, and run only at chosen group boundaries.

Critically, `cargo check` can execute PR-controlled `build.rs`, procedural
macros, compiler wrappers, and build configuration. Building an untrusted PR is
code execution and requires the independent safeguards described in
[safety and permissions](safety-and-permissions.md).

## Validation requirements

- The identity ordering produces byte-for-byte existing presentation.
- Every original hunk appears once in every valid ordering profile.
- Every profile preserves same-file hunk order.
- Applying all groups yields the exact original head file images.
- Unique hunk anchors and GitHub comment coordinates never change.
- Cross-file groups preserve transaction attribution and safe undo.
- Existing review bundles and recovery snapshots still load.
- A failed planner falls back deterministically without losing progress.
- Unauthorized builds cannot run.
- Checkpoint results never claim success after cancellation or stale source.

The staged migration is defined in the
[implementation roadmap](implementation-roadmap.md).
