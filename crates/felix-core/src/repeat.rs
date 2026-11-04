//! The last accepted Insert command belongs to the editor across documents and views.
use crate::{History, Mode};

#[derive(Debug, Default, Clone)]
pub struct LastInsert {
    keys: Vec<String>,
}

impl LastInsert {
    pub fn record(&mut self, key: &str, before: Mode, after: Mode) {
        if before != Mode::Insert {
            if after != Mode::Insert {
                return;
            }
            self.keys.clear();
        }
        self.keys.push(key.to_owned());
    }

    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    pub fn replay(&self, count: usize) -> impl Iterator<Item = ReplayKey<'_>> {
        let count = if self.keys.is_empty() { 0 } else { count };
        (0..count).flat_map(move |iteration| {
            self.keys
                .iter()
                .enumerate()
                .map(move |(index, key)| ReplayKey {
                    key,
                    first: iteration == 0 && index == 0,
                    last: iteration + 1 == count && index + 1 == self.keys.len(),
                })
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReplayKey<'a> {
    pub key: &'a str,
    first: bool,
    last: bool,
}

impl ReplayKey<'_> {
    /// A counted dot is one undo step. Explicit insert checkpoints still split it.
    pub fn history(self, history: History) -> History {
        match history {
            History::Begin if !self.first => History::None,
            History::Commit if !self.last => History::None,
            other => other,
        }
    }
}
