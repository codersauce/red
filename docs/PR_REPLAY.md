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

GitHub metadata, local Git resolution, explicitly confirmed fetches, and
scratch-worktree creation run in bounded background workers. Normal editor
input remains responsive while the original review source is loading. Accepting
a real source immediately opens the dedicated Replay panel, displays the
selected PR or branch and exact scratch path, and shows an animated checkout
status until the original review is ready. If checkout fails, its explanation
remains visible in that same panel.

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
commit, and clean working tree are independently verified. Replay refuses to
overwrite saved reviewer changes or adopt an unrelated directory.

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

For a multi-file review, `h` and `l` select the previous and next original hunk
and switch the existing editor window to that hunk's actual scratch file.
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

## Key bindings

All replay bindings use `Space R`; the existing `Space r` rename binding is
unchanged.

| Keys | Action |
| --- | --- |
| `Space R ?` | Show or hide the compact Replay keyboard help. |
| `Space R g` | Open or return to the guide. |
| `Space R n` | Next reconstruction step. |
| `Space R p` | Previous reconstruction step. |
| `Space R h` | Reveal or hide the current hint. |
| `Space R m` | Switch between Challenge and Snippet mode. |
| `Space R i` | Focus the editable scratch source for manual reconstruction. |
| `Space R v` | Validate the real scratch source against the original hunk. |
| `Space R a` | Immediately apply one exact, undoable original hunk. |
| `Space R u` | Safely undo the most recent Replay-authored scratch hunk. |
| `Space R o` | Add a local, in-memory reviewer observation. |
| `Space R f` | Show local reviewer observations. |
| `Space R q` | Hide the coach without touching the scratch source or progress. |

While the dedicated coach is focused, `j` and `k` scroll the current source
hunk, and `h` and `l` select the previous and next reconstruction steps. The
older `p` and `n` step bindings remain compatibility aliases. Use `Space R h`
for a hint so horizontal navigation never unexpectedly changes the exercise
instead. `i`, `a`, `u`, `m`, `v`, `o`, `f`, `q`, and `?` act directly on
the Replay pane. The pinned action bar keeps scratch-source focus, manual
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

Observations stay local and are never posted as GitHub comments or reviews.
The demo source has a display name but no associated file path or URI; opening
or using the demo never fetches, creates a branch, writes a file, or contacts
GitHub. Real Replay buffers refer only to files inside the explicitly confirmed
scratch worktree. Applying a hunk modifies an in-memory editor buffer; it does
not save the file, stage changes, commit, push, or submit a review.
