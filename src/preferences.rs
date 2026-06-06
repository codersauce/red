use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::LOGGER;

const COMMAND_HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    command_history: Vec<String>,
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

        Self {
            path: Some(path),
            preferences,
        }
    }

    pub fn command_history(&self) -> &[String] {
        &self.preferences.command_history
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

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, serde_json::to_string_pretty(&self.preferences)?)?;
        Ok(())
    }
}

fn load_preferences(path: &Path) -> anyhow::Result<Preferences> {
    if !path.exists() {
        return Ok(Preferences::default());
    }

    let contents = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&contents)?)
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
    fn consecutive_duplicate_commands_are_not_repeated() {
        let mut store = PreferencesStore::in_memory();

        store.record_command("write").unwrap();
        store.record_command("write").unwrap();
        store.record_command("quit").unwrap();

        assert_eq!(store.command_history(), ["write", "quit"]);
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
    fn malformed_preferences_load_empty_preferences() {
        let dir = unique_temp_dir("preferences-malformed");
        let path = dir.join("preferences.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "not json").unwrap();

        let store = PreferencesStore::load(&path);

        assert!(store.command_history().is_empty());
        fs::remove_dir_all(dir).ok();
    }
}
