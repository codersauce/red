# Inline-comment UI prototype

This branch is a presentation experiment. It does not call a model or change
the existing inline-assist tool contract.

Run `cargo run --bin red -- path/to/file` in this worktree. In Normal mode:

| Key | Action |
| --- | --- |
| `Space C` | Add a random sample comment above the current line. Repeat to replace it with a different sample. |
| `Space X` | Clear all sample comments in the current buffer. |

Comments use content-sized gray blocks with two columns of horizontal padding
and half-height top and bottom edges (`▄` and `▀`). ASCII-border mode falls back
to solid blank padding rows. A faint dashed guide sits in the otherwise
empty gutter. The remaining screen columns keep the editor background. The
block uses `red.inlineCommentBackground` or a dark/light gray fallback. Text
uses the theme's `red.inlineCommentForeground` color (falling back to the information/comment
foreground, adjusted for readability). Long comments wrap at word boundaries,
with a four-text-row preview limit. Tiny splits reduce padding to keep the source
line visible. Source line numbers, Vim motions, selections, file contents, and
dirty state stay unchanged.
Clicking a comment moves to its source line. Splits show the same buffer's
comments at their own widths.

The comments are in memory only. Edits above them move their anchors; replacing
an anchor removes its comment. Persistence, threaded replies, full comment
history, and model-generated comments are intentionally not implemented yet.

`Theme::mode()` exposes the editor's dark/light appearance, using the same
perceived-luminance test as the plugin color helper. The terminal emulator's
actual background is not queried yet: that should be retained per attached
terminal client and passed to a detached core, rather than persisted in a theme.
