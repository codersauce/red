# Host-managed Codex app-server client

Red will talk to `codex app-server` through a Rust-owned host client exposed to
plugins, rather than having the Codex plugin connect directly from JavaScript.
This keeps app-server discovery, process lifecycle, session handling,
cancellation, streaming, and privileged error reporting inside the editor host,
while the plugin owns the Codex Chat Panel UI and context-selection behavior.
By default Red will start and own the local app-server process, with a
configuration path for connecting to an already-running endpoint during
development, debugging, or future remote setups.
The owned local connection will use a Unix socket transport by default. WebSocket
endpoints are reserved for explicitly configured remote or development
connections where authentication and exposure are intentional.
The Codex Chat Panel may always include lightweight editor metadata, but source
text is attached explicitly by user action rather than automatically dumping
buffers into each message.
Context References are sent to the Codex App Server as turn-scoped
`additionalContext` entries, while the visible Composer text uses placeholder
spans to show compact labels and character counts.
Red will expose a narrow Codex-specific host API to the bundled Codex plugin
first, rather than a generic app-server API for all plugins. The app-server
protocol is broad and unstable, so Red should only commit to the surface needed
for Codex Chat Window behavior until the abstraction proves itself.
The Rust host client translates app-server notifications into a simplified
`red.codex` event stream for the plugin. Raw app-server messages may be logged
for debugging, but plugin code should depend on Red-owned event names rather
than protocol internals.
When Codex changes files, Red treats app-server file-change events as
authoritative hints and reconciles them through normal buffer reload and dirty
checking. Clean open buffers may reload automatically; dirty buffers enter a
conflict state rather than being silently overwritten.
If the owned local Codex App Server disconnects or dies, Red shows a
disconnected state and attempts one automatic restart. If restart fails, the
user must explicitly retry. Composer drafts remain editable, but v1 does not
queue submissions while disconnected.
Red can list and resume previous Codex Threads through app-server methods:
`thread/list` supports filtering by captured `cwd`, `thread/read` can include
turn history, `thread/turns/list` pages older turns, and `thread/resume` resumes
preferably by `threadId`.
For Codex integration, Red resolves the Workspace Root as the Git root when
available and otherwise falls back to Red's current directory. The Workspace Root
is used as the app-server `cwd`, the initial `runtimeWorkspaceRoots` value, and
the key for plugin-owned Codex Thread restore.
A Codex Chat Window is pinned to the Workspace Root it was opened for. Context
commands should warn or ask before attaching text from a different Workspace
Root; v1 does not silently retarget a conversation across projects.
