use crate::{Range, Selections};
use regex_automata::{Input, meta::Regex, util::syntax};
use ropey::RopeSlice;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Query {
    Select,
    Split,
    Keep,
    Remove,
    Forward,
    Backward,
}

impl Query {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Split => "split",
            Self::Keep => "keep",
            Self::Remove => "remove",
            Self::Forward | Self::Backward => "search",
        }
    }

    pub(crate) fn compile(pattern: &str) -> Option<Regex> {
        if pattern.is_empty() {
            return None;
        }
        Regex::builder()
            .syntax(
                syntax::Config::new()
                    .multi_line(true)
                    .case_insensitive(!pattern.chars().any(char::is_uppercase)),
            )
            .build(pattern)
            .ok()
    }

    pub(crate) fn apply(
        self,
        text: RopeSlice<'_>,
        initial: &Selections,
        regex: &Regex,
        extend: bool,
    ) -> Option<Selections> {
        let contents = Cow::<str>::from(text);
        let input = Input::new(contents.as_bytes());
        let mut ranges = Vec::new();
        if matches!(self, Self::Forward | Self::Backward) {
            let primary = initial.primary();
            let found = if matches!(self, Self::Forward) {
                regex
                    .find(
                        input
                            .clone()
                            .span(text.char_to_byte(primary.end())..text.len_bytes()),
                    )
                    .or_else(|| regex.find(input))
            } else {
                let before = text.char_to_byte(primary.start());
                regex
                    .find_iter(input.clone().span(0..before))
                    .last()
                    .or_else(|| regex.find_iter(input.span(before..text.len_bytes())).last())
            }?;
            if found.end() == 0 {
                return None;
            }
            let range = primary.with_bounds(
                text.byte_to_char(found.start()),
                text.byte_to_char(found.end()),
            );
            ranges.clone_from(&initial.ranges);
            let index = if extend {
                ranges.push(range);
                ranges.len() - 1
            } else {
                ranges[initial.primary] = range;
                initial.primary
            };
            return Selections::new(text, ranges, index);
        }
        for &r in initial.ranges() {
            let scope = input
                .clone()
                .span(text.char_to_byte(r.start())..text.char_to_byte(r.end()));
            if matches!(self, Self::Keep | Self::Remove) {
                if regex.is_match(scope) != matches!(self, Self::Remove) {
                    ranges.push(r);
                }
                continue;
            }
            let mut start = r.start();
            for m in regex.find_iter(scope) {
                let from = text.byte_to_char(m.start());
                let to = text.byte_to_char(m.end());
                if matches!(self, Self::Split) {
                    ranges.push(Range {
                        anchor: start,
                        head: from,
                    });
                    start = to;
                } else if from != r.end() || to != from {
                    ranges.push(Range {
                        anchor: from,
                        head: to,
                    });
                }
            }
            if matches!(self, Self::Split) && (start < r.end() || r.start() == r.end()) {
                ranges.push(Range {
                    anchor: start,
                    head: r.end(),
                });
            }
        }
        Selections::new(text, ranges, 0)
    }
}

/// A regex prompt uses UTF-8 byte offsets. Cursor movement follows Helix's
/// grapheme boundaries, while word movement also stops at path separators.
#[derive(Debug, Clone, Default)]
pub(crate) struct Prompt {
    pub line: String,
    pub cursor: usize,
    history_index: Option<usize>,
    replacement_start: usize,
    pub await_register: bool,
    named_history: Option<std::sync::Arc<[String]>>,
    history_candidates: std::sync::Arc<[String]>,
    matches: std::ops::Range<usize>,
    pub selected: Option<usize>,
}

impl Prompt {
    pub fn new(kind: Query, history: std::borrow::Cow<'_, [String]>) -> Self {
        let mut prompt = Self::default();
        if matches!(kind, Query::Forward | Query::Backward) {
            let mut entries: Vec<_> = history.iter().rev().take(200).cloned().collect();
            entries.sort_unstable();
            entries.dedup();
            prompt.matches = 0..entries.len();
            prompt.history_candidates = entries.into();
        }
        if let std::borrow::Cow::Owned(entries) = history {
            prompt.named_history = Some(entries.into());
        }
        prompt
    }

    pub fn history<'a>(&'a self, default: &'a [String]) -> &'a [String] {
        self.named_history.as_deref().unwrap_or(default)
    }

    pub(crate) fn set_shell_candidates(&mut self, start: usize, items: Vec<String>) {
        self.replacement_start = start;
        self.matches = 0..items.len();
        self.history_candidates = items.into();
    }

    pub fn candidates(&self) -> &[String] {
        &self.history_candidates[self.matches.clone()]
    }

    fn refilter(&mut self) {
        let entries = &self.history_candidates;
        let start = entries.partition_point(|entry| entry < &self.line);
        let count = entries[start..].partition_point(|entry| entry.starts_with(&self.line));
        self.matches = start..start + count;
        self.selected = None;
    }

    pub fn cycle(&mut self, backward: bool) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        let index = if backward {
            (self.selected.unwrap_or(0) + count - 1) % count
        } else {
            self.selected.map_or(0, |i| (i + 1) % count)
        };
        self.selected = Some(index);
        self.line.truncate(self.replacement_start);
        self.line
            .push_str(&self.history_candidates[self.matches.start + index]);
        self.cursor = self.line.len();
        if !backward && count == 1 && self.line.ends_with(std::path::MAIN_SEPARATOR) {
            self.refilter();
        }
    }

    pub fn keep_directory_completion(&mut self) -> bool {
        if self.selected.is_some() && self.line.ends_with(std::path::MAIN_SEPARATOR) {
            self.refilter();
            return true;
        }
        false
    }

    pub fn paste(&mut self, value: &str) {
        self.line.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.refilter();
    }

    fn grapheme(&self, forward: bool) -> usize {
        let mut cursor =
            unicode_segmentation::GraphemeCursor::new(self.cursor, self.line.len(), false);
        let boundary = if forward {
            cursor.next_boundary(&self.line, 0)
        } else {
            cursor.prev_boundary(&self.line, 0)
        };
        boundary.ok().flatten().unwrap_or(self.cursor)
    }

    fn word(&self, forward: bool) -> usize {
        let separator = |c: char| c.is_whitespace() || c == std::path::MAIN_SEPARATOR;
        if forward {
            let mut chars = self.line[self.cursor..].char_indices().peekable();
            while chars.next_if(|(_, c)| !separator(*c)).is_some() {}
            while chars.next_if(|(_, c)| separator(*c)).is_some() {}
            return chars
                .next()
                .map_or(self.line.len(), |(i, _)| self.cursor + i);
        }
        // The reference starts at the last character when the cursor is at EOF,
        // and looks for a separator strictly before the preceding character.
        let mut chars = self.line[..self.cursor].char_indices().rev();
        if self.cursor == self.line.len() {
            chars.next();
        }
        chars.next();
        chars
            .find(|(_, c)| separator(*c))
            .map_or(0, |(i, c)| i + c.len_utf8())
    }

    pub fn recall(&mut self, history: &[String], older: bool) {
        let Some(last) = history.len().checked_sub(1) else {
            return;
        };
        let index = if older {
            self.history_index
                .unwrap_or(history.len())
                .saturating_sub(1)
        } else {
            self.history_index.map_or(0, |i| i + 1).min(last)
        };
        self.line.clone_from(&history[index]);
        self.cursor = self.line.len();
        self.history_index = Some(index);
        self.refilter();
    }

    /// Returns whether the editor should refresh the live regex selection.
    pub fn edit(&mut self, key: &str) -> bool {
        let (target, delete) = match key {
            "<c-q>" => {
                self.selected = None;
                return false;
            }
            "<left>" | "<c-b>" => (self.grapheme(false), false),
            "<right>" | "<c-f>" => (self.grapheme(true), false),
            "<backspace>" | "<c-h>" => (self.grapheme(false), true),
            "<delete>" | "<c-d>" => (self.grapheme(true), true),
            "<a-b>" | "<c-left>" => (self.word(false), false),
            "<a-f>" | "<c-right>" => (self.word(true), false),
            "<c-w>" | "<a-backspace>" | "<c-backspace>" => (self.word(false), true),
            "<a-d>" | "<a-delete>" | "<c-delete>" => (self.word(true), true),
            "<home>" | "<c-a>" => (0, false),
            "<end>" | "<c-e>" => (self.line.len(), false),
            "<c-u>" => (0, true),
            "<c-k>" => (self.line.len(), true),
            _ => {
                let mut chars = key.chars();
                if let Some(ch) = chars.next()
                    && chars.next().is_none()
                {
                    self.line.insert(self.cursor, ch);
                    self.cursor = self.grapheme(true);
                    self.refilter();
                    return true;
                }
                return false;
            }
        };
        if delete {
            self.line
                .replace_range(target.min(self.cursor)..target.max(self.cursor), "");
            self.cursor = target.min(self.cursor);
            self.refilter();
        } else {
            self.cursor = target;
        }
        delete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_uses_the_last_two_hundred_searches_before_deduplicating() {
        let mut history: Vec<_> = (0..201).rev().map(|i| format!("q{i:03}")).collect();
        history.push("q008".into());
        let mut prompt = Prompt::new(Query::Forward, history.as_slice().into());
        assert_eq!(prompt.candidates().len(), 199);
        assert_eq!(prompt.candidates().first().unwrap(), "q000");
        assert_eq!(prompt.candidates().last().unwrap(), "q198");
        prompt.paste("q19");
        assert_eq!(prompt.candidates().len(), 9);
        prompt.cycle(true);
        assert_eq!(prompt.line, "q198");
        assert!(
            Prompt::new(Query::Select, history.as_slice().into())
                .candidates()
                .is_empty()
        );
    }
}
