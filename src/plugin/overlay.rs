//! Plugin-owned floating overlays positioned within the terminal viewport.
//!
//! [`OverlayManager`] stores complete overlay models by stable plugin-provided ID and
//! resolves alignment against current terminal bounds during rendering. Creating an
//! existing ID replaces its configuration; removal is idempotent from the caller's
//! perspective.

use std::collections::HashMap;

use serde::Deserialize;

use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::Style,
    unicode_utils::display_width,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OverlayAlignment {
    Top,
    Bottom,
    AvoidCursor,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OverlayConfig {
    pub align: OverlayAlignment,
    pub x_padding: usize,
    pub y_padding: usize,
    pub relative: String, // "editor" or "window"
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            align: OverlayAlignment::Bottom,
            x_padding: 1,
            y_padding: 0,
            relative: "editor".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct OverlayContent {
    pub lines: Vec<(String, Style)>,
    pub dirty: bool,
}

#[derive(Debug)]
pub struct PluginOverlay {
    pub id: String,
    pub config: OverlayConfig,
    pub content: OverlayContent,
    pub position: Option<Point>,
    pub width: usize,
    pub height: usize,
}

impl PluginOverlay {
    pub fn new(id: String, config: OverlayConfig) -> Self {
        Self {
            id,
            config,
            content: OverlayContent {
                lines: Vec::new(),
                dirty: true,
            },
            position: None,
            width: 0,
            height: 0,
        }
    }

    /// Replaces the overlay content. Returns `false` when the new content is
    /// identical to the current one, so callers can skip a redraw.
    pub fn update_content(&mut self, lines: Vec<(String, Style)>) -> bool {
        if self.content.lines == lines {
            return false;
        }
        self.content.lines = lines;
        self.content.dirty = true;

        // Update dimensions
        self.height = self.content.lines.len();
        self.width = self
            .content
            .lines
            .iter()
            .map(|(text, _)| display_width(text))
            .max()
            .unwrap_or(0);
        true
    }

    pub fn has_content(&self) -> bool {
        !self.content.lines.is_empty()
    }

    pub fn calculate_position(
        &mut self,
        editor_width: usize,
        editor_height: usize,
        cursor_pos: Option<Point>,
    ) -> Point {
        let x = if self.width + self.config.x_padding > editor_width {
            0
        } else {
            editor_width - self.width - self.config.x_padding
        };

        let y = match self.config.align {
            OverlayAlignment::Top => self.config.y_padding,
            OverlayAlignment::Bottom => {
                let bottom = editor_height.saturating_sub(2); // Account for status line
                bottom
                    .saturating_sub(self.height)
                    .saturating_sub(self.config.y_padding)
            }
            OverlayAlignment::AvoidCursor => {
                if let Some(cursor) = cursor_pos {
                    // If cursor is in top half, show at bottom
                    if cursor.y < editor_height / 2 {
                        editor_height
                            .saturating_sub(2)
                            .saturating_sub(self.height)
                            .saturating_sub(self.config.y_padding)
                    } else {
                        self.config.y_padding
                    }
                } else {
                    // Default to bottom if no cursor position
                    editor_height
                        .saturating_sub(2)
                        .saturating_sub(self.height)
                        .saturating_sub(self.config.y_padding)
                }
            }
        };

        let position = Point::new(x, y);
        if self.position != Some(position) {
            self.content.dirty = true;
        }
        self.position = Some(position);
        position
    }

    pub fn render(&self, buffer: &mut RenderBuffer) {
        if let Some(pos) = self.position {
            for (i, (text, style)) in self.content.lines.iter().enumerate() {
                let y = pos.y + i;
                if y < buffer.height - 2 {
                    // Don't render over status line
                    let text_width = display_width(text);
                    let text_x = pos.x + self.width.saturating_sub(text_width);
                    buffer.set_text(text_x, y, text, style);
                }
            }
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.content.dirty
    }

    pub fn mark_clean(&mut self) {
        self.content.dirty = false;
    }
}

#[derive(Default)]
pub struct OverlayManager {
    overlays: HashMap<String, PluginOverlay>,
    z_order: Vec<String>, // Track rendering order
}

impl OverlayManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_overlay(&mut self, id: String, config: OverlayConfig) -> &mut PluginOverlay {
        self.overlays
            .insert(id.clone(), PluginOverlay::new(id.clone(), config));
        if !self.z_order.contains(&id) {
            self.z_order.push(id.clone());
        }
        self.overlays.get_mut(&id).unwrap()
    }

    pub fn get_overlay_mut(&mut self, id: &str) -> Option<&mut PluginOverlay> {
        self.overlays.get_mut(id)
    }

    pub fn remove_overlay(&mut self, id: &str) -> Option<PluginOverlay> {
        self.z_order.retain(|z_id| z_id != id);
        self.overlays.remove(id)
    }

    pub fn update_positions(
        &mut self,
        editor_width: usize,
        editor_height: usize,
        cursor_pos: Option<Point>,
    ) {
        // For now, just update each overlay independently
        // In the future, we might want to handle stacking
        for id in &self.z_order {
            if let Some(overlay) = self.overlays.get_mut(id) {
                overlay.calculate_position(editor_width, editor_height, cursor_pos);
            }
        }
    }

    pub fn render_all(&mut self, buffer: &mut RenderBuffer) {
        for id in &self.z_order {
            if let Some(overlay) = self.overlays.get_mut(id) {
                if overlay.is_dirty() {
                    overlay.render(buffer);
                    overlay.mark_clean();
                }
            }
        }
    }

    pub fn has_dirty_overlays(&self) -> bool {
        self.overlays.values().any(|o| o.is_dirty())
    }

    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// True when any overlay currently has lines to draw. Overlays that exist
    /// but are empty (e.g. an idle progress indicator) don't affect rendering.
    pub fn has_visible_content(&self) -> bool {
        self.overlays.values().any(|o| o.has_content())
    }

    pub fn mark_all_dirty(&mut self) {
        for overlay in self.overlays.values_mut() {
            overlay.content.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        editor::{render_buffer::RenderBuffer, Point},
        plugin::{OverlayAlignment, OverlayConfig},
        theme::Style,
    };

    use super::PluginOverlay;

    #[test]
    fn avoid_cursor_overlay_marks_dirty_when_position_changes() {
        let mut overlay = PluginOverlay::new(
            "completion".to_string(),
            OverlayConfig {
                align: OverlayAlignment::AvoidCursor,
                ..OverlayConfig::default()
            },
        );
        overlay.update_content(vec![("item".to_string(), Style::default())]);

        overlay.calculate_position(80, 24, Some(Point::new(5, 2)));
        overlay.mark_clean();
        assert!(!overlay.is_dirty());

        overlay.calculate_position(80, 24, Some(Point::new(5, 20)));
        assert!(overlay.is_dirty());
    }

    #[test]
    fn avoid_cursor_overlay_stays_clean_when_position_is_unchanged() {
        let mut overlay = PluginOverlay::new(
            "completion".to_string(),
            OverlayConfig {
                align: OverlayAlignment::AvoidCursor,
                ..OverlayConfig::default()
            },
        );
        overlay.update_content(vec![("item".to_string(), Style::default())]);

        overlay.calculate_position(80, 24, Some(Point::new(5, 2)));
        overlay.mark_clean();
        overlay.calculate_position(80, 24, Some(Point::new(6, 3)));

        assert!(!overlay.is_dirty());
    }

    #[test]
    fn overlay_width_uses_display_columns() {
        let mut overlay = PluginOverlay::new("completion".to_string(), OverlayConfig::default());

        overlay.update_content(vec![("a👋".to_string(), Style::default())]);

        assert_eq!(overlay.width, 3);
    }

    #[test]
    fn overlay_render_right_aligns_by_display_width() {
        let mut overlay = PluginOverlay::new(
            "completion".to_string(),
            OverlayConfig {
                align: OverlayAlignment::Top,
                ..OverlayConfig::default()
            },
        );
        overlay.update_content(vec![
            ("long".to_string(), Style::default()),
            ("👋".to_string(), Style::default()),
        ]);
        overlay.calculate_position(8, 6, None);

        let mut buffer = RenderBuffer::new(8, 6, &Style::default());
        for y in 0..buffer.height {
            buffer.set_text(0, y, "........", &Style::default());
        }
        overlay.render(&mut buffer);

        let row = |y: usize| {
            buffer.cells[y * buffer.width..(y + 1) * buffer.width]
                .iter()
                .map(|cell| cell.c)
                .collect::<String>()
        };

        assert_eq!(row(0), "...long.");
        assert_eq!(row(1), ".....👋 .");
    }
}
