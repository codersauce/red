# Red Helix compatibility matrix

**Status:** experimental
**Grammar:** `editing_model = "helix"`
**Vocabulary:** **supported**, **intentional difference**, **not yet supported**

Helix mode is a second key grammar. It does not replace Vim mode, and the Vim
suite is still the regression gate for `editing_model = "vim"`. The selection
kernel lives in `src/felix/` so it can move into a headless crate named
`felix` after this integration is validated. That extraction is not part of
this change.

A row is **supported** only when a keystroke test or a felix unit test covers
the behavior. The expected selections were captured from Helix 25.07.1 with
`scripts/helix-diff/` and encoded in `tests/helix.rs`. They are not copied
from Helix's tests. The per-key result of that capture is in
`docs/HELIX_PARITY.md`.

## How to turn it on

```toml
editing_model = "helix"
```

The value is read at startup. `[keys.*]` is the Vim grammar and is not
consulted while Helix is active. Overrides belong in `[helix_keys.normal]`,
`[helix_keys.select]`, and `[helix_keys.insert]`. A string may be a Red action
or a felix command name such as `"move_next_word_start"`. Agent composers and
panel text areas stay on their Vim engine.

## Normal and select mode

Select mode (`v`) uses the same keys and extends the current selection instead
of replacing it. `Esc` returns to normal and keeps the selection. `;` collapses
each range onto its head. `Alt-;` swaps anchor and head.

| Keys | Status | Behavior |
|---|---|---|
| `h` `j` `k` `l`, arrows | **supported** | Move, collapsing each selection to one scalar. Counts repeat. |
| `w` `b` `e`, `W` `B` `E` | **supported** | Select the word and the whitespace that follows it. The cursor block sits on the last scalar of that span. Punctuation is its own word; `W` keeps punctuation inside a WORD. |
| `f` `t` `F` `T` | **supported** | Select up to a character anywhere in the buffer, not only on the current line. A miss leaves the selection unchanged. |
| `x`, `X`, `Alt-x` | **supported** | Extend to the current line and then downward, extend to full line bounds, shrink to whole lines inside the selection. |
| `%` | **supported** | Select the whole buffer. |
| `d`, `Alt-d`, `c`, `Alt-c` | **supported** | Delete or change every selection. `Alt-` forms do not yank. `c` enters insert. |
| `y` `p` `P` `R` | **supported** | Yank the selections, paste after or before, or replace them with the yank. |
| `r`, `~`, `` ` ``, `Alt-` `` | **supported** | Replace each selected scalar, or change case. Newlines are left in place by `r`. |
| `i` `a` `I` `A` `o` `O` | **supported** | Insert before or after the selection, at the line's first non-blank or end, or open a line. Several selections insert the same typed text until `Esc`. |
| `g` `g`, `g` `e`, `G` | **supported** | File start, last line, or `[count]G` / `[count]gg` as a 1-based line. |
| `g` `h`, `g` `l`, `g` `s` | **supported** | Line start, last scalar of the line, first non-blank. |
| `g` `t`, `g` `c`, `g` `b` | **supported** | Viewport top, middle, and bottom for the primary selection. |
| `g` `d`, `g` `y`, `g` `i`, `g` `D` | **supported** | LSP definition, type definition, implementation, and declaration. They need a running language server. |
| `g` `r` | **intentional difference** | Opens Red's references picker (`Space k` in the Vim map). Selecting every reference is `Space l`. |
| `g` `a`, `g` `n`, `g` `p`, `g` `.` | **supported** | Alternate buffer, next buffer, previous buffer, and the last change mark. |
| `m` `m` | **supported** | Select from the cursor through the matching bracket. |
| `m` `i`, `m` `a` | **supported** | Select inside or around a word, quote, pair, sentence, paragraph, or a syntax object (`m` `f` `c` `k`). |
| `m` `s`, `m` `r`, `m` `d` | **supported** | Add, replace, or delete a one-character surround such as `()` or `"`. |
| `s`, `S`, `Alt-s` | **supported** | Select regex matches inside the selections, split on a regex, or split on newlines. `s` and `S` prompt until Enter. |
| `,`, `Alt-,`, `(`, `)` | **supported** | Keep the primary, drop it when another remains, rotate the primary. |
| `C`, `Alt-C` | **supported** | Copy each selection onto the next or previous line when that line is long enough. |
| `K`, `Alt-K` | **supported** | Keep or drop selections whose text matches a prompted regex. |
| `&`, `_` | **supported** | Pad selections so their starts share a column, or trim whitespace from each selection. |
| `/`, `?`, `n`, `N`, `*` | **supported** | Search, then select the match. `*` searches for the primary selection. |
| `u`, `U` | **supported** | Undo and redo. A Helix edit restores the selection captured for that history revision when one was stored. |
| `Alt-u`, `Alt-U` | **intentional difference** | Red's undo-branch commands (`g-` / `g+` in Vim mode), not a separate earlier/later timeline. |
| `:` | **supported** | Red's command line, including `:w` and `:q`. Helix-only names such as `:bc` are not added. |
| `v` | **supported** | Select mode. |
| `z` `z`, `z` `t`, `z` `b`, `z` `j`, `z` `k` | **supported** | Center, top, bottom, and scroll the view. |
| `>`, `<`, `=`, `J`, `Ctrl-c` | **supported** | Indent, unindent, format the primary selection, join lines inside selections, toggle line comments. |
| `Alt-o`, `Alt-i`, `Alt-n`, `Alt-p` | **supported** | Expand, shrink, and move to the next or previous syntax node when the buffer has a grammar. |
| Page and jump keys | **supported** | `Ctrl-b`/`Ctrl-f`, `Ctrl-u`/`Ctrl-d`, `Ctrl-o`/`Ctrl-i`, Home, End. Page motions move the primary selection. |
| `Space` | **intentional difference** | Red's Space menu is kept where it clashes. `Space e` is Explore, `Space f` is format, `Space h` is git hunks, `Space k` is references, `Space a` is select-all. Free Helix pickers are added beside them: `Space /` project search, `Space S` workspace symbols, `Space l` references as selections, `Space K` hover, `Space y` yank. |
| `Ctrl-w` | **intentional difference** | Red's window commands, including splits and focus. |
| `Ctrl-e` | **intentional difference** | NeoTree, not a Helix binding. |
| `Alt-e` | **intentional difference** | Scroll the view down one line, because `Ctrl-e` is the file tree. |
| `[` and `]` | **intentional difference** | Red's existing diagnostic, hunk, and structure motions. |

## Known gaps

| Area | Status | Notes |
|---|---|---|
| Shell pipe `\|` `!` `$` | **not yet supported** | No per-selection shell filter. |
| `Q` / `q` macros and `"` registers | **not yet supported** | Vim's `q` and `@` recorders are not on this grammar. |
| `g` `w` jump labels | **not yet supported** | |
| `Ctrl-a` / `Ctrl-x` increment | **not yet supported** | |
| `Space j`, `Space '`, debugger | **not yet supported** | No jumplist picker or last-picker restore. |
| Multiple cursors during insert | **intentional difference** | Characters, Enter, Backspace, and Esc apply to every cursor. Other insert keys are ignored until Esc. |
| Dot-repeat | **not yet supported** | `.` is not bound. |
| Live `:set editing_model` | **not yet supported** | Change the file and restart. |
| Text areas | **intentional difference** | `src/editing/textarea.rs` stays on Vim. |
| Markdown conceal and images | **intentional difference** | A one-scalar cursor does not reveal the buffer. A wider selection reveals the lines it covers. Explore is unchanged because `Space e` is still Explore. |

## Defaults chosen for review

- Clashing Space menus stay Red's. `Space e` is Explore and `Space f` is format.
- Other Helix defaults are followed, including motions that select and verbs that act on the selection.
- The setting is startup-only.
- Composers and panel text areas stay on Vim.
- Test vectors are original, not copied from Helix.
