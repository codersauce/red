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
| `w` `b` `e` `W` `B` `E`, `3w`, `w` then `d` | **fixed** | A word motion takes the word plus its trailing whitespace. The cursor is the last scalar of that span. |
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
| `;` `Alt-;` | **matched** | Collapse and flip. |
| `z` `z` `z` `t` `z` `b` | **matched** | The selection stays put. |
| `z` `j` `z` `k` | **fixed** | Scrolling pulls the cursor inside `scrolloff`. |
| `>` `<` `=` `J` `Ctrl-c` | **fixed** | Plain-text indent is a tab. Join keeps the final newline. Comment leaves a cursor. |
| `Alt-o` `Alt-i` `Alt-n` `Alt-p` | **fixed** | The walk sees every syntax node. Previous-sibling from the first token selects back to the end of the buffer. |
| `Ctrl-f` `Ctrl-b` `Ctrl-d` `Ctrl-u`, PageUp, PageDown | **fixed** | The cursor moves by the 22-row text height of a 24-row frame. |
| `Ctrl-o` `Ctrl-i` | **matched** | An empty jumplist leaves the selection alone. |

## Rendering

The head cell is the cursor block. The other cells of each range use the selection style. Side-by-side tmux captures, Helix on the left and Red on the right:

- `/opt/cursor/artifacts/helix-parity/w-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/x-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/percent-side-by-side.png`
- `/opt/cursor/artifacts/helix-parity/multi-side-by-side.png`
