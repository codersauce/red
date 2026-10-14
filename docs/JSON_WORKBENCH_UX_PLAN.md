# JSON Workbench rendering and keyboard plan

Make JSON Workbench preserve and highlight its JSON preview, provide keyboard access to its actions, and show only the most useful buttons. Reuse Red's panel rendering, input handling, action help, and picker infrastructure with small changes and focused tests.

**Goals**

- Preserve indentation and spacing in the preview and use the active theme's syntax colors.
- Make workbench actions accessible from the keyboard while the panel is focused.
- Show Run, Apply, and Close; show Cancel in place of Run while work is running.
- Preserve navigation, search, selection, window controls, and existing plugin compatibility.
- Keep implementation churn and branching low; verify each step and provide manual test instructions.

**Current implementation**

At commit `8cb693dd`, JSON Workbench sends the original jq output as a `plain` text-panel block. Plain-text wrapping collapses whitespace. Copy, Open result, and Apply use the original output independently of the display.

The panel already supports navigation, search, selection, command entry, and `q` to close. Its compact header labels are clickable abbreviations, not shortcuts. RPC header actions have no shortcut declaration. The implementation work described below has not started.

Sources: [workbench plugin](../examples/rpc-json-workbench/plugin.py), [panel renderer](../src/plugin/panel.rs), [rendering primitives](../src/plugin/markdown.rs), and [RPC protocol](RPC_PLUGIN_PROTOCOL.md#text-panels).

**Code blocks**

Add `format: "code"` with an optional `language` to text-panel blocks. JSON Workbench supplies `language: "json"` and the unmodified jq output. Render the code directly in the panel without Markdown fences or an additional frame.

Reuse the existing syntax highlighter and verbatim wrapping. Preserve indentation, repeated spaces, and blank lines. Long lines wrap to the available width without changing the stored source. Selection across wrapped rows must not introduce extra line breaks or collapse spaces. Document the existing renderer's tab and line-ending presentation rules rather than promising byte-identical screen text.

An omitted or unsupported language produces readable preformatted text. A highlighting failure must not hide the output. Reuse existing rendering caches and invalidation rules for content, width, and theme changes. Language metadata must not cause file access or execution.

**One action definition for keyboard and buttons**

Reuse the existing `UiAction` contract for shortcut, help, priority, and enabled metadata. Extend the text-panel action wrapper only with button visibility, then derive keyboard dispatch, header buttons, the contextual `ActionBar`, and F1 discovery from that one action identity and handler. Actions without a button must remain available from the keyboard or action picker.

Allow action availability and button visibility to change without recreating the panel. This is needed to replace Run with Cancel, disable stale Apply, and preserve focus, selection, and scroll position while work runs. Reuse an existing update path if it can express this safely; otherwise add one bounded action-update operation.

Keep declarations bounded and validate duplicate IDs and conflicting shortcuts. Deliver actions only to the owning connection. Dispose their bindings and queued events when the panel is disposed, replaced, disconnected, or reloaded, following current ownership rules.

**Key precedence**

Workbench shortcuts apply only while its panel is focused, in idle Normal mode. Active search text, visual selection, pending find/jump input, and any composer retain their keys. Preserve built-in navigation, command entry, help, and window controls. Enter follows a link under the cursor before it invokes Explore; Enter in visual selection retains its existing behavior.

**Accepted workbench bindings**

| Key | Action |
| --- | --- |
| `o` | Open the current result in an ordinary editor buffer. |
| `c` | Copy the complete result to the system clipboard. |
| `s` | Return to the pinned source document. |
| `u` | Undo the last workbench Apply transaction. |
| `Enter` | Explore the result when there is no link under the cursor. |
| `q` | Close the workbench, as today. |

Keep `e` for word-end movement, `l` for moving right, and `y` for yanking a selection. Live mode and saved queries remain available through an action picker and their existing commands initially.

**Recommended primary bindings**

Use `r` for Run, `a` for Apply, and `Ctrl-c` for Cancel in this panel. These complete the keyboard-first workflow; they are recommended defaults rather than keys explicitly selected in the preceding discussion. All existing `:Json...` commands remain usable.

Use F1 to expose the panel's actual action bindings and execute its actions through Red's native searchable shortcut surface, including Live mode and saved queries. Do not add a parallel JSON-specific action picker or command.

**Visible controls and state**

The header shows the title followed by Run, Apply, and the conventional close control. While a query is active, Cancel occupies Run's place. Apply is enabled only when there is a valid, current result; the existing revision and transaction checks remain authoritative if state changes before dispatch.

Explore, Open result, Copy, Source, Live mode, saved queries, and Undo do not receive header buttons. At narrow widths, omit lower-priority buttons as whole controls and keep keyboard/action-picker access; do not reduce labels to initials. Preserve a usable title and close affordance where space permits.

**Action discovery**

Use Red's existing `F1` shortcut surface as both the complete contextual reference and the searchable, executable action picker. Panel actions must appear there with their current label, shortcut, availability, and description; choosing an entry invokes the same action ID as its key or button. Actions without a direct shortcut, including Live mode and saved queries initially, remain executable from this picker.

Render a one-row contextual action hint at the bottom of a focused action panel. For JSON Workbench it prioritizes `Enter Explore`, `o Open`, and `c Copy`, followed by Red's existing responsive `F1` overflow marker. Search and Visual modes replace or suppress this hint with their own mode help, and narrow panes drop complete low-priority actions rather than clipping labels or keys.

Header buttons retain readable labels. F1 and the contextual hint disclose shortcuts, so this slice does not add mouse-hover tooltips. Red currently discards pointer-move events when no dialog owns them; adding global hover state and repaints is not justified when the keyboard-first discovery surfaces already cover the requirement.

**Compatibility**

Expose these capabilities through the shared panel implementation so Husk and JSON-RPC plugins can use them. Keep `plain` and `markdown` behavior and existing action declarations compatible. New action metadata uses defaults that preserve existing clickable headers. The new panel shortcuts do not become global editor bindings.

Version the additive contract starting at host API `0.27.0`, update the canonical API metadata and protocol documentation, retain support for previous API contracts, and update JSON Workbench's requirement to the earliest contract it actually needs. If the steps ship independently, follow the repository's versioning policy for each shipped addition instead of modifying a released contract in place.

**Implementation order and acceptance**

1. Add code blocks and switch the JSON preview. Verify nested JSON indentation, highlighting, unknown-language fallback, narrow wrapping, and selection/copy text. Use the existing workbench protocol test to check that it sends original jq output.
2. Add shared panel action bindings, state updates, F1 discovery, and contextual hints. Verify focus and mode precedence, Enter-on-link behavior, disabled actions, hidden-button actions, searchable execution, responsive hints, and cleanup/owner isolation. Cover the smallest renderer and RPC boundaries needed to establish those behaviors.
3. Wire the workbench bindings, picker, and reduced header. Derive availability from existing workbench state. Verify Run/Cancel, Apply/Undo, Open, Copy, Source, Explore, stale preview handling, and coexistence with a Husk panel.

For each step, run focused tests and relevant checks, inspect the branching and churn, and visually exercise the changed behavior in a real Red session. Run `cargo clippy --all-targets --all-features -- -D warnings` before any push of Rust changes. When implementation is requested, retain the established delivery convention: commit each validated step and report what changed, total churn, complexity findings, remaining work, and useful manual tests. Saving this plan does not publish or implement it.

**Manual acceptance**

Run the sample active-users query and check indentation and JSON colors. Focus the panel with `Ctrl-w l`; exercise the agreed shortcuts, then repeat while searching or selecting text to confirm that those modes retain control. Follow the source link with Enter, and explore the result with Enter away from a link. Start a slow query and cancel it; verify Apply is unavailable for a stale preview. Resize the panel, use F1 and the action picker, and reload the plugin to check cleanup while ordinary result buffers remain open.
