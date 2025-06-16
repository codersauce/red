use std::collections::HashMap;

use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::Style,
};

#[derive(Debug, Clone)]
pub enum OverlayAlignment {
    Top,
    Bottom,
    AvoidCursor,
}

#[derive(Debug, Clone)]
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

    pub fn update_content(&mut self, lines: Vec<(String, Style)>) {
        self.content.lines = lines;
        self.content.dirty = true;

        // Update dimensions
        self.height = self.content.lines.len();
        self.width = self
            .content
            .lines
            .iter()
            .map(|(text, _)| text.chars().count()) // Use chars().count() for proper Unicode handling
            .max()
            .unwrap_or(0);
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
        self.position = Some(position);
        position
    }

    pub fn render(&self, buffer: &mut RenderBuffer) {
        if let Some(pos) = self.position {
            for (i, (text, style)) in self.content.lines.iter().enumerate() {
                let y = pos.y + i;
                if y < buffer.height - 2 {
                    // Don't render over status line
                    // For right-aligned text, we need to:
                    // 1. Clear the area where the text will be
                    // 2. Render the text right-aligned

                    // Calculate the actual text position for right alignment
                    let text_len = text.chars().count();
                    let text_x = pos.x + self.width.saturating_sub(text_len);

                    // Clear only the area we'll use for the text
                    let clear_width = text_len.min(self.width);
                    let clear_text = " ".repeat(clear_width);
                    buffer.set_text(text_x, y, &clear_text, style);

                    // Then render the actual text
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

    pub fn mark_all_dirty(&mut self) {
        for overlay in self.overlays.values_mut() {
            overlay.content.dirty = true;
        }
    }
}
