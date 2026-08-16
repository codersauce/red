# Inline comments

Inline assist can now leave real annotations or combine annotations with a
bounded code edit. Select a range (or leave the cursor inside a function), press
`Space i`, and ask for a review or explanation without changing the code.
See [the inline-assist contract](AGENT_WORKFLOW.md#inline-assist).

Run `cargo run --bin red -- path/to/file` in this worktree.

| Key | Action |
| --- | --- |
| `Space i` | Ask inline assist to edit, explain, or review the enclosing function or exact selection. |
| Prompt Up/Down / click | Move through word-wrapped prompt rows / place the cursor. |
| Prompt Ctrl-P / Ctrl-N | Recall older/newer submitted prompts, preserving the unsent draft. |
| Result-popup `v` / `A` | Read the full answer / prepare a contextualized Agent draft. |
| `Space H` / `:InlineHistory` | Open unified history with running jobs, ready results, completed discussions, and saved drafts. |
| Working-popup `Esc` / `Ctrl-c` | Hide without cancelling / explicitly cancel the request. |
| Activity marker click / `Space v` | Reopen the selected inline job at its source. |
| Ready-popup `v` / Enter | Inspect an off-screen code edit / apply it if source is unchanged. Explanations appear automatically. |
| Wider-edit popup Enter / `v` / `d` | Review the proposed same-file diff / review / decline. `a` in the full review applies it. |
| History Enter / `g` / `p` | Reopen the item / jump to source / pin the selected turn's annotations. |
| Result-popup `p` | Pin retained annotations without reapplying a code edit. |
| `Space ] c` / `Space [ c` | Select the next/previous annotation, including overlapping comments. |
| `Space ] i` / `Space [ i` | Select the next/previous item in the current overlapping group. |
| Card `[<]` / `[>]` | Select the previous/next overlapping item with the mouse. |
| Full-view `[` / `]` | Browse the overlapping items while reading their full text. |
| `Space v` | Read the full comment in a scrollable plain-text popup. |
| `Space x` | Dismiss the selected comment. |
| Normal-mode `Space C` | Add a random sample comment above the current line. Repeat to replace it with a different sample. |
| Visual or Visual Line `Space C` | Add a sample for the selected lines, return to Normal mode, and show the start of the range. |
| `Space X` | Clear all comments in the current buffer. |

Comments use content-sized gray blocks with two columns of horizontal padding
and half-height top and bottom edges (`▄` and `▀`). ASCII-border mode falls back
to solid blank padding rows. A faint `╭───` joins the first text row of the box
to a dashed guide in a dedicated annotation lane after the line numbers.
A single line gets `╰─›`; a range gets
`├─›`, a vertical rail, and `╰──`. The lane reserves four columns for every
source row while the buffer has comments, so even column-zero text stays clear
of the connector. Narrow splits use a one-character connector and a space;
extremely small splits omit the lane. Existing signs and line numbers remain
independent. The remaining screen columns keep the editor background. The
block uses `red.inlineCommentBackground` or a dark/light gray fallback. Text
uses the theme's `red.inlineCommentForeground` color (falling back to the information/comment
foreground, adjusted for readability). Long comments wrap at word boundaries,
with a four-text-row preview limit. Tiny splits reduce padding to keep the source
line visible. Source line numbers, Vim motions, selections, file contents, and
dirty state stay unchanged.
Clicking an activity marker reopens its job; clicking ordinary comment text opens
its full view. Splits show the same buffer's
comments at their own widths.

Questions, answers, and comments are retained in editor-session history. Edits above them move both range anchors;
changes to their source mark them outdated. Overlapping comments remain stored
and collapse to one numbered box; navigation selects the visible annotation.
Refinement replaces only comments from the same inline-assist invocation.
The sample keys remain available for UI development. `:InlineHistory` provides earlier
turns, reviewed-source snapshots, continuation, and rechecking. Normal editor
recovery restores retained conversations; `persist_inline_history = false`
disables disk retention. Regular-assistant annotation tools are not exposed yet.

Inline assist can inspect project files and `HEAD` differences through bounded,
read-only tools. Unsaved buffers win over disk, and reads never move editor
focus. Successful reads are listed in the history conversation view. The write
boundary remains the original target unless the user explicitly approves a
validated wider same-file proposal. Explicit selections cannot be widened.
Enter opens the proposed diff; `a` applies it and `d` declines it. Applied edits
leave a retained change summary even when the assistant returns no comments.
Open it for the exact edit diff, `[`/`]` changed-location navigation, and safe
`u` undo. Closing the result is not another acceptance decision.

To preview a range, press `V`, extend the selection with `j`/`k`, then press
`Space C`. The status message identifies the inclusive line range. Model tools
use `start_line`, optional `end_line` (defaulting to the start), and `message`.
Their line numbers are relative to the supplied target or replacement, not
absolute document lines.

`Theme::mode()` exposes the editor's dark/light appearance, using the same
perceived-luminance test as the plugin color helper. The terminal emulator's
actual background is not queried yet: that should be retained per attached
terminal client and passed to a detached core, rather than persisted in a theme.
