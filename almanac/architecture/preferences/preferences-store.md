---
title: "Preferences Store"
summary: "Red's preferences store persists convenience state such as command and search history, picker history, plugin-owned JSON, and onboarding progress without becoming recovery state."
topics: [architecture, persistence, preferences, plugins, history, onboarding]
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

The preferences store is Red's best-effort persistence layer for convenience state. It keeps command-line history, search history, picker query history, plugin-owned JSON values, first-run welcome state, Learn Red completion, and tutorial progress in `preferences.json`, while session recovery and crash-safe editor state remain owned by a separate snapshot system [@preferences] [@startup]. This boundary lets editor features remember recent user choices without treating a corrupt or unavailable preferences file as a startup failure.

## Ownership Boundary

`PreferencesStore` owns a serialized `Preferences` value plus an optional filesystem path [@preferences]. Interactive startup loads it from `Config::path("preferences.json")` after configuration, logging, and theme setup, then passes the store into `Editor::new_with_preferences` [@startup]. Tests and embedded callers can use `PreferencesStore::in_memory`, which gives the same mutation semantics without filesystem writes [@preferences].

The stored fields are deliberately narrow. `command_history` is a list of colon commands, `search_history` holds patterns shared by `/` and `?`, `picker_history` is a map from picker namespace to recent queries, `plugin_storage` is a JSON map keyed by plugin and logical key, and the onboarding fields record Learn lesson ids, whether the welcome panel received an explicit decision, and the optional tutorial progress object [@preferences]. The editor reads these values for prompt history navigation, picker history, agent transcript restoration, plugin host storage requests, welcome-panel gating, Learn Red completion checks, and tutorial resume behavior [@editor].

## Histories

Command history is stored from oldest to newest. `record_command` ignores blank commands, skips only a duplicate of the newest entry, caps the list at 100 entries, and saves immediately for filesystem-backed stores [@preferences]. The editor records executed command-line commands through this API and uses prefix-filtered history navigation when the user moves through command history [@editor].

Search history uses the same ordering, limit, consecutive-duplicate handling, and immediate persistence. `record_search` ignores empty patterns but preserves meaningful whitespace. The editor records non-empty patterns on Enter, including invalid patterns and searches with no matches; cancellation does not add an entry. Both `/` and `?` use prefix-filtered Up/Down or Ctrl-p/Ctrl-n navigation and restore the original draft when moving past the newest match [@preferences] [@editor].

Picker history is also namespace-scoped and bounded to 100 entries per key [@preferences]. `record_picker_query` ignores blank keys or blank queries, skips consecutive duplicates within the same namespace, and persists immediately [@preferences]. The editor derives picker keys from picker title and optional ID, exposes stored history to picker UI, records accepted picker queries, and removes the legacy agent composer history namespace `picker:802` when the modern agent composer opens [@editor].

## Plugin Storage

Plugin storage is a simple JSON value store. `set_plugin_storage(plugin, key, value)` stores under the internal string key `{plugin}:{key}` and persists immediately, while `plugin_storage(plugin, key)` reads the same compound key [@preferences]. The editor scopes plugin storage requests further before calling the preferences store, so plugin-facing storage remains plugin-owned even though the underlying file is one shared JSON document [@editor].

Legacy imports run opportunistically during filesystem-backed load. Red checks `state/plugins/session_restore.json` for `latest` and `state/plugins/project_search.json` for either `historyByCwd` or `history_by_cwd`, then imports those values into `session_restore:latest` and `project_search:history_by_cwd` only when the current preferences file does not already contain the target key [@preferences]. If import changed the store, Red saves the new preferences file [@preferences].

## Onboarding Progress

Onboarding progress is preference state, not recovered editor state. `learn_completed_lessons` records stable, versioned Learn Red lesson ids; `welcome_completed` records that the first-run welcome has already received an explicit user decision; and `tutorial_progress` stores the optional versioned guided or quick tutorial position [@preferences]. The editor uses those fields to avoid reopening the startup welcome after a decision, to mark the Learn hub's completed lesson, and to resume the tutorial through `:tutorial resume` [@editor].

This state belongs in preferences because it is user-memory rather than workspace contents. The [Learning And Tutorial](../editor/learning-and-tutorial) page explains the practice-buffer and recovery boundary: tutorial and Learn buffers are temporary, while real workspace recovery stays in crash snapshots.

## Failure And Filesystem Safety

Preferences are not startup-critical. A missing file loads as empty preferences, and unreadable or malformed data logs a message when a logger exists and then falls back to empty state [@preferences]. The architecture therefore preserves editor startup even when this convenience file is damaged.

On Unix, reads and writes use `O_NOFOLLOW` and `O_NONBLOCK`, require the target to be a regular file, and set permissions to `0600` [@preferences]. Writes create parent directories, serialize pretty JSON, open with `0600`, truncate through `set_len(0)`, and write the new contents [@preferences]. The tests cover owner-only agent transcript writes, permission tightening for existing files, and refusal to follow a symlink that points outside the preferences path [@preferences].

This store is adjacent to [Runtime Lifecycle](../startup/runtime-lifecycle), which loads it during startup, [Learning And Tutorial](../editor/learning-and-tutorial), which records onboarding progress here, and [Plugin Host Requests](../editor/plugin-host-requests), which uses it for plugin-owned values.
