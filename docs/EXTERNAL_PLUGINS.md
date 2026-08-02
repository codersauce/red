# External plugins

Red loads external plugins from isolated package directories under
`$XDG_CONFIG_HOME/red/plugins` (or the platform-equivalent Red configuration
directory). A package contains a `red-plugin.toml` manifest and either a Husk
entrypoint, a native companion, or both.

The editor remains the authority for buffers, undo history, UI focus, sessions,
and user confirmations. Plugins request attributed operations through the
versioned host API; they do not edit Red's internal state directly.

The same manager is available in the editor through `:plugins`. It opens
immediately from local installation records and supports install, update,
enable/disable, and data-preserving removal. The destructive purge remains an
explicit CLI operation. Network and build work runs after selection rather than
during editor startup.

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
- `CompanionCall` uses bounded JSON-lines frames, monotonic request IDs,
  timeouts, cancellation, lazy startup, and editor-shutdown supervision.
- Plugin storage is namespaced and co-snapshotted without interpreting unknown
  values.
- Package discovery reads local records only. Updates occur only when the user
  asks for them.
