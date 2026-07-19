# Husk Plugin System

Red uses Husk as its embedded scripting language. Plugins are `.hk` files loaded by the Rust editor process through the `husk` workspace crate.

## Lifecycle

Every plugin may define these functions:

```rust
pub fn activate() {
    red::add_command("HelloWorld", hello_world, Json {
        title: "Say hello",
        category: "Example",
        description: "Print a greeting",
        aliases: ["greeting"],
    });
    red::on("editor:ready", ready);
}

fn hello_world() {
    red::execute("Print", "Hello from Husk");
}

fn ready(event: Json) {
    red::log("ready");
}

pub fn before_exit(snapshot: Json) {
    red::log("saving plugin state");
}

pub fn deactivate() {
    red::log("plugin stopped");
}
```

`activate` runs when Red initializes plugins. `before_exit` and `deactivate` are optional.

## Host API

Husk plugins use the versioned native `red` host module:

| Function | Purpose |
|----------|---------|
| `red::add_command(name, callback[, metadata])` | Register a command callable with `:Name`, from `{ PluginCommand = "Name" }` keymaps, or through the command palette |
| `red::on(event, callback)` | Subscribe to editor events |
| `red::execute(action, ...)` | Call a fire-and-forget Rust host action |
| `red::request(action, callback, ...)` | Issue a one-shot request and invoke the callback with its payload |
| `red::log(...)` | Write to Red's log |

Execute and request actions cover editor state and edits, dialogs, pickers and agent composers, panels and workspace views, overlays and gutter signs, timers, filesystem watches, permitted processes, LSP helpers, and agent/recovery actions. The canonical signatures and compatibility policy live in [PLUGIN_API.md](PLUGIN_API.md) and [`src/plugin/host_api.json`](../src/plugin/host_api.json); use those rather than copying an incomplete action list from prose.

Direct `:Name` invocation requires an exact, case-sensitive registered name and does not
currently pass arguments to the callback. Built-in commands and their abbreviations take
precedence over plugin commands with the same name.

The optional command metadata object accepts `title`, `category`, `description`,
and `aliases`. All fields are optional; `aliases` is an array of additional
search terms, not alternate colon commands. When present, metadata is shown in
the command palette with the command's effective keymaps and its exact `:Name`
invocation. Existing two-argument registrations remain valid.

Use `red::request` for actions that return a value:

```rust
fn ready(event: Json) {
    red::request("GetConfig", config_loaded, "cwd");
}

fn config_loaded(result: Json, request_id: i32) {
    red::log("cwd", result.value);
}
```

The callback is removed after the first response. Its second argument is the opaque request ID returned by `red::request`; plugins may retain that ID only to ignore stale responses. `red::on` remains for durable editor events and resource-scoped events such as picker, composer, timer, watcher, and process notifications. Numeric request/response event names are not part of the host API.

The VM passes event payloads as `Json`. Rich typed wrappers are a follow-up; the v1 bridge keeps payloads dynamic while the host API settles.

### Text panels

Text panels keep conversation content as source-backed blocks instead of flattening it into selectable rows. A block has a stable `id`, a `user`, `agent`, `error`, or `text` kind, a `plain` or `markdown` format, and its original `text`:

```husk
red::execute("CreateTextPanel", "assistant", PanelConfig {
    side: "right",
    width: 52,
    title: "Assistant",
    composer: Json { placeholder: "Ask a follow-up…", rows: 3 },
    header_actions: [
        Json { id: "clear", label: "Clear", compact_label: "C" },
        Json { id: "close", label: "×", compact_label: "×" },
    ],
});
red::execute("UpdateTextPanel", "assistant", [
    TextPanelBlock {
        id: "answer:1",
        kind: "agent",
        format: "markdown",
        text: "# Ready\n\nAsk a question.",
    },
]);
red::execute("AppendTextPanel", "assistant", "answer:1", "\n\nMore detail.");
```

Markdown is rendered semantically and both plain and Markdown blocks wrap to the panel width. New blocks and streamed appends follow the tail until the user scrolls away; `j`/`k`, the arrow keys, `PageUp`/`PageDown`, `Ctrl-b`/`Ctrl-f`, `g`/`G`, and the mouse wheel navigate a focused text panel without disturbing the source text. Configured header actions are clickable and emit their action ID through `panel:event:<id>`; they compact automatically on narrow panels. `SetPanelVisible(id, false)` temporarily removes a panel from the layout without losing its content or composer draft, and `SetPanelVisible(id, true)` restores it.

## Runtime Architecture

The workspace now contains Red plus Husk crates:

```text
Cargo.toml
crates/husk
crates/husk-ast
crates/husk-lexer
crates/husk-parser
crates/husk-semantic
crates/husk-types
```

`crates/husk` owns `Vm`, `Program`, `Value`, `Callback`, and the `Host` trait. Red implements `Host` in `src/plugin/runtime.rs`, and `src/plugin/registry.rs` loads plugin source directly instead of generating JavaScript modules.

The old Deno runtime, TypeScript definitions, JS transpilation, and JS module loader have been removed from the runtime path.

## Bundled Plugin Status

All thirteen bundled plugins run through Husk and exercise the production host bridge.
They include editor-state and theme consumers (`buffer_picker`, `theme_browser`,
`barbecue`), event-driven decorations (`cool_search`, `fidget`, `indent_guides`,
`inlay_hints`), LSP pickers (`lsp_symbols`), watched panels and permitted processes
(`neotree`, `project_search`, `git`), core-backed recovery (`session_restore`), and
the ACP/proposal UI (`agent`). The [README plugin table](../README.md#bundled-plugins-and-themes)
is the concise capability inventory; the bundled `.hk` sources are working examples.

`buffer:changed`, cursor, mode, viewport, file, theme, window, LSP, timer, picker, composer, panel, process, filesystem, workspace, and agent events are emitted by the production runtime. Subscribe only to the events a plugin needs and debounce expensive work.

## Validation

Run:

```shell
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --self-check
cargo run -- --runtime-files
```

`red --runtime-files` should list `.hk` plugins only.
