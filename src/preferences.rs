use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::LOGGER;

const COMMAND_HISTORY_LIMIT: usize = 100;
const PICKER_HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    command_history: Vec<String>,
    #[serde(default)]
    picker_history: HashMap<String, Vec<String>>,
    #[serde(default)]
    plugin_storage: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct PreferencesStore {
    path: Option<PathBuf>,
    preferences: Preferences,
}

impl PreferencesStore {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            preferences: Preferences::default(),
        }
    }

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

    pub fn command_history(&self) -> &[String] {
        &self.preferences.command_history
    }

    pub fn picker_history(&self, key: &str) -> &[String] {
        self.preferences
            .picker_history
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn record_command(&mut self, command: &str) -> anyhow::Result<()> {
        if command.trim().is_empty() {
            return Ok(());
        }

        if self
            .preferences
            .command_history
            .last()
            .is_some_and(|last| last == command)
        {
            return Ok(());
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

        self.save()
    }

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

    pub fn remove_picker_history(&mut self, key: &str) -> anyhow::Result<()> {
        if self.preferences.picker_history.remove(key).is_none() {
            return Ok(());
        }

        self.save()
    }

    pub fn plugin_storage(&self, plugin: &str, key: &str) -> Option<&serde_json::Value> {
        self.preferences
            .plugin_storage
            .get(&plugin_storage_key(plugin, key))
    }

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
