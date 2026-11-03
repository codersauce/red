//! Inclusive selections in Unicode scalar indices.
//!
//! A cursor is a one-scalar range whose anchor and head are equal. The set is
//! never empty: a document with no scalars still has a cursor at 0. Endpoints
//! follow the same left/right affinity rules as Red's edit anchors.

use unicode_segmentation::UnicodeSegmentation;

/// Inclusive range. `head` is the moving end (the cursor); `anchor` stays put
/// while a selection is extended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
}

impl Range {
    pub const fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// One-scalar cursor on `index`. `index` may equal the document length
    /// when the document has no scalars.
    pub const fn cursor(index: usize) -> Self {
        Self {
            anchor: index,
            head: index,
        }
    }

    pub const fn from(self) -> usize {
        if self.anchor < self.head {
            self.anchor
        } else {
            self.head
        }
    }

    pub const fn to(self) -> usize {
        if self.anchor > self.head {
            self.anchor
        } else {
            self.head
        }
    }

    pub const fn is_cursor(self) -> bool {
        self.anchor == self.head
    }

    pub const fn is_forward(self) -> bool {
        self.anchor <= self.head
    }

    /// Half-open scalar span covered by this inclusive range, clamped to `len`.
    pub fn span(self, len: usize) -> (usize, usize) {
        if len == 0 {
            return (0, 0);
        }
        let start = self.from().min(len);
        let end = self.to().saturating_add(1).min(len);
        (start, end.max(start))
    }

    pub const fn flip(self) -> Self {
        Self {
            anchor: self.head,
            head: self.anchor,
        }
    }

    pub const fn collapsed(self) -> Self {
        Self::cursor(self.head)
    }

    /// Covers `[start, end]` inclusive, keeping `forward` direction.
    pub const fn covering(start: usize, end: usize, forward: bool) -> Self {
        if forward {
            Self {
                anchor: start,
                head: end,
            }
        } else {
            Self {
                anchor: end,
                head: start,
            }
        }
    }

    /// Maps both ends through one replacement of `[start, old_end)`.
    ///
    /// A cursor uses right affinity so an insertion at the cursor leaves the
    /// cursor after the inserted text. A wider range keeps its anchor on the
    /// left edge of the edit and its head on the right edge.
    pub fn map_edit(self, start: usize, old_end: usize, new_end: usize) -> Self {
        let cursor = self.is_cursor();
        Self {
            anchor: map_point(self.anchor, start, old_end, new_end, cursor),
            head: map_point(self.head, start, old_end, new_end, true),
        }
    }
}

/// Right affinity sticks to text inserted at the index. Left affinity stays
/// before that insertion. Points strictly inside the replaced span collapse
/// to the corresponding edge.
fn map_point(index: usize, start: usize, old_end: usize, new_end: usize, right: bool) -> usize {
    let replaced = old_end.saturating_sub(start);
    let inserted = new_end.saturating_sub(start);
    if index < start {
        index
    } else if replaced == 0 && index == start {
        if right {
            start.saturating_add(inserted)
        } else {
            start
        }
    } else if index >= old_end {
        index.saturating_sub(replaced).saturating_add(inserted)
    } else if right {
        start.saturating_add(inserted)
    } else {
        start
    }
}

/// A non-empty, ordered, non-overlapping set of ranges.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Selection {
    ranges: Vec<Range>,
    primary: usize,
}

impl Default for Selection {
    fn default() -> Self {
        Self::cursor(0)
    }
}

impl Selection {
    pub fn cursor(index: usize) -> Self {
        Self {
            ranges: vec![Range::cursor(index)],
            primary: 0,
        }
    }

    pub fn single(range: Range) -> Self {
        Self {
            ranges: vec![range],
            primary: 0,
        }
    }

    pub fn from_ranges(ranges: Vec<Range>, primary: usize) -> Self {
        if ranges.is_empty() {
            return Self::cursor(0);
        }
        Self {
            primary: primary.min(ranges.len() - 1),
            ranges,
        }
    }

    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty_document_cursor(&self) -> bool {
        self.len() == 1 && self.primary().is_cursor()
    }

    pub fn map_edit(&mut self, start: usize, old_end: usize, new_end: usize, len: usize) {
        for range in &mut self.ranges {
            *range = range.map_edit(start, old_end, new_end);
        }
        self.normalize(len);
    }

    /// Sorts, clamps, and merges overlapping or touching ranges.
    ///
    /// A merged range covers the union. Its direction follows the range that
    /// contains the primary head: forward ranges put the head on the right.
    pub fn normalize(&mut self, len: usize) {
        if self.ranges.is_empty() {
            *self = Self::cursor(0);
            return;
        }
        let primary_head = self.primary().head.min(len);
        let primary_forward = self.primary().is_forward();
        for range in &mut self.ranges {
            range.anchor = range.anchor.min(len);
            range.head = range.head.min(len);
        }
        self.ranges.sort_by_key(|range| (range.from(), range.to()));
        let mut merged: Vec<Range> = Vec::with_capacity(self.ranges.len());
        for range in self.ranges.drain(..) {
            if let Some(last) = merged.last_mut() {
                if range.from() <= last.to().saturating_add(1) {
                    let start = last.from().min(range.from());
                    let end = last.to().max(range.to());
                    let head_in_range = range.from() <= primary_head && primary_head <= range.to();
                    let head_in_last = last.from() <= primary_head && primary_head <= last.to();
                    let forward = if head_in_range {
                        range.is_forward()
                    } else if head_in_last {
                        last.is_forward()
                    } else {
                        primary_forward
                    };
                    *last = Range::covering(start, end, forward);
                    continue;
                }
            }
            merged.push(range);
        }
        self.ranges = merged;
        self.primary = self
            .ranges
            .iter()
            .position(|range| range.from() <= primary_head && primary_head <= range.to())
            .unwrap_or(0);
    }

    pub fn transform_each(&self, mut visit: impl FnMut(Range) -> Range) -> Self {
        let ranges: Vec<Range> = self.ranges.iter().copied().map(&mut visit).collect();
        let primary_head = ranges
            .get(self.primary)
            .map(|range| range.head)
            .unwrap_or(0);
        let mut selection = Self::from_ranges(ranges, self.primary);
        selection.primary = selection
            .ranges
            .iter()
            .position(|range| range.head == primary_head)
            .or_else(|| {
                selection
                    .ranges
                    .iter()
                    .position(|range| range.from() <= primary_head && primary_head <= range.to())
            })
            .unwrap_or(0);
        selection
    }

    pub fn collapse(&self) -> Self {
        self.transform_each(Range::collapsed)
    }

    pub fn flip(&self) -> Self {
        self.transform_each(Range::flip)
    }

    pub fn keep_primary(&self) -> Self {
        Self::single(self.primary())
    }

    /// Drops the primary range. A one-range set is unchanged.
    pub fn remove_primary(&self) -> Option<Self> {
        if self.ranges.len() <= 1 {
            return None;
        }
        let mut ranges = self.ranges.clone();
        ranges.remove(self.primary);
        let primary = self.primary.saturating_sub(1).min(ranges.len() - 1);
        Some(Self { ranges, primary })
    }

    pub fn rotate(&self, forward: bool) -> Self {
        if self.ranges.len() <= 1 {
            return self.clone();
        }
        let primary = if forward {
            (self.primary + 1) % self.ranges.len()
        } else {
            (self.primary + self.ranges.len() - 1) % self.ranges.len()
        };
        Self {
            ranges: self.ranges.clone(),
            primary,
        }
    }

    /// True when the set is non-empty, ordered, non-overlapping, and every
    /// endpoint is on a grapheme boundary inside `text`.
    pub fn invariants_hold(&self, text: &str) -> bool {
        let len = text.chars().count();
        if self.ranges.is_empty() || self.primary >= self.ranges.len() {
            return false;
        }
        let mut boundaries = vec![false; len + 1];
        boundaries[0] = true;
        let mut index = 0;
        for grapheme in text.graphemes(true) {
            index += grapheme.chars().count();
            if index <= len {
                boundaries[index] = true;
            }
        }
        let mut previous_end = None;
        for range in &self.ranges {
            if range.anchor > len || range.head > len {
                return false;
            }
            if !boundaries[range.anchor] || !boundaries[range.head] {
                return false;
            }
            if let Some(end) = previous_end {
                if range.from() <= end {
                    return false;
                }
            }
            previous_end = Some(range.to());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_covers_one_scalar_and_an_empty_document_stays_at_zero() {
        let cursor = Range::cursor(2);
        assert_eq!(cursor.span(5), (2, 3));
        assert!(cursor.is_cursor());
        assert_eq!(Range::cursor(0).span(0), (0, 0));
        assert!(Selection::default().invariants_hold(""));
    }

    #[test]
    fn edit_mapping_uses_right_affinity_for_a_cursor_and_keeps_a_wide_range() {
        let mut cursor = Selection::cursor(1);
        cursor.map_edit(1, 1, 4, 7);
        assert_eq!(cursor.primary(), Range::cursor(4));

        let mut wide = Selection::single(Range::new(0, 4));
        wide.map_edit(1, 3, 2, 4);
        assert_eq!(wide.primary().from(), 0);
        assert_eq!(wide.primary().to(), 3);
        assert!(wide.invariants_hold("halo"));
    }

    #[test]
    fn deletion_inside_a_selection_collapses_the_covered_end() {
        let mut selection = Selection::single(Range::new(0, 4));
        selection.map_edit(1, 4, 1, 2);
        assert_eq!(selection.primary(), Range::new(0, 1));
        assert!(selection.invariants_hold("ho"));
    }

    #[test]
    fn overlapping_ranges_merge_and_the_set_stays_non_empty() {
        let mut selection = Selection::from_ranges(
            vec![Range::new(0, 2), Range::new(4, 3), Range::cursor(8)],
            1,
        );
        selection.normalize(9);
        assert_eq!(selection.len(), 2);
        assert_eq!(selection.primary().from(), 0);
        assert_eq!(selection.primary().to(), 4);
        assert!(selection.invariants_hold("abcdefghi"));
        assert!(selection.remove_primary().is_some());
        assert!(selection.keep_primary().remove_primary().is_none());
    }

    #[test]
    fn flip_collapse_and_rotate_keep_grapheme_boundaries() {
        let text = "a\u{0301}b";
        let selection = Selection::from_ranges(vec![Range::cursor(0), Range::new(2, 2)], 1);
        assert!(selection.invariants_hold(text));
        let flipped = selection.flip();
        assert_eq!(flipped.primary(), Range::cursor(2));
        assert!(flipped.invariants_hold(text));
        let collapsed = selection.collapse();
        assert!(collapsed.ranges().iter().all(|range| range.is_cursor()));
        assert_eq!(selection.rotate(true).primary_index(), 0);
        assert_eq!(selection.rotate(false).primary_index(), 0);
    }

    #[test]
    fn a_cursor_inside_a_combining_grapheme_is_not_a_valid_selection() {
        let text = "a\u{0301}b";
        assert!(!Selection::cursor(1).invariants_hold(text));
        assert!(Selection::single(Range::new(0, 2)).invariants_hold(text));
    }
}
