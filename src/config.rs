use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::editor::Action;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub keys: Keys,
    pub theme: String,
    #[serde(default)]
    pub plugins: HashMap<String, String>,
    pub log_file: Option<String>,
    pub mouse_scroll_lines: Option<usize>,
    #[serde(default = "default_true")]
    pub show_diagnostics: bool,
}

impl Config {
    pub fn path(p: &str) -> PathBuf {
        #[allow(deprecated)]
        std::env::home_dir()
            .unwrap()
            .join(".config")
            .join("red")
            .join(p)
    }
}

pub fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum KeyAction {
    Single(Action),
    Multiple(Vec<Action>),
    Nested(HashMap<String, KeyAction>),
    Repeating(u16, Box<KeyAction>),
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Keys {
    #[serde(default)]
    pub normal: HashMap<String, KeyAction>,
    #[serde(default)]
    pub insert: HashMap<String, KeyAction>,
    #[serde(default)]
    pub command: HashMap<String, KeyAction>,
    #[serde(default)]
    pub visual: HashMap<String, KeyAction>,
    #[serde(default)]
    pub visual_line: HashMap<String, KeyAction>,
    #[serde(default)]
    pub visual_block: HashMap<String, KeyAction>,
}

#[cfg(test)]
mod test {
    use std::fs;

    use crate::editor::Mode;

    use super::*;

    #[test]
    fn test_persist_config() {
        let config = Config {
            theme: "theme/nightfox.json".to_string(),
            keys: Keys {
                normal: HashMap::from([
                    (
                        "o".to_string(),
                        KeyAction::Single(Action::InsertLineBelowCursor),
                    ),
                    (
                        "i".to_string(),
                        KeyAction::Single(Action::EnterMode(Mode::Normal)),
                    ),
                ]),
                insert: HashMap::new(),
                command: HashMap::new(),
                visual: HashMap::new(),
                visual_line: HashMap::new(),
                visual_block: HashMap::new(),
            },
            ..Default::default()
        };

        let toml = toml::to_string(&config).unwrap();
        println!("{toml}");
    }

    #[test]
    fn test_parse_config() {
        let toml = fs::read_to_string("src/fixtures/config.toml").unwrap();
        let config: Config = toml::from_str(&toml).unwrap();
        println!("{config:#?}");
    }
}
