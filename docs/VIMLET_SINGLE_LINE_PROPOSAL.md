# Proposal for single-line input in Vimlet

Add a small `vimlet::CommandLine` editor for Ex commands, search patterns and ordinary input fields. Each instance owns one draft and one insertion point. A host chooses the prompt kind when it opens the surface. Typing `dw` enters the two letters. Pressing Enter returns the draft to the caller. The file or composer session that opened the prompt keeps its own mode, registers, pending command and undo history.

This is an upstream API proposal against Vimlet 0.2.0. The recovered Red branch still builds with its original Git pin.

## What Neovim does

Neovim has a dedicated Command-line mode. Ex, forward/backward search and `input()` share its editing keys. The prefix or field label is outside the editable text. Commands, searches and general input have separate histories, with both search directions sharing one history. A long command can wrap on screen. A user who wants Normal-mode operators can open the command-line window with `q:` or, while entering a command, `Ctrl-F`. That window is a buffer holding history entries. A REPL-style `buftype=prompt` buffer is a third surface with an editable tail and a submit callback. [Command-line editing and history](https://neovim.io/doc/user/cmdline/) · [input()](https://neovim.io/doc/user/vimfn/#input()) · [Prompt buffers](https://neovim.io/doc/user/channel/#prompt-buffer)

I tested the following with Neovim **0.12.1**, Vimlet’s pinned reference, using a headless process with `-u NONE -i NONE -n`. The probes fed typed keys, sampled the active draft with `getcmdline()` and the insertion point with `getcmdpos()`, and observed `input()` return values separately. Positions below are zero-based UTF-8 byte offsets, one less than Neovim’s reported position.

| Input | Observed result |
|---|---|
| `:iwddu` | Draft `iwddu`, cursor 5. |
| `:ab界<Left>X` | Draft `abX界`, cursor 3, before the wide character. Backspace after either `e` plus a combining acute or the family emoji `👩‍👩‍👦` removed the whole cluster in separate trials. |
| `:one two!<Left><C-W>` / same input ending in `<C-U>` | `one !`, cursor 4 / `!`, cursor 0. Both preserve the suffix. |
| `:abc<Del>` / `:abc<Home><Insert>X` | Delete at the end removed `c`. Overstrike at the start produced `Xbc`. |
| `/dw<Left>X` / `?dwa` | Forward search held `dXw`, backward search held `dwa`. The search direction was separate from the draft. |
| Backspace at an empty prompt | Ex and forward search returned to Normal. An empty `input()` stayed open and accepted the next character. |
| `input()` with default `XY`, then `z<CR>` | Returned `XYz`. An empty Enter returned the empty string. Escape returned the configured cancellation sentinel. |
| `:let<Up>` with a newer `echo` and an older `let` command in history | Recalled the older matching `let` command. |
| Paste `A\r\nB` after `:foo` using `nvim_paste(..., true, -1)` | Draft `fooA`. The prompt stayed active. |
| `:one<C-V><C-J>two` | The draft contained an actual LF between `one` and `two`. Quoted input can exceed a strict one-line field’s text contract. |

To reproduce the field-specific Backspace result, run this with `nvim --headless -u NONE -i NONE -n -l probe.lua`. The initial Backspace leaves the field active. Enter then accepts `Z`.

```lua
local keys = vim.api.nvim_replace_termcodes('<BS>Z<CR>', true, false, true)
vim.api.nvim_feedkeys(keys, 't', false)
local result = vim.fn.input({ prompt = 'Field: ', cancelreturn = 'cancelled' })
assert(result == 'Z')
```

## Proposed contract

```rust
let mut line = CommandLine::new(PromptKind::Ex, "'<,'>")?;
let outcome = line.handle(Key::Left);
```

Reuse Vimlet’s existing `Key` and `ByteOffset`. The proposed `PromptKind` variants are `Ex`, `SearchForward`, `SearchBackward` and `Field`. The initial caret is at the end. The host reads the draft and caret via accessors and decides how to display or horizontally scroll them. The line owns a short UTF-8 string, so a client can use it without Ropey or an editor buffer.

| Input or outcome | First implementation |
|---|---|
| Text and movement | Insert a printable character at the caret. Move by grapheme with Left/Right. Home or Ctrl-B moves to the start. End or Ctrl-E moves to the end. Backspace removes the previous grapheme. Delete removes the next one, or the preceding one at end of line. Ctrl-U removes only the prefix. For Ctrl-W, test Neovim’s default word classes, punctuation and intervening whitespace. User-defined `iskeyword` can follow separately. |
| Empty Backspace | Cancel an Ex or search draft. In a field consume the key and keep the field active. At column zero of a nonempty draft, Backspace leaves it intact. |
| Accept and cancel | Enter, Ctrl-M and Ctrl-J produce `Accepted` with the exact text, including empty text and surrounding spaces. Typed Escape and Ctrl-C produce `Cancelled`. Both are terminal for that instance. The host decides whether to execute, validate, restore focus or resume a suspended operator. |
| Paste and replacement | A `Key::Text` batch is literal input. Insert only the prefix before the first CR or LF, once, at the caret. No character in the batch is re-dispatched as a key. Apply a one-line invariant to initialization and history/completion replacements too. Reject invalid host values or non-grapheme-boundary carets without modifying the draft. Streamed terminal paste should be assembled as one batch or discard its remainder after the first break. |
| History and completion | Return typed requests for older/newer history or next/previous completion. Include the current prefix through the caret and expose the full draft. Red supplies its history store, plugin candidates and picker UI. It can restore the original text **and caret** when the user returns past the newest entry. A host reply applies only to the originating live prompt and draft revision. |
| State changes | Distinguish text edits, caret moves, handled keys with no change, host requests and unhandled keys. Incremental search needs a preview after a text change. A caret move needs only a redraw. A completion popup can consume navigation or Enter before the line receives it. |

For version one, enforce a strict single logical line in every kind. Quoted controls including LF, register insertion with Ctrl-R, the Insert-key overstrike toggle, the command-line window and mapped-Escape execution are later compatibility work. Consume an unsupported quote/register prefix and its operand as a unit, so a quoted Enter cannot submit accidentally. Preserve configured host shortcuts before the default bindings. Hosts that need a Normal-mode editing window can open a regular Vimlet session over a copy of the draft and explicitly return the chosen line.

## Adoption in Red

1. Start with `:`. Today `handle_command_event` appends to a string, and Backspace edits the tail. Route text and movement through the line editor. Render the caret from the display width of the prefix through the byte offset. Preserve Visual range prefill, history, plugin completions and existing command execution on acceptance. [Command input](../src/editor.rs) · [Command-line rendering](../src/editor/rendering.rs)
2. Move `/` and `?` to the same editor. Keep search direction, regex interpretation, preview, original position and commit/cancel in Red. If a pending file operator opened the search, validate the original buffer and revision when resuming it. Exercise Escape as well as Enter after changing the draft mid-line. [Search input](../src/editor.rs) · [Vimlet search continuation](../src/editor/file_vim/targets.rs)
3. Move `InputPrompt` to the field kind. Preserve Red’s selected initial value for rename dialogs as a host selection policy, its Ctrl-A/Alt-Backspace shortcuts, and its current trimmed/empty submission policy at the callback boundary. Neovim’s `input()` appends to a supplied default. Secret fields need masked display and must bypass history, recording and input tracing. Keep the existing prompt size bound. Multiline composers continue to use the document editor with their existing Enter and Escape policies. [Single-line dialog](../src/ui/input_prompt.rs) · [Prompt storage and paste rules](../src/ui/prompt_buffer.rs)

**Acceptance tests.** Extend Vimlet’s pinned Neovim runner with a prompt observation for kind, draft, caret and terminal outcome, captured before teardown. Use the table above as the initial fixtures. Add directed cases for punctuation, a caret inside a wide line, literal paste containing control keys, and cancellation after recalling history. Test host-specific behavior in Red through actual key events: edit a Visual Ex range in the middle, cycle plugin completion, cancel an operator search, replace a selected rename default, and submit or cancel a masked field without recording its value. A prompt edit must leave file undo, registers and dot-repeat intact. [Vimlet conformance runner](https://github.com/codersauce/vimlet/blob/v0.2.0/scripts/conformance.py)
