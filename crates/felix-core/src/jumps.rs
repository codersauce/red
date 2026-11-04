use crate::Selections;

/// A view's navigation history. The host supplies its stable document IDs.
#[derive(Debug)]
pub struct Jumps<D> {
    entries: Vec<(D, Selections)>,
    cursor: usize,
}

impl<D> Default for Jumps<D> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
        }
    }
}

impl<D: Copy + Eq> Jumps<D> {
    pub fn save(&mut self, doc: D, selection: &Selections) {
        self.entries.truncate(self.cursor);
        if self
            .entries
            .last()
            .is_some_and(|(d, s)| *d == doc && s == selection)
        {
            return;
        }
        if self.entries.len() == 30 {
            self.entries.remove(0);
        }
        self.entries.push((doc, selection.clone()));
        self.cursor = self.entries.len();
    }

    pub fn navigate(
        &mut self,
        doc: D,
        selection: &Selections,
        forward: bool,
        count: usize,
    ) -> Option<(D, Selections)> {
        let dest = if forward {
            self.cursor
                .checked_add(count)
                .filter(|&i| i < self.entries.len())?
        } else {
            let mut target = self.cursor.checked_sub(count)?;
            if self.cursor == self.entries.len() {
                let old = self.entries.len();
                let duplicate = self
                    .entries
                    .last()
                    .is_some_and(|(d, s)| *d == doc && s == selection);
                self.save(doc, selection);
                if old == 30 && !duplicate {
                    target = target.saturating_sub(1);
                }
            }
            if self
                .entries
                .get(target)
                .is_some_and(|(d, s)| *d == doc && s == selection)
            {
                target = target.checked_sub(1)?;
            }
            target
        };
        self.cursor = dest;
        self.entries.get(dest).cloned()
    }

    pub fn rebase(
        &mut self,
        doc: D,
        text: ropey::RopeSlice<'_>,
        edits: impl Iterator<Item = (usize, usize, usize)> + Clone,
    ) {
        for (entry_doc, selection) in &mut self.entries {
            if *entry_doc == doc {
                *selection = selection.rebase(text, edits.clone());
            }
        }
    }

    pub fn forget(&mut self, doc: D) {
        self.cursor -= self.entries[..self.cursor]
            .iter()
            .filter(|(d, _)| *d == doc)
            .count();
        self.entries.retain(|(d, _)| *d != doc);
    }
}
