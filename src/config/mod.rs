use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::editor::{Action, Mode};

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub keys: Keys,
    pub theme: String,
    pub log_file: Option<String>,
    pub mouse_scroll_lines: Option<usize>,
    #[serde(default = "default_true")]
    pub show_diagnostics: bool,
}

pub fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum KeyAction {
    Single(Action),
    Multiple(Vec<Action>),
    Nested(HashMap<String, KeyAction>),
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Keys {
    #[serde(default)]
    pub normal: HashMap<String, KeyAction>,
    #[serde(default)]
    pub insert: HashMap<String, KeyAction>,
    #[serde(default)]
    pub command: HashMap<String, KeyAction>,
}

fn get_default_normal() -> HashMap<String, KeyAction> {
    HashMap::from([
        ("n".to_string(), KeyAction::Multiple(vec![Action::FindNext])),
        ("N".to_string(), KeyAction::Multiple(vec![Action::FindPrevious])),
        ("w".to_string(), KeyAction::Multiple(vec![Action::MoveToNextWord])),
        ("b".to_string(), KeyAction::Multiple(vec![Action::MoveToPreviousWord])),
        ("p".to_string(), KeyAction::Multiple(vec![Action::MoveUp, Action::MoveRight])),
        ("a".to_string(), KeyAction::Multiple(vec![Action::EnterMode(Mode::Insert), Action::MoveRight])),
        ("O".to_string(), KeyAction::Multiple(vec![Action::InsertLineAtCursor, Action::EnterMode(Mode::Insert)])),
        ("o".to_string(), KeyAction::Multiple(vec![Action::InsertLineBelowCursor, Action::EnterMode(Mode::Insert)])),
        ("q".to_string(), KeyAction::Single(Action::Quit)),
        ("u".to_string(), KeyAction::Single(Action::Undo)),
        ("k".to_string(), KeyAction::Single(Action::MoveUp)),
        ("Up".to_string(), KeyAction::Single(Action::MoveUp)),
        ("j".to_string(), KeyAction::Single(Action::MoveDown)),
        ("h".to_string(), KeyAction::Single(Action::MoveLeft)),
        ("l".to_string(), KeyAction::Single(Action::MoveRight)),
        ("G".to_string(), KeyAction::Single(Action::MoveToBottom)),
        ("$".to_string(), KeyAction::Single(Action::MoveToLineEnd)),
        ("0".to_string(), KeyAction::Single(Action::MoveToLineStart)),
        ("x".to_string(), KeyAction::Single(Action::DeleteCharAtCursorPos)),
        ("/".to_string(), KeyAction::Single(Action::EnterMode(Mode::Search))),
        ("i".to_string(), KeyAction::Single(Action::EnterMode(Mode::Insert))),
        (";".to_string(), KeyAction::Single(Action::EnterMode(Mode::Command))),
        (":".to_string(), KeyAction::Single(Action::EnterMode(Mode::Command))),
        ("Down".to_string(), KeyAction::Single(Action::MoveDown)),
        ("Left".to_string(), KeyAction::Single(Action::MoveLeft)),
        ("Ctrl-b".to_string(), KeyAction::Single(Action::PageUp)),
        ("Right".to_string(), KeyAction::Single(Action::MoveRight)),
        ("Ctrl-f".to_string(), KeyAction::Single(Action::PageDown)),
        ("End".to_string(), KeyAction::Single(Action::MoveToLineEnd)),
        ("Home".to_string(), KeyAction::Single(Action::MoveToLineStart)),
        ("g".to_string(), KeyAction::Nested(HashMap::from([
            ("g".to_string(), KeyAction::Single(Action::MoveToTop)),
            ("d".to_string(), KeyAction::Single(Action::GoToDefinition)),
        ]))),
        ("d".to_string(), KeyAction::Nested(HashMap::from([
            ("b".to_string(), KeyAction::Single(Action::DumpBuffer)),
            ("w".to_string(), KeyAction::Single(Action::DeleteWord)),
            ("d".to_string(), KeyAction::Single(Action::DeleteCurrentLine)),
        ]))),
        ("z".to_string(), KeyAction::Nested(HashMap::from([
            ("z".to_string(), KeyAction::Single(Action::MoveLineToViewportCenter))
        ]))),
    ])
}


fn get_default_insert() -> HashMap<String, KeyAction> {
    HashMap::from([
        ("Enter".to_string(), KeyAction::Single(Action::InsertNewLine)),
        ("Backspace".to_string(), KeyAction::Single(Action::DeletePreviousChar)),
        ("Tab".to_string(), KeyAction::Single(Action::InsertTab)),
        ("Esc".to_string(), KeyAction::Single(Action::EnterMode(Mode::Normal))),
    ])
}


fn get_default_command() -> HashMap<String, KeyAction> {
    HashMap::from([
        ("Esc".to_string(), KeyAction::Single(Action::EnterMode(Mode::Normal)))
    ])
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "".to_string(),
            log_file: None,
            mouse_scroll_lines: Some(3 as usize),
            show_diagnostics: false,
            keys: Keys {
                normal: get_default_normal(),
                insert: get_default_insert(),
                command: get_default_command(),
            }
        }
    }
}

impl Keys {
    pub fn extend(&mut self, src: Keys) {
        self.normal.extend(src.normal);
        self.insert.extend(src.insert);
        self.command.extend(src.command);
    }
}

impl Config {
    pub fn extend(&mut self, src: Config) {
        self.keys.extend(src.keys); 
        
        if !src.theme.is_empty() && src.theme != self.theme {
            self.theme = src.theme;
        }

        if src.show_diagnostics != self.show_diagnostics {
            self.show_diagnostics = src.show_diagnostics
        }
        
        if let Some(log_file) = src.log_file {
            self.log_file = Some(log_file);
        }

        if let Some(scrolloff) = src.mouse_scroll_lines {
            self.mouse_scroll_lines = Some(scrolloff);
        }
        
    }
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
