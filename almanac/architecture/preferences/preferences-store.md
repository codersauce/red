---
title: "Preferences Store"
summary: "Red's preferences store persists convenience state such as command history, picker history, and plugin-owned JSON without becoming recovery state."
topics: [architecture, preferences, plugins, history]
sources:
  - id: preferences
    type: file
    path: src/preferences.rs
  - id: startup
    type: file
    path: src/main.rs
  - id: editor
    type: file
    path: src/editor.rs
---

# Preferences Store

The preferences store is Red's best-effort persistence layer for convenience state. It keeps command-line history, picker query history, and plugin-owned JSON values in `preferences.json`, while session recovery and crash-safe editor state remain owned by a separate snapshot system [@preferences] [@startup]. This boundary lets editor features remember recent user choices without treating a corrupt or unavailable preferences file as a startup failure.

## Ownership Boundary

`PreferencesStore` owns a serialized `Preferences` value plus an optional filesystem path [@preferences]. Interactive startup loads it from `Config::path("preferences.json")` after configuration, logging, and theme setup, then passes the store into `Editor::new_with_preferences` [@startup]. Tests and embedded callers can use `PreferencesStore::in_memory`, which gives the same mutation semantics without filesystem writes [@preferences].

The stored fields are deliberately narrow. `command_history` is a list of colon commands, `picker_history` is a map from picker namespace to recent queries, and `plugin_storage` is a JSON map keyed by plugin and logical key [@preferences]. The editor reads these values for command history navigation, picker history, agent transcript restoration, and plugin host storage requests [@editor].

## Histories

Command history is stored from oldest to newest. `record_command` ignores blank commands, skips only a duplicate of the newest entry, caps the list at 100 entries, and saves immediately for filesystem-backed stores [@preferences]. The editor records executed command-line commands through this API and uses prefix-filtered history navigation when the user moves through command history [@editor].

Picker history is also namespace-scoped and bounded to 100 entries per key [@preferences]. `record_picker_query` ignores blank keys or blank queries, skips consecutive duplicates within the same namespace, and persists immediately [@preferences]. The editor derives picker keys from picker title and optional ID, exposes stored history to picker UI, records accepted picker queries, and removes the legacy agent composer history namespace `picker:802` when the modern agent composer opens [@editor].

## Plugin Storage

Plugin storage is a simple JSON value store. `set_plugin_storage(plugin, key, value)` stores under the internal string key `{plugin}:{key}` and persists immediately, while `plugin_storage(plugin, key)` reads the same compound key [@preferences]. The editor scopes plugin storage requests further before calling the preferences store, so plugin-facing storage remains plugin-owned even though the underlying file is one shared JSON document [@editor].

Legacy imports run opportunistically during filesystem-backed load. Red checks `state/plugins/session_restore.json` for `latest` and `state/plugins/project_search.json` for either `historyByCwd` or `history_by_cwd`, then imports those values into `session_restore:latest` and `project_search:history_by_cwd` only when the current preferences file does not already contain the target key [@preferences]. If import changed the store, Red saves the new preferences file [@preferences].

## Failure And Filesystem Safety

Preferences are not startup-critical. A missing file loads as empty preferences, and unreadable or malformed data logs a message when a logger exists and then falls back to empty state [@preferences]. The architecture therefore preserves editor startup even when this convenience file is damaged.

On Unix, reads and writes use `O_NOFOLLOW` and `O_NONBLOCK`, require the target to be a regular file, and set permissions to `0600` [@preferences]. Writes create parent directories, serialize pretty JSON, open with `0600`, truncate through `set_len(0)`, and write the new contents [@preferences]. The tests cover owner-only agent transcript writes, permission tightening for existing files, and refusal to follow a symlink that points outside the preferences path [@preferences].

This store is adjacent to [Runtime Lifecycle](../startup/runtime-lifecycle), which loads it during startup, and to plugin host request handling, which uses it for plugin-owned values.
