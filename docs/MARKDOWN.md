# Markdown

Red reads Markdown the way it looks once rendered, and edits it as the source it
is. In Normal mode, tables are drawn as grids, link and emphasis markup is
hidden, and images appear in the buffer. Move onto a line to edit it, and that
line shows its raw Markdown again. None of this changes the file or adds undo
entries.

![A Markdown file in Red with hidden link markup, an inline chart, and a rendered table](images/markdown.jpg)

Try it with the sample that ships in the repository:

```shell
red examples/markdown-images/sample.md
```

## Tables

Pipe tables show as aligned grids with wrapped cells, column alignment, and
inline formatting. Enter Insert or Visual mode in a table to reveal its source;
Escape restores the grid.

`:markdown-tables` (also in the command palette) toggles grids in the current
split. Set `markdown_tables = false` to start in source. Fenced code, nested
tables, malformed rows with extra cells, and documents larger than 2 MiB stay
in source form.

## Hidden markup

With wrapping on, link destinations and brackets, emphasis markers, image
markup, and code-span backticks take no space on screen. A paragraph full of
links shows only their labels and wraps on the words you can see, and `gj` and
`gk` move over those visible rows.

The text is still the source underneath. Insert, Visual, search, and pending
operators show the cursor line raw, and moving the cursor into hidden syntax
reveals that line, so the character under the cursor is always the one an
edit changes. Code fences, escapes, and link reference definitions are never
hidden.

`:markdown-conceal` toggles this in the current split. Set
`markdown_conceal = false` to keep all markup visible.

Bold, emphasis, inline code, and links are highlighted either way, including
nested formatting, reference links, and URL or email autolinks.

## Alerts

A quote that opens with a GitHub alert marker shows a colored label instead of
that marker:

```markdown
> [!NOTE]
> The buffer underneath is still the source.
```

`> [!NOTE]` draws as `ℹ Note`. Tip, Important, Warning, and Caution use their
own label and color, and the `>` at the start of the quote uses that color
too. The kind is case-insensitive, and it has to be the whole first line of
the quote. `> [!NOPE]`, a marker with trailing words, and a quote nested
inside another quote keep the ordinary quote style unless that nested quote
has an alert of its own.

Move onto the marker line, or enter Insert, Visual, or search there, and the
raw `> [!NOTE]` comes back. Other lines of the quote stay as they were.

Colors come from the theme: Note uses `editorInfo.foreground`, Tip
`terminal.ansiGreen`, Important `terminal.ansiMagenta`, Warning
`editorWarning.foreground`, and Caution `editorError.foreground`. A missing
color falls back to `markup.quote`. Alerts follow `markdown_conceal` and have
no separate switch.

## Inline HTML

Balanced inline tags on one line hide the tags and style the text between
them. The buffer line stays one row, so `<br>` disappears and does not insert
a break.

| Tags | What you see |
| --- | --- |
| `<b>`, `<strong>` | bold |
| `<i>`, `<em>` | italic |
| `<u>`, `<ins>` | underline |
| `<s>`, `<del>`, `<strike>` | the letters with a combining strike |
| `<mark>` | `editor.findMatchHighlightBackground` |
| `<code>` | inline code style (`markup.inline.raw`) |
| `<kbd>` | `badge.foreground` and `badge.background`, or an underline when those colors are missing |
| `<sub>`, `<sup>` | Unicode subscripts and superscripts where a mapping exists |
| `<br>` | nothing |

`<b><i>both</i></b>` nests. An unknown tag, an unbalanced tag, a tag inside a
code span or fence, and a tag inside an HTML block stay on screen. `<code>`
and `<kbd>` show their contents as written, including any tags inside them.
Move onto a hidden tag, or enter Insert, Visual, or search on that line, and
the tags come back. This follows `markdown_conceal` and has no separate switch.

## Highlight marks

`==highlight==` hides the markers and paints the text with the same background
as `<mark>`. The text cannot be empty or start or end with a space, and an
unclosed `==` stays as written. Marks inside code spans, fences, and HTML
blocks stay as written. This is not CommonMark. Set
`markdown_extended_marks = false` or run `:markdown-extended-marks` to leave
every `==` visible. It follows `markdown_conceal`.

## HTML

Raw HTML is highlighted with the bundled HTML grammar. Block tags such as
`<details>` and `<div>`, and inline tags such as `<kbd>`, use the same tag,
attribute, and string scopes as an `.html` file. A fenced `html` block is
highlighted the same way, so Markdown does not need an HTML language pack.
HTML written inside another language's fence stays that language.

## Outline

`:MarkdownOutline` lists the headings in the current buffer. In a Markdown
buffer, `Space o` opens the same list. It is not bound in other languages, so
the leader key stays free there. Type to filter the titles. The preview shows
the heading in place, including unsaved edits. Enter jumps to that line, and
`Ctrl-o` comes back.

Headings inside lists and quotes are included. Headings inside fenced or
indented code are not. Duplicate headings use the same `-1`, `-2` suffixes as
`#heading` links. The list is read from the buffer and does not change it.

With Code explore open (`Space e e`), a Markdown file shows this outline in
the sidebar instead of the code index. The section under the cursor stays
highlighted as you move, and clicking a heading jumps there.

## Code blocks

`:yank-code-block` copies the fenced block around the cursor. The copy is the
lines between the fences, with a trailing newline when those lines exist. An
empty fence copies an empty string. The opening fence, its language tag, and
the closing fence stay out of the copy. The buffer is left as it was, and the
copy is not an undo entry.

The text goes to the default register. With clipboard sync on yank enabled, it
also goes to the system clipboard. The cursor can sit on the opening line, a
content line, or the closing line. A fence with no closing line runs through
the end of the buffer. A fence starts with at least three backticks or tildes
and at most three spaces of indent. `:ycb` is the short form. Bind the action
in a keymap as `yank-code-block`.

## Images

A line that holds only an image becomes the picture itself in Normal mode:

```markdown
![Token bucket refill over eight seconds](bucket.png)
```

Local paths are relative to the Markdown file, and `https` images are
downloaded; either way an image can be up to 8 MB. Height follows the image's
proportions, up to `markdown_image_max_height` rows (16 by default).

A line that is only an `<img>` tag is drawn the same way. It may be wrapped in
one `<p>`. `src` and `alt` pick the file and the placeholder label. `width`
and `height` are CSS pixels or a percent: `width="200"` is twenty columns when
a cell is 10 pixels wide, and `width="50%"` is half the window. A missing side
follows the picture's proportions. `data:` sources stay as the tag.

Move onto the image line and its Markdown appears in the block's first row,
with the picture still below it, so nothing under the image moves. `j` and `k`
step onto that line and then past the picture. Counts, half-page motion,
`gg`/`G`, and the mouse wheel never stop inside an image. An image scrolled
partly off screen is cropped, not squashed, and a popup drawn over an image
covers it cleanly.

### Terminal support

`markdown_images = "auto"` asks the terminal what it can draw and uses the best
answer:

| Terminal | What you see |
| --- | --- |
| Kitty, Ghostty, WezTerm, Konsole | Full-resolution images (Kitty graphics) |
| iTerm2, VS Code | Full-resolution images (iTerm2 protocol) |
| foot, mlterm, Windows Terminal, other Sixel terminals | Full-resolution images (Sixel) |
| Other truecolor terminals | A smoothed picture drawn with block characters |
| Anything else | `[image: alt (path)]` |

Inside Herdr or Zellij 0.45+, images reach a capable outer terminal directly.
In tmux they need passthrough (`set -g allow-passthrough on`); without it Red
uses block characters. GNU screen always gets block characters.

To choose for yourself, set `markdown_images` to `"kitty"`, `"iterm2"`,
`"sixel"`, `"halfblocks"`, or `"off"` (the placeholder only).

## Tasks

List markers and task checkboxes keep their source characters and indentation.
In Normal mode, press `gT` anywhere on a task's first line, or choose **Toggle
task** in the command palette, to switch between `[ ]` and `[x]`. Nested tasks
and tasks in block quotes work; code examples are left alone. Each toggle keeps
the cursor in place and is one undo step. Bind `ToggleMarkdownTask` to use
another key.

## Links

Press `gx` on a link's label or destination, or choose **Follow link** in the
command palette:

- Web links open in your browser and email links in your mail app.
- Local files open in Red relative to the Markdown file, reusing open buffers
  and their unsaved edits.
- `#heading` and `guide.md#heading` jump to headings; duplicate headings use
  `-1`, `-2`, and so on.
- `src/main.rs:42:3` and `src/main.rs#L42` jump to a line and column.

`Ctrl-o` and `Ctrl-i` go back and forward. Reference links, autolinks, and
links inside rendered tables work the same way. A missing target leaves you
where you are.

### Reading with a split

Press `Ctrl-w x`, or choose **Open link in split**, to keep the document visible
while a local link opens in a split to its right. Later `gx` and `Ctrl-w x`
from that document reuse the same split, and a `[links → N]` marker in the
file name shows which pane it is. The split has its own `Ctrl-o`/`Ctrl-i`
history.

Focus stays in the document. Set `markdown_link_split_focus = "target"` and
restart to move focus to the opened file. **Clear link destination** in the
command palette lets go of the split without closing it, and closing either
pane or switching the document to another file does the same.

## Settings

| Setting | Default | Effect |
| --- | --- | --- |
| `markdown_tables` | `true` | Draw pipe tables as grids |
| `markdown_conceal` | `true` | Hide inline markup while wrapping, including GitHub alert markers and balanced inline HTML tags |
| `markdown_extended_marks` | `true` | Hide `==highlight==` markers and paint the text like `<mark>` |
| `markdown_images` | `"auto"` | How to draw images: `auto`, `kitty`, `iterm2`, `sixel`, `halfblocks`, `off` |
| `markdown_image_max_height` | `16` | Most rows one image may use |
| `markdown_link_split_focus` | `"source"` | Pane that keeps focus after `Ctrl-w x` |

Tables, images, links, and tasks apply to Markdown documents up to 2 MiB.
