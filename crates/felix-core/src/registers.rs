//! Register values belong to the editing session and can span documents.

use crate::Selections;
use ropey::RopeSlice;
use std::{borrow::Cow, collections::BTreeMap};

#[derive(Debug, Default)]
pub struct Registers {
    // Ordinary slots are case-sensitive and retain one fragment per selection.
    slots: BTreeMap<char, Vec<String>>,
    /// Prompt history in chronological order. Reading the search register returns newest first.
    search: Option<Vec<String>>,
}

impl Registers {
    pub fn search_history(&self) -> &[String] {
        self.search.as_deref().unwrap_or_default()
    }

    pub fn stored(&self, name: char) -> Option<&[String]> {
        self.slots.get(&name).map(Vec::as_slice)
    }

    pub fn previews(&self) -> impl Iterator<Item = (char, &str)> {
        self.slots
            .iter()
            .map(|(name, values)| (*name, values.first().map_or("<empty>", String::as_str)))
            .chain(
                self.search
                    .as_ref()
                    .map(|values| ('/', values.last().map_or("<empty>", String::as_str))),
            )
    }

    pub fn exists(&self, name: char) -> bool {
        match name {
            '_' | '#' | '.' | '%' => true,
            '/' => self.search.is_some(),
            _ => self.slots.contains_key(&name),
        }
    }

    /// A submitted query becomes the first item when the register is next read.
    pub fn push_query(&mut self, name: char, value: String) {
        match name {
            '/' => self.search.get_or_insert_default().push(value),
            '_' => {}
            _ => self.slots.entry(name).or_default().insert(0, value),
        }
    }

    pub fn write(&mut self, name: char, values: Vec<String>) {
        match name {
            '_' => {}
            '/' => self.search = Some(values.into_iter().rev().collect()),
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
            '/' => Cow::Owned(self.search_history().iter().rev().cloned().collect()),
            _ => Cow::Borrowed(self.stored(name).unwrap_or_default()),
        }
    }
}
