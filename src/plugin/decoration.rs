//! Namespaced inline decorations rendered against buffer lines.
//!
//! [`DecorationManager`] replaces one plugin namespace atomically and indexes accepted
//! decorations by buffer and line for the renderer. Coordinates refer to buffer lines
//! plus the anchor-specific character or display position documented by
//! [`DecorationAnchor`]. Namespace replacement prevents stale decorations from surviving
//! a plugin refresh.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::theme::Style;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecorationAnchor {
    #[default]
    Column,
    Eol,
    RightAlign,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decoration {
    #[serde(default)]
    pub buffer_index: Option<usize>,
    #[serde(default)]
    pub anchor: DecorationAnchor,
    pub line: usize,
    #[serde(default)]
    pub column: usize,
    pub text: String,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub repeat_linebreak: bool,
    #[serde(default)]
    pub only_whitespace: bool,
}

#[derive(Debug, Default)]
pub struct DecorationManager {
    namespaces: HashMap<String, Vec<Decoration>>,
    line_index: HashMap<(usize, usize), Vec<Decoration>>,
}

impl DecorationManager {
    pub fn set(&mut self, namespace: String, decorations: Vec<Decoration>) -> bool {
        if self.namespaces.get(&namespace) == Some(&decorations) {
            return false;
        }

        self.namespaces.insert(namespace, decorations);
        self.rebuild_index();
        true
    }

    pub fn clear(&mut self, namespace: &str) -> bool {
        if self.namespaces.remove(namespace).is_none() {
            return false;
        }

        self.rebuild_index();
        true
    }

    pub fn decorations_for_line(
        &self,
        buffer_index: usize,
        line: usize,
    ) -> impl Iterator<Item = &Decoration> {
        self.line_index
            .get(&(buffer_index, line))
            .into_iter()
            .flatten()
    }

    pub fn buffers_for_namespace(&self, namespace: &str) -> HashSet<usize> {
        self.namespaces
            .get(namespace)
            .into_iter()
            .flatten()
            .filter_map(|decoration| decoration.buffer_index)
            .collect()
    }

    fn rebuild_index(&mut self) {
        self.line_index.clear();

        for decorations in self.namespaces.values() {
            for decoration in decorations {
                let Some(buffer_index) = decoration.buffer_index else {
                    continue;
                };
                self.line_index
                    .entry((buffer_index, decoration.line))
                    .or_default()
                    .push(decoration.clone());
            }
        }

        for decorations in self.line_index.values_mut() {
            decorations.sort_by_key(|decoration| decoration.priority);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoration(buffer_index: usize, line: usize, column: usize, priority: i32) -> Decoration {
        Decoration {
            buffer_index: Some(buffer_index),
            anchor: DecorationAnchor::Column,
            line,
            column,
            text: "|".to_string(),
            style: Style::default(),
            priority,
            repeat_linebreak: false,
            only_whitespace: false,
        }
    }

    #[test]
    fn rejects_camel_case_decoration_fields() {
        let result = serde_json::from_value::<Decoration>(serde_json::json!({
            "bufferIndex": 1,
            "line": 1,
            "text": "|"
        }));

        assert!(result.is_err());
    }

    #[test]
    fn replaces_namespace_and_indexes_by_buffer_line() {
        let mut manager = DecorationManager::default();

        assert!(manager.set(
            "guides".to_string(),
            vec![decoration(0, 1, 4, 10), decoration(1, 1, 2, 5)]
        ));

        let current = manager.decorations_for_line(0, 1).collect::<Vec<_>>();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].column, 4);

        assert!(manager.set("guides".to_string(), vec![decoration(0, 2, 8, 10)]));
        assert_eq!(manager.decorations_for_line(0, 1).count(), 0);
        assert_eq!(manager.decorations_for_line(0, 2).count(), 1);
    }

    #[test]
    fn reports_unchanged_payload_as_noop() {
        let mut manager = DecorationManager::default();
        let payload = vec![decoration(0, 1, 4, 10)];

        assert!(manager.set("guides".to_string(), payload.clone()));
        assert!(!manager.set("guides".to_string(), payload));
    }

    #[test]
    fn returns_decorations_in_priority_order() {
        let mut manager = DecorationManager::default();

        manager.set(
            "guides".to_string(),
            vec![decoration(0, 1, 8, 20), decoration(0, 1, 4, 1)],
        );

        let columns = manager
            .decorations_for_line(0, 1)
            .map(|decoration| decoration.column)
            .collect::<Vec<_>>();
        assert_eq!(columns, vec![4, 8]);
    }

    #[test]
    fn clears_namespace() {
        let mut manager = DecorationManager::default();
        manager.set("guides".to_string(), vec![decoration(0, 1, 4, 10)]);

        assert!(manager.clear("guides"));
        assert!(!manager.clear("guides"));
        assert_eq!(manager.decorations_for_line(0, 1).count(), 0);
    }
}
