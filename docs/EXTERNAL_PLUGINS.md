# External plugins

Red loads external plugins from isolated package directories under
`$XDG_CONFIG_HOME/red/plugins` (or the platform-equivalent Red configuration
directory). A package contains a `red-plugin.toml` manifest and a Husk
entrypoint, a standalone JSON-RPC process, a native companion, language
definitions, or one of the supported combinations.

The editor remains the authority for buffers, undo history, UI focus, sessions,
and user confirmations. Plugins request attributed operations through the
versioned host API; they do not edit Red's internal state directly.

The same manager is available in the editor through `:plugins` or **Language
packs** in the command palette. It opens immediately from local installation
records, then fetches the curated catalog in the background. It supports
catalog and custom installs, update, native-grammar trust, enable/disable, and
data-preserving removal. The destructive purge remains an explicit CLI
operation. Network and build work runs after selection rather than during
editor startup.

Packages using companion RPC or document transactions declare:

```toml
[plugin]
red_api = "^0.6.0"
```

## Development install

```console
red plugin install --path ~/code/my-red-plugin
red plugin list
```

Language-only packages can also be installed from the catalog:

```console
red plugin catalog
red plugin install --catalog go-language
```

Catalog packages are immutable release bundles with catalog-recorded checksums.
Custom `owner/repository` sources remain supported and are presented as
unreviewed. Neither source type receives implicit approval to execute a native
Tree-sitter grammar.

Path installs are linked through an installation record, so editing a package
and restarting or reloading Red picks up the local source without copying it.
User keymaps override defaults declared by a package.

Plugin keymaps may share a leader, such as `Space R g` and `Space R n`, but a
package cannot bind both a leader (`Space R`) and one of its descendants
(`Space R g`). Install and update reject these ambiguous declarations instead
of silently dropping one binding.

## Husk entrypoint

Prefer declaration-local attributes over registration-only `activate` blocks:

```husk
struct PluginState {
    enabled: bool,
}

#[red::state]
fn initial_state() -> PluginState {
    return PluginState { enabled: true };
}

#[red::command(
    name = "MyPlugin",
    title = "Open my plugin",
    category = "Extensions",
)]
fn open() {
    let state: PluginState = red::state();
    if state.enabled {
        red::execute("Print", "Plugin is ready");
    }
}

#[red::config("plugin_config")]
fn configuration_loaded(event: Json) {
    red::state_patch(PluginState {
        enabled: event.value.my_plugin.enabled,
    });
}

#[red::on("editor:ready")]
fn editor_ready(event: EmptyEvent) {}

#[red::lifecycle("deactivate")]
fn stop_background_work() {}
```

Use `visible = false` on `#[red::command(...)]` for directly callable commands
that should stay out of the palette and colon completion. Keep imperative
`red::on(event_name, handler)` for process IDs, filesystem watch IDs, or other
event names that are only known at runtime. Existing keyed state, imperative
registration, and conventionally named lifecycle functions remain compatible.
Prefer sparse `red::state_patch(PluginState { field: value })` updates over
replacing the complete state record, especially when other fields contain
larger result collections.

## Standalone JSON-RPC process

The optional `[rpc]` section declares a language-agnostic plugin process:

```toml
[plugin]
id = "document-tools"
name = "Document Tools"
version = "0.1.0"
red_api = "^0.19.0"

[rpc]
command = "python3"
args = ["plugin.py"]
```

A bare command such as `python3` resolves through `PATH`. A command containing
a path separator, such as `bin/document-tools`, must name an existing file
inside the package. Red sets the process working directory to the package root
and passes arguments directly without a shell.

RPC processes inherit only Red's baseline home, path, locale, terminal,
temporary-directory, XDG, SSH-agent, and Windows variables. A user can grant
additional variables under the configured plugin owner; the package cannot
grant itself access:

```toml
[plugin_permissions.document_tools]
inherit_env = ["DOCUMENT_TOOLS_TOKEN"]
```

This environment filtering reduces accidental exposure and is not a native
process sandbox.

An RPC package cannot also declare a Husk entrypoint or `[companion]`.
Language definitions may accompany any supported executable modality. Existing
Husk, companion, and language-only manifests retain their current meaning.

See the [JSON-RPC plugin protocol](RPC_PLUGIN_PROTOCOL.md) for framing,
lifecycle, capabilities, and the bounded version 1 surface. Declared commands
appear in colon completion and the command palette before activation. Invoking
one starts the process, validates its command metadata, and sends the typed
`command/execute` request. Active plugins can call `red/DocumentSnapshot`,
`red/DocumentApply`, and `red/DocumentUndo` to read and transactionally edit
open documents through Red's existing revision, dirty-state, undo, and LSP
paths. They can also create, replace the content of, focus, and dispose
host-rendered text panels with connection-owned IDs, and set or clear one
cached statusline contribution. Declared `editor:ready`, `file:opened`, and
`file:saved` events arrive as typed `event/notify` notifications; other editor
and callback events remain outside protocol version 1.

## Lifecycle

Plugins may be enabled, disabled, updated, and removed:

```console
red plugin disable replay
red plugin enable replay
red plugin update replay
red plugin remove replay
red plugin remove replay --purge
```

Ordinary removal preserves namespaced plugin data for later reinstall. `--purge`
also removes that data. Install and update stage a complete package, validate
compatibility and checksums, and atomically replace the active package. Failed
updates leave the previous installation intact.

## Manifest

```toml
schema_version = 1

[plugin]
id = "my-plugin"
name = "My Plugin"
version = "0.1.0"
red_api = "^0.6.0"
husk_manifest = "husk/Husk.toml"

[activation]
commands = ["MyPlugin"]

[companion]
command = "bin/my-plugin"

[companion.commands]
x86_64-pc-windows-msvc = "bin/my-plugin.cmd"
```

Husk packages are compiled during install before an installation record is
replaced. Companions start lazily on the first `CompanionCall`; listing plugins
and opening Red do not start them. Release packages may omit `command` and
provide per-target `artifacts` with HTTPS GitHub URLs and SHA-256 digests.

Packages extracted from Red can declare migration without adding product
knowledge to the host:

```toml
[migration.legacy_session_fields]
old_top_level_field = "private_storage_key"
```

Unknown legacy fields remain in the session snapshot. When a compatible package
is installed and enabled, Red copies the declared value once into that
package's private storage. The package owns validation and conversion.

## Host-owned safety boundaries

- `DocumentSnapshot`, `DocumentApply`, and `DocumentUndo` preserve editor
  revisions, preimages, attributed transactions, dirty tracking, and LSP
  notifications.
- RPC text panels are capped per connection, use private host IDs, and are
  disposed when their owner disconnects or reloads.
- RPC statusline contributions are cached by the host and cleared when their
  owner disconnects or reloads; rendering never waits for plugin IPC.
- `CompanionCall` uses bounded JSON-lines frames, monotonic request IDs,
  timeouts, cancellation, lazy startup, and editor-shutdown supervision.
- Plugin storage is namespaced and co-snapshotted without interpreting unknown
  values.
- Package discovery reads local records only. Updates occur only when the user
  asks for them.
