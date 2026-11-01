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
(`src/plugin/tree.rs`, `src/plugin/panel.rs`) fill their column with
`sideBar.background` and stack rows from the top. A framed panel uses
`Dialog`: the title sits in the top border, the way `popup_title` does, and
the body is inset by the frame. Do not draw a private rail, title bar, or
palette when `Dialog` or the plugin panel styles already cover it.

Section headings are short and stacked with a single blank row between groups.
Empty rows after the content stay panel background.

## Spacing

- One cell of padding inside a border.
- One blank row between sections.
- One cell between a key and its label. Which-key uses `KEY_GAP` in
  `src/ui/keymap_hints.rs`. The action bar uses the same one-cell gap
  (`src/ui/action_bar.rs`).
- Which-key columns are separated by three blank cells (`COLUMN_GAP`).
- The action bar separates a key/label pair from the next pair with the
  three-cell string ` · ` (space, middot, space). That is a separator, not a
  blank column gap.
- Do not pad a short label out to the full width with extra words.

## Borders

Dialogs and pickers use a single-line frame: `┌ ┐ └ ┘ ─ │`. Rounded corners
exist on `BorderStyle::Rounded` and are used by welcome, what's new, signature
help, and the status-line layout. Pickers, keymap hints, and the agent
composer use `BorderStyle::Single`. When `window_borders_ascii` is set, window
splits use `+ - |`. Dialogs and which-key keep the box-drawing frame.

The frame uses `theme.ui_style.popup_border` for popups and
`theme.ui_style.dialog_border` for dialogs. A side panel uses
`sideBar.border` for the same `Dialog` frame. The title is bold, in the top
border, using `popup_title` or `dialog_title` on a popup and
`sideBarTitle.foreground` on a side panel.

Plugin trees draw ancestry with `│`, `├`, and `└` in the indent-guide color.

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
  `list.activeSelectionForeground`, `focusBorder`.
- Links and which-key labels: `textLink.foreground`,
  `markup.underline.link`, `peekViewResult.fileForeground`.
- Search and follow: `editor.findMatchBackground`,
  `editor.findMatchHighlightBackground`.
- Status and warnings: `statusBar.background`, `statusBar.foreground`,
  `statusline_style`, `editorWarning.foreground`.

Do not hard-code a palette for a feature. If a key is missing, fall back
through the role's existing chain, then `theme.ui_style`, then `theme.style`.

The selected row uses `picker_selected_item` or
`list.activeSelectionBackground` / `list.activeSelectionForeground`. Follow
highlights use the find-match styles.

A side-panel heading uses `sideBarTitle.foreground`, bold, the same treatment
as a plugin tree root (`src/plugin/tree.rs`). Do not recolor those headings
with `textLink.foreground`. Link color belongs to links and which-key labels.

Mode indicators use the status line. The mode name is uppercase (`NORMAL`,
`INSERT`) in `statusline_style.outer_style`. A feature mode on a status-like
row uses that same prominent style (`DIM`, `REVIEW`). Counts are plain
numbers, the way search matches render `3/12` and diagnostics render a marker
plus a number. Do not invent a `×N` prefix or a colored pill for either.

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
`window_borders_ascii` when the surface already consults that flag.

## Keys

Red has one notation for a chord: space-separated key names, the same strings
the shortcut catalog stores. Examples: `Space e f`, `Space E`, `Ctrl-w h`,
`Alt-p`, `Esc`, `F1`. No brackets, no chips, and no second spelling of the
same chord.

Three surfaces already show keys, and they do not overlap:

- Which-key (`src/ui/keymap_hints.rs`) shows only the next key in each row
  (`f`, `s`, `d`). The pending prefix is the popup title (`Space e · keymaps`)
  and the command line. A group that continues ends with ` …`.
- Keyboard shortcuts (`:keys`, `F1`, `Alt+/`, `src/ui/keyboard_shortcuts.rs`)
  shows the full chord in bold, then the action. The command palette shows
  the same full chord.
- The action bar (`src/ui/action_bar.rs`) is the footer hint. The key is bold
  in the secondary role and the label is muted. Pairs are separated by ` · `.

A panel does not grow its own key renderer. `Space e ?` opens Keyboard
shortcuts scoped to the explore bindings. Any other footer hint goes through
`ActionBar`.

## Words

Use the same plain voice as the command palette: "Find file", "Go to
definition". Plugin empty and loading lines do the same: "No themes found",
"No edits yet", "Loading sessions…".

- Say what the user sees. "Variable" rather than "binding". "Matches" rather
  than "references" when the match is only a shared spelling. "Language
  server" rather than "LSP" on a first-view label. `gd` can stay, because it
  is the key.
- Section headings are short title case ("Inside", "Matches"), in the same
  voice as picker titles. The mode name (`NORMAL`, `DIM`) stays uppercase,
  because it is a status-line mode.
- Empty states say what is missing and what to do: "No name under the cursor."
  "No matches for this name."
- Loading and partial results name the scope: "6 changed files indexed." Do
  not leave a spinner with no noun.

## Truncation

Clip on a grapheme boundary. Dialog titles, which-key, the action bar, plugin
panels, and keyboard shortcuts use `truncate_display_width` and stop at the
edge (`src/unicode_utils.rs`). They do not add an ellipsis. A surface that
already ellipsizes, such as a picker path, keeps `…` through
`truncate_display_width_with_marker`. Match the surface you are extending.
Do not add a third clipping rule, and do not ellipsize every row by default.

Paths keep the file name when the directory does not fit. A count stays
visible when the row is a title-plus-count; shorten the title before the
count. Wrap a note on word boundaries before clipping a single word.

## Inconsistencies this pass does not pretend away

- Completion icons in `src/ui/icons.rs` are single-column Unicode (`λ`, `ƒ`).
  Pickers and the status line are Nerd Font glyphs with ASCII fallbacks.
- Plugin tree icons are Nerd Font only. They do not consult
  `PickerIconStyle`, so an ASCII icon setting still draws ``.
- Some cards use a single border and some use a rounded one. Which-key and
  pickers ignore `window_borders_ascii`; window splits honor it.
- Keyboard shortcuts draws its scope toggle as `[This context]` and
  `[All Red keys]`. That is a control label, not a key chord. Key chords
  stay unbracketed.
- A few command-line messages still say "LSP" where a sidebar should say
  "language server".
