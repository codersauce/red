# Helix parity against 25.07.1

Captured from the installed binary `helix 25.07.1 (a05c151b)` with
`scripts/helix-diff/`. Each case compares the buffer text, the selection
anchor and head, the primary index, and the mode. The same 300 cases are
regression-tested in `tests/helix.rs`.

The first run found mismatches. Those keys are **fixed**. Keys that already
agreed with Helix are **matched**. No supported key in the matrix still
differs on the captured cases.

The harness uses a 100 by 24 truecolor frame. Red's `scrolloff` is 3, and the
Helix probe is given the same value so page and window motions are compared
on equal terms. Helix's own default scrolloff is 5.

| Keys | Result | What changed |
|---|---|---|
| `h` `j` `k` `l`, arrows, counts | **fixed** | `l` and `j` can sit on the blank line after a trailing newline. |
| `w` `b` `e` `W` `B` `E`, `3w`, `w` then `d` | **fixed** | A word motion takes the word plus its trailing whitespace. The cursor is the last scalar of that span. A count paints only the final selection. The selection style covers every scalar except that head. |
| `f` `t` `F` `T` | **fixed** | A count is the nth match, as in `2fx`. |
| `x` `X` `Alt-x` `%` | **matched** | Line extend, line bounds, and select-all. |
| `d` `Alt-d` `c` `Alt-c` | **fixed** | After a change, every cursor sits on the scalar after the inserted text. |
| `y` `p` `P` `R` | **fixed** | Paste leaves the cursor on the pasted text. |
| `r` `~` `` ` `` `Alt-`` ` | **fixed** | The cursor stays on the changed scalar. `r` replaces a newline. |
| `i` `a` `I` `A` `o` `O` | **fixed** | Esc does not step left. `a` keeps a selection from the original scalar through the inserted text. |
| `g` `g` `g` `e` `G` | **fixed** | `G` without a count does nothing. `3gg` and `2G` go to that line. |
| `g` `h` `g` `l` `g` `s`, Home, End | **matched** | |
| `g` `t` `g` `c` `g` `b` | **fixed** | They use the text height and `scrolloff`. |
| `g` `d` `g` `y` `g` `i` `g` `D` | **matched** | With no language server both editors leave the cursor where it is. |
| `g` `a` `g` `n` `g` `p` | **matched** | One buffer, so there is nowhere to jump. |
| `g` `.` | **fixed** | The last-change mark is the cursor after the edit. |
| `m` `m` | **fixed** | The cursor moves onto the matching bracket. |
| `m` `i` `m` `a` | **matched** | |
| `m` `s` `m` `r` `m` `d` | **fixed** | The surround covers both brackets, and delete finds a pair around the cursor. |
| `s` `S` `Alt-s` | **matched** | Including `s` then `c` on two matches. |
| `,` `Alt-,` `(` `)` `C` `Alt-C` | **fixed** | `Alt-C` and `Alt-K` keep their case, so they no longer collide with `Alt-c`. |
| `K` `Alt-K` `&` `_` | **fixed** | `&` pads on the left until the selection ends share a column. |
| `/` `?` `n` `N` `*` | **fixed** | `*` only sets the search. `N` starts before the current match. |
| `u` `U` `:` `v` | **fixed** | `v` toggles select mode. Esc leaves select from a count, a pending character, or a keymap menu, and the ranges stay. |
| mouse click, alt-click, drag, wheel | **fixed** | The selection follows the pointer. A click is a point, alt-click adds a cursor, drag extends, and the wheel moves the cursor only to stay inside `scrolloff`. |
| `;` `Alt-;` | **matched** | Collapse and flip. |
| `z` `z` `z` `t` `z` `b` | **matched** | The selection stays put. |
| `z` `j` `z` `k` | **fixed** | Scrolling pulls the cursor inside `scrolloff`. |
| `>` `<` `=` `J` `Ctrl-c` | **fixed** | Plain-text indent is a tab. Join keeps the final newline. Comment leaves a cursor. |
| `Alt-o` `Alt-i` `Alt-n` `Alt-p` | **fixed** | The walk sees every syntax node. Previous-sibling from the first token selects back to the end of the buffer. |
| `Ctrl-f` `Ctrl-b` `Ctrl-d` `Ctrl-u`, PageUp, PageDown | **fixed** | The cursor moves by the 22-row text height of a 24-row frame. |
| `Ctrl-o` `Ctrl-i` | **matched** | An empty jumplist leaves the selection alone. |

## Tutor

`scripts/helix-diff/tutor.py` replays each exercise from the Helix 25.07.1
`:tutor` file (`helix-term/runtime/tutor`). A section matches when Red and
Helix finish with the same buffer, selection anchors and heads, primary
index, and mode. `python3 scripts/helix-diff/tutor.py` reruns it.

| Section | Result | Notes |
|---|---|---|
| 1.1 Movement | **matched** | `hjkl` on a small grid. |
| 1.2 Exiting | **matched** | `:` enters command mode and Esc leaves it. `q`, `quit`, `q!`, and `quit!` are the existing quit commands; the probe does not send them because they close the editor. |
| 1.3 Deletion | **matched** | `d` deletes the selected scalar. |
| 1.4 Insert | **matched** | `i` and Esc. |
| 1.5 Saving | **matched** | `:w`. |
| 2.1 More insert | **matched** | `A` inserts at the end of the line. |
| 2.2 Opening lines | **matched** | `o` opens a line below. |
| 3.1 Motions | **matched** | `w` then `d`. |
| 3.2 More motions | **matched** | `e` and `b` around a word. |
| 3.3 Words and WORDs | **matched** | `w` and `W`. |
| 3.4 Change | **matched** | `c` replaces the selection. |
| 3.5 Counts | **matched** | The count is applied inside one motion. |
| 3.6 Select mode | **fixed** | `d` leaves select mode, so the next `v` enters it again. |
| 3.7 Lines | **matched** | `x` and `2x`. |
| 3.8 Collapse | **matched** | `;`. |
| 4.1 Undo | **matched** | `u` and `U`. |
| 4.2 Yank and paste | **matched** | `y`, `p`. Space y / Space p are not Helix clipboard commands. |
| 4.3 Search | **matched** | `/`, `n`, `N`. |
| 5.1 Multiple cursors | **matched** | `C` and `,`. |
| 5.2 Select command | **matched** | `s`. |
| 5.3 Regex select | **matched** | `s` with ` +`. |
| 5.4 Align | **matched** | `&`. |
| 5.5 Split into lines | **matched** | Alt-s then `&`. |
| 6.1 Find and till | **fixed** | `t` and `T` on a run of the same character cover the cursor and the neighboring match, as Helix does. |
| 6.2 Replace | **matched** | `r`. |
| 6.3 Repetition | **not supported** | `.` replays the last insert and Alt-. replays the last `f`/`t`. Red does not record those. |
| 7.1 Replace with yank | **matched** | `R`. |
| 7.2 Join | **matched** | `J`. |
| 7.3 Indent | **matched** | `>` and `<`. |
| 7.4 Increment | **fixed** | Ctrl-a and Ctrl-x add or subtract 1 from the integer under the selection. |
| 8.1 Registers | **not supported** | `"` chooses a named register. Yank and paste use one default register. |
| 8.2 Macros | **not supported** | `Q` records and `q` replays. That needs a keystroke log. |
| 9.1 Search selection | **matched** | `*` sets the search, `n` and `N` move. |
| 9.2 Select on next match | **matched** | `n` in select mode adds a selection. |
| 9.3 Jumplist | **fixed** | Ctrl-s pushes the cursor onto the jumplist. Ctrl-o returns to it. |
| 9.4 Two-character labels | **not supported** | `gw` paints labels over the visible words. That is a popup, not a buffer edit. |
| 10.1 Cycle selections | **fixed** | Alt-, leaves the following selection primary. |
| 10.2 Cycle contents | **fixed** | Alt-( and Alt-) rotate the text inside the selections. |
| 10.3 Case | **matched** | `~`, `` ` ``, and Alt-`. |
| 10.4 Split on regex | **fixed** | `S` keeps the first piece primary. |
| 11.1 Comment | **fixed** | A buffer with no language comment uses `# `, and a cursor stays a cursor. |
| 11.2 Comment several lines | **matched** | The line selection is kept. |
| 12.1 Match jump | **matched** | `mm`. |
| 12.2 Select inside | **matched** | `mi(`. |
| 12.3 Select around | **matched** | `ma(`. |
| 12.4 Surround | **fixed** | Surround leaves select mode. |
| 12.5 Delete surround | **fixed** | `md` looks around the cursor, including when `f` has a wide selection. |
| 12.6 Replace surround | **fixed** | `mr([` finds `(` and replaces it with `[`. |
| 13.1–13.7 Splits | **not supported** | Red keeps Ctrl-w as its own window menu, and Space f is format rather than Helix's file picker. The tutor's `Ctrl-w n v`, `Ctrl-w o`, `Ctrl-w t`, `Ctrl-w HJKL`, `:vs FILENAME`, and picker Ctrl-v / Ctrl-s chords are not Helix's. |

Side-by-side frames after the exercise keys, Helix on the left and Red on the right:

- `/opt/cursor/artifacts/helix-parity/tutor-1.4-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/tutor-3.6-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/tutor-5.2-side-by-side.png`

## Rendering

The head cell is the cursor block. The other cells of each range use the selection style. Side-by-side tmux captures, Helix on the left and Red on the right:

- `/opt/cursor/artifacts/helix-parity/w-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/x-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/percent-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/multi-side-by-side.png`
