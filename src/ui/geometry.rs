//! Saturating terminal-cell rectangles shared by editor-owned surfaces.

/// A rectangle expressed in absolute terminal columns and rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScreenRect {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

impl ScreenRect {
    /// Returns whether an absolute terminal cell lies inside the rectangle.
    #[must_use]
    pub(crate) fn contains(self, column: usize, row: usize) -> bool {
        column >= self.x
            && column < self.x.saturating_add(self.width)
            && row >= self.y
            && row < self.y.saturating_add(self.height)
    }

    /// Returns rows remaining after a one-line surface header.
    #[must_use]
    pub(crate) fn content_height(self) -> usize {
        self.height.saturating_sub(1)
    }

    /// Maps an absolute terminal row to its zero-based content row.
    #[must_use]
    pub(crate) fn content_offset(self, row: usize) -> Option<usize> {
        let content_y = self.y.saturating_add(1);
        (row >= content_y && row < self.y.saturating_add(self.height))
            .then_some(row.saturating_sub(content_y))
    }
}

#[cfg(test)]
mod tests {
    use super::ScreenRect;

    #[test]
    fn rectangle_uses_exclusive_saturating_edges() {
        let rect = ScreenRect {
            x: 3,
            y: 4,
            width: 5,
            height: 3,
        };

        assert!(rect.contains(3, 4));
        assert!(rect.contains(7, 6));
        assert!(!rect.contains(8, 6));
        assert!(!rect.contains(7, 7));
        assert_eq!(rect.content_offset(5), Some(0));
        assert_eq!(rect.content_offset(4), None);
    }

    #[test]
    fn empty_and_near_maximum_rectangles_do_not_overflow() {
        let rect = ScreenRect {
            x: usize::MAX,
            y: usize::MAX,
            width: 2,
            height: 0,
        };

        assert!(!rect.contains(usize::MAX, usize::MAX));
        assert_eq!(rect.content_height(), 0);
        assert_eq!(rect.content_offset(usize::MAX), None);
    }
}
