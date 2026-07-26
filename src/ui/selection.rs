//! Selection and scrolling state shared by fixed-height terminal lists.

/// Keeps a selected row visible in a bounded list viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelectionViewport {
    selected: usize,
    top: usize,
    len: usize,
    height: usize,
}

impl SelectionViewport {
    /// Creates a selection for a list with the given row count and viewport height.
    #[must_use]
    pub(crate) const fn new(len: usize, height: usize) -> Self {
        Self {
            selected: 0,
            top: 0,
            len,
            height,
        }
    }

    #[must_use]
    pub(crate) const fn selected(self) -> usize {
        self.selected
    }

    #[must_use]
    pub(crate) const fn top(self) -> usize {
        self.top
    }

    #[must_use]
    pub(crate) const fn len(self) -> usize {
        self.len
    }

    /// Replaces the list and returns selection and scrolling to the first row.
    pub(crate) fn reset(&mut self, len: usize) {
        self.len = len;
        self.selected = 0;
        self.top = 0;
    }

    /// Updates viewport height while keeping the current selection visible.
    pub(crate) fn set_height(&mut self, height: usize) {
        self.height = height;
        self.ensure_visible();
    }

    /// Selects a bounded row and adjusts the visible window.
    pub(crate) fn select(&mut self, index: usize) {
        if self.len == 0 {
            self.selected = 0;
            self.top = 0;
            return;
        }
        self.selected = index.min(self.len.saturating_sub(1));
        self.ensure_visible();
    }

    /// Moves by a signed row count without overflowing terminal coordinates.
    pub(crate) fn move_by(&mut self, delta: isize) {
        if self.len == 0 {
            return;
        }
        let selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize)
        };
        self.select(selected);
    }

    fn ensure_visible(&mut self) {
        if self.len == 0 {
            self.selected = 0;
            self.top = 0;
        } else if self.height == 0 || self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top.saturating_add(self.height) {
            self.top = self.selected.saturating_sub(self.height.saturating_sub(1));
        }
    }
}

/// Tracks a streaming transcript without overriding manually interrupted tail following.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FollowTailViewport {
    offset: usize,
    following: bool,
}

impl Default for FollowTailViewport {
    fn default() -> Self {
        Self {
            offset: 0,
            following: true,
        }
    }
}

impl FollowTailViewport {
    #[must_use]
    pub(crate) const fn offset(self) -> usize {
        self.offset
    }

    #[must_use]
    pub(crate) const fn is_following(self) -> bool {
        self.following
    }

    #[must_use]
    pub(crate) fn visible_offset(self, maximum: usize) -> usize {
        if self.following {
            maximum
        } else {
            self.offset.min(maximum)
        }
    }

    pub(crate) fn restore(&mut self, offset: usize, following: bool) {
        self.offset = offset;
        self.following = following;
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn move_by(&mut self, delta: isize, maximum: usize) {
        self.offset = self.offset.saturating_add_signed(delta).min(maximum);
        self.following = self.offset == maximum;
    }

    pub(crate) fn scroll_to_top(&mut self) {
        self.offset = 0;
        self.following = false;
    }

    pub(crate) fn follow(&mut self, maximum: usize) {
        self.offset = maximum;
        self.following = true;
    }

    pub(crate) fn clamp(&mut self, maximum: usize) {
        self.offset = self.offset.min(maximum);
    }

    pub(crate) fn reveal(&mut self, row: usize, visible_rows: usize) {
        self.following = false;
        if row < self.offset {
            self.offset = row;
        } else if row >= self.offset.saturating_add(visible_rows) {
            self.offset = row.saturating_sub(visible_rows.saturating_sub(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FollowTailViewport, SelectionViewport};

    #[test]
    fn movement_keeps_selection_inside_the_viewport() {
        let mut viewport = SelectionViewport::new(12, 3);

        viewport.move_by(5);
        assert_eq!((viewport.selected(), viewport.top()), (5, 3));

        viewport.move_by(-4);
        assert_eq!((viewport.selected(), viewport.top()), (1, 1));
    }

    #[test]
    fn resizing_and_reset_keep_selection_bounded() {
        let mut viewport = SelectionViewport::new(8, 5);
        viewport.select(7);

        viewport.set_height(2);
        assert_eq!((viewport.selected(), viewport.top()), (7, 6));

        viewport.reset(0);
        assert_eq!(
            (viewport.selected(), viewport.top(), viewport.len()),
            (0, 0, 0)
        );
    }

    #[test]
    fn empty_and_zero_height_viewports_do_not_underflow() {
        let mut empty = SelectionViewport::new(0, 0);
        empty.move_by(isize::MIN);
        assert_eq!((empty.selected(), empty.top()), (0, 0));

        let mut hidden = SelectionViewport::new(4, 0);
        hidden.select(3);
        assert_eq!((hidden.selected(), hidden.top()), (3, 3));
    }

    #[test]
    fn streaming_viewport_follows_growth_until_manual_scroll() {
        let mut viewport = FollowTailViewport::default();
        viewport.follow(8);
        assert_eq!((viewport.offset(), viewport.is_following()), (8, true));

        viewport.scroll_to_top();
        assert_eq!(viewport.visible_offset(12), 0);
        assert!(!viewport.is_following());

        viewport.move_by(12, 12);
        assert_eq!((viewport.offset(), viewport.is_following()), (12, true));
    }

    #[test]
    fn revealing_a_link_interrupts_following_and_keeps_it_visible() {
        let mut viewport = FollowTailViewport::default();
        viewport.follow(10);

        viewport.reveal(2, 4);

        assert_eq!(viewport.offset(), 2);
        assert!(!viewport.is_following());
    }
}
