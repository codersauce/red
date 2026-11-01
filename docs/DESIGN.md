# Red interface guidelines

These rules describe the interface Red already ships, so a new surface looks like
the editor people already use. They are drawn from the status line, command
line, pickers, dialogs, keymap hints, the action bar, completion, diagnostics,
hovers, and the bundled plugin panels.

## Layout

Persistent chrome stays at the edges. The status line and command line occupy
the last two rows. Splits fill the area above them. A popup, picker, or hover
is a bordered card, usually anchored near the cursor or the lower right, and
it covers only as much of the file as the content needs (`src/ui/dialog.rs`,
`src/ui/picker.rs`, `src/ui/keymap_hints.rs`, `src/ui/hover_info.rs`).

Side panels sit beside the text, not on top of it. Plugin trees
(`src/plugin/tree.rs`) and the explore sidebar start at the top of the panel
and stack downward. Content starts one cell inside the frame. Section headings
are short and stacked with a single blank row between groups.

Do not pin a footer to the bottom of a tall panel and leave a dead band above
it. The footer sits under the last line of content. Leftover rows are empty
panel background after the footer.

## Spacing

- One cell of padding inside a border.
- One blank row between sections.
- One cell between a key and its label, matching the action bar
  (`src/ui/action_bar.rs`).
- Three cells between paired hints on the same row. Keymap hints use a blank
  gap (`COLUMN_GAP` in `src/ui/keymap_hints.rs`). The action bar uses the same
  width as ` · `.
- Do not pad a short label out to the full width with extra words.

## Borders

Dialogs and pickers use a single-line frame: `┌ ┐ └ ┘ ─ │`. Rounded corners
exist on `BorderStyle::Rounded` and are used by welcome, what's new, signature
help, and the status-line layout. Pickers, keymap hints, and the agent
composer use `BorderStyle::Single`. When `window_borders_ascii` is set, use
`+ - |` instead of box drawing.

The frame uses `theme.ui_style.popup_border` for popups and
`theme.ui_style.dialog_border` for dialogs. Do not invent a third border
color. The title sits in the top border, in `popup_title` or `dialog_title`,
and is bold.

Plugin trees draw ancestry with `│`, `├`, and `└` in the indent-guide color.
A side panel uses the same single-line vertical rules for its frame.

## Color

Color comes from the active theme. `SurfacePalette` in `src/theme/surface.rs`
is the contrast-safe way to derive primary, muted, accent, and divider roles.
Selection text keeps at least `MINIMUM_SELECTION_TEXT_CONTRAST` (4.5) against
its background.

- Editor text and background: `theme.style`.
- Popups and pickers: `theme.ui_style.popup`, `picker_item`,
  `picker_selected_item`, `picker_prompt`.
- Quiet text: `theme.ui_style.muted`, or `descriptionForeground` /
  `editorLineNumber.foreground` through `SurfacePalette`.
- Side panels: `sideBar.background`, `sideBar.foreground`, `sideBar.border`,
  `sideBarTitle.foreground`, `sideBarSectionHeader.background`,
  `sideBarSectionHeader.foreground`.
- Lists and focus: `list.activeSelectionBackground`,
  `list.activeSelectionForeground`, `focusBorder`, `textLink.foreground`.
- Search and follow: `editor.findMatchBackground`,
  `editor.findMatchHighlightBackground`.
- Status and warnings: `statusBar.background`, `statusBar.foreground`,
  `editorWarning.foreground`.

Do not hard-code a palette for a feature. If a key is missing, fall back
through the role's existing chain, then `theme.ui_style`, then `theme.style`.
A one-off gold or navy that ignores the user's theme is a bug.

The selected row uses `picker_selected_item` or
`list.activeSelectionBackground` / `list.activeSelectionForeground`. Follow
highlights use the find-match styles. The selected row keeps enough contrast
to read the whole label.

## Glyphs

Nerd Font icons are the default (`PickerIconStyle::NerdFont`) for pickers,
diagnostics, files, and the status line. The status line picks one glyph per
section in `statusline_section_icon` (`src/editor/rendering.rs`): Nerd Font,
then a Unicode fallback, then a short ASCII word, and nothing when the icon
style is `None`. Plugin trees use Nerd Font file and folder glyphs
(``, ``, `` in `src/plugin/tree.rs`) and color them with an ordered list of
theme keys.

Icons are one column wide so rows stay aligned. Do not put an icon in front of
every word. A section heading does not need one.

Every new icon has an ASCII fallback, and a Unicode fallback where the status
line already has one. Choose the glyph from the user's icon style and from
`window_borders_ascii`.

## Key hints

Two existing surfaces already split the key from the action:

- Keymap hints (`src/ui/keymap_hints.rs`) put the key first, in
  `peekViewResult.lineForeground` or muted, and the action one cell later, in
  the link color (`markup.underline.link`, or
  `peekViewResult.fileForeground`). A group that continues ends with ` …`.
- The action bar (`src/ui/action_bar.rs`) draws the key bold in the secondary
  role and the label in muted. Pairs are separated by ` · `.

A hint that lives in a panel footer has no card around it, so color alone
reads as loose text. Draw it as a keycap: the key in brackets, bold, on a
chip. Prefer `sideBarSectionHeader.background` when that color differs from
the panel. When it does not, tint the chip toward
`list.activeSelectionBackground` so the key is still a separate object. The
action stays in muted on the panel background. Separate pairs with ` · `,
the same separator the action bar uses. Write the key as the user presses it
(`[\f]`, `[space E]`, `gd`), not as an internal action name. Truncate a label
with `…` rather than running it into the next key. Drop a pair that does not
fit before drawing a broken bracket.

## Words

Use the same plain voice as the command palette: "Find file", "Go to
definition". Plugin empty and loading lines do the same: "No themes found",
"No edits yet", "Loading sessions…".

- Say what the user sees. "Variable" rather than "binding". "Matches" rather
  than "references" when the match is only a shared spelling. "Language
  server" rather than "LSP" on a first-view label. `gd` can stay, because it
  is the key.
- Section headings are short title case ("Inside", "Matches"), in the same
  voice as picker titles. The mode pill (`NORMAL`, `INSERT`) is the exception
  that stays uppercase, because it names the editor mode.
- A mode pill on a reading surface says the effect: "Dim", "Review".
- Empty states say what is missing and what to do: "No name under the cursor."
  "No matches for this name."
- Loading and partial results name the scope: "6 changed files indexed." Do
  not leave a spinner with no noun.

## Truncation

Clip on a grapheme boundary with `truncate_display_width_with_marker` and a
`…` marker (`src/unicode_utils.rs`). Never cut a token in the middle and leave
the row looking finished. Paths keep the file name when the directory does
not fit. A count such as `×7` stays visible; shorten the title before the
count. Wrap a note on word boundaries before clipping a single word.

## Inconsistencies this pass does not pretend away

- Completion icons in `src/ui/icons.rs` are single-column Unicode (`λ`, `ƒ`).
  Pickers and the status line are Nerd Font glyphs with ASCII fallbacks.
- Plugin tree icons are Nerd Font only. They do not consult
  `PickerIconStyle`, so an ASCII icon setting still draws ``.
- Some cards use a single border and some use a rounded one.
- The action bar truncates with `truncate_display_width` and no ellipsis.
  New surfaces use `…`.
- A few command-line messages still say "LSP" where a sidebar should say
  "language server".
