//! Plugin-defined semantic bars rendered above individual editor windows.
//!
//! [`WindowBarManager`] selects at most one bar for each stable window ID. Higher
//! priority wins and the most recently created bar breaks ties. Segment clipping is
//! display-width aware, preserves action hit regions for visible text, and resolves
//! semantic theme styles before applying concrete overrides.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    theme::{Style, Theme, ThemeStyleSpec},
    unicode_utils::display_width,
    window::WindowId,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowBarEdge {
    #[default]
    Top,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowBarOverflow {
    TruncateRight,
    #[default]
    TruncateLeft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WindowBarConfig {
    #[serde(default)]
    pub edge: WindowBarEdge,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub overflow: WindowBarOverflow,
    #[serde(default = "default_truncate_marker")]
    pub truncate_marker: String,
    #[serde(default)]
    pub style: WindowBarStyle,
}

impl Default for WindowBarConfig {
    fn default() -> Self {
        Self {
            edge: WindowBarEdge::Top,
            priority: 0,
            overflow: WindowBarOverflow::TruncateLeft,
            truncate_marker: default_truncate_marker(),
            style: WindowBarStyle::default(),
        }
    }
}

fn default_truncate_marker() -> String {
    "…".to_string()
}

/// A style can use concrete colors, a semantic theme key, or both.
///
/// When both are supplied, callers should resolve the semantic key first and
/// overlay the concrete style on the result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WindowBarStyle {
    #[serde(default)]
    pub semantic: Option<WindowBarSemanticStyle>,
    #[serde(default)]
    pub style: Option<Style>,
}

impl WindowBarStyle {
    pub fn resolve(&self, theme: &Theme) -> Style {
        let mut resolved = match &self.semantic {
            Some(WindowBarSemanticStyle::Key(key)) => theme.resolve_style(&ThemeStyleSpec {
                foreground: vec![key.clone()],
                ..ThemeStyleSpec::default()
            }),
            Some(WindowBarSemanticStyle::Spec(spec)) => theme.resolve_style(spec),
            None => Style::default(),
        };
        if let Some(concrete) = &self.style {
            if concrete.fg.is_some() {
                resolved.fg = concrete.fg;
            }
            if concrete.bg.is_some() {
                resolved.bg = concrete.bg;
            }
            resolved.bold = concrete.bold;
            resolved.italic = concrete.italic;
        }
        resolved
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WindowBarSemanticStyle {
    Key(String),
    Spec(ThemeStyleSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WindowBarSegment {
    #[serde(default)]
    pub id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub style: WindowBarStyle,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowBarHitRegion {
    pub start_column: usize,
    pub end_column: usize,
    pub segment_id: Option<String>,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedWindowBar {
    pub bar_id: String,
    pub style: WindowBarStyle,
    pub segments: Vec<WindowBarSegment>,
    pub hit_regions: Vec<WindowBarHitRegion>,
    pub width: usize,
}

#[derive(Debug, Clone)]
struct WindowBar {
    id: String,
    config: WindowBarConfig,
    sequence: u64,
    content: HashMap<WindowId, Vec<WindowBarSegment>>,
}

/// Owns plugin-defined chrome displayed above editor windows.
///
/// Only one bar occupies the top row of a window. Higher priority wins; ties
/// are resolved in favor of the most recently created bar.
#[derive(Debug, Default)]
pub struct WindowBarManager {
    bars: HashMap<String, WindowBar>,
    next_sequence: u64,
}

impl WindowBarManager {
    pub fn create_bar(&mut self, id: String, config: WindowBarConfig) -> bool {
        self.create(id, config)
    }

    pub fn create(&mut self, id: String, config: WindowBarConfig) -> bool {
        let sequence = self.next_sequence;
        self.next_sequence += 1;

        match self.bars.get_mut(&id) {
            Some(bar) => {
                bar.config = config;
                bar.sequence = sequence;
                true
            }
            None => {
                self.bars.insert(
                    id.clone(),
                    WindowBar {
                        id,
                        config,
                        sequence,
                        content: HashMap::new(),
                    },
                );
                true
            }
        }
    }

    pub fn update(
        &mut self,
        id: &str,
        window_id: WindowId,
        segments: Vec<WindowBarSegment>,
    ) -> bool {
        let Some(bar) = self.bars.get_mut(id) else {
            return false;
        };
        if bar.content.get(&window_id) == Some(&segments) {
            return false;
        }
        bar.content.insert(window_id, segments);
        true
    }

    pub fn update_bar(
        &mut self,
        id: &str,
        window_id: WindowId,
        segments: Vec<WindowBarSegment>,
    ) -> bool {
        self.update(id, window_id, segments)
    }

    pub fn clear_window(&mut self, id: &str, window_id: WindowId) -> bool {
        self.bars
            .get_mut(id)
            .is_some_and(|bar| bar.content.remove(&window_id).is_some())
    }

    pub fn close_window(&mut self, window_id: WindowId) -> bool {
        let mut changed = false;
        for bar in self.bars.values_mut() {
            changed |= bar.content.remove(&window_id).is_some();
        }
        changed
    }

    pub fn close(&mut self, id: &str) -> bool {
        self.bars.remove(id).is_some()
    }

    pub fn close_bar(&mut self, id: &str) -> bool {
        self.close(id)
    }

    pub fn has_bar_for_window(&self, window_id: WindowId) -> bool {
        self.selected_bar(window_id).is_some()
    }

    pub fn reserved_top_height(&self, window_id: WindowId) -> usize {
        usize::from(self.has_bar_for_window(window_id))
    }

    pub fn render(&self, window_id: WindowId, width: usize) -> Option<RenderedWindowBar> {
        let bar = self.selected_bar(window_id)?;
        let source = bar.content.get(&window_id)?;
        let segments = clip_segments(
            source,
            width,
            bar.config.overflow,
            &bar.config.truncate_marker,
        );
        let hit_regions = hit_regions(&segments);
        let rendered_width = segments
            .iter()
            .map(|segment| display_width(&segment.text))
            .sum();

        Some(RenderedWindowBar {
            bar_id: bar.id.clone(),
            style: bar.config.style.clone(),
            segments,
            hit_regions,
            width: rendered_width,
        })
    }

    pub fn render_resolved(
        &self,
        window_id: WindowId,
        width: usize,
        theme: &Theme,
    ) -> Option<RenderedWindowBar> {
        let mut rendered = self.render(window_id, width)?;
        rendered.style = WindowBarStyle {
            semantic: None,
            style: Some(rendered.style.resolve(theme)),
        };
        for segment in &mut rendered.segments {
            segment.style = WindowBarStyle {
                semantic: None,
                style: Some(segment.style.resolve(theme)),
            };
        }
        Some(rendered)
    }

    fn selected_bar(&self, window_id: WindowId) -> Option<&WindowBar> {
        self.bars
            .values()
            .filter(|bar| bar.content.contains_key(&window_id))
            .max_by_key(|bar| (bar.config.priority, bar.sequence))
    }
}

fn clip_segments(
    segments: &[WindowBarSegment],
    width: usize,
    overflow: WindowBarOverflow,
    marker: &str,
) -> Vec<WindowBarSegment> {
    let source_width: usize = segments
        .iter()
        .map(|segment| display_width(&segment.text))
        .sum();
    if source_width <= width {
        return segments.to_vec();
    }

    let marker = truncate_right(marker, width);
    let marker_width = display_width(&marker);
    let content_width = width.saturating_sub(marker_width);
    match overflow {
        WindowBarOverflow::TruncateRight => {
            let mut clipped = take_prefix(segments, content_width);
            push_marker(&mut clipped, marker);
            clipped
        }
        WindowBarOverflow::TruncateLeft => {
            let mut clipped = take_suffix(segments, content_width);
            insert_marker(&mut clipped, marker);
            clipped
        }
    }
}

fn take_prefix(segments: &[WindowBarSegment], width: usize) -> Vec<WindowBarSegment> {
    let mut remaining = width;
    let mut result = Vec::new();
    for segment in segments {
        if remaining == 0 {
            break;
        }
        let text = truncate_right(&segment.text, remaining);
        remaining = remaining.saturating_sub(display_width(&text));
        if !text.is_empty() {
            result.push(WindowBarSegment {
                text,
                ..segment.clone()
            });
        }
    }
    result
}

fn take_suffix(segments: &[WindowBarSegment], width: usize) -> Vec<WindowBarSegment> {
    let mut remaining = width;
    let mut result = Vec::new();
    for segment in segments.iter().rev() {
        if remaining == 0 {
            break;
        }
        let text = truncate_left(&segment.text, remaining);
        remaining = remaining.saturating_sub(display_width(&text));
        if !text.is_empty() {
            result.push(WindowBarSegment {
                text,
                ..segment.clone()
            });
        }
    }
    result.reverse();
    result
}

fn truncate_right(text: &str, width: usize) -> String {
    let mut used = 0;
    text.graphemes(true)
        .take_while(|grapheme| {
            let next = used + display_width(grapheme);
            if next <= width {
                used = next;
                true
            } else {
                false
            }
        })
        .collect()
}

fn truncate_left(text: &str, width: usize) -> String {
    let mut used = 0;
    let mut graphemes = text
        .graphemes(true)
        .rev()
        .take_while(|grapheme| {
            let next = used + display_width(grapheme);
            if next <= width {
                used = next;
                true
            } else {
                false
            }
        })
        .collect::<Vec<_>>();
    graphemes.reverse();
    graphemes.concat()
}

fn push_marker(segments: &mut Vec<WindowBarSegment>, text: String) {
    if text.is_empty() {
        return;
    }
    let style = segments
        .last()
        .map(|segment| segment.style.clone())
        .unwrap_or_default();
    segments.push(WindowBarSegment {
        id: None,
        text,
        style,
        tooltip: None,
        action: None,
    });
}

fn insert_marker(segments: &mut Vec<WindowBarSegment>, text: String) {
    if text.is_empty() {
        return;
    }
    let style = segments
        .first()
        .map(|segment| segment.style.clone())
        .unwrap_or_default();
    segments.insert(
        0,
        WindowBarSegment {
            id: None,
            text,
            style,
            tooltip: None,
            action: None,
        },
    );
}

fn hit_regions(segments: &[WindowBarSegment]) -> Vec<WindowBarHitRegion> {
    let mut column = 0;
    let mut regions = Vec::new();
    for segment in segments {
        let end_column = column + display_width(&segment.text);
        if let Some(action) = &segment.action {
            regions.push(WindowBarHitRegion {
                start_column: column,
                end_column,
                segment_id: segment.id.clone(),
                action: action.clone(),
            });
        }
        column = end_column;
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(id: &str, text: &str) -> WindowBarSegment {
        WindowBarSegment {
            id: Some(id.to_string()),
            text: text.to_string(),
            style: WindowBarStyle::default(),
            tooltip: None,
            action: Some(format!("open:{id}")),
        }
    }

    #[test]
    fn rejects_camel_case_window_bar_fields() {
        let result = serde_json::from_value::<WindowBarConfig>(serde_json::json!({
            "truncateMarker": "..."
        }));

        assert!(result.is_err());
    }

    #[test]
    fn higher_priority_bar_owns_the_window_row() {
        let window_id = WindowId(4);
        let mut manager = WindowBarManager::default();
        manager.create("low".to_string(), WindowBarConfig::default());
        manager.create(
            "high".to_string(),
            WindowBarConfig {
                priority: 10,
                ..WindowBarConfig::default()
            },
        );
        manager.update("low", window_id, vec![segment("low", "low")]);
        manager.update("high", window_id, vec![segment("high", "high")]);

        assert_eq!(manager.render(window_id, 20).unwrap().bar_id, "high");
    }

    #[test]
    fn left_truncation_preserves_unicode_graphemes_and_current_symbol() {
        let window_id = WindowId(1);
        let mut manager = WindowBarManager::default();
        manager.create("crumbs".to_string(), WindowBarConfig::default());
        manager.update(
            "crumbs",
            window_id,
            vec![segment("path", "src/👨‍👩‍👧‍👦/"), segment("symbol", "function")],
        );

        let rendered = manager.render(window_id, 10).unwrap();
        let text = rendered
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert_eq!(text, "…/function");
        assert_eq!(display_width(&text), 10);
    }

    #[test]
    fn hit_regions_follow_clipped_segment_columns() {
        let window_id = WindowId(2);
        let mut manager = WindowBarManager::default();
        manager.create("crumbs".to_string(), WindowBarConfig::default());
        manager.update(
            "crumbs",
            window_id,
            vec![segment("file", "main.rs"), segment("symbol", " › run")],
        );

        let rendered = manager.render(window_id, 20).unwrap();
        assert_eq!(rendered.hit_regions.len(), 2);
        assert_eq!(rendered.hit_regions[0].start_column, 0);
        assert_eq!(rendered.hit_regions[0].end_column, 7);
        assert_eq!(rendered.hit_regions[1].start_column, 7);
        assert_eq!(rendered.hit_regions[1].end_column, 13);
    }

    #[test]
    fn closing_a_window_removes_only_its_content() {
        let mut manager = WindowBarManager::default();
        manager.create("crumbs".to_string(), WindowBarConfig::default());
        manager.update("crumbs", WindowId(1), vec![segment("one", "one")]);
        manager.update("crumbs", WindowId(2), vec![segment("two", "two")]);

        assert!(manager.close_window(WindowId(1)));
        assert!(manager.render(WindowId(1), 10).is_none());
        assert!(manager.render(WindowId(2), 10).is_some());
    }
}
