---
title: "UI Components"
summary: "Modal UI components share a Component trait for drawing, event handling, resize/theme updates, plugin handles, cursor placement, and sensitive input reporting."
topics: [editor, ui, plugins]
sources:
  - id: ui-core
    type: file
    path: src/ui/mod.rs
  - id: picker
    type: file
    path: src/ui/picker.rs
  - id: completion
    type: file
    path: src/ui/completion.rs
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

`CompletionUI` is the completion menu for LSP items. It draws rows produced by its internal renderer, moves selection with arrow keys, `Tab`, `BackTab`, `Ctrl-J`, `Ctrl-K`, and page keys, applies the selected completion on `Enter`, and can apply a completion with an LSP commit character [@completion]. It returns `allows_event_passthrough() == true`, so normal typed characters and backspace can continue into editor input while the completion menu updates its filter [@completion].

## Composer And Prompt Components

`AgentComposer` is a multiline prompt component for agent requests. It exposes a `ComposerHandle` only for callback-owned composers, submits or cancels through either plugin notifications or composer callbacks, supports paste, `Shift-Enter` or `Ctrl-J` for newlines, history navigation, word deletion, cursor movement, resize, theme updates, and cursor positioning over wrapped text [@agent-composer]. It reports `is_sensitive_input() == true`, and it enforces a 128 KiB prompt limit with validation status instead of accepting oversized input [@agent-composer].

`InputPrompt` is a single-line prompt. It can be normal, secret, or callback-owned, masks secret rendering with asterisks, returns submitted values through an action or composer callback, cancels empty submissions, and keeps cursor edits grapheme-aware [@input-prompt]. It exposes `composer_handle()` for callback prompts and computes cursor position differently for masked and unmasked input, using grapheme count for masked values and display width for visible values [@input-prompt].

## Resource Ownership

Plugin-owned pickers and composers are identified by handles returned from `picker_handle()` and `composer_handle()` [@ui-core]. [Plugin resource ownership](../../architecture/plugins/resource-ownership) covers the lifecycle around those handles, while [callback-scoped dialogs](../../concepts/plugins/callback-scoped-dialogs) explains why callback-owned dialogs close by notifying the handle owner instead of sending global plugin events.
