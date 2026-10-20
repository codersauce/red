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

        let previous = self.namespaces.insert(namespace.clone(), decorations);
        if let Some(previous) = previous {
            Self::remove_indexed(&mut self.line_index, &previous);
        }
        Self::insert_indexed(&mut self.line_index, &self.namespaces[&namespace]);
        true
    }

    pub fn clear(&mut self, namespace: &str) -> bool {
        let Some(previous) = self.namespaces.remove(namespace) else {
            return false;
        };

        Self::remove_indexed(&mut self.line_index, &previous);
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

    fn remove_indexed(
        line_index: &mut HashMap<(usize, usize), Vec<Decoration>>,
        decorations: &[Decoration],
    ) {
        for decoration in decorations {
            let Some(buffer_index) = decoration.buffer_index else {
                continue;
            };
            let key = (buffer_index, decoration.line);
            let remove_line = line_index.get_mut(&key).is_some_and(|indexed| {
                if let Some(position) = indexed.iter().position(|current| current == decoration) {
                    indexed.remove(position);
                }
                indexed.is_empty()
            });
            if remove_line {
                line_index.remove(&key);
            }
        }
    }

    fn insert_indexed(
        line_index: &mut HashMap<(usize, usize), Vec<Decoration>>,
        decorations: &[Decoration],
    ) {
        for decoration in decorations {
            let Some(buffer_index) = decoration.buffer_index else {
                continue;
            };
            let indexed = line_index
                .entry((buffer_index, decoration.line))
                .or_default();
            let position =
                indexed.partition_point(|current| current.priority <= decoration.priority);
            indexed.insert(position, decoration.clone());
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

    #[test]
    fn namespace_updates_preserve_other_namespaces_on_shared_lines() {
        let mut manager = DecorationManager::default();
        let shared = decoration(0, 3, 4, 10);
        manager.set("first".to_string(), vec![shared.clone()]);
        manager.set("second".to_string(), vec![shared.clone()]);
        manager.set("third".to_string(), vec![decoration(0, 3, 8, 20)]);

        manager.set("first".to_string(), vec![decoration(0, 7, 2, 5)]);

        assert_eq!(
            manager
                .decorations_for_line(0, 3)
                .map(|decoration| decoration.column)
                .collect::<Vec<_>>(),
            vec![4, 8]
        );
        assert_eq!(manager.decorations_for_line(0, 7).count(), 1);
        assert!(manager.clear("second"));
        assert_eq!(
            manager
                .decorations_for_line(0, 3)
                .map(|decoration| decoration.column)
                .collect::<Vec<_>>(),
            vec![8]
        );
    }

    #[test]
    fn removing_unindexed_decorations_does_not_disturb_indexed_lines() {
        let mut manager = DecorationManager::default();
        let mut unindexed = decoration(0, 3, 4, 10);
        unindexed.buffer_index = None;
        manager.set("unindexed".to_string(), vec![unindexed]);
        manager.set("indexed".to_string(), vec![decoration(0, 3, 8, 20)]);

        assert!(manager.clear("unindexed"));
        assert_eq!(manager.decorations_for_line(0, 3).count(), 1);
    }
}
