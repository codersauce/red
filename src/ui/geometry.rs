//! Saturating terminal-cell rectangles shared by editor-owned surfaces.

/// A rectangle expressed in absolute terminal columns and rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScreenRect {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// Terminal-space constraints for an editor-owned overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayLayout {
    pub(crate) viewport: ScreenRect,
    pub(crate) anchor: (usize, usize),
    pub(crate) avoid_rows: Option<(usize, usize)>,
    /// The clicked card takes priority when its complete source range will not fit.
    pub(crate) protected_rows: Option<(usize, usize)>,
}

impl OverlayLayout {
    /// Fit a source-anchored popup inside its owning window. Prefer avoiding the
    /// complete source range, but retain usable controls when the range fills it.
    pub(crate) fn popup_geometry(self, width: usize, height: usize) -> (usize, usize, usize) {
        let viewport = self.viewport;
        let anchor = (
            self.anchor
                .0
                .saturating_sub(viewport.x)
                .min(viewport.width.saturating_sub(1)),
            self.anchor
                .1
                .saturating_sub(viewport.y)
                .min(viewport.height.saturating_sub(1)),
        );
        let local_rows = |(start, end): (usize, usize)| {
            let viewport_end = viewport.y.saturating_add(viewport.height.saturating_sub(1));
            let start = start.max(viewport.y);
            let end = end.min(viewport_end);
            (start <= end).then_some((
                start.saturating_sub(viewport.y),
                end.saturating_sub(viewport.y),
            ))
        };
        let avoid_rows = self.avoid_rows.and_then(local_rows);
        let protected_rows = self.protected_rows.and_then(local_rows);
        let (x, y, available_height) = avoid_rows.map_or_else(
            || anchored_popup_geometry(anchor, viewport.width, viewport.height, width, height),
            |rows| {
                anchored_popup_geometry_avoiding_rows(
                    anchor,
                    rows,
                    viewport.width,
                    viewport.height,
                    width,
                    height,
                )
            },
        );
        let (x, y, height) = if available_height < height.min(2) {
            let protected = protected_rows.map(|rows| {
                anchored_popup_geometry_avoiding_rows(
                    anchor,
                    rows,
                    viewport.width,
                    viewport.height,
                    width,
                    height,
                )
            });
            if let Some(geometry) = protected.filter(|(_, _, available)| *available > 0) {
                geometry
            } else {
                anchored_popup_geometry(anchor, viewport.width, viewport.height, width, height)
            }
        } else {
            (x, y, available_height)
        };
        (
            viewport.x.saturating_add(x),
            viewport.y.saturating_add(y),
            height,
        )
    }
}

/// Fits a cursor-anchored popup inside the editor viewport.
///
/// The popup prefers the side of the cursor with the most vertical room and
/// shifts left when its right edge would leave the viewport. The returned
/// height is the visible content height; callers add their own border cells.
pub(crate) fn anchored_popup_geometry(
    anchor: (usize, usize),
    viewport_width: usize,
    viewport_height: usize,
    content_width: usize,
    content_height: usize,
) -> (usize, usize, usize) {
    anchored_popup_geometry_avoiding_rows(
        anchor,
        (anchor.1, anchor.1),
        viewport_width,
        viewport_height,
        content_width,
        content_height,
    )
}

/// Fits a popup above or below a rendered row band without covering it.
///
/// The inclusive `avoid_rows` band replaces the single cursor row used by
/// [`anchored_popup_geometry`]. This is useful for editor actions whose target
/// can span multiple rendered rows or grow after an edit.
pub(crate) fn anchored_popup_geometry_avoiding_rows(
    anchor: (usize, usize),
    avoid_rows: (usize, usize),
    viewport_width: usize,
    viewport_height: usize,
    content_width: usize,
    content_height: usize,
) -> (usize, usize, usize) {
    let width = content_width.min(viewport_width.saturating_sub(2));
    let max_x = viewport_width.saturating_sub(width.saturating_add(2));
    let wide = width.saturating_add(2) >= viewport_width.saturating_mul(2) / 3;
    let x = if wide {
        usize::from(max_x > 0)
    } else {
        anchor.0.min(max_x)
    };
    let (avoid_start, avoid_end) = if avoid_rows.0 <= avoid_rows.1 {
        avoid_rows
    } else {
        (avoid_rows.1, avoid_rows.0)
    };
    let below = viewport_height.saturating_sub(avoid_end.saturating_add(3));
    let above = avoid_start.saturating_sub(2);
    let capacity = if below >= content_height || below >= above {
        below
    } else {
        above
    };
    let height = content_height.min(capacity);
    let y = if capacity == above && above > below {
        avoid_start.saturating_sub(height.saturating_add(2))
    } else {
        avoid_end.saturating_add(1)
    };
    (x, y, height)
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
    use super::{
        anchored_popup_geometry, anchored_popup_geometry_avoiding_rows, OverlayLayout, ScreenRect,
    };

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

    #[test]
    fn anchored_popup_prefers_available_space_and_stays_in_bounds() {
        assert_eq!(anchored_popup_geometry((8, 1), 20, 10, 8, 3), (8, 2, 3));
        assert_eq!(anchored_popup_geometry((18, 8), 20, 10, 8, 3), (10, 3, 3));
        assert_eq!(anchored_popup_geometry((0, 0), 1, 1, 8, 3), (0, 1, 0));
    }

    #[test]
    fn anchored_popup_can_avoid_a_multi_row_target() {
        assert_eq!(
            anchored_popup_geometry_avoiding_rows((8, 4), (4, 7), 20, 14, 8, 2),
            (8, 8, 2)
        );
        assert_eq!(
            anchored_popup_geometry_avoiding_rows((8, 10), (8, 11), 20, 14, 8, 2),
            (8, 4, 2)
        );
    }

    #[test]
    fn clicked_card_has_priority_when_the_source_fills_the_viewport() {
        let layout = OverlayLayout {
            viewport: ScreenRect {
                x: 40,
                y: 3,
                width: 60,
                height: 18,
            },
            anchor: (45, 9),
            avoid_rows: Some((3, 20)),
            protected_rows: Some((6, 8)),
        };
        let (_, y, height) = layout.popup_geometry(50, 12);
        assert!(height >= 2);
        assert!(y > 8 || y + height + 2 <= 6);
        assert!(y >= 3 && y + height + 2 <= 21);
    }
}
