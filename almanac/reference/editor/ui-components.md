---
title: "UI Components"
summary: "Modal UI components share a Component trait for drawing, event handling, resize/theme updates, plugin handles, cursor placement, and sensitive input reporting."
topics: [reference, editor, ui, plugins]
sources:
  - id: ui-core
    type: file
    path: src/ui/mod.rs
  - id: picker
    type: file
    path: src/ui/picker.rs
  - id: diagnostic-info
    type: file
    path: src/ui/diagnostic_info.rs
  - id: completion
    type: file
    path: src/ui/completion.rs
  - id: confirmation
    type: file
    path: src/ui/confirmation.rs
  - id: agent-composer
    type: file
    path: src/ui/agent_composer.rs
  - id: input-prompt
    type: file
    path: src/ui/input_prompt.rs
  - id: file-picker
    type: file
    path: src/ui/file_picker.rs
---

Red's modal UI components implement a common `Component` trait above the editor and plugin surfaces. The trait defines drawing into a `RenderBuffer`, optional ticking, live picker updates, plugin-owned picker and composer handles, resizing, theme updates, event handling, event passthrough, sensitive-input reporting, and cursor placement [@ui-core]. Components return `KeyAction` values instead of mutating editor state directly, so the editor remains the owner of action execution and resource cleanup [@ui-core].

## Component Contract

| Method | Contract |
| --- | --- |
| `draw(&self, &mut RenderBuffer)` | Paints the component into the render buffer and returns errors to the editor [@ui-core]. |
| `tick(&mut self)` | Lets asynchronous components report whether their state changed; the default returns `false` [@ui-core]. |
| `update_picker(id, update)` | Applies live picker updates when a component owns the matching picker; the default rejects updates [@ui-core]. |
| `picker_id()` | Returns the legacy numeric picker id when present [@ui-core]. |
| `picker_handle()` | Returns a scoped plugin picker handle when the dialog is callback-owned [@ui-core]. |
| `composer_handle()` | Returns a scoped composer handle when the dialog is callback-owned [@ui-core]. |
| `resize(width, height)` | Recomputes component geometry and returns whether redraw is needed; the default is unchanged [@ui-core]. |
| `set_theme(theme)` | Applies current theme styles; the default is a no-op [@ui-core]. |
| `handle_event(event)` | Converts keys, mouse, or paste events into `KeyAction`s; the default closes on Escape or mouse down [@ui-core]. |
| `allows_event_passthrough()` | Indicates whether ordinary editor input may also see events; the default is `false` [@ui-core]. |
| `is_sensitive_input()` | Marks prompts whose contents must not be serialized into traces or logs; the default is `false` [@ui-core]. |
| `cursor_position()` | Returns the terminal cursor position for focused text input; the default has no cursor [@ui-core]. |

## Picker Components

`Picker` is the structured fuzzy picker. Its component implementation accepts `PickerUpdate` values for the matching id, exposes both legacy picker ids and callback handles, resizes to the viewport, applies theme updates, edits its query from key and paste events, navigates history and result lists, and returns either editor actions or plugin callback notifications on selection and cancellation [@picker]. Query editing is grapheme-aware, while query cursor placement uses display width so the terminal cursor follows visible text width [@picker].

`FilePicker` wraps `Picker` with asynchronous workspace discovery. Its `tick` method drains a channel of file-loading results, ignores stale generations, and updates the underlying picker; `Ctrl-E` toggles hidden and ignored entries and starts a new load, while an empty-query `>` opens the command palette [@file-picker]. Drawing, resize, theme updates, and cursor position delegate to the embedded picker [@file-picker].

## Completion UI

`CompletionUI` is the completion menu for LSP items. It draws rows produced by its internal renderer, moves selection with arrow keys, `Tab`, `BackTab`, `Ctrl-J`, `Ctrl-K`, and page keys, applies the selected completion on `Enter`, and can apply a completion with an LSP commit character [@completion]. When filtering leaves no selected item, `Enter` closes the dialog and inserts a newline, while `Esc` closes the dialog and enters Normal mode whether or not candidates are visible [@completion]. It returns `allows_event_passthrough() == true`, so normal typed characters and backspace can continue into editor input while the completion menu updates its filter [@completion].

## Diagnostic Popup

`DiagnosticInfo` is the line-diagnostics popup opened by the editor action behind `ShowLineDiagnostics`. It draws rounded dialog chrome, formats numbered diagnostics with severity-colored message spans, diagnostic codes, multiline wrapping, and related information, then handles close, scroll, resize, and theme changes through the `Component` trait [@diagnostic-info]. The [Diagnostics UI](../../architecture/lsp/diagnostics-ui) page explains how this component is fed from the editor's LSP diagnostic snapshot.

## Confirmation Dialogs

`Confirmation` is the compact accept/cancel dialog used for callback-owned plugin confirmations and editor-owned terminal actions [@confirmation]. It defaults focus to Cancel, closes with the cancel action on `Esc` or `Ctrl-C`, accepts or cancels with `y` and `n`, and returns the selected terminal action on `Enter` [@confirmation]. Button focus moves to Accept with Left, `BackTab`, `h`, or `k`, and moves to Cancel with Right, `Tab`, `j`, or `l`; multiline confirmations keep Up and Down for scrolling instead of button focus changes [@confirmation]. This button-focus contract is scoped to shared `Confirmation` instances; other modal components keep their own component-specific navigation instead of inheriting accept/cancel buttons [@confirmation] [@ui-core]. The dialog returns callback selections through the owning picker handle or explicit editor actions, preserving the same resource-ownership boundary as other modal components [@confirmation] [@ui-core].

## Composer And Prompt Components

`AgentComposer` is a multiline prompt component for agent requests. It exposes a `ComposerHandle` only for callback-owned composers, submits or cancels through either plugin notifications or composer callbacks, supports paste, `Shift-Enter` or `Ctrl-J` for newlines, history navigation, word deletion, cursor movement, resize, theme updates, and cursor positioning over wrapped text [@agent-composer]. It reports `is_sensitive_input() == true`, and it enforces a 128 KiB prompt limit with validation status instead of accepting oversized input [@agent-composer].

`InputPrompt` is a single-line prompt. It can be normal, secret, or callback-owned, masks secret rendering with asterisks, returns submitted values through an action or composer callback, cancels empty submissions, and keeps cursor edits grapheme-aware [@input-prompt]. It exposes `composer_handle()` for callback prompts and computes cursor position differently for masked and unmasked input, using grapheme count for masked values and display width for visible values [@input-prompt].

Editable dialogs accept `Alt-Backspace` (Option-Delete on macOS) and
`Ctrl-Backspace` (Windows) to remove the preceding word. Both shortcuts work
on either platform when the terminal reports the modifier. Selected initial
values in single-line prompts are cleared as a whole. Composers retain
`Ctrl-W`; active search fields edit their own query instead of the underlying
draft. Word boundaries remain whitespace-delimited, including punctuation
and paths within a word.

## Resource Ownership

Plugin-owned pickers and composers are identified by handles returned from `picker_handle()` and `composer_handle()` [@ui-core]. [Plugin resource ownership](../../architecture/plugins/resource-ownership) covers the lifecycle around those handles, while [callback-scoped dialogs](../../concepts/plugins/callback-scoped-dialogs) explains why callback-owned dialogs close by notifying the handle owner instead of sending global plugin events.
