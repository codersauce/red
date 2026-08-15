# Inline-comment UI prototype

This branch is a presentation experiment. It does not call a model or change
the existing inline-assist tool contract.

Run `cargo run --bin red -- path/to/file` in this worktree.

| Key | Action |
| --- | --- |
| Normal-mode `Space C` | Add a random sample comment above the current line. Repeat to replace it with a different sample. |
| Visual or Visual Line `Space C` | Add a sample for the selected lines, return to Normal mode, and show the start of the range. |
| `Space X` | Clear all sample comments in the current buffer. |

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
Clicking a comment moves to its source line. Splits show the same buffer's
comments at their own widths.

The comments are in memory only. Edits above them move both range anchors;
replacing an endpoint removes its comment. A new sample replaces overlapping
ranges so the single lane remains unambiguous. Replacing a sample from its first
line preserves its range. Persistence, threaded replies, full comment history,
and model-generated comments are intentionally not implemented yet.

To preview a range, press `V`, extend the selection with `j`/`k`, then press
`Space C`. The status message identifies the inclusive line range. A future
model tool can use the same representation: `start_line`, optional `end_line`
(defaulting to the start), and `message`.

`Theme::mode()` exposes the editor's dark/light appearance, using the same
perceived-luminance test as the plugin color helper. The terminal emulator's
actual background is not queried yet: that should be retained per attached
terminal client and passed to a detached core, rather than persisted in a theme.
