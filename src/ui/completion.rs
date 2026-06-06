use std::cmp::min;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::{
    config::KeyAction,
    editor::{Action, RenderBuffer},
    log,
    lsp::types::{CompletionItemKind, CompletionResponseItem, Documentation},
    theme::{Style, Theme, UiStyle},
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
};

use super::Component;

const MAX_WIDTH: usize = 80;
const PAGE_SIZE: usize = 10;
const PREVIEW_MAX_ROWS: usize = 7;

#[derive(Default, Clone)]
pub struct CompletionUI {
    all_items: Vec<CompletionResponseItem>,
    items: Vec<CompletionResponseItem>,
    filter: String,
    selected: usize,
    scroll_offset: usize,
    visible: bool,
    x: usize,
    y: usize,
    max_height: usize,
    width: usize,
    max_rows: usize,
    commit_chars: Vec<char>,
    styles: UiStyle,
}

impl CompletionUI {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_theme(theme: &Theme) -> Self {
        Self {
            styles: theme.ui_style.clone(),
            ..Default::default()
        }
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        self.styles = theme.ui_style.clone();
    }

    pub fn show(&mut self, items: Vec<CompletionResponseItem>, x: usize, y: usize) {
        self.show_with_bounds(items, x, y, usize::MAX, usize::MAX);
    }

    pub fn show_with_bounds(
        &mut self,
        mut items: Vec<CompletionResponseItem>,
        mut x: usize,
        mut y: usize,
        bounds_width: usize,
        bounds_height: usize,
    ) {
        // Collect commit characters from all items
        self.commit_chars = items
            .iter()
            .filter_map(|item| item.commit_characters.as_ref())
            .flat_map(|chars| chars.iter())
            .filter_map(|s| s.chars().next())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        // Sort items by preselect and label
        items.sort_by(|a, b| {
            b.preselect
                .unwrap_or(false)
                .cmp(&a.preselect.unwrap_or(false))
                .then(a.label.cmp(&b.label))
        });

        // Find first preselected item or default to 0
        let selected = items
            .iter()
            .position(|item| item.preselect.unwrap_or(false))
            .unwrap_or(0);

        let width = Self::calculate_width_for(&items)
            .min(bounds_width.max(2))
            .max(2);
        let unbounded_height = bounds_height == usize::MAX;
        let max_rows = if unbounded_height {
            usize::MAX
        } else {
            let desired_rows = min(items.len(), PAGE_SIZE).saturating_add(2);
            let rows_below = bounds_height.saturating_sub(y.saturating_add(1));
            let rows_above = y;
            if rows_below < desired_rows && rows_above > rows_below {
                let max_rows = desired_rows.min(rows_above);
                y = y.saturating_sub(max_rows);
                max_rows
            } else {
                desired_rows.min(rows_below)
            }
        };

        if bounds_width > width {
            x = x.min(bounds_width - width);
        } else {
            x = 0;
        }

        self.all_items = items.clone();
        self.items = items;
        self.filter.clear();
        self.selected = selected;
        self.scroll_offset = 0;
        self.visible = true;
        self.x = x;
        self.y = y;
        self.width = width;
        self.max_rows = max_rows;
        self.max_height = min(min(self.items.len(), PAGE_SIZE), max_rows.saturating_sub(2));
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.all_items.clear();
        self.items.clear();
        self.filter.clear();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn selected_item(&self) -> Option<&CompletionResponseItem> {
        self.items.get(self.selected)
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter.clear();
        self.filter.push_str(filter);
        self.refilter_items();
    }

    fn calculate_width_for(items: &[CompletionResponseItem]) -> usize {
        let max_item_width = items
            .iter()
            .map(|item| {
                let kind_width = item
                    .kind
                    .as_ref()
                    .map_or(0, |kind| display_width(Self::kind_to_icon(kind)) + 1);
                display_width(&item.label) + kind_width + 4 // +4 for prefix and padding
            })
            .max()
            .unwrap_or(20);

        max_item_width.clamp(40, MAX_WIDTH)
    }

    fn item_filter_score(item: &CompletionResponseItem, filter: &str) -> Option<usize> {
        if filter.is_empty() {
            return Some(0);
        }

        let filter = filter.to_ascii_lowercase();
        [
            item.filter_text.as_deref(),
            Some(item.label.as_str()),
            item.sort_text.as_deref(),
            item.insert_text.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|candidate| {
            let candidate = candidate.to_ascii_lowercase();
            if candidate.starts_with(&filter) {
                Some(0)
            } else {
                candidate.contains(&filter).then_some(1)
            }
        })
        .min()
    }

    fn refilter_items(&mut self) {
        if self.filter.is_empty() {
            self.items = self.all_items.clone();
        } else {
            let mut matches = self
                .all_items
                .iter()
                .cloned()
                .enumerate()
                .filter_map(|(idx, item)| {
                    Self::item_filter_score(&item, &self.filter).map(|score| (score, idx, item))
                })
                .collect::<Vec<_>>();
            matches.sort_by_key(|(score, idx, _)| (*score, *idx));
            self.items = matches.into_iter().map(|(_, _, item)| item).collect();
        }

        self.selected = 0;
        self.scroll_offset = 0;
        self.max_height = min(
            min(self.items.len(), PAGE_SIZE),
            self.max_rows.saturating_sub(2),
        );
    }

    fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.refilter_items();
    }

    fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.refilter_items();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() || self.max_height == 0 {
            return;
        }

        let new_selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize)
        };

        self.selected = min(new_selected, self.items.len() - 1);

        // Adjust scroll if selection is out of view
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.max_height {
            self.scroll_offset = self.selected - self.max_height + 1;
        }
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
        match kind {
            CompletionItemKind::Text => "abc",
            CompletionItemKind::Method => "ƒ",
            CompletionItemKind::Function => "λ",
            CompletionItemKind::Constructor => "⚡",
            CompletionItemKind::Field => "◆",
            CompletionItemKind::Variable => "𝑥",
            CompletionItemKind::Class => "○",
            CompletionItemKind::Interface => "◌",
            CompletionItemKind::Module => "□",
            CompletionItemKind::Property => "◇",
            CompletionItemKind::Unit => "∅",
            CompletionItemKind::Value => "=",
            CompletionItemKind::Enum => "ℰ",
            CompletionItemKind::Keyword => "🔑",
            CompletionItemKind::Snippet => "✂",
            CompletionItemKind::Color => "🎨",
            CompletionItemKind::File => "📄",
            CompletionItemKind::Reference => "→",
            CompletionItemKind::Folder => "📁",
            CompletionItemKind::EnumMember => "ℯ",
            CompletionItemKind::Constant => "π",
            CompletionItemKind::Struct => "⚪",
            CompletionItemKind::Event => "⚡",
            CompletionItemKind::Operator => "±",
            CompletionItemKind::TypeParameter => "𝑇",
        }
    }

    fn row_segments(
        &self,
        y_offset: usize,
        content: &str,
        content_style: Style,
    ) -> Vec<(usize, usize, String, Style)> {
        let y = self.y + y_offset;
        let inner_width = self.width.saturating_sub(2);

        vec![
            (self.x, y, "│".to_string(), self.styles.popup_border.clone()),
            (
                self.x + 1,
                y,
                fit_display_width(content, inner_width),
                content_style,
            ),
            (
                self.x + self.width.saturating_sub(1),
                y,
                "│".to_string(),
                self.styles.popup_border.clone(),
            ),
        ]
    }

    fn separator_segments(&self, y_offset: usize) -> Vec<(usize, usize, String, Style)> {
        let y = self.y + y_offset;
        vec![
            (self.x, y, "├".to_string(), self.styles.popup_border.clone()),
            (
                self.x + 1,
                y,
                "─".repeat(self.width.saturating_sub(2)),
                self.styles.popup_border.clone(),
            ),
            (
                self.x + self.width.saturating_sub(1),
                y,
                "┤".to_string(),
                self.styles.popup_border.clone(),
            ),
        ]
    }

    fn ellipsize(content: &str, width: usize) -> String {
        if display_width(content) <= width {
            return fit_display_width(content, width);
        }

        if width <= 3 {
            return ".".repeat(width);
        }

        let mut truncated = truncate_display_width(content, width - 3);
        truncated.push_str("...");
        fit_display_width(&truncated, width)
    }

    fn render_completion(&self) -> Vec<(usize, usize, String, Style)> {
        if !self.visible || self.items.is_empty() || self.width < 2 || self.max_rows < 2 {
            return Vec::new();
        }

        let mut output = Vec::new();
        let mut y_offset = 1;
        let last_row_offset = self.max_rows.saturating_sub(1);

        // Draw top border
        output.push((
            self.x,
            self.y + y_offset,
            format!("╭{}╮", "─".repeat(self.width - 2)),
            self.styles.popup_border.clone(),
        ));
        y_offset += 1;

        log!(
            "[ui] CompletionUI::render_completion: width={}, height={}",
            self.width,
            self.max_height
        );

        let selected_preview_rows = self
            .selected_item()
            .map(|item| self.preview_rows(item, PREVIEW_MAX_ROWS))
            .unwrap_or_default();
        let has_preview = !selected_preview_rows.is_empty();
        let available_content_rows = last_row_offset.saturating_sub(y_offset);
        let min_list_rows = self
            .items
            .len()
            .min(self.max_height)
            .min(5)
            .min(available_content_rows);
        let available_preview_rows = available_content_rows.saturating_sub(min_list_rows);
        let preview_row_count = if has_preview && available_preview_rows >= 2 {
            1 + selected_preview_rows
                .len()
                .min(PREVIEW_MAX_ROWS)
                .min(available_preview_rows - 1)
        } else {
            0
        };
        let list_capacity = last_row_offset
            .saturating_sub(y_offset)
            .saturating_sub(preview_row_count);
        let visible_count = self.max_height.min(list_capacity);
        let scroll_offset = if visible_count == 0 {
            self.scroll_offset
        } else {
            let max_scroll_offset = self.items.len().saturating_sub(visible_count);
            let offset = if self.selected < self.scroll_offset {
                self.selected
            } else if self.selected >= self.scroll_offset + visible_count {
                self.selected - visible_count + 1
            } else {
                self.scroll_offset
            };
            offset.min(max_scroll_offset)
        };
        let visible_items = self.items.iter().skip(scroll_offset).take(visible_count);

        // Render completion items.
        for (idx, item) in visible_items.enumerate() {
            let is_selected = idx + scroll_offset == self.selected;
            let marker = if is_selected { ">" } else { " " };
            // Format item with icon and handle deprecated items
            let is_deprecated = item.deprecated.unwrap_or(false);

            let display = if let Some(kind) = &item.kind {
                format!(
                    "{} {} {}{}",
                    marker,
                    Self::kind_to_icon(kind),
                    if is_deprecated { "⚠ " } else { "" },
                    item.label
                )
            } else {
                format!(
                    "{}   {}{}",
                    marker,
                    if is_deprecated { "⚠ " } else { "" },
                    item.label
                )
            };

            let display = Self::ellipsize(&display, self.width.saturating_sub(2));

            let style = if is_deprecated {
                self.styles.deprecated.clone()
            } else if is_selected {
                self.styles.picker_selected_item.clone()
            } else {
                self.styles.picker_item.clone()
            };
            output.extend(self.row_segments(y_offset, &display, style));
            y_offset += 1;
        }

        if has_preview && y_offset < last_row_offset {
            output.extend(self.separator_segments(y_offset));
            y_offset += 1;

            for row in selected_preview_rows.into_iter().take(PREVIEW_MAX_ROWS) {
                if y_offset >= last_row_offset {
                    break;
                }
                output.extend(self.row_segments(y_offset, &row, self.styles.muted.clone()));
                y_offset += 1;
            }
        }

        // Draw bottom border
        output.push((
            self.x,
            self.y + y_offset,
            format!("╰{}╯", "─".repeat(self.width - 2)),
            self.styles.popup_border.clone(),
        ));

        // Show scroll indicators
        if scroll_offset > 0 {
            output.push((
                self.x + 1,
                self.y + 1,
                "↑".to_string(),
                self.styles.muted.clone(),
            ));
        }
        if scroll_offset + visible_count < self.items.len() && y_offset > 1 {
            output.push((
                self.x + 1,
                self.y + y_offset - 1,
                "↓".to_string(),
                self.styles.muted.clone(),
            ));
        }

        output
    }

    fn preview_rows(&self, item: &CompletionResponseItem, max_rows: usize) -> Vec<String> {
        let mut rows = Vec::new();
        let width = self.width.saturating_sub(4).max(1);

        if let Some(detail) = &item.detail {
            rows.push(format!("  {}", Self::ellipsize(detail, width)));
        }

        if let Some(chars) = &item.commit_characters {
            let commit_text = format!(
                "Complete with: {}",
                chars
                    .iter()
                    .map(|c| format!("'{}'", c))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            rows.push(format!("  {}", Self::ellipsize(&commit_text, width)));
        }

        if let Some(doc) = &item.documentation {
            let doc_text = match doc {
                Documentation::String(s) => s,
                Documentation::MarkupContent(content) => &content.value,
            };

            for line in textwrap::wrap(doc_text, width) {
                if rows.len() >= max_rows {
                    break;
                }
                rows.push(format!("  {line}"));
            }
        }

        rows
    }
}

impl Component for CompletionUI {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        for (x, y, text, style) in self.render_completion() {
            buffer.set_text(x, y, &text, &style);
        }
        Ok(())
    }

    fn handle_event(&mut self, ev: &Event) -> Option<KeyAction> {
        match ev {
            Event::Key(KeyEvent {
                code: KeyCode::Up, ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                self.move_selection(-1);
                Some(KeyAction::None)
            }
            Event::Key(KeyEvent {
                code: KeyCode::BackTab,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::SHIFT,
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
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Tab, ..
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
                code: KeyCode::Enter,
                ..
            }) => {
                if let Some(item) = self.selected_item() {
                    Some(KeyAction::Multiple(vec![
                        Action::ApplyCompletion {
                            item: Box::new(item.clone()),
                            commit_character: None,
                        },
                        Action::CloseDialog,
                    ]))
                } else {
                    Some(KeyAction::Single(Action::CloseDialog))
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => Some(KeyAction::Single(Action::CloseDialog)),
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
    fn completion_rows_pad_detail_by_display_width() {
        let mut completion = item("hello", Some(CompletionItemKind::Text));
        completion.detail = Some("returns 👋 世界".to_string());

        let mut ui = CompletionUI::new();
        ui.show(vec![completion], 0, 0);

        let rows = ui.render_completion();

        assert!(rows
            .iter()
            .any(|(_, _, row, _)| row.contains("returns 👋 世界")));
        assert_segments_within_popup(&ui, &rows);
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
        let mut completion = item("hello", Some(CompletionItemKind::Text));
        completion.detail = Some("returns value".to_string());
        completion.documentation = Some(Documentation::String(
            "long documentation that would normally add several rows".to_string(),
        ));

        let mut ui = CompletionUI::new();
        ui.show_with_bounds(vec![completion], 0, 0, 20, 4);

        let rows = ui.render_completion();

        assert!(rows.iter().all(|(_, y, _, _)| *y < 4));
        assert_eq!(rows.len(), 3);
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

        assert!(rows.iter().any(|(_, _, row, style)| {
            row.contains("hello") && *style == theme.ui_style.picker_selected_item
        }));
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

        assert!(rows
            .iter()
            .any(|(_, _, row, style)| row == "│" && *style == theme.ui_style.popup_border));
        assert!(rows.iter().any(|(_, _, row, style)| {
            row.contains("hello") && *style == theme.ui_style.picker_selected_item
        }));
    }

    #[test]
    fn completion_preview_renders_below_visible_list() {
        let mut completion = item("alpha", Some(CompletionItemKind::Function));
        completion.detail = Some("fn alpha()".to_string());
        let mut items = vec![completion];
        items
            .extend((0..8).map(|idx| item(&format!("item_{idx}"), Some(CompletionItemKind::Text))));

        let mut ui = CompletionUI::new();
        ui.show_with_bounds(items, 0, 0, 80, 16);

        let rows = ui.render_completion();
        let detail_y = rows
            .iter()
            .find_map(|(_, y, row, _)| row.contains("fn alpha()").then_some(*y))
            .expect("selected item detail should render");
        let last_item_y = rows
            .iter()
            .filter_map(|(_, y, row, _)| row.contains("item_").then_some(*y))
            .max()
            .expect("list items should render");

        assert!(detail_y > last_item_y);
    }

    #[test]
    fn completion_selected_item_stays_visible_when_preview_reduces_list_rows() {
        let mut items = Vec::new();
        for idx in 0..12 {
            let mut completion = item(&format!("item_{idx}"), Some(CompletionItemKind::Text));
            completion.detail = Some(format!("detail {idx}"));
            items.push(completion);
        }

        let mut ui = CompletionUI::new();
        ui.show_with_bounds(items, 0, 0, 80, 16);
        for _ in 0..8 {
            ui.move_selection(1);
        }
        let selected_label = ui.selected_item().unwrap().label.clone();

        let rows = ui.render_completion();

        assert!(rows
            .iter()
            .any(|(_, _, row, _)| row.contains(&selected_label)));
    }

    #[test]
    fn tab_and_backtab_move_completion_selection() {
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

        ui.handle_event(&Event::Key(KeyEvent::from(KeyCode::Tab)));
        assert_eq!(ui.selected_item().unwrap().label, "beta");

        ui.handle_event(&Event::Key(KeyEvent::from(KeyCode::BackTab)));
        assert_eq!(ui.selected_item().unwrap().label, "alpha");
    }

    #[test]
    fn ctrl_j_and_ctrl_k_move_completion_selection() {
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

        ui.handle_event(&key(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(ui.selected_item().unwrap().label, "beta");

        ui.handle_event(&key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(ui.selected_item().unwrap().label, "alpha");
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
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["as_mut_os_str", "as_os_str"]);
        assert_eq!(ui.selected_item().unwrap().label, "as_mut_os_str");
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
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["exists", "add_extension"]
        );

        ui.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(
            ui.items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["exists", "add_extension", "ancestors"]
        );
    }
}
