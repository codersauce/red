# PR Replay Coach

PR Replay helps reviewers understand a pull request by reconstructing the
original author's changes, step by step, in a separate scratch workspace.
Choose a real GitHub pull request, a local feature branch against its actual
merge base, or a safe in-memory demonstration. Every step has its own complete
original unified diff. The coach is a dedicated read-only panel, and
reconstruction happens only in editable scratch-source buffers.

## Start Replay

Start Red from a checkout of the repository you want to review:

```sh
cargo run -p red
```

Open the command palette and run `Replay`, enter `:Replay`, or press `Space R g`.
Replay first discovers safely recoverable reviews across editor sessions:

- If no review exists, the source picker opens immediately.
- If exactly one review exists, its original guide and scratch source reopen
  directly.
- If multiple reviews exist, a review picker shows their pull request or branch,
  repository, actual completion, private-note count, and unsaved or active
  state. Choose a review or select **Start a new review**.

The source picker offers:

- **GitHub pull request:** enter its PR number, such as `145`, or its canonical
  URL. Red verifies the PR belongs to the current repository and pins the
  original author head and merge base. If immutable source objects are missing,
  it requests permission before fetching only Replay-owned Git refs.
- **Local branch:** enter a feature branch or use `HEAD`, then enter an explicit
  base such as `origin/master`. Leave the base blank to detect the local
  `origin/HEAD`, `origin/main`, `origin/master`, `main`, or `master`. Red uses
  the actual merge base, not the current default-branch tip.
- **Safe in-memory demo:** inspect the original five-step mock without Git,
  network access, file writes, or worktree creation.

GitHub metadata, local Git resolution, explicitly confirmed fetches,
scratch-worktree creation, private review-file operations, and human-confirmed
review submission run in bounded background workers. Normal editor input
remains responsive while the original review source is loading. Accepting a
real source immediately opens the dedicated Replay panel, displays the selected
PR or branch and exact scratch path, and shows an animated checkout status until
the original review is ready. If checkout fails, its explanation remains
visible in that same panel.

Replay disables Git's filesystem monitor only for its own Git commands. This
prevents an unavailable repository monitor from stalling scratch checkout
without changing the repository's Git configuration.

Use `:ReplayPR` or `:ReplayBranch` to go directly to the corresponding source
input, or `:ReplayDemo` to bypass the picker and open the no-side-effect mock.

For real sources, Red displays the exact proposed sibling worktree and scratch
branch. Nothing is created until the reviewer accepts that specific
confirmation. Original branches are never checked out, modified, reset,
committed, or pushed.

Returning to the same pull request safely resumes its existing scratch worktree
only when its exact path, shared repository, local branch, original merge-base
commit, and clean working tree are independently verified. The review picker
also recognizes GitHub scratch worktrees created before source-linked recovery
metadata existed. Reopening an existing review never creates a replacement
branch, overwrites saved reviewer changes, or adopts an unrelated directory.

If a previous checkout was interrupted after creating the Replay branch, Red
can also restore its missing scratch worktree. It reuses the branch only when
the branch still points to the exact verified merge base; an unrelated or
modified branch is never reset or overwritten.

Replay initially places its dedicated coach panel on the left and the editable
scratch source on the right, matching the pull-request replay mockup. The
coach is rendered by Red's panel system; it is not a Markdown file, a scratch
document, or an editable editor split. It contains the verified source, original
author and branch, the actual unified diff for each individual step, the
reconstruction task, optional hints, and progress.

The panel also shows the original pull-request title. Real learning-step titles
and reconstruction tasks identify the changed source symbol rather than exposing
Git's raw hunk heading. Explanations prefer the author's documentation on the
exact changed source, then the matching author-written pull-request change, and
finally the pull request's actual motivation. Markdown headings are never
presented as explanations. This guidance is derived from the pinned original
source and review context; it does not invent or attribute undocumented intent
to the author.

For a multi-file review, `h` and `l` select the previous and next original hunk,
while `[` and `]` jump directly to the first hunk in the previous or next
changed file. Both motions switch the existing editor window to the actual
scratch file. The changes heading keeps the current file and total file count
visible without introducing a second tree or another pane.
The source cursor and viewport jump directly to the original hunk, even when
the change occurs thousands of lines into a large file.
Changes from different files stay in their own buffers; no unrelated file tree,
editable guide, or extra pane is created.

The coach is a structured editor surface, not a rendered Markdown document. PR
context and the current reconstruction task stay at the top. The exact original
hunk occupies only the space its source needs. One blank row separates it from
the change list; longer hunks scroll without hiding the current change. A compact
action bar stays pinned at the bottom. Source retains its original line numbers,
language-aware Tree-sitter highlighting, and the active theme's addition,
removal, and modification colors. Git transport headers are hidden from the
visual presentation without changing the complete patch used for validation and
application. Source lines are clipped rather than wrapped, preserving
indentation; a visible `›` marks code that extends beyond a narrow pane.

Focus the coach and use the existing Vim edge-movement commands to move the
panel itself:

- `Ctrl-w H`: Dock the focused panel on the left.
- `Ctrl-w J`: Dock the focused panel at the bottom.
- `Ctrl-w K`: Dock the focused panel at the top.
- `Ctrl-w L`: Dock the focused panel on the right.

Lowercase `Ctrl-w h/j/k/l` continues to move focus without changing split
topology. `Ctrl-w w` cycles between the coach and source. A focused guide shows
`▌ PR REPLAY`, a theme-accented `┃` or `━` at its docking edge, and a `▶`
beside the current step. The real terminal cursor rests on that marker and the
status line reads `REPLAY`. Focusing the source restores the normal editor
status line; the guide, its original diff, and syntax highlighting remain
visible.

The guide also inherits Red's generic pane resizing. Use `Ctrl-w >` and
`Ctrl-w <` to grow or shrink a left or right Replay pane; use `Ctrl-w +` and
`Ctrl-w -` when the pane is docked above or below the source. Prefix either
binding with a count, such as `5 Ctrl-w >`, for a larger adjustment.
`Ctrl-w =` restores the pane's original size. You can also drag its dividing
line with the mouse; the captured divider brightens immediately and returns to
its normal focus appearance when released.

Real review progress is included in Red's crash-safe editor session. Restart
Red with `--resume` to reopen the verified scratch buffers, original source
guide, selected change, completed hunks, private observations, learning mode,
and attributed Replay undo history. Resume does not create or overwrite a
worktree, save the scratch files, or reuse an automatic-application token. If
the pinned source, hunk, worktree, or undo attribution cannot be verified, Red
recovers the normal editor buffers and refuses only the unsafe Replay session.

## Review role and local outbox

For a GitHub pull request, the guide verifies the authenticated GitHub viewer
against the original pull-request author, repository, head branch, and exact
head commit. The header then shows one honest role:

- `AUTHOR`: you are the verified original PR author. You can draft inline
  comments, PR-level summaries, and proposed fixes to your own PR.
- `REVIEW`: you are reviewing another user's PR, or ownership could not be
  verified. You can draft inline comments and PR-level summaries; proposing a
  code change to someone else's PR is refused.

Write access to a shared repository is not proof that you own a PR. The guide
also shows the original head branch and seven-character commit prefix. Replay
uses the complete immutable commit internally; the short prefix is only for
display. Reopening a review saved before role detection refreshes only the
authenticated GitHub identity in a bounded background worker; it refuses a
moved PR head and never creates another scratch worktree.

Press `c` in the focused guide to compose a multiline inline comment about the
current original change, or `s` to compose a PR-level summary. Authors can also
press `F` to record a proposed fix. In the composer, `Enter` inserts a new line
and `Ctrl-Enter` saves the complete draft locally. The selected original diff
determines each
inline comment's path, head commit, exact changed-line range, and GitHub `LEFT`
or `RIGHT` side. Scratch-buffer cursor positions and later edits never replace
those coordinates. A proposed fix is only text in this first milestone: it does
not edit either the original PR or its learning scratch source.

Press `r` to open the review outbox. It shows the verified role, original
branch and commit, source-linked comments, author fix proposals, PR summaries,
and whether each outcome is `LOCAL` or already `POSTED`. Until you explicitly
approve a GitHub submission, its status reads `nothing sent to GitHub`. Use `h`
and `l` to select drafts, `e` to edit a local draft, and `d` to discard one
after a local confirmation. Posted review comments are read-only and cannot
be silently discarded or submitted twice. Press `r` again to return to the
original guide.

Every local draft and verified GitHub receipt is part of the crash-safe editor
session and survives `--resume`. The outbox is the same structured, dedicated
Replay pane as the source guide; it preserves the focused `▌ PR REPLAY` title,
highlighted divider, `REPLAY` status, real selected draft cursor, scrollable
content, and pinned review action bar. At the default 46-column width, the
relevant `P` publish, `S` save, and `r` return actions remain visible.

### Save or move a private review

Use `S` in the focused outbox or `Space R S` to save comments, PR-level
summaries, local observations, author fix proposals, and verified submission
receipts into a private portable review file. Red suggests a path inside the
repository's shared `.git/red/replay-reviews/` metadata, so saving never dirties
the scratch worktree or original repository. You can choose a different private
location explicitly.

Use `L` or `Space R L` to load a review on the same or another computer. Red
first checks the exact original host, repository, PR, base, head commit, and
complete diff. It previews the new drafts, observations, and receipts, requires
confirmation before merging, and refuses conflicting text or a file that changes
after the preview. Existing version-one private review files remain readable.
Neither saving nor loading sends anything to GitHub.

### Publish an explicitly approved GitHub review

For a verified GitHub pull request, add at least one local inline comment or
PR-level summary. Press `P` in the outbox or `Space R P` to choose:

- **Comment only:** submit feedback without changing the PR's approval state.
- **Approve:** approve another author's exact original PR head.
- **Request changes:** request changes to another author's PR. Add a PR-level
  summary with `s` so the author receives a clear explanation.

The original PR author can choose **Comment only**; self-approval and requesting
changes on your own PR are not offered. Author `F` fix proposals always stay
local and are never inserted into a review comment. Local branch reviews and
the demo cannot publish to GitHub.

Selecting an outcome does not post anything. Red first shows a
`NOTHING POSTED YET` confirmation containing the exact original repository,
PR number, full pinned head, authenticated GitHub viewer, selected outcome,
PR-level body, and every included human- or agent-proposed inline comment. It
also identifies any original-PR fix proposals that will stay local. Cancel
preserves all drafts. Only explicitly accepting this confirmation starts the
background GitHub request.

Immediately before publication, Red verifies the original PR head and viewer
again. It sends all selected comments and the chosen outcome in one atomic,
event-bearing GitHub review request; it never creates a remote `PENDING`
review. If the head, reviewer, original diff anchor, or previewed draft changes,
submission fails without claiming success. On a verified response, Red marks
the exact submitted drafts `POSTED` and retains their portable GitHub review
receipt. Private notes, the scratch worktree, and the original PR branch are
never modified by review publication.

If a network or provider failure happens after the review request may have
reached GitHub, Red explicitly reports that the review might already be posted.
It never claims that nothing happened or silently retries. A second confirmation
requires you to inspect the original PR before choosing whether to preview
another submission.

## Key bindings

All replay bindings use `Space R`; the existing `Space r` rename binding is
unchanged.

| Keys | Action |
| --- | --- |
| `Space R ?` | Show or hide the compact Replay keyboard help. |
| `Space R g` | Reopen the current review or choose among existing reviews. |
| `Space R [` | Jump to the first change in the previous file. |
| `Space R ]` | Jump to the first change in the next file. |
| `Space R n` | Next reconstruction step. |
| `Space R p` | Previous reconstruction step. |
| `Space R h` | Reveal or hide the current hint. |
| `Space R m` | Switch between Challenge and Snippet mode. |
| `Space R i` | Focus the editable scratch source for manual reconstruction. |
| `Space R v` | Validate the real scratch source against the original hunk. |
| `Space R a` | Immediately apply one exact, undoable original hunk. |
| `Space R u` | Safely undo the most recent Replay-authored scratch hunk. |
| `Space R o` | Add a private, recoverable source-linked observation. |
| `Space R f` | Show local reviewer observations. |
| `Space R c` | Draft an exact original-source inline review comment. |
| `Space R F` | Draft a proposed fix for your verified original PR. |
| `Space R r` | Show the local review outbox or return to the guide. |
| `Space R s` | Draft a pull-request-level review summary. |
| `Space R e` | Edit the selected local review draft. |
| `Space R d` | Discard the selected local review draft after confirmation. |
| `Space R P` | Preview and explicitly confirm publishing a GitHub PR review. |
| `Space R S` | Save a private portable review, observations, and submission receipts. |
| `Space R L` | Preview and load a source-verified portable private review. |
| `Space R q` | Hide the coach without touching the scratch source or progress. |

While the dedicated coach is focused, `j` and `k` scroll the current source
hunk, `h` and `l` select the previous and next reconstruction steps, and `[` and
`]` jump between changed files. Outside the focused coach, `[` and `]` retain
their existing editor and Git-hunk motions. The older `p` and `n` step bindings
remain compatibility aliases. Use `Space R h`
for a hint so horizontal navigation never unexpectedly changes the exercise
instead. `i`, `a`, `u`, `m`, `v`, `o`, `f`, `c`, `F`, `r`, `s`, `e`, `d`, `P`,
`S`, `L`, `q`, and `?` act directly on the Replay pane. The pinned action bar keeps
scratch-source focus, manual
validation, immediate application, safe undo, `h/l` step navigation, and help
visible even when the panel is narrow. The title shows the selected step
separately from the number of genuinely reviewed changes. A `✓` identifies a
manually reconstructed step; `⊕` identifies an automatic
application.

Every step always displays its exact, independently parseable unified diff.
Challenge mode emphasizes manually reconstructing the hunk in the real source
buffer. Snippet mode additionally reveals the complete resulting original-author
source for that hunk. Full scratch-file images remain editor-owned instead of
being copied into every guide step.

To apply a step automatically, press `a` while the guide is focused, or use
`Space R a` from either surface. Rust checks the original step, scratch-buffer
revision, authenticated hunk pre-image, and transaction boundary before
immediately applying only that original hunk as one editor transaction.
Unrelated source text is never replaced. Focus stays where the action started;
no confirmation interrupts the reconstruction.

Press `u` in the focused guide or `Space R u` to undo the latest transaction
only when it belongs to the current Replay session. If a different file is
selected, Replay returns to the exact file and step where that hunk was applied.
If newer manual edits are
present, Replay refuses to skip over them; return to the source and use normal
Vim `u` first. Undoing or subsequently editing a completed current step
automatically removes its completion mark without disturbing earlier
reconstructed steps. Replay explicitly confirms when undo restores the original
scratch source; it reserves the revalidation warning for subsequent manual edits.

To apply it manually, use `Space R i`, edit the visible Rust buffer to match the
original diff, return to normal mode, and press `Space R v`. A step is marked
complete only when the actual source matches its original post-image.

For example, on the first exercise focus the source with `i`, jump to the
diagnostic parameter with `:10` and `Enter`, press `o`, type
`visible_start: usize,`, press `Enter`, type `visible_end: usize,`, and press
`Esc`. Then press `Space R v`. The guide displays `✓ 01` and `1 / 5 reviewed`.

Real-source observations stay local, survive `--resume`, and are never posted
as GitHub comments or reviews.
The demo source has a display name but no associated file path or URI; opening
or using the demo never fetches, creates a branch, writes a file, or contacts
GitHub. Real Replay buffers refer only to files inside the explicitly confirmed
scratch worktree. Applying a hunk modifies an in-memory editor buffer; it does
not save the file, stage changes, commit, push, or submit a review.
