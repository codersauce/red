# Red Vim compatibility matrix

**Matrix version:** 1.1
**Validated against:** Red 0.1.1, July 2026  
**Status vocabulary:** **supported**, **intentional difference**, **not yet supported**

“Real Vim keys” means the rows marked **supported** below. It does not mean complete
Vim emulation. Every release that changes editing behavior must update this document and
the corresponding integration tests.

## Normal editing

| Area | Status | Red behavior |
|---|---|---|
| Counts | **supported** | Decimal prefixes apply to motions, joins, line-end edits, substitute/delete-character aliases, macro playback, dot-repeat, find/till, and `r`. Nested mappings preserve the prefix until their final key. |
| Basic motions | **supported** | `h j k l`, arrows, `0`, `^`, `$`, `w`, `b`, `e`, `ge`, `B`, `E`, `gE`, `gg`, `G`, screen-line motions, full/half-page motions, and file percentages use grapheme-safe cursor positions. The typed `MoveToNextBigWord` action can be mapped when `W` semantics are preferred. |
| Character motions | **supported** | `f{char}`, `t{char}`, `F{char}`, `T{char}`, counted forms, and `,` reverse-repeat; delete, change, and yank accept the same suffixes. Forward-repeat is available as the remappable `RepeatCharSearch` action. |
| Operators | **supported** | `d`, `c`, and `y` with line, vertical, line-start/end, forward/backward word, find/till, match, and supported text-object targets. `cw` preserves trailing whitespace like Vim. |
| Text objects | **supported** | Inner/around word, parentheses, brackets, braces, single quotes, double quotes, and backticks. |
| `r{char}` | **supported** | Replaces one or a counted run of graphemes and is one undoable change. A count longer than the remaining line is rejected without editing. |
| Editing aliases | **supported** | `D`, `C`, and Neovim-style `Y` operate to line end; `S`, `s`, and `X` provide line/character substitute and backward-delete shortcuts. Counts, default-register kind, undo, and Insert transitions are preserved. `U` is an additional redo alias. |
| Case changes | **supported** | `~`, `gu{motion}`, `gU{motion}`, `g~{motion}`, and the `guu`/`gUU`/`g~~` line forms transform Unicode text as one transaction. |
| Join | **supported** | `J` joins at least two lines, removes following indentation, and inserts a space unless trailing whitespace or `)` makes it unnecessary; `gJ` preserves whitespace. Normal counts, Visual joins, `:j[oin][!] [count]`, undo, dot-repeat, and macros are covered. |
| Ex commands and default-key differences | **intentional difference** | Red implements a documented Ex subset and uses `;` as an additional command-line entry key, `W` to toggle wrapping, and `Ctrl-e` for NeoTree; these defaults intentionally differ from Vim and can be remapped. Red does not implement Vimscript. |

## Registers, repeat, and macros

| Area | Status | Red behavior |
|---|---|---|
| Default register | **supported** | Yank, delete, change, `p`, and `P`; default-register writes also update the configured system clipboard. |
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
| Visual `r` replace and case changes | **supported** | Visual `r{char}`, `u`, `U`, and `~` replace/change the selection in one transaction, including shifted terminal key events and Visual-line/block selections. |
| Wrapped-line motions | **supported** | `gj`, `gk`, `g0`, `g^`, and `g$`; scroll and cursor state are window-local. |

## Search, substitution, history, and marks

| Area | Status | Red behavior |
|---|---|---|
| Search | **supported** | `/`, `?`, incremental preview, `n`, `N`, `*`, wrapscan, smartcase/ignorecase, cancellation, and highlight clearing. |
| Search syntax | **intentional difference** | Patterns use Rust `regex` syntax rather than Vim's regex dialect. |
| Substitute ranges | **supported** | Current line, `%`, one-based numeric line/range, and `'<,'>` last-visual range. |
| Substitute flags | **supported** | `g`, `i`, and explicit `c` confirmation with `y/n/a/q/l`. All accepted replacements from one command form one transaction. |
| Substitute syntax | **intentional difference** | Patterns and capture expansion use Rust `regex`; delimiters may be escaped. Vim magic modes, expression replacement, and omitted trailing delimiters are not supported. |
| Undo/redo | **supported** | Linear, per-buffer transactions with dirty-state checkpoints. |
| Undo tree | **supported** | Undo followed by a new edit creates a sibling branch. `g-`/`g+` select a sibling deterministically and redo traverses it; `:undotree` opens the small visual navigator. |
| Jumplist | **supported** | Search and long/file motions record jumps; `Ctrl-o` and `Tab` traverse backward/forward. |
| Local marks | **supported** | `ma`–`mz`, exact backtick jump, and first-nonblank apostrophe jump. They remain tied to the in-memory buffer and report an error after it is deleted. |
| Global marks | **supported** | `mA`–`mZ`; an existing marked file is reopened after its buffer closes. A deleted file produces an error and is never recreated by a jump. |
| Special marks | **supported** | Previous jump (`''`/````), last change (`'.`/``.` ``), and last visual bounds (`'<`, `'>`, `` `< ``, `` `> ``). |
| Mark edit affinity | **intentional difference** | Named marks have right insertion affinity; last-visual start has left affinity and end has right affinity. All anchors transform through edits, multi-edit transactions, undo, and redo using Unicode character coordinates. |

## Edge and integration coverage

| Area | Status | Red behavior |
|---|---|---|
| Unicode graphemes | **supported** | Cursoring, replacement, selection, paste, undo, and marks are tested with multi-codepoint graphemes. Rust-regex offsets are converted to character coordinates before editing. |
| Empty buffers | **supported** | The synthetic editable line remains cursor-safe across insert, delete, render, and undo. |
| Final line / trailing newline | **supported** | Both forms render and edit without exposing a phantom gutter line. |
| Multi-window | **supported** | Active-buffer cursor, viewport, wrapping, gutter width, and focus-cycle state are window-aware. |
| Multi-window Vim window command parity | **intentional difference** | Red supports its published `Ctrl-w` subset; arbitrary Vim layouts and every resizing command are not promised. |

## Release gate

Automated evidence is the `editing` integration suite plus the full all-feature test and
clippy gates. Manual dogfood evidence is recorded in
[`VIM_DOGFOOD.md`](VIM_DOGFOOD.md). A row may be promoted to **supported** only with a
production-path test. A Phase 1 public launch additionally requires two external
Vim-native testers to complete the manual one-week protocol with no unresolved
release-blocking compatibility issue.
