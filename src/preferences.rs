//! Best-effort persistence for histories, workspace panel layouts, and plugin-owned values.
//!
//! Preferences are convenience state rather than recovery state: malformed or
//! unreadable data loads as an empty store and is reported through the configured
//! logger. File writes are owner-only and refuse unsafe symlink targets on supported
//! platforms. An in-memory store gives tests and embedded callers identical mutation
//! semantics without filesystem effects.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{plugin::PanelSide, LOGGER};

const COMMAND_HISTORY_LIMIT: usize = 100;
const SEARCH_HISTORY_LIMIT: usize = 100;
const PICKER_HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Serialized convenience state owned by [`PreferencesStore`].
pub struct Preferences {
    #[serde(default)]
    command_history: Vec<String>,
    #[serde(default)]
    search_history: Vec<String>,
    #[serde(default)]
    picker_history: HashMap<String, Vec<String>>,
    #[serde(default)]
    plugin_storage: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_seen_version: Option<String>,
    #[serde(default)]
    panel_layouts: HashMap<String, HashMap<String, PanelLayoutPreference>>,
    #[serde(default)]
    copilot_setup_hint_seen: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    learn_completed_lessons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Workspace-scoped layout chosen for one stable plugin panel.
pub struct PanelLayoutPreference {
    pub side: PanelSide,
    #[serde(default)]
    pub vertical_size: Option<usize>,
    #[serde(default)]
    pub horizontal_size: Option<usize>,
}

impl PanelLayoutPreference {
    /// Returns the preferred size for the axis containing `side`.
    pub fn size_for(self, side: PanelSide) -> Option<usize> {
        if matches!(side, PanelSide::Left | PanelSide::Right) {
            self.vertical_size
        } else {
            self.horizontal_size
        }
    }
}

#[derive(Debug, Clone)]
/// Best-effort preferences persistence with an optional filesystem backing.
pub struct PreferencesStore {
    path: Option<PathBuf>,
    preferences: Preferences,
}

impl PreferencesStore {
    /// Creates a store whose mutations never touch the filesystem.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            preferences: Preferences::default(),
        }
    }

    /// Whether the store can remember a release across separate editor sessions.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    /// Whether a stable, versioned Learn Red lesson has been completed.
    pub(crate) fn learn_lesson_completed(&self, id: &str) -> bool {
        self.preferences
            .learn_completed_lessons
            .iter()
            .any(|lesson| lesson == id)
    }

    /// Records completion without changing any project or editor buffer.
    pub(crate) fn complete_learn_lesson(&mut self, id: &str) -> anyhow::Result<()> {
        if self.learn_lesson_completed(id) {
            return Ok(());
        }
        self.preferences.learn_completed_lessons.push(id.to_owned());
        self.save()
    }

    /// Loads preferences, falling back to empty state on any read or parse error.
    ///
    /// Failures are logged when a logger exists. Legacy plugin state is
    /// imported opportunistically.
    pub fn load(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let preferences = load_preferences(&path).unwrap_or_else(|error| {
            log_if_configured(&format!(
                "failed to load preferences from {}: {error}",
                path.display()
            ));
            Preferences::default()
        });

        let mut store = Self {
            path: Some(path),
            preferences,
        };
        if let Err(error) = store.import_legacy_plugin_storage() {
            log_if_configured(&format!("failed to import legacy plugin storage: {error}"));
        }
        store
    }

    /// Returns command-line history from oldest to newest.
    pub fn command_history(&self) -> &[String] {
        &self.preferences.command_history
    }

    /// Returns shared forward/backward search history from oldest to newest.
    pub fn search_history(&self) -> &[String] {
        &self.preferences.search_history
    }

    /// Returns history for a picker namespace from oldest to newest.
    pub fn picker_history(&self, key: &str) -> &[String] {
        self.preferences
            .picker_history
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Returns the most recent Red release successfully presented to the user.
    #[must_use]
    pub fn last_seen_version(&self) -> Option<&str> {
        self.preferences.last_seen_version.as_deref()
    }

    /// Records a release only after its announcement has actually been rendered.
    pub fn set_last_seen_version(&mut self, version: &str) -> anyhow::Result<()> {
        if self.last_seen_version() == Some(version) {
            return Ok(());
        }
        self.preferences.last_seen_version = Some(version.to_string());
        self.save()
    }

    /// Whether the optional Copilot setup hint has already been presented.
    #[must_use]
    pub fn copilot_setup_hint_seen(&self) -> bool {
        self.preferences.copilot_setup_hint_seen
    }

    /// Records presentation of the one-time Copilot setup hint.
    pub fn mark_copilot_setup_hint_seen(&mut self) -> anyhow::Result<()> {
        if self.preferences.copilot_setup_hint_seen {
            return Ok(());
        }
        self.preferences.copilot_setup_hint_seen = true;
        self.save()
    }

    /// Appends a non-empty command unless it duplicates the newest entry.
    ///
    /// History is bounded and a filesystem-backed store saves immediately.
    pub fn record_command(&mut self, command: &str) -> anyhow::Result<()> {
        if self.record_command_deferred(command) {
            self.save()?;
        }
        Ok(())
    }

    /// Records a possible quit command without blocking terminal restoration.
    /// The caller must flush preferences after the command is accepted or rejected.
    pub(crate) fn record_command_deferred(&mut self, command: &str) -> bool {
        if command.trim().is_empty() {
            return false;
        }

        if self
            .preferences
            .command_history
            .last()
            .is_some_and(|last| last == command)
        {
            return false;
        }

        self.preferences.command_history.push(command.to_string());
        let overflow = self
            .preferences
            .command_history
            .len()
            .saturating_sub(COMMAND_HISTORY_LIMIT);
        if overflow > 0 {
            self.preferences.command_history.drain(0..overflow);
        }

        true
    }

    /// Appends a non-empty search pattern unless it duplicates the newest entry.
    ///
    /// Whitespace is meaningful in search patterns. History is bounded and a
    /// filesystem-backed store saves immediately.
    pub fn record_search(&mut self, pattern: &str) -> anyhow::Result<()> {
        let history = &mut self.preferences.search_history;
        if pattern.is_empty() || history.last().is_some_and(|last| last == pattern) {
            return Ok(());
        }

        history.push(pattern.to_string());
        let overflow = history.len().saturating_sub(SEARCH_HISTORY_LIMIT);
        if overflow > 0 {
            history.drain(0..overflow);
        }

        self.save()
    }

    /// Appends a non-empty picker query within a bounded namespace history.
    pub fn record_picker_query(&mut self, key: &str, query: &str) -> anyhow::Result<()> {
        if key.trim().is_empty() || query.trim().is_empty() {
            return Ok(());
        }

        let history = self
            .preferences
            .picker_history
            .entry(key.to_string())
            .or_default();
        if history.last().is_some_and(|last| last == query) {
            return Ok(());
        }

        history.push(query.to_string());
        let overflow = history.len().saturating_sub(PICKER_HISTORY_LIMIT);
        if overflow > 0 {
            history.drain(0..overflow);
        }

        self.save()
    }

    /// Removes and persists one picker history namespace.
    pub fn remove_picker_history(&mut self, key: &str) -> anyhow::Result<()> {
        if self.preferences.picker_history.remove(key).is_none() {
            return Ok(());
        }

        self.save()
    }

    /// Reads the saved layout for one panel in a workspace.
    pub fn panel_layout(&self, workspace: &Path, panel_id: &str) -> Option<&PanelLayoutPreference> {
        self.preferences
            .panel_layouts
            .get(&workspace_key(workspace))?
            .get(panel_id)
    }

    /// Saves one panel layout and immediately persists it.
    pub fn set_panel_layout(
        &mut self,
        workspace: &Path,
        panel_id: &str,
        layout: PanelLayoutPreference,
    ) -> anyhow::Result<()> {
        self.preferences
            .panel_layouts
            .entry(workspace_key(workspace))
            .or_default()
            .insert(panel_id.to_string(), layout);
        self.save()
    }

    /// Removes one saved panel layout from a workspace.
    pub fn remove_panel_layout(
        &mut self,
        workspace: &Path,
        panel_id: &str,
    ) -> anyhow::Result<bool> {
        let workspace = workspace_key(workspace);
        let Some(layouts) = self.preferences.panel_layouts.get_mut(&workspace) else {
            return Ok(false);
        };
        if layouts.remove(panel_id).is_none() {
            return Ok(false);
        }
        if layouts.is_empty() {
            self.preferences.panel_layouts.remove(&workspace);
        }
        self.save()?;
        Ok(true)
    }

    /// Removes every saved panel layout for a workspace.
    pub fn clear_panel_layouts(&mut self, workspace: &Path) -> anyhow::Result<bool> {
        if self
            .preferences
            .panel_layouts
            .remove(&workspace_key(workspace))
            .is_none()
        {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    /// Reads a plugin-owned preference value.
    pub fn plugin_storage(&self, plugin: &str, key: &str) -> Option<&serde_json::Value> {
        self.preferences
            .plugin_storage
            .get(&plugin_storage_key(plugin, key))
    }

    /// Sets and immediately persists a plugin-owned preference value.
    pub fn set_plugin_storage(
        &mut self,
        plugin: &str,
        key: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.preferences
            .plugin_storage
            .insert(plugin_storage_key(plugin, key), value);
        self.save()
    }

    /// Flushes deferred preferences and ordered exit-hook values in one write.
    pub(crate) fn flush_plugin_storage(
        &mut self,
        updates: Vec<(String, String, serde_json::Value)>,
    ) -> anyhow::Result<()> {
        for (plugin, key, value) in updates {
            self.preferences
                .plugin_storage
                .insert(plugin_storage_key(&plugin, &key), value);
        }
        self.save()
    }

    /// Returns an opaque copy suitable for co-snapshotting with editor recovery.
    ///
    /// Keys and values are intentionally not interpreted here so data from an
    /// unavailable or newer plugin survives Red upgrades and plugin reinstalls.
    #[must_use]
    pub fn plugin_storage_snapshot(&self) -> HashMap<String, serde_json::Value> {
        self.preferences.plugin_storage.clone()
    }

    /// Restores opaque plugin values without deleting keys written after the snapshot.
    pub fn merge_plugin_storage_snapshot(
        &mut self,
        snapshot: &HashMap<String, serde_json::Value>,
    ) -> anyhow::Result<()> {
        for (key, value) in snapshot {
            self.preferences
                .plugin_storage
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        self.save()
    }

    /// Persists owner-only JSON for filesystem-backed stores.
    ///
    /// In-memory stores treat saving as a successful no-op.
    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&self.preferences)?;
        write_preferences(path, contents.as_bytes())?;
        Ok(())
    }

    fn import_legacy_plugin_storage(&mut self) -> anyhow::Result<()> {
        let Some(preferences_path) = &self.path else {
            return Ok(());
        };
        let Some(config_dir) = preferences_path.parent() else {
            return Ok(());
        };
        let legacy_dir = config_dir.join("state").join("plugins");
        let mut changed = false;
        changed |= self
            .import_legacy_key(
                &legacy_dir.join("session_restore.json"),
                "latest",
                "session_restore",
                "latest",
            )
            .unwrap_or_else(|error| {
                log_if_configured(&format!(
                    "failed to import legacy session_restore storage: {error}"
                ));
                false
            });
        let imported_project_search = self
            .import_legacy_key(
                &legacy_dir.join("project_search.json"),
                "historyByCwd",
                "project_search",
                "history_by_cwd",
            )
            .unwrap_or_else(|error| {
                log_if_configured(&format!(
                    "failed to import legacy project_search storage: {error}"
                ));
                false
            });
        changed |= imported_project_search;
        if !imported_project_search {
            changed |= self
                .import_legacy_key(
                    &legacy_dir.join("project_search.json"),
                    "history_by_cwd",
                    "project_search",
                    "history_by_cwd",
                )
                .unwrap_or_else(|error| {
                    log_if_configured(&format!(
                        "failed to import legacy project_search storage: {error}"
                    ));
                    false
                });
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    fn import_legacy_key(
        &mut self,
        path: &Path,
        legacy_key: &str,
        plugin: &str,
        key: &str,
    ) -> anyhow::Result<bool> {
        let storage_key = plugin_storage_key(plugin, key);
        if self.preferences.plugin_storage.contains_key(&storage_key) || !path.exists() {
            return Ok(false);
        }
        let contents = fs::read_to_string(path)?;
        let legacy: serde_json::Value = serde_json::from_str(&contents)?;
        let Some(value) = legacy.get(legacy_key) else {
            return Ok(false);
        };
        self.preferences
            .plugin_storage
            .insert(storage_key, value.clone());
        Ok(true)
    }
}

fn plugin_storage_key(plugin: &str, key: &str) -> String {
    format!("{plugin}:{key}")
}

fn workspace_key(workspace: &Path) -> String {
    workspace.to_string_lossy().into_owned()
}

fn load_preferences(path: &Path) -> anyhow::Result<Preferences> {
    if !path.exists() {
        return Ok(Preferences::default());
    }

    let contents = read_preferences(path)?;
    Ok(serde_json::from_str(&contents)?)
}

#[cfg(unix)]
fn read_preferences(path: &Path) -> anyhow::Result<String> {
    use std::{
        io::Read as _,
        os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    };

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "preferences path {} is not a regular file",
        path.display()
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

#[cfg(not(unix))]
fn read_preferences(path: &Path) -> anyhow::Result<String> {
    Ok(fs::read_to_string(path)?)
}

#[cfg(unix)]
fn write_preferences(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::{
        io::Write as _,
        os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    };

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "preferences path {} is not a regular file",
        path.display()
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.set_len(0)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_preferences(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    Ok(fs::write(path, contents)?)
}

fn log_if_configured(message: &str) {
    if let Some(Some(logger)) = LOGGER.get() {
        logger.log(message);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("red-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn missing_file_loads_empty_preferences() {
        let path = unique_temp_dir("missing-preferences").join("preferences.json");

        let store = PreferencesStore::load(path);

        assert!(store.command_history().is_empty());
        assert!(store.search_history().is_empty());
        assert_eq!(store.last_seen_version(), None);
    }

    #[test]
    fn seen_release_version_survives_a_preferences_reload() {
        let dir = unique_temp_dir("release-preferences");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);

        store.set_last_seen_version("0.5.0").unwrap();

        let reloaded = PreferencesStore::load(&path);
        assert_eq!(reloaded.last_seen_version(), Some("0.5.0"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn copilot_setup_hint_is_unseen_by_default_and_survives_reload() {
        let dir = unique_temp_dir("copilot-hint-preferences");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);

        assert!(!store.copilot_setup_hint_seen());
        store.mark_copilot_setup_hint_seen().unwrap();

        let reloaded = PreferencesStore::load(&path);
        assert!(reloaded.copilot_setup_hint_seen());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn older_preferences_without_a_seen_release_remain_compatible() {
        let dir = unique_temp_dir("legacy-release-preferences");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("preferences.json");
        fs::write(&path, r#"{"command_history":["write"]}"#).unwrap();

        let store = PreferencesStore::load(&path);

        assert_eq!(store.command_history(), ["write"]);
        assert!(store.search_history().is_empty());
        assert_eq!(store.last_seen_version(), None);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn saving_creates_preferences_file() {
        let dir = unique_temp_dir("preferences-save");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);

        store.record_command("write").unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("write"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn saved_command_history_reloads_in_order() {
        let dir = unique_temp_dir("preferences-reload");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);
        store.record_command("write").unwrap();
        store.record_command("quit").unwrap();

        let store = PreferencesStore::load(&path);

        assert_eq!(store.command_history(), ["write", "quit"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn saved_picker_history_reloads_by_key() {
        let dir = unique_temp_dir("preferences-picker-reload");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);
        store.record_picker_query("find_files", "src").unwrap();
        store.record_picker_query("buffers", "main").unwrap();

        let store = PreferencesStore::load(&path);

        assert_eq!(store.picker_history("find_files"), ["src"]);
        assert_eq!(store.picker_history("buffers"), ["main"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn removed_picker_history_does_not_reload_or_clear_other_keys() {
        let dir = unique_temp_dir("preferences-picker-remove");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);
        store
            .record_picker_query("picker:802", "legacy agent prompt")
            .unwrap();
        store.record_picker_query("find_files", "src").unwrap();

        store.remove_picker_history("picker:802").unwrap();
        let store = PreferencesStore::load(&path);

        assert!(store.picker_history("picker:802").is_empty());
        assert_eq!(store.picker_history("find_files"), ["src"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_imports_legacy_session_and_project_search_storage() {
        let dir = unique_temp_dir("legacy-plugin-storage");
        let legacy_dir = dir.join("state").join("plugins");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(
            legacy_dir.join("session_restore.json"),
            r#"{"latest":{"version":1,"cwd":"/repo"}}"#,
        )
        .unwrap();
        fs::write(
            legacy_dir.join("project_search.json"),
            r#"{"historyByCwd":{"/repo":["needle"]}}"#,
        )
        .unwrap();

        let store = PreferencesStore::load(dir.join("preferences.json"));

        assert_eq!(
            store.plugin_storage("session_restore", "latest").unwrap()["cwd"],
            "/repo"
        );
        assert_eq!(
            store
                .plugin_storage("project_search", "history_by_cwd")
                .unwrap()["/repo"][0],
            "needle"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_keeps_existing_plugin_storage_over_legacy_values() {
        let dir = unique_temp_dir("legacy-plugin-storage-precedence");
        let legacy_dir = dir.join("state").join("plugins");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(
            legacy_dir.join("session_restore.json"),
            r#"{"latest":{"cwd":"/legacy"}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("preferences.json"),
            r#"{"plugin_storage":{"session_restore:latest":{"cwd":"/current"}}}"#,
        )
        .unwrap();

        let store = PreferencesStore::load(dir.join("preferences.json"));

        assert_eq!(
            store.plugin_storage("session_restore", "latest").unwrap()["cwd"],
            "/current"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn consecutive_duplicate_commands_are_not_repeated() {
        let mut store = PreferencesStore::in_memory();

        store.record_command("write").unwrap();
        store.record_command("write").unwrap();
        store.record_command("quit").unwrap();

        assert_eq!(store.command_history(), ["write", "quit"]);
    }

    #[test]
    fn search_history_preserves_whitespace_and_skips_empty_or_consecutive_duplicates() {
        let mut store = PreferencesStore::in_memory();

        for pattern in ["", "alpha", "alpha", "   ", "   ", "alpha"] {
            store.record_search(pattern).unwrap();
        }

        assert_eq!(store.search_history(), ["alpha", "   ", "alpha"]);
        assert!(store.command_history().is_empty());
    }

    #[test]
    fn search_history_reloads_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        let mut store = PreferencesStore::load(&path);
        store.record_command("write").unwrap();
        store.record_search("alpha").unwrap();
        store.record_search(" beta ").unwrap();

        let reloaded = PreferencesStore::load(&path);

        assert_eq!(reloaded.search_history(), ["alpha", " beta "]);
        assert_eq!(reloaded.command_history(), ["write"]);
    }

    #[test]
    fn search_history_is_capped_at_limit() {
        let mut store = PreferencesStore::in_memory();
        for index in 0..(SEARCH_HISTORY_LIMIT + 5) {
            store.record_search(&format!("pattern-{index}")).unwrap();
        }

        assert_eq!(store.search_history().len(), SEARCH_HISTORY_LIMIT);
        assert_eq!(store.search_history().first().unwrap(), "pattern-5");
        assert_eq!(store.search_history().last().unwrap(), "pattern-104");
    }

    #[test]
    fn consecutive_duplicate_picker_queries_are_not_repeated() {
        let mut store = PreferencesStore::in_memory();

        store.record_picker_query("find_files", "src").unwrap();
        store.record_picker_query("find_files", "src").unwrap();
        store.record_picker_query("find_files", "test").unwrap();

        assert_eq!(store.picker_history("find_files"), ["src", "test"]);
    }

    #[test]
    fn command_history_is_capped_at_limit() {
        let mut store = PreferencesStore::in_memory();

        for i in 0..(COMMAND_HISTORY_LIMIT + 5) {
            store.record_command(&format!("cmd-{i}")).unwrap();
        }

        assert_eq!(store.command_history().len(), COMMAND_HISTORY_LIMIT);
        assert_eq!(store.command_history().first().unwrap(), "cmd-5");
    }

    #[test]
    fn picker_history_is_capped_at_limit() {
        let mut store = PreferencesStore::in_memory();

        for i in 0..(PICKER_HISTORY_LIMIT + 5) {
            store
                .record_picker_query("find_files", &format!("query-{i}"))
                .unwrap();
        }

        assert_eq!(
            store.picker_history("find_files").len(),
            PICKER_HISTORY_LIMIT
        );
        assert_eq!(
            store.picker_history("find_files").first().unwrap(),
            "query-5"
        );
    }

    #[test]
    fn malformed_preferences_load_empty_preferences() {
        let dir = unique_temp_dir("preferences-malformed");
        let path = dir.join("preferences.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "not json").unwrap();

        let store = PreferencesStore::load(&path);

        assert!(store.command_history().is_empty());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn plugin_storage_persists_by_plugin_and_key() {
        let dir = unique_temp_dir("plugin-storage");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);
        store
            .set_plugin_storage("project_search", "history", serde_json::json!(["needle"]))
            .unwrap();
        store
            .set_plugin_storage("other", "history", serde_json::json!(["other"]))
            .unwrap();

        let store = PreferencesStore::load(&path);

        assert_eq!(
            store.plugin_storage("project_search", "history"),
            Some(&serde_json::json!(["needle"]))
        );
        assert_eq!(
            store.plugin_storage("other", "history"),
            Some(&serde_json::json!(["other"]))
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn plugin_storage_batch_preserves_order_and_unrelated_values() {
        let dir = unique_temp_dir("plugin-storage-batch");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);
        assert!(store.record_command_deferred("q"));
        assert!(!store.record_command_deferred("q"));
        assert!(!store.record_command_deferred("  "));
        assert!(!path.exists());
        store.flush_plugin_storage(Vec::new()).unwrap();
        assert_eq!(PreferencesStore::load(&path).command_history(), ["q"]);
        store
            .set_plugin_storage("other", "keep", serde_json::json!(1))
            .unwrap();
        store
            .flush_plugin_storage(vec![
                ("test".into(), "key".into(), serde_json::json!(2)),
                ("test".into(), "key".into(), serde_json::json!(3)),
                ("test".into(), "second".into(), serde_json::json!(4)),
            ])
            .unwrap();
        let reloaded = PreferencesStore::load(&path);
        assert_eq!(
            reloaded.plugin_storage("other", "keep"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            reloaded.plugin_storage("test", "key"),
            Some(&serde_json::json!(3))
        );
        assert_eq!(
            reloaded.plugin_storage("test", "second"),
            Some(&serde_json::json!(4))
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn panel_layouts_persist_by_workspace_and_panel() {
        let dir = unique_temp_dir("panel-layouts");
        let path = dir.join("preferences.json");
        let first_workspace = Path::new("/repo/first");
        let second_workspace = Path::new("/repo/second");
        let mut store = PreferencesStore::load(&path);
        let first_layout = PanelLayoutPreference {
            side: PanelSide::Right,
            vertical_size: Some(48),
            horizontal_size: Some(9),
        };
        let second_layout = PanelLayoutPreference {
            side: PanelSide::Bottom,
            vertical_size: Some(30),
            horizontal_size: Some(12),
        };

        store
            .set_panel_layout(first_workspace, "agent-conversation", first_layout)
            .unwrap();
        store
            .set_panel_layout(second_workspace, "agent-conversation", second_layout)
            .unwrap();

        let store = PreferencesStore::load(&path);
        assert_eq!(
            store.panel_layout(first_workspace, "agent-conversation"),
            Some(&first_layout)
        );
        assert_eq!(
            store.panel_layout(second_workspace, "agent-conversation"),
            Some(&second_layout)
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn clearing_panel_layouts_only_affects_the_selected_workspace() {
        let first_workspace = Path::new("/repo/first");
        let second_workspace = Path::new("/repo/second");
        let layout = PanelLayoutPreference {
            side: PanelSide::Left,
            vertical_size: Some(32),
            horizontal_size: None,
        };
        let mut store = PreferencesStore::in_memory();
        store
            .set_panel_layout(first_workspace, "tree", layout)
            .unwrap();
        store
            .set_panel_layout(second_workspace, "tree", layout)
            .unwrap();

        assert!(store.clear_panel_layouts(first_workspace).unwrap());

        assert_eq!(store.panel_layout(first_workspace, "tree"), None);
        assert_eq!(store.panel_layout(second_workspace, "tree"), Some(&layout));
    }

    #[cfg(unix)]
    #[test]
    fn saving_agent_transcript_creates_owner_only_preferences() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = unique_temp_dir("private-agent-transcript");
        let path = dir.join("preferences.json");
        let mut store = PreferencesStore::load(&path);

        store
            .set_plugin_storage(
                "agent",
                "transcript",
                serde_json::json!("You: private prompt\nAgent: private response\n"),
            )
            .unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("private response"));
        fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn loading_existing_preferences_removes_group_and_world_access() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = unique_temp_dir("private-existing-preferences");
        let path = dir.join("preferences.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            r#"{"plugin_storage":{"agent:transcript":"private transcript"}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

        let store = PreferencesStore::load(&path);

        assert_eq!(
            store.plugin_storage("agent", "transcript"),
            Some(&serde_json::json!("private transcript"))
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn saving_preferences_refuses_to_follow_a_symlink() {
        let dir = unique_temp_dir("private-preferences-symlink");
        let path = dir.join("preferences.json");
        let outside = dir.join("outside.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&outside, "outside secret").unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        let mut store = PreferencesStore::load(&path);

        assert!(store
            .set_plugin_storage("agent", "transcript", serde_json::json!("must not write"))
            .is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside secret");
        fs::remove_dir_all(dir).ok();
    }
}
