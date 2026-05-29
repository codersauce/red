# Red Editor

Red is a modal text editor with an extensible plugin surface for editor-adjacent
tools and UI.

## Language

**Editor Window**:
An editor split that displays a text buffer and participates in the editor's
window layout, focus, navigation, and persisted window state.
_Avoid_: Pane, panel

**Plugin Window**:
A split-tree window whose layout, focus, navigation, and close behavior match an
Editor Window, but whose content is rendered and controlled by a plugin rather
than a text buffer.
_Avoid_: Pane, panel

**Plugin Panel**:
A plugin-owned side UI region that reserves space to the left or right of the
editor windows without becoming a text buffer window.
_Avoid_: Pane, window

**Codex Chat Window**:
A Plugin Window for an interactive Codex Conversation, opened by default as a
right-side vertical split beside the active Editor Window.
_Avoid_: Codex pane, Codex Chat Panel

**Transcript**:
The scrollback area of the Codex Chat Panel that displays conversation turns and
streams new Codex output toward the bottom.
_Avoid_: Text stream, log

**Composer**:
The focused input area of the Codex Chat Window where the user drafts and
submits the next message in a Codex Conversation.
_Avoid_: Chat box, prompt box, command line

**Multiline Composer**:
A Composer that supports editing prompts across multiple lines before
submission.
_Avoid_: Single-line prompt

**Plugin Input Mode**:
A plugin-owned local input state for a focused Plugin Window, used after Red has
routed focus and key events to that window.
_Avoid_: Editor mode, submode

**Plugin Command**:
A command registered by a plugin that Red can expose for keybinding, command
discovery, and invocation.
_Avoid_: Callback, action

**Context Reference**:
An explicit reference to editor-owned material attached to a Codex Conversation
message, such as the current file, current line, selection, diagnostics, or diff.
_Avoid_: Attachment, implicit context

**Context Placeholder**:
A compact Composer rendering of a Context Reference or pasted content that hides
large text behind a label and character count while preserving the full content
for submission.
_Avoid_: Truncation, summary

**Follow Changes**:
A Codex Chat Window option that keeps the editor view synchronized with files
currently being changed by Codex.
_Avoid_: Auto-open, live preview

**Active Codex Turn**:
A Codex Conversation turn that is currently running or awaiting user input,
independent of whether its Codex Chat Window is visible.
_Avoid_: Background job, task

**Codex Thread**:
The app-server representation of a resumable Codex Conversation, identified by a
thread ID and associated with a captured working directory.
_Avoid_: Session, chat

**Workspace Root**:
The project root Red uses for Codex Thread ownership and app-server working
directory selection; normally the Git root, falling back to Red's current
directory when no Git root is available.
_Avoid_: LSP root, project folder

**Codex App Server**:
The Codex service endpoint that owns Codex conversations and streams responses
to editor integrations.
_Avoid_: Codex CLI, subprocess

**Codex Conversation**:
A persistent Codex chat thread associated with the current workspace root.
_Avoid_: Chat, session, request

**Editor Context**:
The editor state and selected source material that may be attached to a Codex
Conversation message.
_Avoid_: Prompt context, workspace dump
