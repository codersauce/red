# PR Replay Coach: UI checkpoint

PR Replay helps reviewers understand a pull request by reconstructing the
original author's changes, step by step, in a separate scratch workspace. This
checkpoint contains a real, editor-owned reconstruction workflow using an
in-memory mock pull request. Each step has a complete original unified diff,
the coach is a dedicated read-only panel, and the scratch source is the only
editable editor buffer.

## Run the mock

From the dedicated worktree:

```sh
cd ~/code/red.fcoury-pr-replay
env CARGO_TARGET_DIR=/private/tmp/red-pr-replay-target cargo run -p red
```

Open the command palette and run `Replay`, or enter `:Replay`. In normal mode,
press `Space R g` to open the same panel.

Replay initially places its dedicated coach panel on the left and the editable
Rust scratch source on the right, matching the pull-request replay mockup. The
coach is rendered by Red's panel system; it is not a Markdown file, a scratch
document, or an editable editor split. It contains a five-step mock PR, its
original author and branch, the actual unified diff for each individual step,
the reconstruction task, optional hints, and progress.

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
source.

To apply a step automatically, press `a` while the guide is focused, or use
`Space R a` from either surface. Rust checks the original step, scratch-buffer
revision, complete pre-image, and transaction boundary before immediately
applying the exact original hunk as one editor transaction. Focus stays where
the action started; no confirmation interrupts the reconstruction.

Press `u` in the focused guide or `Space R u` to undo the latest transaction
only when it belongs to the current Replay session. If newer manual edits are
present, Replay refuses to skip over them; return to the source and use normal
Vim `u` first. Undoing or subsequently editing a completed current step
automatically removes its completion mark without disturbing earlier
reconstructed steps.

To apply it manually, use `Space R i`, edit the visible Rust buffer to match the
original diff, return to normal mode, and press `Space R v`. A step is marked
complete only when the actual source matches its original post-image.

For example, on the first exercise focus the source with `i`, jump to the
diagnostic parameter with `:10` and `Enter`, press `o`, type
`visible_start: usize,`, press `Enter`, type `visible_end: usize,`, and press
`Esc`. Then press `Space R v`. The guide displays `✓ 01` and `1 / 5 reviewed`.

Observations are local demo state and are never posted as GitHub comments or
reviews. The editable source has a display name but no associated file path or
URI; the dedicated coach is not a buffer at all. Opening the mock, applying a
hunk, moving the pane, and hiding or reopening the coach never write a file,
launch source-file LSP, create a branch, fetch a pull request, or contact
GitHub. Restarting the editor clears the mock session. Live GitHub source
resolution, durable scratch worktrees, and recoverable observations remain the
next checkpoint after this UI has been reviewed.
