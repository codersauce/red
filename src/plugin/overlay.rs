//! Plugin-owned floating overlays positioned within the terminal viewport.
//!
//! [`OverlayManager`] stores complete overlay models by stable plugin-provided ID and
//! resolves alignment against current terminal bounds during rendering. Creating an
//! existing ID replaces its configuration; removal is idempotent from the caller's
//! perspective.

use std::{collections::HashMap, time::Instant};

use serde::Deserialize;

use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::Style,
    ui::{spinner_frame, SPINNER_FRAME_INTERVAL_MS},
    unicode_utils::{display_width, truncate_display_width_with_marker, TruncationSide},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OverlayAlignment {
    Top,
    Bottom,
    AvoidCursor,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OverlayOverflow {
    TruncateRight,
    #[default]
    TruncateLeft,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OverlayConfig {
    pub align: OverlayAlignment,
    pub x_padding: usize,
    pub y_padding: usize,
    pub relative: String, // "editor" or "window"
    #[serde(default)]
    pub max_width: usize,
    #[serde(default)]
    pub overflow: OverlayOverflow,
    #[serde(default = "default_truncate_marker")]
    pub truncate_marker: String,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            align: OverlayAlignment::Bottom,
            x_padding: 1,
            y_padding: 0,
            relative: "editor".to_string(),
            max_width: 0,
            overflow: OverlayOverflow::TruncateLeft,
            truncate_marker: default_truncate_marker(),
        }
    }
}

fn default_truncate_marker() -> String {
    "…".to_string()
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
    busy_since: Option<Instant>,
    busy_frame: u64,
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
            busy_since: None,
            busy_frame: 0,
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

        self.update_dimensions();
        true
    }

    fn update_dimensions(&mut self) {
        self.height = self.content.lines.len();
        let content_width = self
            .content
            .lines
            .iter()
            .map(|(text, _)| display_width(text))
            .max()
            .unwrap_or(0);
        let busy_width = usize::from(self.busy_since.is_some() && self.has_content()) * 2;
        let natural_width = content_width.saturating_add(busy_width);
        self.width = match self.config.max_width {
            0 => natural_width,
            max_width => natural_width.min(max_width),
        };
    }

    /// Enables or disables host-driven spinner animation for this overlay.
    pub fn set_busy(&mut self, busy: bool) -> bool {
        if busy == self.busy_since.is_some() {
            return false;
        }
        self.busy_since = busy.then(Instant::now);
        self.busy_frame = 0;
        self.update_dimensions();
        self.content.dirty = true;
        true
    }

    /// Advances the spinner when its visible frame changes.
    pub fn poll_animation(&mut self) -> bool {
        let Some(started) = self.busy_since else {
            return false;
        };
        let frame = started.elapsed().as_millis() as u64 / SPINNER_FRAME_INTERVAL_MS;
        if frame == self.busy_frame {
            return false;
        }
        self.busy_frame = frame;
        self.content.dirty = true;
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
        let previous_width = self.width;
        self.update_dimensions();
        self.width = self
            .width
            .min(editor_width.saturating_sub(self.config.x_padding));
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
        if self.position != Some(position) || self.width != previous_width {
            self.content.dirty = true;
        }
        self.position = Some(position);
        position
    }

    pub fn render(&self, buffer: &mut RenderBuffer) {
        if let Some(pos) = self.position {
            let content_height = buffer.height.saturating_sub(2);
            for (i, (text, style)) in self.content.lines.iter().enumerate() {
                let Some(y) = pos.y.checked_add(i) else {
                    break;
                };
                if y >= content_height {
                    break;
                }

                let spinner = if i == 0 {
                    self.busy_since.map(|started| {
                        format!("{} ", spinner_frame(started.elapsed().as_millis() as u64))
                    })
                } else {
                    None
                };
                let spinner_width = spinner.as_deref().map(display_width).unwrap_or(0);
                let text_width = self.width.saturating_sub(spinner_width);
                let text = truncate_display_width_with_marker(
                    text,
                    text_width,
                    &self.config.truncate_marker,
                    match self.config.overflow {
                        OverlayOverflow::TruncateRight => TruncationSide::Right,
                        OverlayOverflow::TruncateLeft => TruncationSide::Left,
                    },
                );
                let rendered = spinner.unwrap_or_default() + &text;
                let text_width = display_width(&rendered);
                let text_x = pos.x.saturating_add(self.width.saturating_sub(text_width));
                buffer.set_text(text_x, y, &rendered, style);
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
                overlay.render(buffer);
                overlay.mark_clean();
            }
        }
    }

    pub fn has_dirty_overlays(&self) -> bool {
        self.overlays.values().any(|o| o.is_dirty())
    }

    pub fn poll_animation(&mut self) -> bool {
        let mut changed = false;
        for overlay in self.overlays.values_mut() {
            changed |= overlay.poll_animation();
        }
        changed
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
    use std::time::{Duration, Instant};

    use crate::{
        editor::{render_buffer::RenderBuffer, Point},
        plugin::{OverlayAlignment, OverlayConfig},
        theme::Style,
    };

    use super::{OverlayManager, OverlayOverflow, PluginOverlay, SPINNER_FRAME_INTERVAL_MS};

    fn render_row(buffer: &RenderBuffer, y: usize) -> String {
        buffer.cells[y * buffer.width..(y + 1) * buffer.width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

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

    #[test]
    fn overlay_max_width_left_truncates_by_display_width() {
        let mut overlay = PluginOverlay::new(
            "progress".to_string(),
            OverlayConfig {
                align: OverlayAlignment::Top,
                x_padding: 0,
                max_width: 6,
                overflow: OverlayOverflow::TruncateLeft,
                ..OverlayConfig::default()
            },
        );
        overlay.update_content(vec![("prefix/👋end".to_string(), Style::default())]);
        overlay.calculate_position(10, 6, None);

        let mut buffer = RenderBuffer::new(10, 6, &Style::default());
        overlay.render(&mut buffer);

        assert_eq!(overlay.width, 6);
        assert_eq!(render_row(&buffer, 0), "    …👋 end");
    }

    #[test]
    fn overlay_width_recovers_after_a_narrow_terminal_resize() {
        let mut overlay = PluginOverlay::new(
            "progress".to_string(),
            OverlayConfig {
                align: OverlayAlignment::Top,
                max_width: 60,
                ..OverlayConfig::default()
            },
        );
        overlay.update_content(vec![(
            "a reasonably long progress message".into(),
            Style::default(),
        )]);

        overlay.calculate_position(12, 6, None);
        assert_eq!(overlay.width, 11);

        overlay.calculate_position(80, 6, None);
        assert_eq!(overlay.width, 34);
    }

    #[test]
    fn clean_overlay_is_composited_into_every_new_frame() {
        let mut overlays = OverlayManager::new();
        let overlay = overlays.create_overlay(
            "persistent".to_string(),
            OverlayConfig {
                align: OverlayAlignment::Top,
                x_padding: 0,
                ..OverlayConfig::default()
            },
        );
        overlay.update_content(vec![("visible".to_string(), Style::default())]);
        overlays.update_positions(12, 6, None);

        let mut first = RenderBuffer::new(12, 6, &Style::default());
        overlays.render_all(&mut first);
        assert!(render_row(&first, 0).contains("visible"));
        assert!(!overlays.has_dirty_overlays());

        let mut next = RenderBuffer::new(12, 6, &Style::default());
        overlays.render_all(&mut next);
        assert!(render_row(&next, 0).contains("visible"));
        assert!(!overlays.has_dirty_overlays());
    }

    #[test]
    fn overlay_rendering_saturates_tiny_terminal_heights() {
        for height in 0..=2 {
            let mut overlays = OverlayManager::new();
            let overlay = overlays.create_overlay(
                "tiny".to_string(),
                OverlayConfig {
                    align: OverlayAlignment::Top,
                    ..OverlayConfig::default()
                },
            );
            overlay.update_content(vec![("hidden".to_string(), Style::default())]);
            overlay.set_busy(true);
            overlays.update_positions(8, height, None);

            let mut buffer = RenderBuffer::new(8, height, &Style::default());
            overlays.render_all(&mut buffer);

            assert_eq!(buffer.cells.len(), 8 * height);
            assert!(buffer.cells.iter().all(|cell| cell.c == ' '));
        }
    }

    #[test]
    fn overlays_preserve_z_order_across_recomposed_frames() {
        let mut overlays = OverlayManager::new();
        for (id, text) in [("lower", "first"), ("upper", "above")] {
            let overlay = overlays.create_overlay(
                id.to_string(),
                OverlayConfig {
                    align: OverlayAlignment::Top,
                    x_padding: 0,
                    ..OverlayConfig::default()
                },
            );
            overlay.update_content(vec![(text.to_string(), Style::default())]);
        }
        overlays.update_positions(10, 6, None);

        for _ in 0..2 {
            let mut buffer = RenderBuffer::new(10, 6, &Style::default());
            overlays.render_all(&mut buffer);
            assert!(render_row(&buffer, 0).ends_with("above"));
        }

        overlays.remove_overlay("upper");
        let mut buffer = RenderBuffer::new(10, 6, &Style::default());
        overlays.render_all(&mut buffer);
        assert!(render_row(&buffer, 0).ends_with("first"));
    }

    #[test]
    fn busy_overlay_reserves_spinner_width_and_advances_frames() {
        let mut overlay = PluginOverlay::new(
            "push".to_string(),
            OverlayConfig {
                align: OverlayAlignment::Top,
                ..OverlayConfig::default()
            },
        );
        overlay.update_content(vec![("Pushing".to_string(), Style::default())]);
        assert!(overlay.set_busy(true));
        assert_eq!(overlay.width, 9);

        overlay.busy_since =
            Some(Instant::now() - Duration::from_millis(SPINNER_FRAME_INTERVAL_MS * 2));
        assert!(overlay.poll_animation());
        overlay.calculate_position(20, 5, None);
        let mut buffer = RenderBuffer::new(20, 5, &Style::default());
        overlay.render(&mut buffer);

        assert!(render_row(&buffer, 0).contains("⠹ Pushing"));
        assert!(overlay.set_busy(false));
        assert_eq!(overlay.width, 7);
    }
}
