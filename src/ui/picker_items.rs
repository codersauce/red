//! Immutable picker rows shared between discovery, matching, and the UI.
//!
//! A snapshot copies only batch references. Appending a discovery batch never
//! clones previously published rows, and dropping a view does not free the index.

use std::{ops::Index, sync::Arc};

use rayon::prelude::*;

use super::PickerItem;

#[derive(Clone, Default)]
pub(crate) struct PickerItems {
    chunks: Arc<[Arc<[PickerItem]>]>,
    ends: Arc<[usize]>,
    len: usize,
}

impl PickerItems {
    pub(crate) fn from_chunks(chunks: Vec<Arc<[PickerItem]>>) -> Self {
        let mut len = 0;
        let ends = chunks
            .iter()
            .map(|chunk| {
                len += chunk.len();
                len
            })
            .collect();
        Self {
            chunks: chunks.into(),
            ends,
            len,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn get(&self, index: usize) -> Option<&PickerItem> {
        let chunk = self.ends.partition_point(|end| *end <= index);
        let start = chunk
            .checked_sub(1)
            .map_or(0, |previous| self.ends[previous]);
        self.chunks.get(chunk)?.get(index - start)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &PickerItem> {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }

    pub(crate) fn par_entries_from(
        &self,
        start: usize,
    ) -> impl ParallelIterator<Item = (usize, &PickerItem)> {
        self.chunks
            .par_iter()
            .enumerate()
            .flat_map_iter(move |(chunk_index, chunk)| {
                let offset = self.ends[chunk_index] - chunk.len();
                chunk
                    .iter()
                    .enumerate()
                    .skip(start.saturating_sub(offset))
                    .map(move |(index, item)| (offset + index, item))
            })
    }
}

impl From<Vec<PickerItem>> for PickerItems {
    fn from(items: Vec<PickerItem>) -> Self {
        Self::from_chunks(vec![items.into()])
    }
}

impl Index<usize> for PickerItems {
    type Output = PickerItem;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("picker row index is in bounds")
    }
}
