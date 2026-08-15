//! Compact completion-menu state, filtering, selection, and terminal rendering.
//!
//! [`CompletionUI`] owns a snapshot of LSP completion items and the selection derived
//! from the current query. It produces actions for the editor to apply; accepting an
//! item does not itself mutate a buffer.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

use crate::{
    config::KeyAction,
    editor::{Action, Mode, RenderBuffer},
    lsp::types::{CompletionItemKind, CompletionResponseItem},
    theme::{SelectionForegroundPriority, Style, Theme, UiStyle},
    unicode_utils::{
        display_width, fit_display_width, truncate_display_width_with_marker, TruncationSide,
    },
};

use super::{dialog::BorderStyle, Component, IconCatalog, SelectionViewport};

const MIN_INNER_WIDTH: usize = 15;
const MAX_INNER_WIDTH: usize = 78;
const PAGE_SIZE: usize = 10;
const LEFT_PADDING: usize = 1;
const RIGHT_PADDING: usize = 1;
const ICON_COLUMN_WIDTH: usize = 2;

#[derive(Clone)]
struct CompletionSelectionIdentity {
    label: String,
    sort_text: Option<String>,
    insert_text: Option<String>,
}

impl CompletionSelectionIdentity {
    fn from_item(item: &CompletionResponseItem) -> Self {
        Self {
            label: item.label.clone(),
            sort_text: item.sort_text.clone(),
            insert_text: item.insert_text.clone(),
        }
    }

    fn matches(&self, item: &CompletionResponseItem) -> bool {
        self.label == item.label
            && self.sort_text == item.sort_text
            && self.insert_text == item.insert_text
    }
}

#[derive(Default, Clone)]
pub struct CompletionUI {
    all_items: Vec<CompletionResponseItem>,
    items: Vec<usize>,
    matched_label_indices: Vec<Vec<usize>>,
    filter: String,
    viewport: SelectionViewport,
    visible: bool,
    anchor_x: usize,
    anchor_y: usize,
    bounds_width: usize,
    bounds_height: usize,
    x: usize,
    y: usize,
    width: usize,
    visible_rows: usize,
    commit_chars: Vec<char>,
    styles: UiStyle,
}

impl CompletionUI {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_theme(theme: &Theme) -> Self {
        let mut styles = theme.ui_style.clone();
        styles.picker_selected_item = theme.selected_style(
            &styles.picker_item,
            &styles.picker_selected_item,
            SelectionForegroundPriority::Selection,
        );
        Self {
            styles,
            ..Default::default()
        }
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        self.styles = Self::with_theme(theme).styles;
    }

    pub fn show(&mut self, items: Vec<CompletionResponseItem>, x: usize, y: usize) {
        self.show_with_bounds(items, x, y, usize::MAX, usize::MAX);
    }

    pub fn show_with_bounds(
        &mut self,
        items: Vec<CompletionResponseItem>,
        x: usize,
        y: usize,
        bounds_width: usize,
        bounds_height: usize,
    ) {
        self.anchor_x = x;
        self.anchor_y = y;
        self.bounds_width = bounds_width;
        self.bounds_height = bounds_height;
        self.visible = true;
        self.filter.clear();
        self.replace_items(items, None);
    }

    pub fn update_items(&mut self, items: Vec<CompletionResponseItem>, filter: &str) {
        let selected = self
            .selected_item()
            .map(CompletionSelectionIdentity::from_item);
        self.filter.clear();
        self.filter.push_str(filter);
        self.replace_items(items, selected.as_ref());
    }

    fn replace_items(
        &mut self,
        mut items: Vec<CompletionResponseItem>,
        selected: Option<&CompletionSelectionIdentity>,
    ) {
        self.commit_chars = items
            .iter()
            .filter_map(|item| item.commit_characters.as_ref())
            .flat_map(|chars| chars.iter())
            .filter_map(|s| s.chars().next())
            .collect();
        self.commit_chars.sort_unstable();
        self.commit_chars.dedup();
        items.sort_by(|a, b| {
            b.preselect
                .unwrap_or(false)
                .cmp(&a.preselect.unwrap_or(false))
                .then_with(|| {
                    a.sort_text
                        .as_deref()
                        .unwrap_or(&a.label)
                        .cmp(b.sort_text.as_deref().unwrap_or(&b.label))
                })
                .then(a.label.cmp(&b.label))
        });
        self.all_items = items;
        self.refilter_items(selected);
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.all_items.clear();
        self.items.clear();
        self.matched_label_indices.clear();
        self.filter.clear();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn selected_item(&self) -> Option<&CompletionResponseItem> {
        self.items
            .get(self.viewport.selected())
            .and_then(|index| self.all_items.get(*index))
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter.clear();
        self.filter.push_str(filter);
        self.refilter_items(None);
    }

    fn calculate_inner_width(&self) -> usize {
        let max_item_width = self
            .items
            .iter()
            .filter_map(|index| self.all_items.get(*index))
            .map(|item| {
                let label_width = display_width(Self::item_display_name(item))
                    + item
                        .label_details
                        .as_ref()
                        .and_then(|details| details.detail.as_deref())
                        .map_or(0, display_width);
                let description_width = item
                    .label_details
                    .as_ref()
                    .and_then(|details| details.description.as_deref())
                    .map_or(0, |description| display_width(description) + 1);
                LEFT_PADDING + ICON_COLUMN_WIDTH + label_width + description_width + RIGHT_PADDING
            })
            .max()
            .unwrap_or(MIN_INNER_WIDTH);

        max_item_width.clamp(MIN_INNER_WIDTH, MAX_INNER_WIDTH)
    }

    fn item_filter_match(
        matcher: &SkimMatcherV2,
        item: &CompletionResponseItem,
        filter: &str,
    ) -> Option<(u8, i64, Vec<usize>)> {
        if filter.is_empty() {
            return Some((0, 0, Vec::new()));
        }

        let display_name = Self::item_display_name(item);
        let candidate = item.filter_text.as_deref().unwrap_or(display_name);
        let (score, _) = matcher.fuzzy_indices(candidate, filter)?;
        let normalized_candidate = candidate.to_lowercase();
        let normalized_filter = filter.to_lowercase();
        let match_class = if normalized_candidate == normalized_filter {
            3
        } else if normalized_candidate.starts_with(&normalized_filter) {
            2
        } else {
            1
        };
        let label_indices = matcher
            .fuzzy_indices(display_name, filter)
            .map(|(_, indices)| indices)
            .unwrap_or_default();
        Some((match_class, score, label_indices))
    }

    fn refilter_items(&mut self, selected: Option<&CompletionSelectionIdentity>) {
        let matcher = SkimMatcherV2::default().ignore_case();
        if self.filter.is_empty() {
            self.items.clear();
            self.items.extend(0..self.all_items.len());
            self.matched_label_indices = vec![Vec::new(); self.items.len()];
        } else {
            let mut matches = self
                .all_items
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| {
                    Self::item_filter_match(&matcher, item, &self.filter)
                        .map(|(class, score, indices)| (class, score, idx, indices))
                })
                .collect::<Vec<_>>();
            matches.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| b.1.cmp(&a.1))
                    .then_with(|| a.2.cmp(&b.2))
            });
            self.items.clear();
            self.matched_label_indices.clear();
            for (_, _, index, indices) in matches {
                self.items.push(index);
                self.matched_label_indices.push(indices);
            }
        }

        self.recalculate_layout();
        self.viewport = SelectionViewport::new(self.items.len(), self.visible_rows);
        let exact_selection = selected.and_then(|selected| {
            self.items.iter().position(|index| {
                self.all_items
                    .get(*index)
                    .is_some_and(|item| selected.matches(item))
            })
        });
        let matching_label = selected.and_then(|selected| {
            self.items.iter().position(|index| {
                self.all_items
                    .get(*index)
                    .is_some_and(|item| item.label == selected.label)
            })
        });
        if let Some(selection) = exact_selection.or(matching_label) {
            self.viewport.select(selection);
        } else if let Some(preselected) = self.items.iter().position(|index| {
            self.all_items
                .get(*index)
                .and_then(|item| item.preselect)
                .unwrap_or(false)
        }) {
            self.viewport.select(preselected);
        }
    }

    fn recalculate_layout(&mut self) {
        let available_width = if self.bounds_width == usize::MAX {
            MAX_INNER_WIDTH.saturating_add(2)
        } else {
            self.bounds_width
        };
        self.width = self
            .calculate_inner_width()
            .saturating_add(2)
            .min(available_width)
            .max(2);

        let label_offset = 1 + LEFT_PADDING + ICON_COLUMN_WIDTH;
        let desired_x = self
            .anchor_x
            .saturating_sub(display_width(&self.filter))
            .saturating_sub(label_offset);
        self.x = if available_width > self.width {
            desired_x.min(available_width - self.width)
        } else {
            0
        };

        if self.bounds_height == usize::MAX {
            self.visible_rows = self.items.len().min(PAGE_SIZE);
            self.y = self.anchor_y.saturating_add(1);
            return;
        }

        let desired_height = self.items.len().min(PAGE_SIZE).saturating_add(2);
        let rows_below = self
            .bounds_height
            .saturating_sub(self.anchor_y.saturating_add(1));
        let rows_above = self.anchor_y;
        let available_height = if rows_below < desired_height && rows_above > rows_below {
            let height = desired_height.min(rows_above);
            self.y = self.anchor_y.saturating_sub(height);
            height
        } else {
            self.y = self.anchor_y.saturating_add(1);
            desired_height.min(rows_below)
        };
        self.visible_rows = self
            .items
            .len()
            .min(PAGE_SIZE)
            .min(available_height.saturating_sub(2));
    }

    fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.anchor_x = self.anchor_x.saturating_add(display_width(&c.to_string()));
        self.refilter_items(None);
    }

    fn pop_filter_char(&mut self) {
        if let Some(c) = self.filter.pop() {
            self.anchor_x = self.anchor_x.saturating_sub(display_width(&c.to_string()));
        }
        self.refilter_items(None);
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() || self.visible_rows == 0 {
            return;
        }

        self.viewport.move_by(delta);
    }

    fn move_page(&mut self, up: bool) {
        let delta = if up {
            -(PAGE_SIZE as isize)
        } else {
            PAGE_SIZE as isize
        };
        self.move_selection(delta);
    }

    fn kind_to_icon(kind: &CompletionItemKind) -> &'static str {
        IconCatalog::completion(kind).glyph
    }

    fn ellipsize(content: &str, width: usize) -> String {
        truncate_display_width_with_marker(content, width, "…", TruncationSide::Right)
    }

    fn item_description(item: &CompletionResponseItem) -> Option<&str> {
        item.label_details
            .as_ref()
            .and_then(|details| details.description.as_deref())
    }

    fn item_display_name(item: &CompletionResponseItem) -> &str {
        item.label
            .trim_start_matches(|character: char| character.is_whitespace() || character == '•')
    }

    fn item_label(item: &CompletionResponseItem) -> String {
        let mut label = Self::item_display_name(item).to_string();
        if let Some(detail) = item
            .label_details
            .as_ref()
            .and_then(|details| details.detail.as_deref())
        {
            label.push_str(detail);
        }
        label
    }

    fn row_style(&self, selected: bool, deprecated: bool) -> Style {
        if selected {
            self.styles.picker_selected_item.clone()
        } else if deprecated {
            self.styles.deprecated.clone()
        } else {
            self.styles.picker_item.clone()
        }
    }

    fn label_segments(
        &self,
        x: usize,
        y: usize,
        label: &str,
        matched_indices: &[usize],
        base_style: &Style,
    ) -> Vec<(usize, usize, String, Style)> {
        let mut output = Vec::new();
        let mut cell_x = x;
        let mut chunk = String::new();
        let mut chunk_matched = None;

        let flush = |output: &mut Vec<(usize, usize, String, Style)>,
                     cell_x: &mut usize,
                     chunk: &mut String,
                     matched: bool| {
            if chunk.is_empty() {
                return;
            }
            let mut style = base_style.clone();
            if matched {
                style.bold = true;
            }
            let width = display_width(chunk);
            output.push((*cell_x, y, std::mem::take(chunk), style));
            *cell_x += width;
        };

        for (index, character) in label.chars().enumerate() {
            let matched = matched_indices.binary_search(&index).is_ok();
            if chunk_matched.is_some_and(|current| current != matched) {
                flush(
                    &mut output,
                    &mut cell_x,
                    &mut chunk,
                    chunk_matched.unwrap_or(false),
                );
            }
            chunk_matched = Some(matched);
            chunk.push(character);
        }
        flush(
            &mut output,
            &mut cell_x,
            &mut chunk,
            chunk_matched.unwrap_or(false),
        );
        output
    }

    fn render_completion(&self) -> Vec<(usize, usize, String, Style)> {
        if !self.visible || self.items.is_empty() || self.width < 2 || self.visible_rows == 0 {
            return Vec::new();
        }

        let mut output = Vec::new();
        let [horizontal, _, top_left, top_right, bottom_left, bottom_right] = BorderStyle::Rounded
            .glyphs()
            .expect("rounded completion borders have frame glyphs");
        let horizontal = horizontal.to_string().repeat(self.width.saturating_sub(2));
        output.push((
            self.x,
            self.y,
            format!("{top_left}{horizontal}{top_right}"),
            self.styles.popup_border.clone(),
        ));

        let visible_count = self.visible_rows;
        let max_scroll_offset = self.items.len().saturating_sub(visible_count);
        let offset = if self.viewport.selected() < self.viewport.top() {
            self.viewport.selected()
        } else if self.viewport.selected() >= self.viewport.top().saturating_add(visible_count) {
            self.viewport
                .selected()
                .saturating_sub(visible_count.saturating_sub(1))
        } else {
            self.viewport.top()
        };
        let scroll_offset = offset.min(max_scroll_offset);

        for (visible_index, item_position) in (scroll_offset..self.items.len())
            .take(visible_count)
            .enumerate()
        {
            let Some(item) = self
                .items
                .get(item_position)
                .and_then(|index| self.all_items.get(*index))
            else {
                continue;
            };
            let y = self.y + visible_index + 1;
            let selected = item_position == self.viewport.selected();
            let base_style = self.row_style(selected, item.deprecated.unwrap_or(false));
            let inner_width = self.width.saturating_sub(2);
            output.push((self.x, y, "│".to_string(), self.styles.popup_border.clone()));
            output.push((self.x + 1, y, " ".repeat(inner_width), base_style.clone()));
            output.push((
                self.x + self.width.saturating_sub(1),
                y,
                "│".to_string(),
                self.styles.popup_border.clone(),
            ));

            let icon = item.kind.as_ref().map(Self::kind_to_icon).unwrap_or(" ");
            output.push((
                self.x + 1,
                y,
                fit_display_width(&format!(" {icon} "), LEFT_PADDING + ICON_COLUMN_WIDTH),
                base_style.clone(),
            ));

            let description = Self::ellipsize(
                Self::item_description(item).unwrap_or(""),
                inner_width.saturating_sub(LEFT_PADDING + ICON_COLUMN_WIDTH + RIGHT_PADDING + 2),
            );
            let description_width = display_width(&description);
            let description_space = (!description.is_empty()).then_some(description_width + 1);
            let label_width = inner_width
                .saturating_sub(LEFT_PADDING + ICON_COLUMN_WIDTH + RIGHT_PADDING)
                .saturating_sub(description_space.unwrap_or(0));
            let label = Self::ellipsize(&Self::item_label(item), label_width);
            let matched_indices = self
                .matched_label_indices
                .get(item_position)
                .map(Vec::as_slice)
                .unwrap_or_default();
            output.extend(self.label_segments(
                self.x + 1 + LEFT_PADDING + ICON_COLUMN_WIDTH,
                y,
                &label,
                matched_indices,
                &base_style,
            ));

            if let Some(description_space) = description_space {
                let mut description_style = self.styles.muted.clone();
                description_style.bg = base_style.bg;
                output.push((
                    self.x + 1 + inner_width.saturating_sub(description_space),
                    y,
                    format!(" {description}"),
                    description_style,
                ));
            }
        }

        let bottom_y = self.y + visible_count + 1;
        output.push((
            self.x,
            bottom_y,
            format!("{bottom_left}{horizontal}{bottom_right}"),
            self.styles.popup_border.clone(),
        ));

        if scroll_offset > 0 {
            output.push((
                self.x + 1,
                self.y,
                "↑".to_string(),
                self.styles.muted.clone(),
            ));
        }
        if scroll_offset + visible_count < self.items.len() {
            output.push((
                self.x + 1,
                bottom_y,
                "↓".to_string(),
                self.styles.muted.clone(),
            ));
        }

        output
    }
}

impl Component for CompletionUI {
    fn set_theme(&mut self, theme: &Theme) {
        CompletionUI::set_theme(self, theme);
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        for (x, y, text, style) in self.render_completion() {
            buffer.set_text(x, y, &text, &style);
        }
        Ok(())
    }

    fn update_completion(&mut self, items: Vec<CompletionResponseItem>, filter: &str) -> bool {
        self.update_items(items, filter);
        true
    }

    fn handle_event(&mut self, ev: &Event) -> Option<KeyAction> {
        match ev {
            Event::Key(KeyEvent {
                code: KeyCode::Up, ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                self.move_selection(-1);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                self.move_selection(1);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::PageUp,
                ..
            }) => {
                self.move_page(true);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::PageDown,
                ..
            }) => {
                self.move_page(false);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            }) => self.selected_item().map(|item| {
                KeyAction::Multiple(vec![
                    Action::ApplyCompletion {
                        item: Box::new(item.clone()),
                        commit_character: None,
                    },
                    Action::CloseDialog,
                ])
            }),
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            }) => Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::InsertNewLine,
            ])),
            Event::Key(KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => Some(KeyAction::Single(Action::CloseDialog)),
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::EnterMode(Mode::Normal),
            ])),
            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) if self.commit_chars.contains(c) => {
                if let Some(item) = self.selected_item() {
                    Some(KeyAction::Multiple(vec![
                        Action::ApplyCompletion {
                            item: Box::new(item.clone()),
                            commit_character: Some(*c),
                        },
                        Action::CloseDialog,
                    ]))
                } else {
                    Some(KeyAction::Single(Action::CloseDialog))
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            }) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.push_filter_char(*c);
                None
            }
            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                modifiers,
                ..
            }) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.pop_filter_char();
                None
            }
            _ => None,
        }
    }

    fn allows_event_passthrough(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{color::Color, unicode_utils::display_width};

    fn assert_segments_within_popup(ui: &CompletionUI, rows: &[(usize, usize, String, Style)]) {
        for (x, _, row, _) in rows {
            assert!(*x >= ui.x);
            assert!(x + display_width(row) <= ui.x + ui.width);
            assert!(row.is_char_boundary(row.len()));
        }
    }

    fn item(label: &str, kind: Option<CompletionItemKind>) -> CompletionResponseItem {
        CompletionResponseItem {
            label: label.to_string(),
            label_details: None,
            kind,
            detail: None,
            documentation: None,
            deprecated: None,
            preselect: None,
            sort_text: None,
            filter_text: None,
            insert_text: None,
            insert_text_format: None,
            text_edit: None,
            additional_text_edits: None,
            command: None,
            data: None,
            commit_characters: None,
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn completion_respects_server_sort_text() {
        let mut later_label = item("alpha", Some(CompletionItemKind::Variable));
        later_label.sort_text = Some("20".to_string());
        let mut earlier_label = item("zeta", Some(CompletionItemKind::Variable));
        earlier_label.sort_text = Some("10".to_string());

        let mut ui = CompletionUI::new();
        ui.show(vec![later_label, earlier_label], 0, 0);

        assert_eq!(ui.selected_item().unwrap().label, "zeta");
    }

    #[test]
    fn completion_rows_fit_display_width_with_wide_labels() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![item(
                "function_with_emoji_👋_and_cjk_世界_that_must_truncate",
                Some(CompletionItemKind::Function),
            )],
            0,
            0,
        );

        let rows = ui.render_completion();

        assert_segments_within_popup(&ui, &rows);
    }

    #[test]
    fn completion_renders_label_description_without_automatic_documentation() {
        let mut completion = item("hello", Some(CompletionItemKind::Text));
        completion.label_details = Some(crate::lsp::types::CompletionItemLabelDetails {
            detail: None,
            description: Some("typing 👋".to_string()),
        });
        completion.detail = Some("returns 世界".to_string());

        let mut ui = CompletionUI::new();
        ui.show(vec![completion], 0, 0);

        let rows = ui.render_completion();

        assert!(rows.iter().any(|(_, _, row, _)| row.contains("typing 👋")));
        assert!(!rows
            .iter()
            .any(|(_, _, row, _)| row.contains("returns 世界")));
        assert_segments_within_popup(&ui, &rows);
    }

    #[test]
    fn completion_removes_server_label_markers_before_rendering() {
        let mut function = item("•assert", Some(CompletionItemKind::Function));
        function.label_details = Some(crate::lsp::types::CompletionItemLabelDetails {
            detail: Some("(e)".to_string()),
            description: None,
        });
        let items = vec![
            item("asteroids", Some(CompletionItemKind::Text)),
            item(" Asteroid", Some(CompletionItemKind::Class)),
            function,
        ];

        let mut ui = CompletionUI::new();
        ui.show(items, 0, 0);
        let rows = ui.render_completion();
        let label_x = ui.x + 1 + LEFT_PADDING + ICON_COLUMN_WIDTH;

        for expected in ["asteroids", "Asteroid", "assert(e)"] {
            assert!(
                rows.iter()
                    .any(|(x, _, text, _)| { *x == label_x && text == expected }),
                "{expected:?} did not render at the shared label column"
            );
        }
        assert!(!rows.iter().any(|(_, _, text, _)| text.contains('•')));
        assert!(!rows.iter().any(|(_, _, text, _)| text == " Asteroid"));
    }

    #[test]
    fn completion_width_tracks_filtered_content_and_aligns_the_label_to_the_prefix() {
        let mut ui = CompletionUI::new();
        ui.show_with_bounds(
            vec![
                item(
                    "a_very_long_completion_candidate",
                    Some(CompletionItemKind::Text),
                ),
                item("many", Some(CompletionItemKind::Function)),
            ],
            40,
            5,
            80,
            24,
        );
        let initial_width = ui.width;

        ui.set_filter("many");

        assert!(ui.width < initial_width);
        assert_eq!(ui.x + 1 + LEFT_PADDING + ICON_COLUMN_WIDTH, 40 - 4);
        assert_eq!(ui.width, MIN_INNER_WIDTH + 2);
    }

    #[test]
    fn completion_label_anchor_stays_fixed_as_filter_grows_and_shrinks() {
        let mut ui = CompletionUI::new();
        ui.show_with_bounds(
            vec![item("manual_seed", Some(CompletionItemKind::Function))],
            40,
            5,
            80,
            24,
        );
        ui.set_filter("m");
        let label_x = ui.x + 1 + LEFT_PADDING + ICON_COLUMN_WIDTH;

        assert_eq!(
            ui.handle_event(&key(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(ui.x + 1 + LEFT_PADDING + ICON_COLUMN_WIDTH, label_x);
        assert_eq!(
            ui.handle_event(&key(KeyCode::Char('n'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(ui.x + 1 + LEFT_PADDING + ICON_COLUMN_WIDTH, label_x);

        assert_eq!(
            ui.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE)),
            None
        );
        assert_eq!(ui.x + 1 + LEFT_PADDING + ICON_COLUMN_WIDTH, label_x);
    }

    #[test]
    fn completion_bolds_fuzzy_matched_label_characters() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![item("manual_seed", Some(CompletionItemKind::Function))],
            20,
            0,
        );
        ui.set_filter("mns");

        let rows = ui.render_completion();
        let bold_text = rows
            .iter()
            .filter(|(_, _, _, style)| style.bold)
            .map(|(_, _, text, _)| text.as_str())
            .collect::<String>();

        assert_eq!(bold_text, "mns");
    }

    #[test]
    fn asynchronous_item_updates_preserve_the_selected_label() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![
                item("manual_seed", Some(CompletionItemKind::Text)),
                item("many", Some(CompletionItemKind::Text)),
            ],
            20,
            0,
        );
        ui.set_filter("man");
        if ui.selected_item().unwrap().label != "manual_seed" {
            ui.move_selection(1);
        }
        assert_eq!(ui.selected_item().unwrap().label, "manual_seed");

        let mut lsp_manual_seed = item("manual_seed", Some(CompletionItemKind::Function));
        lsp_manual_seed.sort_text = Some("00".to_string());
        ui.update_items(
            vec![
                item("match", Some(CompletionItemKind::Snippet)),
                lsp_manual_seed,
                item("many", Some(CompletionItemKind::Text)),
            ],
            "man",
        );

        assert_eq!(ui.selected_item().unwrap().label, "manual_seed");
    }

    #[test]
    fn completion_popup_stays_within_bounds_near_bottom_right() {
        let mut ui = CompletionUI::new();
        ui.show_with_bounds(
            vec![
                item("alpha", Some(CompletionItemKind::Function)),
                item("beta", Some(CompletionItemKind::Function)),
                item("gamma", Some(CompletionItemKind::Function)),
            ],
            18,
            5,
            20,
            6,
        );

        let rows = ui.render_completion();

        assert!(!rows.is_empty());
        for (x, y, row, _) in rows {
            assert!(y < 6);
            assert!(x + display_width(&row) <= 20);
        }
    }

    #[test]
    fn completion_popup_trims_extra_rows_to_height_bound() {
        let mut ui = CompletionUI::new();
        ui.show_with_bounds(
            (0..12)
                .map(|index| item(&format!("item_{index}"), Some(CompletionItemKind::Text)))
                .collect(),
            0,
            0,
            20,
            4,
        );

        let rows = ui.render_completion();

        assert!(rows.iter().all(|(_, y, _, _)| *y < 4));
        assert!(rows.iter().any(|(_, _, row, _)| row.contains("item_0")));
    }

    #[test]
    fn bounded_single_item_completion_keeps_its_candidate_visible() {
        let mut ui = CompletionUI::new();
        ui.show_with_bounds(
            vec![item("manual_seed", Some(CompletionItemKind::Function))],
            0,
            0,
            80,
            24,
        );

        let rows = ui.render_completion();

        assert!(rows
            .iter()
            .any(|(_, _, row, _)| row.contains("manual_seed")));
        assert!(!rows.iter().any(|(_, _, row, _)| row == "↓"));
    }

    #[test]
    fn completion_selected_row_uses_theme_ui_style() {
        let mut theme = Theme::default();
        theme.ui_style.picker_selected_item = Style {
            fg: Some(Color::Rgb {
                r: 31,
                g: 32,
                b: 33,
            }),
            bg: Some(Color::Rgb {
                r: 34,
                g: 35,
                b: 36,
            }),
            ..Default::default()
        };

        let mut ui = CompletionUI::with_theme(&theme);
        ui.show(vec![item("hello", Some(CompletionItemKind::Text))], 0, 0);

        let rows = ui.render_completion();
        let selected_style = theme.selected_style(
            &theme.ui_style.picker_item,
            &theme.ui_style.picker_selected_item,
            SelectionForegroundPriority::Selection,
        );

        assert!(rows
            .iter()
            .any(|(_, _, row, style)| { row.contains("hello") && *style == selected_style }));
    }

    #[test]
    fn completion_selected_row_keeps_border_style() {
        let mut theme = Theme::default();
        theme.ui_style.popup_border = Style {
            fg: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
            bg: Some(Color::Rgb { r: 4, g: 5, b: 6 }),
            ..Default::default()
        };
        theme.ui_style.picker_selected_item = Style {
            fg: Some(Color::Rgb { r: 7, g: 8, b: 9 }),
            bg: Some(Color::Rgb {
                r: 10,
                g: 11,
                b: 12,
            }),
            ..Default::default()
        };

        let mut ui = CompletionUI::with_theme(&theme);
        ui.show(vec![item("hello", Some(CompletionItemKind::Text))], 0, 0);

        let rows = ui.render_completion();
        let selected_style = theme.selected_style(
            &theme.ui_style.picker_item,
            &theme.ui_style.picker_selected_item,
            SelectionForegroundPriority::Selection,
        );

        assert!(rows
            .iter()
            .any(|(_, _, row, style)| row == "│" && *style == theme.ui_style.popup_border));
        assert!(rows
            .iter()
            .any(|(_, _, row, style)| { row.contains("hello") && *style == selected_style }));
    }

    #[test]
    fn completion_does_not_render_an_automatic_preview() {
        let mut completion = item("alpha", Some(CompletionItemKind::Function));
        completion.detail = Some("fn alpha()".to_string());

        let mut ui = CompletionUI::new();
        ui.show_with_bounds(vec![completion], 0, 0, 80, 16);

        let rows = ui.render_completion();
        assert!(rows.iter().any(|(_, _, row, _)| row.contains("alpha")));
        assert!(!rows.iter().any(|(_, _, row, _)| row.contains("fn alpha()")));
        assert_eq!(rows.iter().map(|(_, y, _, _)| *y).max(), Some(3));
    }

    #[test]
    fn completion_caps_the_menu_at_ten_rows_and_keeps_selection_visible() {
        let mut items = Vec::new();
        for idx in 0..12 {
            items.push(item(
                &format!("item_{idx:02}"),
                Some(CompletionItemKind::Text),
            ));
        }

        let mut ui = CompletionUI::new();
        ui.show_with_bounds(items, 0, 0, 80, 16);
        for _ in 0..11 {
            ui.move_selection(1);
        }
        let selected_label = ui.selected_item().unwrap().label.clone();

        let rows = ui.render_completion();

        assert!(rows
            .iter()
            .any(|(_, _, row, _)| row.contains(&selected_label)));
        let content_rows = rows.iter().filter(|(_, _, row, _)| row == "│").count() / 2;
        assert_eq!(content_rows, PAGE_SIZE);
    }

    #[test]
    fn tab_accepts_the_selected_completion() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![
                item("alpha", Some(CompletionItemKind::Text)),
                item("beta", Some(CompletionItemKind::Text)),
                item("gamma", Some(CompletionItemKind::Text)),
            ],
            0,
            0,
        );

        let selected = ui.selected_item().unwrap().clone();
        assert_eq!(
            ui.handle_event(&Event::Key(KeyEvent::from(KeyCode::Tab))),
            Some(KeyAction::Multiple(vec![
                Action::ApplyCompletion {
                    item: Box::new(selected),
                    commit_character: None,
                },
                Action::CloseDialog,
            ]))
        );
    }

    #[test]
    fn ctrl_n_and_ctrl_p_move_completion_selection() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![
                item("alpha", Some(CompletionItemKind::Text)),
                item("beta", Some(CompletionItemKind::Text)),
                item("gamma", Some(CompletionItemKind::Text)),
            ],
            0,
            0,
        );

        ui.handle_event(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(ui.selected_item().unwrap().label, "beta");

        ui.handle_event(&key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(ui.selected_item().unwrap().label, "alpha");
    }

    #[test]
    fn enter_inserts_a_newline_and_ctrl_e_only_dismisses_completion() {
        let mut ui = CompletionUI::new();
        ui.show(vec![item("alpha", Some(CompletionItemKind::Text))], 0, 0);

        assert_eq!(
            ui.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::InsertNewLine,
            ]))
        );
        assert_eq!(
            ui.handle_event(&key(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            Some(KeyAction::Single(Action::CloseDialog))
        );
    }

    #[test]
    fn plain_typing_keys_pass_through_completion_popup() {
        let mut ui = CompletionUI::new();
        ui.show(vec![item("alpha", Some(CompletionItemKind::Text))], 0, 0);

        assert!(ui.allows_event_passthrough());
        assert_eq!(
            ui.handle_event(&key(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            ui.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn escape_closes_completion_and_leaves_insert_mode() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![item("manual_seed", Some(CompletionItemKind::Text))],
            0,
            0,
        );

        assert_eq!(
            ui.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::EnterMode(Mode::Normal),
            ]))
        );

        ui.set_filter("manual_seed(1337)");
        assert!(ui.selected_item().is_none());
        assert_eq!(
            ui.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::EnterMode(Mode::Normal),
            ]))
        );
    }

    #[test]
    fn typing_filters_completion_items_without_capturing_keys() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![
                item("ancestors", Some(CompletionItemKind::Function)),
                item("as_mut_os_str", Some(CompletionItemKind::Function)),
                item("as_os_str", Some(CompletionItemKind::Function)),
                item("canonicalize", Some(CompletionItemKind::Function)),
                item("components", Some(CompletionItemKind::Function)),
            ],
            0,
            0,
        );

        assert_eq!(
            ui.handle_event(&key(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            ui.handle_event(&key(KeyCode::Char('s'), KeyModifiers::NONE)),
            None
        );

        let labels = ui
            .items
            .iter()
            .map(|index| ui.all_items[*index].label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 3);
        assert!(labels[..2].contains(&"as_mut_os_str"));
        assert!(labels[..2].contains(&"as_os_str"));
    }

    #[test]
    fn backspace_restores_completion_filter_matches() {
        let mut ui = CompletionUI::new();
        ui.show(
            vec![
                item("add_extension", Some(CompletionItemKind::Function)),
                item("ancestors", Some(CompletionItemKind::Function)),
                item("exists", Some(CompletionItemKind::Function)),
            ],
            0,
            0,
        );

        ui.handle_event(&key(KeyCode::Char('e'), KeyModifiers::NONE));
        ui.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(
            ui.items
                .iter()
                .map(|index| ui.all_items[*index].label.as_str())
                .collect::<Vec<_>>(),
            vec!["exists", "add_extension"]
        );

        ui.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(
            ui.items
                .iter()
                .map(|index| ui.all_items[*index].label.as_str())
                .collect::<Vec<_>>(),
            vec!["exists", "add_extension", "ancestors"]
        );
    }

    #[test]
    fn filtering_keeps_completion_payloads_in_place_and_matches_ascii_case() {
        let mut completion = item("target", Some(CompletionItemKind::Function));
        completion.filter_text = Some("prefix🎯VALUE".to_string());
        completion.detail = Some("large completion payload".repeat(64));
        let mut ui = CompletionUI::new();
        ui.show(vec![item("other", None), completion], 0, 0);
        let payload = ui.all_items[1].detail.as_ref().unwrap().as_ptr();

        ui.set_filter("🎯value");

        assert_eq!(ui.items, vec![1]);
        assert_eq!(ui.selected_item().unwrap().label, "target");
        assert_eq!(ui.all_items[1].detail.as_ref().unwrap().as_ptr(), payload);
    }

    #[test]
    fn filtering_uses_filter_text_or_label_but_not_sort_or_insert_text() {
        let mut sort_only = item("BaseException", Some(CompletionItemKind::Class));
        sort_only.sort_text = Some("xb".to_string());
        sort_only.insert_text = Some("xb".to_string());
        let mut overridden_label = item("xb_alias", Some(CompletionItemKind::Variable));
        overridden_label.filter_text = Some("alias".to_string());
        let exact = item("xb", Some(CompletionItemKind::Variable));
        let mut ui = CompletionUI::new();
        ui.show(vec![sort_only, overridden_label, exact], 0, 0);

        ui.set_filter("xb");

        assert_eq!(ui.items.len(), 1);
        assert_eq!(ui.selected_item().unwrap().label, "xb");
    }
}
