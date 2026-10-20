//! Namespaced signs rendered in the fixed-width editor gutter.
//!
//! [`GutterSignManager`] owns plugin sign sets and resolves collisions deterministically
//! by priority. A refresh replaces a complete namespace; plugins should not assume that
//! insertion order can preserve an older sign after the next update.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::theme::Style;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GutterSign {
    pub buffer_index: usize,
    pub line: usize,
    pub text: String,
    #[serde(default)]
    pub style: Style,
    #[serde(default = "default_sign_priority")]
    pub priority: i32,
}

fn default_sign_priority() -> i32 {
    10
}

impl GutterSign {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.text.chars().any(char::is_control),
            "gutter sign text must contain only printable characters"
        );
        let width = crate::unicode_utils::display_width(&self.text);
        anyhow::ensure!(
            (1..=2).contains(&width),
            "gutter sign text must occupy one or two display cells"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedGutterSign {
    namespace: String,
    sign: GutterSign,
}

#[derive(Debug, Default)]
pub struct GutterSignManager {
    namespaces: HashMap<String, Vec<GutterSign>>,
    line_index: HashMap<(usize, usize), Vec<IndexedGutterSign>>,
}

impl GutterSignManager {
    pub fn set(&mut self, namespace: String, signs: Vec<GutterSign>) -> bool {
        if self.namespaces.get(&namespace) == Some(&signs) {
            return false;
        }
        let previous = self.namespaces.insert(namespace.clone(), signs);
        if let Some(previous) = previous {
            Self::remove_indexed(&mut self.line_index, &namespace, &previous);
        }
        Self::insert_indexed(
            &mut self.line_index,
            &namespace,
            &self.namespaces[&namespace],
        );
        true
    }

    pub fn clear(&mut self, namespace: &str) -> bool {
        let Some(previous) = self.namespaces.remove(namespace) else {
            return false;
        };
        Self::remove_indexed(&mut self.line_index, namespace, &previous);
        true
    }

    pub fn visible_sign(&self, buffer_index: usize, line: usize) -> Option<&GutterSign> {
        self.line_index
            .get(&(buffer_index, line))
            .and_then(|signs| signs.first())
            .map(|indexed| &indexed.sign)
    }

    pub fn buffers_for_namespace(&self, namespace: &str) -> HashSet<usize> {
        self.namespaces
            .get(namespace)
            .into_iter()
            .flatten()
            .map(|sign| sign.buffer_index)
            .collect()
    }

    fn remove_indexed(
        line_index: &mut HashMap<(usize, usize), Vec<IndexedGutterSign>>,
        namespace: &str,
        signs: &[GutterSign],
    ) {
        for sign in signs {
            let key = (sign.buffer_index, sign.line);
            let remove_line = line_index.get_mut(&key).is_some_and(|indexed| {
                if let Some(position) = indexed
                    .iter()
                    .position(|current| current.namespace == namespace)
                {
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
        line_index: &mut HashMap<(usize, usize), Vec<IndexedGutterSign>>,
        namespace: &str,
        signs: &[GutterSign],
    ) {
        for sign in signs {
            let indexed = line_index
                .entry((sign.buffer_index, sign.line))
                .or_default();
            let position = indexed.partition_point(|current| {
                current.sign.priority > sign.priority
                    || (current.sign.priority == sign.priority
                        && current.namespace.as_str() <= namespace)
            });
            indexed.insert(
                position,
                IndexedGutterSign {
                    namespace: namespace.to_string(),
                    sign: sign.clone(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(priority: i32, text: &str) -> GutterSign {
        GutterSign {
            buffer_index: 1,
            line: 2,
            text: text.to_string(),
            style: Style::default(),
            priority,
        }
    }

    #[test]
    fn highest_priority_sign_wins() {
        let mut manager = GutterSignManager::default();
        manager.set("git".to_string(), vec![sign(10, "+")]);
        manager.set("diagnostics".to_string(), vec![sign(20, "!")]);
        assert_eq!(
            manager.visible_sign(1, 2).map(|sign| sign.text.as_str()),
            Some("!")
        );
    }

    #[test]
    fn clearing_namespace_reveals_lower_priority_sign() {
        let mut manager = GutterSignManager::default();
        manager.set("git".to_string(), vec![sign(10, "+")]);
        manager.set("diagnostics".to_string(), vec![sign(20, "!")]);
        assert!(manager.clear("diagnostics"));
        assert_eq!(
            manager.visible_sign(1, 2).map(|sign| sign.text.as_str()),
            Some("+")
        );
    }

    #[test]
    fn equal_priorities_use_namespace_order() {
        let mut manager = GutterSignManager::default();
        manager.set("git".to_string(), vec![sign(10, "+")]);
        manager.set("diagnostics".to_string(), vec![sign(10, "!")]);

        assert_eq!(
            manager.visible_sign(1, 2).map(|sign| sign.text.as_str()),
            Some("!")
        );
    }

    #[test]
    fn validates_sign_display_width() {
        assert!(sign(10, "+").validate().is_ok());
        assert!(sign(10, "!!").validate().is_ok());
        assert!(sign(10, "界").validate().is_ok());
        assert!(sign(10, "").validate().is_err());
        assert!(sign(10, "!!!").validate().is_err());
        assert!(sign(10, "\n").validate().is_err());
    }

    #[test]
    fn deserialization_defaults_priority_to_ten() {
        let sign: GutterSign = serde_json::from_value(serde_json::json!({
            "buffer_index": 1,
            "line": 2,
            "text": "+"
        }))
        .unwrap();

        assert_eq!(sign.priority, 10);
    }

    #[test]
    fn replacing_one_namespace_preserves_collision_order_and_unrelated_lines() {
        let mut manager = GutterSignManager::default();
        manager.set("git".to_string(), vec![sign(10, "+")]);
        manager.set("diagnostics".to_string(), vec![sign(20, "!")]);
        manager.set("annotations".to_string(), vec![sign(20, "?")]);

        let mut relocated = sign(30, "*");
        relocated.line = 7;
        manager.set("diagnostics".to_string(), vec![relocated]);

        assert_eq!(
            manager.visible_sign(1, 2).map(|sign| sign.text.as_str()),
            Some("?")
        );
        assert_eq!(
            manager.visible_sign(1, 7).map(|sign| sign.text.as_str()),
            Some("*")
        );
        assert!(manager.clear("annotations"));
        assert_eq!(
            manager.visible_sign(1, 2).map(|sign| sign.text.as_str()),
            Some("+")
        );
    }

    #[test]
    fn replacing_duplicate_signs_removes_only_their_namespace() {
        let mut manager = GutterSignManager::default();
        manager.set("first".to_string(), vec![sign(10, "+"), sign(5, "-")]);
        manager.set("second".to_string(), vec![sign(15, "!")]);

        manager.set("first".to_string(), vec![sign(20, "*")]);

        assert_eq!(
            manager.visible_sign(1, 2).map(|sign| sign.text.as_str()),
            Some("*")
        );
        assert!(manager.clear("first"));
        assert_eq!(
            manager.visible_sign(1, 2).map(|sign| sign.text.as_str()),
            Some("!")
        );
    }
}
