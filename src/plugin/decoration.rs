use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::theme::{Style, Theme, ThemeStyleSpec};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecorationAnchor {
    #[default]
    Column,
    Eol,
    RightAlign,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decoration {
    #[serde(default, alias = "bufferIndex")]
    pub buffer_index: Option<usize>,
    #[serde(default)]
    pub anchor: DecorationAnchor,
    pub line: usize,
    #[serde(default)]
    pub column: usize,
    pub text: String,
    #[serde(default)]
    pub semantic: Option<ThemeStyleSpec>,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub priority: i32,
    #[serde(default, alias = "repeatLinebreak")]
    pub repeat_linebreak: bool,
    #[serde(default, alias = "onlyWhitespace")]
    pub only_whitespace: bool,
}

impl Decoration {
    pub fn resolve_style(&self, theme: &Theme) -> Style {
        let mut resolved = self
            .semantic
            .as_ref()
            .map(|semantic| theme.resolve_style(semantic))
            .unwrap_or_default();

        if resolved.fg.is_none() {
            resolved.fg = self.style.fg;
        }
        if resolved.bg.is_none() {
            resolved.bg = self.style.bg;
        }
        resolved.bold |= self.style.bold;
        resolved.italic |= self.style.italic;
        resolved
    }
}

#[derive(Debug, Default)]
pub struct DecorationManager {
    namespaces: HashMap<String, Vec<Decoration>>,
    line_index: HashMap<(usize, usize), Vec<IndexedDecoration>>,
    namespace_lines: HashMap<String, Vec<(usize, usize)>>,
}

#[derive(Debug, Clone)]
struct IndexedDecoration {
    namespace: String,
    decoration: Decoration,
}

impl DecorationManager {
    pub fn set(&mut self, namespace: String, decorations: Vec<Decoration>) -> bool {
        self.set_with_changed_lines(namespace, decorations)
            .is_some()
    }

    pub fn set_with_changed_lines(
        &mut self,
        namespace: String,
        decorations: Vec<Decoration>,
    ) -> Option<HashSet<(usize, usize)>> {
        if self.namespaces.get(&namespace) == Some(&decorations) {
            return None;
        }

        let mut changed_lines = self.lines_for_namespace(&namespace);
        changed_lines.extend(decorations.iter().filter_map(|decoration| {
            decoration
                .buffer_index
                .map(|buffer_index| (buffer_index, decoration.line))
        }));

        self.remove_namespace_from_index(&namespace);
        self.index_namespace(&namespace, &decorations);
        self.namespaces.insert(namespace, decorations);
        Some(changed_lines)
    }

    pub fn clear(&mut self, namespace: &str) -> bool {
        self.clear_with_changed_lines(namespace).is_some()
    }

    pub fn clear_with_changed_lines(&mut self, namespace: &str) -> Option<HashSet<(usize, usize)>> {
        let changed_lines = self.lines_for_namespace(namespace);
        self.namespaces.remove(namespace)?;
        self.remove_namespace_from_index(namespace);
        Some(changed_lines)
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
            .map(|indexed| &indexed.decoration)
    }

    pub fn buffers_for_namespace(&self, namespace: &str) -> HashSet<usize> {
        self.namespaces
            .get(namespace)
            .into_iter()
            .flatten()
            .filter_map(|decoration| decoration.buffer_index)
            .collect()
    }

    fn lines_for_namespace(&self, namespace: &str) -> HashSet<(usize, usize)> {
        self.namespaces
            .get(namespace)
            .into_iter()
            .flatten()
            .filter_map(|decoration| {
                decoration
                    .buffer_index
                    .map(|buffer_index| (buffer_index, decoration.line))
            })
            .collect()
    }

    fn remove_namespace_from_index(&mut self, namespace: &str) {
        let Some(keys) = self.namespace_lines.remove(namespace) else {
            return;
        };

        for key in keys {
            let should_remove = if let Some(decorations) = self.line_index.get_mut(&key) {
                decorations.retain(|decoration| decoration.namespace != namespace);
                decorations.is_empty()
            } else {
                false
            };
            if should_remove {
                self.line_index.remove(&key);
            }
        }
    }

    fn index_namespace(&mut self, namespace: &str, decorations: &[Decoration]) {
        let mut keys = HashSet::new();

        for decoration in decorations {
            let Some(buffer_index) = decoration.buffer_index else {
                continue;
            };
            let key = (buffer_index, decoration.line);
            keys.insert(key);
            self.line_index
                .entry(key)
                .or_default()
                .push(IndexedDecoration {
                    namespace: namespace.to_string(),
                    decoration: decoration.clone(),
                });
        }

        for key in &keys {
            if let Some(decorations) = self.line_index.get_mut(key) {
                decorations.sort_by_key(|decoration| decoration.decoration.priority);
            }
        }

        if !keys.is_empty() {
            self.namespace_lines
                .insert(namespace.to_string(), keys.into_iter().collect());
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
            semantic: None,
            style: Style::default(),
            priority,
            repeat_linebreak: false,
            only_whitespace: false,
        }
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
    fn replacing_namespace_keeps_other_namespace_entries() {
        let mut manager = DecorationManager::default();
        manager.set("guides".to_string(), vec![decoration(0, 1, 4, 10)]);
        manager.set("hints".to_string(), vec![decoration(0, 1, 8, 20)]);

        assert!(manager.set("guides".to_string(), vec![decoration(0, 2, 4, 10)]));

        let current = manager
            .decorations_for_line(0, 1)
            .map(|decoration| decoration.column)
            .collect::<Vec<_>>();
        assert_eq!(current, vec![8]);
        assert_eq!(manager.decorations_for_line(0, 2).count(), 1);
    }

    #[test]
    fn set_reports_old_and_new_changed_lines() {
        let mut manager = DecorationManager::default();
        manager.set("scope".to_string(), vec![decoration(0, 1, 4, 10)]);

        let changed = manager
            .set_with_changed_lines("scope".to_string(), vec![decoration(0, 3, 4, 10)])
            .unwrap();

        assert!(changed.contains(&(0, 1)));
        assert!(changed.contains(&(0, 3)));
        assert_eq!(changed.len(), 2);
    }

    #[test]
    fn clear_reports_removed_lines() {
        let mut manager = DecorationManager::default();
        manager.set("scope".to_string(), vec![decoration(0, 1, 4, 10)]);

        let changed = manager.clear_with_changed_lines("scope").unwrap();

        assert!(changed.contains(&(0, 1)));
        assert_eq!(changed.len(), 1);
        assert_eq!(manager.decorations_for_line(0, 1).count(), 0);
    }
}
