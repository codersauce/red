use super::*;
use std::collections::HashSet;

pub(super) fn positions(
    text: RopeSlice<'_>,
    selections: &Selections,
    key: char,
    count: usize,
    resolve: Option<&dyn SyntaxPairs>,
) -> Result<Vec<(usize, usize)>, &'static str> {
    let mut used = HashSet::new();
    selections
        .ranges()
        .iter()
        .map(|&range| {
            let pair = if key == 'm' {
                resolve.map_or_else(
                    || crate::motion::closest_pair(text, range, count),
                    |pairs| pairs.closest(text, range, count),
                )
            } else {
                let (open, close) = crate::motion::brackets(key).unwrap_or((key, key));
                if text.len_chars() < 2 {
                    return Err("Surround pair not found around all cursors");
                }
                if range.end() >= text.len_chars() {
                    return Err("Cursor range exceeds text length");
                }
                if open == close && text.get_char(range.cursor(text)) == Some(open) {
                    return Err("Cursor on ambiguous surround pair");
                }
                crate::motion::explicit_pair(text, range, key, count)
            }
            .ok_or("Surround pair not found around all cursors")?;
            if !used.insert(pair.0) || !used.insert(pair.1) {
                return Err("Cursors overlap for a single surround pair range");
            }
            Ok(pair)
        })
        .collect()
}

impl Plan {
    pub(super) fn add_surround(&mut self, selections: &Selections, open: &str, close: &str) {
        let width = open.chars().count() + close.chars().count();
        for (i, &r) in selections.ranges().iter().enumerate() {
            for (pos, value) in [(r.start(), open), (r.end(), close)] {
                self.edits.push(Edit {
                    start: pos,
                    end: pos,
                    insert: value.into(),
                });
            }
            self.ranges[i] = r.with_bounds(r.start() + i * width, r.end() + (i + 1) * width);
        }
        self.history = History::Atomic;
        self.next.mode = Mode::Normal;
    }

    pub(super) fn edit_pairs(
        &mut self,
        positions: &[(usize, usize)],
        replacement: Option<(char, char)>,
    ) {
        for &(start, end) in positions {
            for (pos, value) in [
                (start, replacement.map(|p| p.0)),
                (end, replacement.map(|p| p.1)),
            ] {
                self.edits.push(Edit {
                    start: pos,
                    end: pos + 1,
                    insert: value.into_iter().collect(),
                });
            }
        }
        self.edits.sort_by_key(|e| e.start);
        if replacement.is_none() {
            for r in &mut self.ranges {
                let shifted = |p| p - self.edits.partition_point(|e| e.start < p);
                r.anchor = shifted(r.anchor);
                r.head = shifted(r.head);
            }
        }
        self.history = History::Atomic;
        self.next.mode = Mode::Normal;
    }
}
