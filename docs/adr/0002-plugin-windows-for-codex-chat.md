# Plugin windows for Codex chat

Red will model Codex chat as a Plugin Window in the normal split tree, rather
than as a Plugin Panel that reserves global side space. This matches the editor
window mental model used by modal editors: the Codex Chat Window can be focused,
resized, navigated to, and closed like other splits, while still rendering
plugin-owned transcript and composer content instead of a text buffer.
Plugin Windows will be represented as a distinct split-tree leaf kind rather
than as hidden synthetic buffers, so buffer-specific editing and file lifecycle
rules do not leak into plugin-owned UI.
Session restore will persist Plugin Window layout placeholders, while the owning
plugin restores its own content and backing conversation state. If a plugin is
unavailable, Red may drop the leaf or show an unavailable-plugin placeholder
instead of manufacturing a text buffer.
Red remains responsible for global focus and key routing, but a focused Plugin
Window may own a local Plugin Input Mode for interactions such as transcript
navigation versus composer editing.
The Codex Chat Window starts with transcript/navigation mode and composer
editing mode. Transcript navigation supports `j`/`k`, arrow keys, `Ctrl-f` /
`Ctrl-b`, and PageUp/PageDown. Composer editing accepts regular text input,
supports multiline prompt editing from the first version, uses a deliberate
newline command for multiline input, Escape returns to transcript navigation,
and `Ctrl-c` cancels an in-flight Codex turn. Enter submits the Composer;
Shift+Enter, `Ctrl-j`, and Alt+Enter insert newlines. Red-level window
navigation remains available from transcript/navigation mode.
When the terminal cannot distinguish Shift+Enter or Alt+Enter from Enter,
`Ctrl-j` is the guaranteed newline fallback and the Codex Chat Window should
only display key hints that are actually available.
Codex integration will use a richer Plugin Command registry with stable string
IDs plus display metadata such as title, category, description, keybinding
suggestions, and context requirements. Existing simple command registration can
remain as shorthand for backward compatibility.
Context Reference commands attach visible context entries to the Composer draft
rather than immediately sending a Codex message. Submission remains a deliberate
Composer action.
Context References snapshot their text when attached and keep source metadata
such as path, line range, buffer version, and dirty state so the user is not
surprised by later edits changing what gets sent.
Large Context References and large pasted content should render in the Composer
as Context Placeholders, for example `[Pasted Content 2045 chars]`, while still
sending the full snapshotted content to the Codex App Server. This follows the
Codex app-server protocol shape: user-visible text can contain placeholder
spans, while full content is carried as additional turn context.
The bundled Codex plugin will be authored in TypeScript. It owns the Codex Chat
Window render model, while Rust owns terminal drawing primitives, wrapping,
clipping, focus borders, and key routing.
Plugin Windows will use a structured semantic render model rather than raw cell
buffers. The Codex Chat Window model includes title/status, transcript blocks,
Composer text and cursor state, Context Placeholder spans, scroll state, and
optional key-hint actions; Rust renders that model using Red's terminal and
theme constraints.
The Plugin Window container is generic, but the first supported content kind is
`chat`. Red should avoid designing arbitrary plugin UI layout until additional
window content kinds prove necessary.
Codex approval and user-input requests render inline as interactive Transcript
blocks inside the Codex Chat Window. The plugin owns command-level actions such
as approve, approve for session, decline, cancel, and answer from the Composer;
Rust owns the pending request registry and sends the resolved response back to
the app-server.
The Codex Chat Window may expose a Follow Changes option that keeps the editor
view synchronized with files currently being changed by Codex.
When Follow Changes is enabled, Red reuses the active Editor Window as a preview
target, switches it to the latest Codex-touched file, and scrolls to the latest
changed hunk without stealing focus from the Codex Chat Window. Rapid updates
should be debounced and reflected in Codex Chat Window status.
Follow Changes is off by default and exposed through an easy toggle plus a
bindable Plugin Command such as `codex.toggleFollowChanges`.
The `codex.open` Plugin Command opens or focuses the Codex Chat Window. Context
Reference commands open or focus it when needed so the Composer shows attached
context before submission. Session restore only reopens Codex when the restored
layout included a Codex Chat Window.
Codex Chat Window state is root-scoped. Each resolved workspace root maps to a
deterministic Plugin Window ID and plugin storage key, allowing independent chat
windows, drafts, follow state, pending requests, and thread IDs for different
workspace roots while preserving legacy single-window storage as a read fallback.
Closing the Codex Chat Window hides the view but does not cancel an Active Codex
Turn. Cancellation is explicit through `codex.cancel` or the Codex Chat Window's
cancel keybinding, and `codex.open` reattaches to any running conversation.
The `codex.resume` Plugin Command opens a picker filtered to the current
workspace root by default, sorted by latest update. Picker rows should show the
Codex Thread name or preview, status, last updated time, and source. Selecting a
thread resumes it into the Codex Chat Window and loads recent turns into the
Transcript.
Plugin restore may resume the specific Codex Thread ID previously owned by the
Codex plugin for the project. Plain `codex.open` must not indiscriminately
auto-resume the latest historical thread for the workspace; explicit
`codex.resume` handles that.
The Codex plugin persists its project-owned Codex Thread ID in plugin storage
keyed by workspace root, including at least the thread ID, session ID, workspace
root, and last update time. Restore validates the stored thread before
reattaching and falls back to an empty conversation if it is unavailable.
The first implementation milestone is a vertical slice: Plugin Window leaf
support, the `chat` render model and key routing, a bundled TypeScript Codex
plugin, a narrow `red.codex` host API for app-server connection/thread/turn
streaming/cancel/request resolution, Composer and Transcript rendering,
`codex.open`, `codex.cancel`, context attachment commands, Workspace Root
thread restore, explicit session resume, Follow Changes, and inline app-server
request handling. Remaining work should focus on richer approval UI affordances,
changed-hunk centering polish, and stronger terminal smoke coverage for a live
app-server turn.
