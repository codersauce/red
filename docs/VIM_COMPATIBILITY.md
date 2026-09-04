# Red Vim compatibility matrix

**Matrix version:** 1.10
**Tracked against:** Red v0.7.0
**Status vocabulary:** **supported**, **intentional difference**, **not yet supported**

“Real Vim keys” means the rows marked **supported** below. It does not mean complete
Vim emulation. Every release that changes editing behavior must update this document and
the corresponding integration tests.

## Normal editing

| Area | Status | Red behavior |
|---|---|---|
| Counts | **supported** | Decimal prefixes apply to motions, joins, line-end edits, substitute/delete-character aliases, macro playback, dot-repeat, find/till, `r`, and pane or split resizing. Nested mappings preserve the prefix until their final key, including `5 Ctrl-w >`. |
| Basic motions | **supported** | `h j k l`, arrows, `0`, `^`, `$`, `w`, `W`, `b`, `e`, `ge`, `B`, `E`, `gE`, `gg`, `G`, viewport-relative `H`, `M`, and `L`, screen-line motions, full/half-page motions, and file percentages use grapheme-safe cursor positions. Counted `H` and `L` honor the visible viewport. |
| Paragraph and sentence motions | **supported** | `{`/`}` stop on genuinely empty paragraph-separator lines; whitespace-only lines are not boundaries. `(`/`)` recognize `.`, `!`, and `?`, closing delimiters, line endings, and paragraph boundaries. Both motion families support counts, Visual mode, operator-pending forms, Unicode, document boundaries, and window-local jumps. |
| Character motions | **supported** | `f{char}`, `t{char}`, `F{char}`, `T{char}`, counted forms, `;` forward-repeat, and `,` reverse-repeat; delete, change, and yank accept the same suffixes. |
| Structural motions | **supported** | Tree-sitter-backed `]m`/`[m` move between calls, `]f`/`[f` between functions, and `]c`/`[c` between classes. Counts, operator-pending forms such as `d2]f`, Visual selections, and window-local jumps are supported without wrapping past the document boundary. |
| Bracket navigation boundaries | **supported** | The default `]%`, `]c`, `]d`, `]f`, `]h`, `]i`, and `]m` navigation commands leave the cursor in place and show a transient, target-specific warning when no eligible target exists. Their `[` counterparts provide the same feedback at the start boundary. Diagnostic navigation does not wrap between the first and last diagnostic. |
| Operators | **supported** | `d`, `c`, and `y` with horizontal, line, vertical, file-boundary, line-start/end, small/big-word, previous-word-end, paragraph, sentence, find/till, match, supported text-object, and structural-motion targets. Paragraph/sentence operators preserve Neovim's exclusive-motion and linewise conversion rules; `cw` and `cW` preserve trailing whitespace like Vim. |
| Text objects | **supported** | Inner/around small words, big words, sentences, paragraphs, parentheses, brackets, braces, single quotes, double quotes, and backticks. Counted `is`/`as` distinguish sentence text from its adjacent whitespace. |
| Structural text objects | **supported** | Syntax-aware `am`/`im` select calls, `af`/`if` functions, `ac`/`ic` classes, and `ak`/`ik` comments. Objects work in Visual mode and with delete, change, yank, and case transforms. Outer functions and classes produce linewise selections and registers. |
| Structural swaps | **supported** | `Space ] a`/`Space [ a` exchange adjacent parameters and `Space ] m`/`Space [ m` exchange adjacent functions in the same syntax container. Separators remain in place; each swap supports one-step undo, dot-repeat, macros, and jumplist navigation. |
| `r{char}` | **supported** | Replaces one or a counted run of graphemes and is one undoable change. A count longer than the remaining line is rejected without editing. |
| Editing aliases | **supported** | `D`, `C`, and Neovim-style `Y` operate to line end; `S`, `s`, and `X` provide line/character substitute and backward-delete shortcuts. Counts, default-register kind, undo, and Insert transitions are preserved. `U` is an additional redo alias. |
| Case changes | **supported** | `~`, `gu{motion}`, `gU{motion}`, `g~{motion}`, and the `guu`/`gUU`/`g~~` line forms transform Unicode text as one transaction. |
| Join | **supported** | `J` joins at least two lines, removes following indentation, and inserts a space unless trailing whitespace or `)` makes it unnecessary; `gJ` preserves whitespace. Normal counts, Visual joins, `:j[oin][!] [count]`, and `%`, numeric, or last-Visual Ex ranges without a separate count are covered alongside undo, dot-repeat, and macros. |
| Ex commands and default-key differences | **intentional difference** | Red implements a documented Ex subset, uses `gW` to toggle wrapping, and uses `Ctrl-e` for NeoTree; these Red-specific defaults can be remapped. `:` enters the command line and prefills `'<,'>` from Visual mode, while `;` and `W` retain their Vim meanings. Red does not implement Vimscript. |

## Registers, repeat, and macros

| Area | Status | Red behavior |
|---|---|---|
| Default register | **supported** | Yank, delete, change, `p`, and `P`; characterwise paste preserves Neovim cursor placement for single-line and multiline text. Default-register writes also update the configured system clipboard. |
| Named text-register prefix (`"a`) | **not yet supported** | Named storage exists for macros, but interactive text-operation register selection is not implemented. |
| Dot-repeat (`.`) | **supported** | Replays the last completed content-changing input recipe through normal key resolution. Covered: direct changes, operator+motion, operator+text object, insert sessions, paste, replace, indent, open-line, and visual-block insert. |
| Count before dot | **supported** | `N.` replays the completed change N times. A failed/no-op change does not replace the previous definition. |
| Dot after confirmed substitute | **not yet supported** | The substitute is undoable as one transaction, but confirmation answers are not a reusable dot recipe. |
| Macro record/play | **supported** | `q{register}`, `@{register}`, `@@`, counts, uppercase append, and recursion/instruction limits. |
| Macro inspection/editing | **supported** | `:registers` lists notation; `:register {name} {key-notation}` validates and replaces it. |
| Macro event policy | **intentional difference** | Only normalized key press/repeat events are recorded. Mouse, paste, resize, focus, plugin callbacks, LSP messages, and other asynchronous/background events are ignored, so playback is deterministic. |

## Modes and selection

| Area | Status | Red behavior |
|---|---|---|
| Insert / Normal | **supported** | `i`, `a`, `I`, `A`, `o`, `O`, Escape, newline, backspace, tab, and bracketed paste. |
| Visual character | **supported** | Motions, supported text objects, yank/delete/change/paste, and Unicode selections. |
| Visual line | **supported** | Linewise yank/delete/change/paste, including whole-document and interior replacements. |
| Visual block | **supported** | Block delete/change/insert, one-transaction replay, undo/redo, and dot-repeat for block insert. |
| Multi-cursor selections | **supported** | In Normal mode, `Ctrl-n` selects successive whole-word occurrences; from a single-line characterwise Visual selection, it uses the exact selected text and immediately adds the next literal occurrence. `Ctrl-Up` / `Ctrl-Down` add vertical cursors while preserving display columns. `n` / `N` navigate matches, `q` skips, and `Q` removes the active selection. `Tab` toggles extend mode; Shift-arrows and `h`, `l`, `w`, `e`, `0`, and `$` extend complete Unicode graphemes, and `o` reverses each anchor. `c`, `i`, `a`, `d`, `x`, `y`, `p`, and `P` apply across selections. Insert, change, delete, and paste are grouped into one undoable edit. |
| Restore Visual selection | **supported** | `gv` restores the previous buffer-local Visual area with its character, line, or block shape and original direction. In Visual mode it exchanges the current and previous areas. Selection metadata survives session recovery, while `<` and `>` continue to track edits. |
| Visual indent | **supported** | `[count]>` and `[count]<` shift every covered line right or left by `count × shiftwidth` in one undoable transaction for character, line, and block selections. Empty lines remain empty, indentation saturates at column zero, `.` repeats the shift over the same number of lines from the current line, and `gv` restores the shifted range. |
| Visual `r` replace and case changes | **supported** | Visual `r{char}`, `u`, `U`, and `~` replace/change the selection in one transaction, including shifted terminal key events and Visual-line/block selections. |
| Visual command line | **supported** | `:` captures the selection as `'<` and `'>`, opens Command mode with `'<,'>` prefilled, and supports normal command-line editing and cancellation. Character, line, and block selections produce line-oriented Ex ranges. |
| Wrapped-line motions | **supported** | `gj`, `gk`, `g0`, `g^`, and `g$`; scroll and cursor state are window-local. |

## Search, substitution, history, and marks

| Area | Status | Red behavior |
|---|---|---|
| Search | **supported** | `/`, `?`, persistent shared search history with prefix-filtered Up/Down and Ctrl-p/Ctrl-n recall, incremental preview, `n`, `N`, `*`, wrapscan, smartcase/ignorecase, cancellation, and highlight clearing. |
| Search syntax | **intentional difference** | Patterns use Rust `regex` syntax rather than Vim's regex dialect. |
| Substitute ranges | **supported** | Current line, `%`, one-based numeric line/range, and `'<,'>` last-Visual range. Visual `:` prefills that range, so substitution applies to every line touched by character, line, or block selections. |
| Substitute flags | **supported** | `g`, `i`, and explicit `c` confirmation with `y/n/a/q/l`. All accepted replacements from one command form one transaction. |
| Substitute syntax | **intentional difference** | Patterns and capture expansion use Rust `regex`; delimiters may be escaped. Replacement `\r` splits the line and preserves its LF or CRLF ending, and `\\` inserts one backslash. Captures use Rust forms such as `$0`, `$1`, and `$name`, rather than Vim's `&`, `\0`, and `\1`. Unlike Neovim, `\n` remains the two literal characters instead of inserting NUL. Vim magic modes, expression replacement, and omitted trailing delimiters are not supported. |
| Undo/redo | **supported** | Linear, per-buffer transactions with dirty-state checkpoints. |
| Undo tree | **supported** | Undo followed by a new edit creates a sibling branch. `g-`/`g+` select a sibling deterministically and redo traverses it; `:undotree` opens the small visual navigator. |
| Jumplist | **supported** | Search, long/file motions, structural motions, and structural swaps record window-local jumps; splits copy their source window's list, positions follow edits, same-line entries are cleaned up, and `Ctrl-o` / `Ctrl-i` (`Tab`) traverse backward/forward without discarding the forward branch. |
| Local marks | **supported** | `ma`–`mz`, exact backtick jump, and first-nonblank apostrophe jump. They remain tied to the in-memory buffer and report an error after it is deleted. |
| Global marks | **supported** | `mA`–`mZ`; an existing marked file is reopened after its buffer closes. A deleted file produces an error and is never recreated by a jump. |
| Special marks | **supported** | Previous jump (`''`/````), last change (`'.`/``.` ``), and last visual bounds (`'<`, `'>`, `` `< ``, `` `> ``). |
| Mark edit affinity | **intentional difference** | Named marks have right insertion affinity; last-visual start has left affinity and end has right affinity. All anchors transform through edits, multi-edit transactions, undo, and redo using Unicode character coordinates. |

## Edge and integration coverage

| Area | Status | Red behavior |
|---|---|---|
| Unicode graphemes | **supported** | Cursoring, replacement, selection, paste, undo, and marks are tested with multi-codepoint graphemes. Rust-regex offsets are converted to character coordinates before editing. |
| Empty buffers | **supported** | The synthetic editable line remains cursor-safe across insert, delete, render, and undo. |
| Unnamed buffer creation | **supported** | `:enew` opens an empty unnamed buffer in the current window, preserves existing unsaved buffers, and reuses an already-empty unnamed buffer. |
| Ex command abbreviations | **supported** | Built-in Vim commands honor their documented minimum prefixes, including `:e[dit]`, `:ene[w]`, `:wa[ll]`, `:sav[eas]`, `:se[t]`, and `:sy[ntax]`. Exact names and existing aliases take precedence; ambiguous and too-short prefixes are rejected. |
| Ex shell commands | **supported** | `:!{command}` executes the exact command through the user's non-interactive shell in Red's working directory, with asynchronously streamed stdout/stderr, bounded Messages history, cancellation with `Ctrl-c`, detached-session continuity, `%`/`#` filename expansion, escaped special characters, and previous-command expansion including `:!!`. `:%!{command}`, numeric line ranges, and `:'<,'>!{command}` send the selected text to stdin and apply complete UTF-8 stdout as one undoable edit. Failed, cancelled, and stale filters preserve the original buffer. Interactive terminal programs, `:read !`, `:write !`, and Normal-mode filter operators are not supported. |
| Buffer commands | **supported** | `:new` and `:vnew` create new buffers in splits; `:b[uffer] {name|number|#}`, `:bnext`, `:bprevious`, and `:ls`/`:buffers`/`:files` navigate or list stable buffer identities. `:wa[ll]` writes every modified named buffer without changing the active buffer, then reports the number written, pending format-on-save work, or that no buffers were modified; dirty unnamed buffers remain open and report an aggregate error after named buffers are saved. `:[buffer-id-range]bufd[o][!] {command}` snapshots the open buffers in stable-ID order, stops on the first error, and leaves the final or failing buffer active. Red treats every open buffer as listed and preserves dirty buffers while switching, so the outer `!` is accepted but does not change traversal. Nested non-interactive editing and buffer-local option commands are supported; buffer/window restructuring, modal UI, plugin commands, and `|` command chains are not. The substitute `e` flag suppresses missing-pattern errors during traversal. `:saveas {path}` writes a new association, while `:file {path}` names the buffer without writing. |
| Final line / trailing newline | **supported** | Both forms render and edit without exposing a phantom gutter line. |
| Multi-window and docked panes | **supported** | Active-buffer cursor, viewport, wrapping, gutter width, and focus-cycle state are window-aware. `Ctrl-w h/j/k/l` moves between editor windows and panes; `Ctrl-w H/J/K/L` moves the focused editor window, row pane, or text pane to the corresponding outer edge without replacing its identity, content, or draft. |
| Embedded plugin text areas | **supported** | Agent dialogs and text-panel composers reuse Unicode-aware word, paragraph, and sentence motions, character searches, ordinary and sentence text objects, and transactional replacement. Counts, operators, Visual selections, local registers, undo/redo, dot-repeat, macros, and prompt-local search remain isolated. Tree-sitter structural objects and swaps stay editor-owned and are unavailable in grammar-free composers. |
| Inline assist selection | **intentional difference** | `Space i` targets the enclosing function in Normal mode when syntax information is available, otherwise the current line; characterwise and linewise Visual selections remain exact. Visual-block targets are rejected. The popup has its own Insert-like, soft-wrapped prompt, remains within the initiating split, and its applied result is one unsaved, undoable editor transaction. |
| Window and pane resizing | **supported** | `Ctrl-w >` / `<` grow or shrink vertical panes and editor splits; `Ctrl-w +` / `-` grow or shrink horizontal panes and editor splits. Counts are supported. `Ctrl-w =` balances editor splits or restores the focused pane's original size. Mouse dragging immediately highlights the captured pane or nested split divider without stealing focus; release or `Esc` restores its normal appearance. |
| Multi-window Vim window command parity | **intentional difference** | Red supports the documented navigation, edge-movement, resizing, and balancing commands; arbitrary Vim layouts and undocumented window commands are not promised. |

## Release gate

Automated evidence is the `editing` integration suite plus the full all-feature test and
clippy gates. Manual dogfood evidence is recorded in
[`VIM_DOGFOOD.md`](VIM_DOGFOOD.md). A row may be promoted to **supported** only with a
production-path test. A Phase 1 public launch additionally requires two external
Vim-native testers to complete the manual one-week protocol with no unresolved
release-blocking compatibility issue.
