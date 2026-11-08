# Git range diff

`:GitDiff <range> [-- path]` opens the existing Git workspace on a commit range.
The file list stays the rows. The selected file's unified diff stays the detail
document. Git still runs only through the plugin process boundary (`SpawnProcess`
of `git`, never a shell). The host does not invoke Git.

## Range arguments

The command accepts the same range forms as leaf:

| Invocation | Git diff |
|---|---|
| `cached` or `staged` | `git diff --cached` |
| `working` | `git diff` |
| `HEAD~2`, a branch, or `main..feature` | `git diff <rev>` |
| `main feature` (two refs) | `git diff main feature` |
| any of the above, then `-- path` | the same command limited to those paths |

`--` is required before paths. A second token is a revision only when
`git rev-parse --verify --quiet <token>^{commit}` succeeds. That check is also a
plugin-owned Git process. `git_core` builds the argv. The plugin does not
interpolate a shell string.

`q` leaves the range and returns to the working-tree dashboard, the same way a
commit inspection does.

## Paired-column rendering

The detail document stays a unified list of lines (`added`, `removed`,
`context`, `hunk`, `meta`). The host aligns that list into paired rows when the
workspace asks for it. Plugins do not send a second document.

`WorkspaceModel.paired_columns` (default false) opts the open workspace into the
layout. The Git range view sets it. Other workspaces, including the working-tree
dashboard, keep `v` as visual line selection.

While the flag is set and the detail pane is focused, `v` toggles
`DetailLayout::{Unified, Paired}` inside the host. The toggle is a core detail
interaction, like wrap, so it does not round-trip through the plugin. Later
events still report `detail_layout` (`unified` or `paired`) and `detail_side`
(`before` or `after`).

Each paired row is one screen row (wrapped rows use the taller cell and pad the
shorter one). The pane splits in half with a one-column rule. The left cell is
the old side and shows the old line number. The right cell is the new side.
Hunk headers and patch metadata span both columns. A pane narrower than 16
columns renders unified even when the layout is paired, because two gutters
would leave no code.

`h` and `l` move the focused column. The focused cell is the cursor. `j` and
`k` move by paired row, and the cursor line reported to the plugin is the
focused cell's unified line. A blank pad has no line, so the cursor snaps to
the cell that exists.

## Hunk alignment

Within one hunk, pairing follows the unified order:

- A context line is the same text on both sides.
- A run of removed lines followed by a run of added lines is zipped by index.
  The longer run continues alone. The short side is a blank pad, not a copy of
  the other line.
- Added lines that were not preceded by removals stay on the right only.
  Removals with no following additions stay on the left only.
- A `\ No newline at end of file` marker stays on the side of the line it
  follows. After context it is shown on both.
- Lines before the first hunk, and context notes with no line numbers, span the
  width.

Alignment is by hunk position, not by a similarity search. Word highlights
still come from the unified document and paint only the cell whose line index
owns them.

## Before and after preview

`p` in a range view previews the focused side. In the unified layout, a removed
line is the before side and any other line is the after side. In the paired
layout, the focused column decides.

The plugin reads the blob ids from the diff it already fetched. It loads a real
blob with `git show <oid>` through `SpawnProcess`. An all-zero oid is an empty
side. When the new side is the worktree and has no blob, `git_core`
reconstructs it by applying the hunks to the before text. That reconstruction
uses the patch the plugin already holds. It does not read the file from the
host.

`OpenReadonlySplit` (host API `0.23.0`) places that text in a read-only buffer
and a vertical split. The split reuses the follow-link destination: the same
key updates the same window, and `markdown_link_split_focus` chooses which pane
keeps focus. Opening the preview suspends the modal workspace so the split is
visible. Closing the preview window resumes the workspace. Insert, replace, and
save on the preview buffer are refused.

## Plugin API 0.23.0

`0.22.0` is still an unreleased host API. This lane adds the next minor,
`0.23.0`, because the range view needs a new request and new workspace fields.
`0.22.0` stays in the accepted set. The additions are backward compatible:
omitted fields default off, and events gain fields that older listeners ignore.

| Symbol | Kind |
|---|---|
| `WorkspaceModel.paired_columns` | field, default `false` |
| `WorkspaceEvent.detail_layout` | `unified` or `paired` |
| `WorkspaceEvent.detail_side` | `before` or `after` |
| `OpenReadonlySplit(callback, preview: Json)` | request |

`preview` carries `key`, `name`, `text`, `line` (zero-based), `syntax` (a path
used only to pick a highlighter), and `side`.
