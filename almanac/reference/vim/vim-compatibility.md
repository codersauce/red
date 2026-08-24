---
title: "Vim Compatibility"
summary: "Red documents a supported Vim-style editing subset, intentional differences, and test gates for motions, operators, registers, macros, modes, search, marks, undo, and edge cases."
topics: [reference, vim, editor, testing]
sources:
  - id: default-config
    type: file
    path: default_config.toml
  - id: vim-doc
    type: file
    path: docs/VIM_COMPATIBILITY.md
  - id: editing-tests
    type: file
    path: tests/editing.rs
  - id: movement-tests
    type: file
    path: tests/movement.rs
  - id: editor
    type: file
    path: src/editor.rs
---

Red's Vim compatibility contract is an explicit supported subset, not a promise of complete Vim emulation. `docs/VIM_COMPATIBILITY.md` marks each behavior as supported, an intentional difference, or not yet supported, and says every release that changes editing behavior must update both the document and corresponding integration tests [@vim-doc]. The supported surface covers normal editing, registers, repeat, macros, modes, selections, search, substitution, history, marks, Unicode, empty buffers, final-line behavior, and multi-window behavior [@vim-doc]. This reference belongs with [Red Editor](../../concepts/red-editor), the [Text Mutation Boundary](../../architecture/editor/text-mutation-boundary), and [Registers, Clipboard, And Macros](../../reference/editor/registers-clipboard-and-macros).

## Status Vocabulary

| Status | Meaning |
| --- | --- |
| `supported` | Red intends the listed behavior to work and requires production-path test coverage before promoting a row to this status [@vim-doc]. |
| `intentional difference` | Red knowingly diverges from Vim for the listed area and documents the difference as part of its public behavior [@vim-doc]. |
| `not yet supported` | The behavior is acknowledged but not implemented as part of the supported surface [@vim-doc]. |

The compatibility document states that "real Vim keys" means rows marked supported, not complete Vim emulation [@vim-doc]. The release gate names the `editing` integration suite plus all-feature tests and clippy as automated evidence, and it names manual dogfood evidence separately [@vim-doc].

## Supported Editing Surface

Red supports Vim-style counts, basic motions, character motions, operators, common text objects, replacement with `r`, editing aliases, case changes, and line joining [@vim-doc]. Integration tests exercise dot repeat, counted operators, find and till motions, replace, join, undo grouping, visual changes, and line-edge or word operations through editor actions rather than direct buffer edits [@editing-tests]. Movement tests cover word motion, search navigation, wrapped-line movement, file percentages, and operator deletion through match motions [@movement-tests].

Tree-sitter structural motions add `]m`/`[m` for calls, `]f`/`[f` for functions, and `]c`/`[c` for classes, including counted, operator-pending, and Visual forms [@vim-doc] [@movement-tests]. Structural objects use `am`/`im`, `af`/`if`, `ac`/`ic`, and `ak`/`ik`; outer functions and classes are linewise [@vim-doc] [@editing-tests]. `Space ] a`/`Space [ a` swap sibling parameters and `Space ] m`/`Space [ m` swap sibling functions without crossing syntactic containers, while preserving separators, undo grouping, repeat, and jump history [@vim-doc] [@editing-tests]. These operations require an active language with structural queries and intentionally remain outside grammar-free embedded text areas [@vim-doc].

The supported register and repeat surface includes the default text register, dot-repeat, count before dot, macro record and playback, and macro inspection or editing [@vim-doc]. Named text-register selection such as `"a` is not yet supported for interactive text operations, and dot after confirmed substitute is not yet supported even though the substitute itself is undoable as one transaction [@vim-doc]. The macro policy intentionally records only normalized key press and repeat events; mouse, paste, resize, focus, plugin callbacks, LSP messages, and other asynchronous events are excluded so playback stays deterministic [@vim-doc].

## Modes, Search, And Marks

Insert and Normal mode basics are supported, along with Visual character, Visual line, Visual block, Visual replace and case changes, and wrapped-line motions [@vim-doc]. Tests cover visual mode inheriting normal motions, visual block insert undo and redo, visual paste shapes, visual replace with shifted terminal keys, and visual multi-line or block case changes [@editing-tests] [@movement-tests].

Red keeps the previous Visual area as buffer-local state, separate from the text register store. Leaving Visual mode captures the selected bounds into the special `'<` and `'>` marks and records whether the area was characterwise, linewise, or blockwise plus which end was the anchor [@editor]. `gv` restores that shape and direction from the marks, and when `gv` runs while already in a Visual mode it first captures the current area so the current and previous selections exchange [@editor] [@editing-tests]. Session snapshots serialize the last-visual-selection metadata, so `gv` can restore linewise and blockwise selections after recovery when the matching marks also restore [@editor] [@editing-tests].

The supported Ex shell subset includes `:!{command}`, previous-command repeat
with `:!!`, current and alternate filename expansion with `%` and `#`, and
backslash-escaped special characters. `:%!{command}`, numeric line ranges, and
`:'<,'>!{command}` filter the selected text through the user's shell and replace
it with complete stdout in one undoable transaction. Every command streams
bounded diagnostics into Messages, supports cancellation, and continues inside
detached owners. Unlike Neovim, failed or outdated filters do not overwrite the
original buffer. Interactive terminal programs and Normal-mode filter operators
remain outside the supported subset [@vim-doc] [@editor].

Visual indentation is a line-range operation even for characterwise and blockwise selections. `>` and `<` capture the current Visual area for later `gv`, shift every covered line by `count * shiftwidth`, leave empty lines unchanged, saturate unindentation at column zero, commit the shift as one undo transaction, notify normal change consumers when content changes, and then return to Normal mode [@editor] [@editing-tests]. The compatibility matrix records this as supported for `[count]>` and `[count]<`, with `gv` reselecting the shifted range [@vim-doc].

Search supports `/`, `?`, incremental preview, `n`, `N`, `*`, wrapscan, smartcase and ignorecase, cancellation, and highlight clearing, but search patterns use Rust `regex` syntax instead of Vim's regex dialect [@vim-doc]. Substitution supports current-line, whole-file, numeric, and last-visual ranges with `g`, `i`, and confirmation flags, while replacement syntax also follows Rust `regex` capture expansion rather than Vim magic modes or expression replacement [@vim-doc]. Tests cover search previews, failed searches, invalid regex reporting, Rust regex case options, substitution ranges, confirmation flow, and escaped delimiters [@editing-tests] [@movement-tests].

Local marks, global marks, previous-jump marks, last-change marks, and last-visual-bound marks are supported [@vim-doc]. Mark edit affinity is an intentional difference: named marks have right insertion affinity, while last-visual start has left affinity and end has right affinity [@vim-doc]. Tests cover named marks through insertions and undo/redo, jumplist participation, and last-change or last-visual marks [@editing-tests].

## Jumplist

The default normal keymap binds `Ctrl-o` to `JumpBack` and binds both `Ctrl-i` and `Tab` to `JumpForward` [@default-config]. Red documents jumplist support for search and long/file motions, with window-local lists, split windows copying their source list, edit-tracked positions, cleanup of same-line entries, and forward/back traversal that does not discard the forward branch [@vim-doc].

Integration coverage matches that contract. Movement tests cover the default key bindings, split-window independence, edit-tracked positions, same-line cleanup, per-window session recovery, boundary no-ops, and page scrolling that does not create jump entries [@movement-tests]. Editing tests cover buffer deletion removing invalid jump targets while preserving usable jumps to remaining buffers [@editing-tests].

## Intentional Differences

Red implements a documented Ex subset and does not implement Vimscript [@vim-doc]. Its default keys intentionally diverge in several places: `;` is an additional command-line entry key, `W` toggles wrapping, and `Ctrl-e` opens NeoTree, though defaults can be remapped [@vim-doc]. Multi-window compatibility is also scoped to Red's published `Ctrl-w` subset rather than arbitrary Vim layouts and every resizing command [@vim-doc].

Regex syntax is the most visible editing-language difference. Search and substitute use Rust `regex`, including capture expansion and escaped delimiters for substitution, so behavior can be compatible at the command level while differing in pattern dialect [@vim-doc]. Future compatibility work must preserve this distinction unless the underlying parser and tests change [@vim-doc].

## Test Evidence

The compatibility document requires a production-path test before a row is promoted to supported [@vim-doc]. `tests/editing.rs` includes focused tests for dot-repeat, macros, marks, substitutions, operator counts, joins, undo and redo, visual modes, registers, clipboard behavior, and Vim editing shortcuts [@editing-tests]. `tests/movement.rs` covers normal and visual motion behavior, search motion, wrapped-line cursor movement, word semantics, file percentage jumps, and match-based operator motion [@movement-tests].

The editor implementation remains the runtime source of truth when a compatibility claim and code disagree. The editor action surface contains the modes, actions, LSP-excluded macro policy, command parsing, transaction handling, and rendering interactions that tests drive [@editor].
