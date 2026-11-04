//! The last accepted Insert command belongs to the editor across documents and views.
use crate::{History, Mode};
use std::ops::Range;

#[derive(Debug, Clone)]
pub enum InsertEvent {
    Key(String),
    Completion(CompletionChange),
}

/// The part of the accepted main edit touching the primary cursor. Helix
/// stores this text after snippet expansion and before additional LSP edits.
#[derive(Debug, Clone)]
pub struct CompletionChange {
    before: usize,
    after: usize,
    pub(crate) text: String,
}

impl CompletionChange {
    pub(crate) fn destination(&self, cursor: usize, len: usize) -> Option<Range<usize>> {
        let end = cursor.checked_add(self.after)?;
        (end <= len).then(|| cursor.saturating_sub(self.before)..end)
    }
}

#[derive(Debug, Default, Clone)]
pub struct LastInsert {
    events: Vec<InsertEvent>,
}

impl LastInsert {
    pub fn record(&mut self, key: &str, before: Mode, after: Mode) {
        if before != Mode::Insert {
            if after != Mode::Insert {
                return;
            }
            self.events.clear();
        }
        self.events.push(InsertEvent::Key(key.to_owned()));
    }

    /// Adjacent edits are one change in the reference transaction. Only
    /// a change spanning the trigger offset becomes part of the dot recipe.
    pub fn record_completion(&mut self, ranges: &[Range<usize>], trigger: usize, text: &str) {
        for group in ranges.chunk_by(|a, b| a.end == b.start) {
            let start = group[0].start;
            let end = group[group.len() - 1].end;
            if (start..=end).contains(&trigger) {
                self.events.push(InsertEvent::Completion(CompletionChange {
                    before: trigger - start,
                    after: end - trigger,
                    text: text.repeat(group.len()),
                }));
                break;
            }
        }
    }

    /// Mark the start of tentative input used to filter an open completion.
    pub fn checkpoint(&self) -> usize {
        self.events.len()
    }

    pub fn restore(&mut self, checkpoint: usize) {
        self.events.truncate(checkpoint);
    }

    pub fn keys(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                InsertEvent::Key(key) => Some(key.as_str()),
                InsertEvent::Completion(_) => None,
            })
            .collect()
    }

    pub fn replay(&self, count: usize) -> impl Iterator<Item = ReplayStep<'_>> {
        let count = if self.events.is_empty() { 0 } else { count };
        (0..count).flat_map(move |iteration| {
            self.events
                .iter()
                .enumerate()
                .map(move |(index, event)| ReplayStep {
                    event,
                    first: iteration == 0 && index == 0,
                    last: iteration + 1 == count && index + 1 == self.events.len(),
                })
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReplayStep<'a> {
    pub event: &'a InsertEvent,
    first: bool,
    last: bool,
}

impl ReplayStep<'_> {
    /// A counted dot is one undo step. Explicit insert checkpoints still split it.
    pub fn history(self, history: History) -> History {
        match history {
            History::Begin if !self.first => History::None,
            History::Commit if !self.last => History::None,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Range as Selection, Selections, Session};
    use ropey::Rope;

    #[test]
    fn completion_repeat_keeps_adjacent_replacements_and_uses_the_new_primary_cursor() {
        let mut recipe = LastInsert::default();
        recipe.record("i", Mode::Normal, Mode::Insert);
        recipe.record_completion(&[0..1, 1..3, 5..6], 1, "λ");
        let mut text = Rope::from_str("😀 café\n");
        let mut selections = Selections::new(
            text.slice(..),
            vec![
                Selection::at(text.slice(..), 0),
                Selection::at(text.slice(..), 4),
            ],
            1,
        )
        .unwrap();
        let step = recipe.replay(1).nth(1).unwrap();
        let InsertEvent::Completion(change) = step.event else {
            panic!("expected completion")
        };
        let plan = Session::default().replay_completion(text.slice(..), &selections, change);
        assert_eq!(plan.edits.len(), 1);
        let edit = &plan.edits[0];
        assert_eq!((edit.start, edit.end), (3, 6));
        text.remove(edit.start..edit.end);
        text.insert(edit.start, &edit.insert);
        selections = plan.selections(text.slice(..));
        assert_eq!(text.to_string(), "😀 cλλ\n");
        assert_eq!(selections.primary_index(), 1);
        assert_eq!(selections.ranges()[0], Selection::at(text.slice(..), 0));
        assert_eq!(change.destination(0, 0), None);
    }

    #[test]
    fn unrelated_completion_edits_and_discarded_filter_keys_are_not_replayed() {
        let mut recipe = LastInsert::default();
        recipe.record("a", Mode::Normal, Mode::Insert);
        recipe.record("F", Mode::Insert, Mode::Insert);
        let trigger = recipe.checkpoint();
        recipe.record("e", Mode::Insert, Mode::Insert);
        recipe.restore(trigger);
        recipe.record_completion(&[0..1, 3..4], 2, "other");
        assert_eq!(recipe.keys(), ["a", "F"]);
        assert_eq!(recipe.replay(2).count(), 4);
    }
}
