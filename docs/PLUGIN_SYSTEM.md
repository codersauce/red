# Husk Plugin System

Red uses Husk as its embedded scripting language. Plugins are `.hk` files loaded by the Rust editor process through the `husk` workspace crate.

## Lifecycle

Every plugin may define these functions:

```rust
pub fn activate() {
    red::add_command("HelloWorld", hello_world);
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

The initial native Husk host module is intentionally small:

| Function | Purpose |
|----------|---------|
| `red::add_command(name, callback)` | Register a command callable from `{ PluginCommand = "Name" }` keymaps |
| `red::on(event, callback)` | Subscribe to editor events |
| `red::execute(action, ...)` | Call a Rust host action |
| `red::log(...)` | Write to Red's log |

Supported `red::execute` actions currently include `Print`, `FilePicker`, `ClearSearchHighlight`, `RefreshDiagnostics`, `Refresh`, `ShowDialog`, `CloseDialog`, `GoToDefinition`, `Hover`, `ViewLogs`, and `ListPlugins`.

The VM passes event payloads as `Json`. Rich typed wrappers are a follow-up; the v1 bridge keeps payloads dynamic while the host API settles.

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

## Current Porting Status

Bundled `.hk` plugins are present so the editor boots through Husk. The first pass ports command and lifecycle registration; the deeper UI/process-heavy behavior from the old JS plugins still needs native Husk host functions.

Use this order for the next implementation passes:

1. Buffer/theme/session basics.
2. Search and LSP event helpers.
3. Window bar, overlays, gutter signs, and panels.
4. Filesystem, process permissions, project search, and Git workflows.

## Validation

Run:

```shell
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --self-check
cargo run -- --runtime-files
```

`red --runtime-files` should list `.hk` plugins only.
