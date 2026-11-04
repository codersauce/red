//! Register values belong to the editing session and can span documents.

use crate::Selections;
use ropey::RopeSlice;
use std::{borrow::Cow, collections::BTreeMap};

#[derive(Debug, Default)]
pub struct Registers {
    // Ordinary slots are case-sensitive and retain one fragment per selection.
    slots: BTreeMap<char, Vec<String>>,
    /// Prompt history in chronological order. Reading the search register returns newest first.
    pub search: Vec<String>,
}

impl Registers {
    pub fn stored(&self, name: char) -> Option<&[String]> {
        self.slots.get(&name).map(Vec::as_slice)
    }

    pub fn previews(&self) -> impl Iterator<Item = (char, &str)> {
        self.slots
            .iter()
            .filter_map(|(name, values)| Some((*name, values.first()?.as_str())))
    }

    pub fn write(&mut self, name: char, values: Vec<String>) {
        match name {
            '_' => {}
            '/' => self.search = values.into_iter().rev().collect(),
            _ => {
                self.slots.insert(name, values);
            }
        }
    }

    pub fn read(
        &self,
        name: char,
        text: RopeSlice<'_>,
        selections: &Selections,
        document: &str,
    ) -> Cow<'_, [String]> {
        match name {
            '_' => Cow::Borrowed(&[]),
            '#' => Cow::Owned(
                (1..=selections.ranges().len())
                    .map(|i| i.to_string())
                    .collect(),
            ),
            '.' => Cow::Owned(
                selections
                    .ranges()
                    .iter()
                    .map(|r| text.slice(r.start()..r.end()).to_string())
                    .collect(),
            ),
            '%' => Cow::Owned(vec![document.to_owned()]),
            '/' => Cow::Owned(self.search.iter().rev().cloned().collect()),
            _ => Cow::Borrowed(self.stored(name).unwrap_or_default()),
        }
    }
}
