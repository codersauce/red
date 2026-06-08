use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Reverse;

use crate::{
    color::Color,
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::{
        byte_to_char, char_slice, display_width, fit_display_width, truncate_display_width,
    },
};

use super::{dialog::BorderStyle, Component, Dialog, List};

type SelectAction = Box<dyn Fn(String) -> Action + Send>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<[usize; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_matches: Vec<[usize; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PickerPreview>,
}

impl PickerItem {
    fn display_text(&self) -> String {
        let mut text = self.label.clone();
        if let Some(annotation) = self.annotation.as_deref() {
            text.push_str(annotation);
        }
        if let Some(detail) = self.detail.as_deref().filter(|detail| !detail.is_empty()) {
            text.push_str("  ");
            text.push_str(detail);
        }
        text
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
pub enum PickerPreview {
    Text {
        text: String,
        #[serde(default)]
        language: Option<String>,
    },
    Location {
        path: String,
        #[serde(default)]
        line: Option<usize>,
        #[serde(default)]
        column: Option<usize>,
        /// UTF-8 byte ranges on the focused line.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        matches: Vec<[usize; 2]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerKeyAction {
    pub key: String,
    #[serde(alias = "id")]
    pub action: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerOptions {
    #[serde(default)]
    pub external_filter: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub initial_query: String,
    #[serde(default)]
    pub initial_selection: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub actions: Vec<PickerKeyAction>,
    #[serde(default)]
    pub preview: Option<PickerPreview>,
}

#[derive(Debug, Clone)]
pub enum PickerUpdate {
    Items(Vec<PickerItem>),
    Query(String),
    Status(Option<String>),
    Preview(Option<PickerPreview>),
}

pub struct Picker {
    id: Option<i32>,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    items: Vec<String>,
    list: List,
    dialog: Dialog,
    matcher: SkimMatcherV2,
    select_action: Option<SelectAction>,
    search: String,
    empty_message: Option<String>,
    theme: Theme,
    live: bool,
    dynamic_items: Option<Vec<PickerItem>>,
    visible_dynamic_items: Vec<PickerItem>,
    external_filter: bool,
    status: Option<String>,
    key_actions: Vec<PickerKeyAction>,
    preview: Option<PickerPreview>,
    placeholder: Option<String>,
    preview_scroll: isize,
}

impl Picker {
    pub fn new(title: Option<String>, editor: &Editor, items: &[String], id: Option<i32>) -> Self {
        let total_width = editor.vwidth();
        let total_height = editor.vheight();

        let width = total_width * 80 / 100;
        let height = total_height * 80 / 100;
        let x = (total_width / 2) - (width / 2);
        let y = (total_height / 2) - (height / 2);

        let style = editor.theme.ui_style.popup.clone();
        let item_style = editor.theme.ui_style.picker_item.clone();
        let selected_style = editor.theme.ui_style.picker_selected_item.clone();
        let border_style = editor.theme.ui_style.popup_border.clone();
        let title_style = editor.theme.ui_style.popup_title.clone();

        let dialog = Dialog::new(
            title,
            x,
            y,
            width,
            height.saturating_sub(1),
            &style,
            BorderStyle::Single,
            &editor.theme,
        )
        .with_border_draw_style(&border_style)
        .with_title_style(&title_style);
        let list = List::new(
            x + 1,
            y + 1,
            width,
            height.saturating_sub(3),
            // TODO: remove the clone
            items.to_vec(),
            &item_style,
            &selected_style,
        );

        Picker {
            id,
            x,
            y,
            width,
            height,
            items: items.to_vec(),
            list,
            dialog,
            matcher: SkimMatcherV2::default(),
            select_action: None,
            search: String::new(),
            empty_message: None,
            theme: editor.theme.clone(),
            live: false,
            dynamic_items: None,
            visible_dynamic_items: Vec::new(),
            external_filter: false,
            status: None,
            key_actions: Vec::new(),
            preview: None,
            placeholder: None,
            preview_scroll: 0,
        }
    }

    pub fn new_dynamic(
        title: Option<String>,
        editor: &Editor,
        items: Vec<PickerItem>,
        id: i32,
        options: PickerOptions,
    ) -> Self {
        let labels = items
            .iter()
            .map(PickerItem::display_text)
            .collect::<Vec<_>>();
        let mut picker = Self::new(title, editor, &labels, Some(id));
        picker.live = true;
        picker.dynamic_items = Some(items.clone());
        picker.visible_dynamic_items = items;
        picker.external_filter = options.external_filter;
        picker.placeholder = options.placeholder;
        picker.status = options.status;
        picker.key_actions = options.actions;
        picker.preview = options.preview;
        picker.search = options.initial_query;
        if !picker.external_filter {
            let query = picker.search.clone();
            picker.filter(&query);
        }
        if let Some(selection) = options.initial_selection {
            picker.select_dynamic_id(&selection);
        }
        picker
    }

    pub fn new_live(
        title: Option<String>,
        editor: &Editor,
        items: &[String],
        id: Option<i32>,
        initial_selection: Option<&str>,
    ) -> Self {
        let mut picker = Self::new(title, editor, items, id);
        picker.live = true;
        if let Some(initial_selection) = initial_selection {
            picker.list.set_selected_item(initial_selection);
        }
        picker
    }

    pub fn builder() -> PickerBuilder {
        PickerBuilder::new()
    }

    pub fn filter(&mut self, term: &str) {
        if let Some(items) = &self.dynamic_items {
            if self.external_filter || term.is_empty() {
                self.visible_dynamic_items = items.clone();
            } else {
                let mut matches = items
                    .iter()
                    .filter_map(|item| {
                        self.matcher
                            .fuzzy_match(&item.label, term)
                            .map(|score| (item.clone(), score))
                    })
                    .collect::<Vec<_>>();
                matches.sort_by_key(|(_, score)| Reverse(*score));
                self.visible_dynamic_items = matches.into_iter().map(|(item, _)| item).collect();
            }
            self.list.set_items(
                self.visible_dynamic_items
                    .iter()
                    .map(PickerItem::display_text)
                    .collect(),
            );
            return;
        }
        if term.is_empty() {
            self.list.set_items(self.items.clone());
            return;
        }

        let mut new_items = self
            .items
            .iter()
            .filter_map(|i| {
                if let Some(item) = self.matcher.fuzzy_indices(i, term) {
                    Some((i, item.0))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        new_items.sort_by_key(|item| Reverse(item.1));

        let new_items = new_items
            .iter()
            .map(|(item, _)| item.to_string())
            .collect::<Vec<_>>();
        self.list.set_items(new_items);
    }

    pub fn replace_items(&mut self, items: Vec<String>) {
        self.items = items;
        let search = self.search.clone();
        self.filter(&search);
    }

    pub fn apply_update(&mut self, id: i32, update: PickerUpdate) -> bool {
        if self.id != Some(id) || self.dynamic_items.is_none() {
            return false;
        }
        match update {
            PickerUpdate::Items(items) => {
                let selected_id = self.selected_dynamic_item().map(|item| item.id.clone());
                self.dynamic_items = Some(items);
                let query = self.search.clone();
                self.filter(&query);
                if let Some(selected_id) = selected_id {
                    self.select_dynamic_id(&selected_id);
                }
            }
            PickerUpdate::Query(query) => {
                self.search = query;
                let query = self.search.clone();
                self.filter(&query);
            }
            PickerUpdate::Status(status) => self.status = status,
            PickerUpdate::Preview(preview) => self.preview = preview,
        }
        true
    }

    fn selected_dynamic_item(&self) -> Option<&PickerItem> {
        self.list
            .selected_index()
            .and_then(|index| self.visible_dynamic_items.get(index))
    }

    fn select_dynamic_id(&mut self, id: &str) {
        if let Some(index) = self
            .visible_dynamic_items
            .iter()
            .position(|item| item.id == id)
        {
            self.list.set_selected_index(index);
        }
    }

    pub fn set_empty_message(&mut self, message: Option<String>) {
        self.empty_message = message;
    }

    fn selected_item(&self) -> Option<String> {
        if self.list.items().is_empty() {
            return None;
        }
        Some(self.list.selected_item())
    }

    fn selected_value(&self) -> Option<Value> {
        if let Some(item) = self.selected_dynamic_item() {
            return serde_json::to_value(item).ok();
        }
        self.selected_item().map(Value::String)
    }

    fn notify_selection_changed(&self, previous: Option<String>) -> Option<KeyAction> {
        if !self.live {
            return None;
        }
        let id = self.id?;
        let selected = self.selected_item()?;
        if previous.as_deref() == Some(selected.as_str()) {
            return None;
        }

        Some(KeyAction::Single(Action::NotifyPlugins(
            format!("picker:changed:{id}"),
            self.selected_value().unwrap_or(Value::Null),
        )))
    }

    fn notify_query_changed(&self) -> Option<KeyAction> {
        let id = self.id?;
        self.dynamic_items.as_ref()?;
        Some(KeyAction::Single(Action::NotifyPlugins(
            format!("picker:query:{id}"),
            json!(self.search),
        )))
    }

    fn changed_actions(&self, previous: Option<String>) -> Option<KeyAction> {
        let mut actions = Vec::new();
        if let Some(KeyAction::Single(action)) = self.notify_query_changed() {
            actions.push(action);
        }
        if let Some(KeyAction::Single(action)) = self.notify_selection_changed(previous) {
            actions.push(action);
        }
        match actions.len() {
            0 => None,
            1 => Some(KeyAction::Single(actions.remove(0))),
            _ => Some(KeyAction::Multiple(actions)),
        }
    }

    fn custom_action(&self, event: &event::KeyEvent) -> Option<KeyAction> {
        let id = self.id?;
        self.dynamic_items.as_ref()?;
        let key = normalized_key(event)?;
        let action = self
            .key_actions
            .iter()
            .find(|action| action.key.to_ascii_lowercase().replace("ctrl-", "c-") == key)?;
        Some(KeyAction::Single(Action::NotifyPlugins(
            format!("picker:action:{id}"),
            json!({
                "action": action.action,
                "item": self.selected_value(),
                "query": self.search,
            }),
        )))
    }

    fn notify_cancelled(&self) -> Option<KeyAction> {
        if !self.live {
            return Some(KeyAction::Single(Action::CloseDialog));
        }
        let Some(id) = self.id else {
            return Some(KeyAction::Single(Action::CloseDialog));
        };
        Some(KeyAction::Multiple(vec![
            Action::NotifyPlugins(format!("picker:cancelled:{id}"), json!(null)),
            Action::CloseDialog,
        ]))
    }

    fn theme_color(&self, key: &str) -> Option<Color> {
        self.theme.colors.get(key).copied()
    }

    fn semantic_foreground(&self, base: &Style, semantic: Option<Style>) -> Style {
        let Some(semantic) = semantic else {
            return base.clone();
        };
        Style {
            fg: semantic.fg.or(base.fg),
            bg: base.bg,
            bold: base.bold || semantic.bold,
            italic: base.italic || semantic.italic,
        }
    }

    fn result_row_style(&self, selected: bool) -> Style {
        let mut style = if selected {
            self.theme.ui_style.picker_selected_item.clone()
        } else {
            self.theme.ui_style.picker_item.clone()
        };
        if selected {
            style.bg = self
                .theme_color("peekViewResult.selectionBackground")
                .or(style.bg);
        }
        style
    }

    fn result_file_style(&self, base: &Style) -> Style {
        let semantic = self
            .theme
            .get_style("markup.underline.link")
            .or_else(|| {
                self.theme_color("peekViewResult.fileForeground")
                    .map(|fg| Style {
                        fg: Some(fg),
                        ..Style::default()
                    })
            })
            .or_else(|| self.theme.get_style("string.other.link"))
            .or_else(|| Some(self.theme.ui_style.picker_prompt.clone()));
        self.semantic_foreground(base, semantic)
    }

    fn result_annotation_style(&self, base: &Style) -> Style {
        self.semantic_foreground(base, Some(self.theme.gutter_style.clone()))
    }

    fn result_content_style(&self, base: &Style) -> Style {
        let semantic = self
            .theme_color("peekViewResult.lineForeground")
            .map(|fg| Style {
                fg: Some(fg),
                ..Style::default()
            })
            .or_else(|| Some(self.theme.ui_style.muted.clone()));
        self.semantic_foreground(base, semantic)
    }

    fn result_match_style(&self, base: &Style) -> Style {
        let themed = self
            .theme
            .find_match_highlight_style
            .as_ref()
            .or(self.theme.find_match_style.as_ref());
        Style {
            fg: themed.and_then(|style| style.fg).or(base.fg),
            bg: self
                .theme_color("peekViewResult.matchHighlightBackground")
                .or_else(|| themed.and_then(|style| style.bg))
                .or(base.bg),
            bold: base.bold || themed.is_some_and(|style| style.bold),
            italic: base.italic || themed.is_some_and(|style| style.italic),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_with_matches(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        text: &str,
        width: usize,
        style: &Style,
        match_style: &Style,
        matches: &[[usize; 2]],
    ) -> usize {
        let visible = truncate_display_width(text, width);
        buffer.set_text(x, y, &visible, style);
        let visible_width = display_width(&visible);

        for [start, end] in matches {
            if start >= end {
                continue;
            }
            let prefix = char_slice(text, /*start*/ 0, *start);
            let match_text = char_slice(text, *start, *end);
            let match_x = display_width(prefix);
            if match_x >= width {
                continue;
            }
            let match_text = truncate_display_width(match_text, width - match_x);
            buffer.set_text(x + match_x, y, &match_text, match_style);
        }

        visible_width
    }

    fn draw_dynamic_items(&self, buffer: &mut RenderBuffer) {
        let selected = self.list.selected_index();
        let top = self.list.top_index();
        for (offset, item) in self
            .visible_dynamic_items
            .iter()
            .skip(top)
            .take(self.list.height())
            .enumerate()
        {
            let item_index = top + offset;
            let row_style = self.result_row_style(selected == Some(item_index));
            let y = self.y + 1 + offset;
            buffer.set_text(self.x + 1, y, &" ".repeat(self.width), &row_style);

            let mut x = self.x + 2;
            let mut remaining = self.width.saturating_sub(1);
            let file_style = self.result_file_style(&row_style);
            let match_style = self.result_match_style(&file_style);
            let used = self.draw_text_with_matches(
                buffer,
                x,
                y,
                &item.label,
                remaining,
                &file_style,
                &match_style,
                &item.matches,
            );
            x += used;
            remaining = remaining.saturating_sub(used);

            if let Some(annotation) = item.annotation.as_deref().filter(|value| !value.is_empty()) {
                let annotation_style = self.result_annotation_style(&row_style);
                let visible = truncate_display_width(annotation, remaining);
                buffer.set_text(x, y, &visible, &annotation_style);
                let used = display_width(&visible);
                x += used;
                remaining = remaining.saturating_sub(used);
            }

            if let Some(detail) = item.detail.as_deref().filter(|value| !value.is_empty()) {
                let separator = truncate_display_width("  ", remaining);
                buffer.set_text(x, y, &separator, &row_style);
                let used = display_width(&separator);
                x += used;
                remaining = remaining.saturating_sub(used);

                let content_style = self.result_content_style(&row_style);
                let match_style = self.result_match_style(&content_style);
                self.draw_text_with_matches(
                    buffer,
                    x,
                    y,
                    detail,
                    remaining,
                    &content_style,
                    &match_style,
                    &item.detail_matches,
                );
            }
        }
    }

    fn draw_preview(
        &self,
        buffer: &mut RenderBuffer,
        preview: &PickerPreview,
    ) -> anyhow::Result<()> {
        let divider_x = self.x + self.width / 2;
        let preview_x = divider_x + 1;
        let preview_width = (self.x + self.width + 1).saturating_sub(preview_x);
        let preview_height = self.height.saturating_sub(3);
        if preview_width == 0 || preview_height == 0 {
            return Ok(());
        }

        let blank_line = " ".repeat(preview_width);
        for offset in 0..preview_height {
            buffer.set_char(
                divider_x,
                self.y + 1 + offset,
                '│',
                &self.theme.ui_style.popup_border,
                &self.theme,
            );
            buffer.set_text(
                preview_x,
                self.y + 1 + offset,
                &blank_line,
                &self.theme.ui_style.picker_item,
            );
        }

        let (text, focus_line, byte_matches) = match preview {
            PickerPreview::Text { text, .. } => (text.clone(), None, Vec::new()),
            PickerPreview::Location {
                path,
                line,
                matches,
                ..
            } => {
                let text = std::fs::read_to_string(path)
                    .unwrap_or_else(|error| format!("Unable to preview {path}: {error}"));
                (text, *line, matches.clone())
            }
        };
        let lines = text.lines().collect::<Vec<_>>();
        let centered_start = focus_line
            .unwrap_or_default()
            .saturating_sub(preview_height / 2)
            .min(lines.len().saturating_sub(preview_height));
        let max_start = lines.len().saturating_sub(preview_height);
        let start =
            (centered_start as isize + self.preview_scroll).clamp(0, max_start as isize) as usize;
        for (line_index, line) in lines.iter().enumerate().skip(start).take(preview_height) {
            let offset = line_index - start;
            let focused = focus_line == Some(line_index);
            let mut line_style = self.theme.ui_style.picker_item.clone();
            if focused {
                line_style.bg = self
                    .theme
                    .line_highlight_style
                    .as_ref()
                    .and_then(|style| style.bg)
                    .or(self.theme.ui_style.picker_selected_item.bg)
                    .or(line_style.bg);
            }
            let line = fit_display_width(line, preview_width);
            buffer.set_text(preview_x, self.y + 1 + offset, &line, &line_style);

            if focused {
                let match_style = self.preview_match_style(&line_style);
                let line = lines[line_index];
                let char_matches = byte_matches
                    .iter()
                    .map(|[start, end]| {
                        [
                            byte_to_char(line, floor_char_boundary(line, *start)),
                            byte_to_char(line, floor_char_boundary(line, *end)),
                        ]
                    })
                    .collect::<Vec<_>>();
                self.draw_text_with_matches(
                    buffer,
                    preview_x,
                    self.y + 1 + offset,
                    line,
                    preview_width,
                    &line_style,
                    &match_style,
                    &char_matches,
                );
            }
        }
        Ok(())
    }

    fn preview_match_style(&self, base: &Style) -> Style {
        let themed = self
            .theme
            .find_match_style
            .as_ref()
            .or(self.theme.find_match_highlight_style.as_ref());
        Style {
            fg: themed.and_then(|style| style.fg).or(base.fg),
            bg: self
                .theme_color("peekViewEditor.matchHighlightBackground")
                .or_else(|| themed.and_then(|style| style.bg))
                .or(base.bg),
            bold: base.bold || themed.is_some_and(|style| style.bold),
            italic: base.italic || themed.is_some_and(|style| style.italic),
        }
    }
}

fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

impl Component for Picker {
    fn update_picker(&mut self, id: i32, update: PickerUpdate) -> bool {
        self.apply_update(id, update)
    }

    fn picker_id(&self) -> Option<i32> {
        self.id
    }

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(event) => {
                if let Some(action) = self.custom_action(event) {
                    return Some(action);
                }
                match event.code {
                    KeyCode::Char('j') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        let previous = self.selected_item();
                        self.list.move_down();
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::Char('k') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        let previous = self.selected_item();
                        self.list.move_up();
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::Char('f') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        if self.dynamic_items.is_some() && self.current_preview().is_some() {
                            self.preview_scroll = self
                                .preview_scroll
                                .saturating_add(self.height.saturating_sub(3).max(1) as isize);
                            Some(KeyAction::Single(Action::Refresh))
                        } else {
                            let previous = self.selected_item();
                            self.list.page_down();
                            self.notify_selection_changed(previous)
                        }
                    }
                    KeyCode::Char('b') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        if self.dynamic_items.is_some() && self.current_preview().is_some() {
                            self.preview_scroll = self
                                .preview_scroll
                                .saturating_sub(self.height.saturating_sub(3).max(1) as isize);
                            Some(KeyAction::Single(Action::Refresh))
                        } else {
                            let previous = self.selected_item();
                            self.list.page_up();
                            self.notify_selection_changed(previous)
                        }
                    }
                    KeyCode::Char('d') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        let previous = self.selected_item();
                        self.list.page_down();
                        self.preview_scroll = 0;
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        let previous = self.selected_item();
                        self.list.page_up();
                        self.preview_scroll = 0;
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::PageDown => {
                        let previous = self.selected_item();
                        self.list.page_down();
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::PageUp => {
                        let previous = self.selected_item();
                        self.list.page_up();
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::Down => {
                        let previous = self.selected_item();
                        self.list.move_down();
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::Up => {
                        let previous = self.selected_item();
                        self.list.move_up();
                        self.notify_selection_changed(previous)
                    }
                    KeyCode::Esc => self.notify_cancelled(),
                    KeyCode::Backspace => {
                        let previous = self.selected_item();
                        self.search.pop();
                        let search = self.search.clone();
                        self.filter(&search);
                        self.changed_actions(previous)
                    }
                    KeyCode::Enter => {
                        if self.list.items().is_empty() {
                            return None;
                        }
                        let action = if self.dynamic_items.is_some() {
                            Action::NotifyPlugins(
                                format!("picker:selected:{}", self.id.unwrap_or_default()),
                                self.selected_value().unwrap_or(Value::Null),
                            )
                        } else if let Some(select_action) = &self.select_action {
                            let item = self.list.selected_item();
                            select_action(item)
                        } else {
                            Action::Picked(self.list.selected_item(), self.id)
                        };

                        Some(KeyAction::Multiple(vec![Action::CloseDialog, action]))
                    }
                    KeyCode::Char(c) => {
                        let previous = self.selected_item();
                        let search = format!("{}{}", &self.search, &c);
                        self.filter(&search);
                        self.search = search;
                        self.changed_actions(previous)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        if self.dynamic_items.is_some() {
            self.draw_dynamic_items(buffer);
        } else {
            self.list.draw(buffer)?;
        }
        if self.list.items().is_empty() {
            if let Some(message) = &self.empty_message {
                let line = fit_display_width(message, self.width.saturating_sub(2));
                buffer.set_text(
                    self.x + 2,
                    self.y + 1,
                    &line,
                    &self.theme.ui_style.picker_item,
                );
            }
        }

        let dy = self.y + self.height.saturating_sub(2);
        let border_style = &self.theme.ui_style.popup_border;
        let prompt_style = &self.theme.ui_style.picker_prompt;
        buffer.set_char(self.x, dy, '├', border_style, &self.theme);
        buffer.set_char(self.x + self.width + 1, dy, '┤', border_style, &self.theme);
        buffer.set_text(self.x + 1, dy, &"─".repeat(self.width), border_style);
        if self.search.is_empty() {
            if let Some(placeholder) = &self.placeholder {
                buffer.set_text(
                    self.x + 2,
                    dy + 1,
                    placeholder,
                    &self.theme.ui_style.picker_item,
                );
            }
        } else {
            buffer.set_text(self.x + 2, dy + 1, &self.search, prompt_style);
        }

        if let Some(status) = &self.status {
            let status = truncate_display_width(status, self.width.saturating_sub(4));
            let status = format!(" {status} ");
            let status_x = self.x + self.width + 1 - display_width(&status);
            buffer.set_text(status_x, dy, &status, prompt_style);
        }

        let preview = self.current_preview();
        if let Some(preview) = preview {
            self.draw_preview(buffer, preview)?;
        }

        Ok(())
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let cx = self.x + 2 + display_width(&self.search);
        let cy = self.y + self.height.saturating_sub(1);

        Some((cx, cy))
    }
}

impl Picker {
    fn current_preview(&self) -> Option<&PickerPreview> {
        self.preview.as_ref().or_else(|| {
            self.selected_dynamic_item()
                .and_then(|item| item.preview.as_ref())
        })
    }
}

fn normalized_key(event: &event::KeyEvent) -> Option<String> {
    let name = match event.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "backtab".to_string(),
        KeyCode::F(number) => format!("f{number}"),
        _ => return None,
    };
    let mut prefixes = Vec::new();
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        prefixes.push("c");
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        prefixes.push("alt");
    }
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        prefixes.push("shift");
    }
    prefixes.push(&name);
    Some(prefixes.join("-"))
}

pub struct PickerBuilder {
    title: Option<String>,
    items: Vec<String>,
    id: Option<i32>,
    select_action: Option<SelectAction>,
}

impl Default for PickerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerBuilder {
    pub fn new() -> Self {
        PickerBuilder {
            title: None,
            items: vec![],
            id: None,
            select_action: None,
        }
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_string());
        self
    }

    pub fn items(mut self, items: Vec<String>) -> Self {
        self.items = items;
        self
    }

    #[allow(unused)]
    pub fn id(mut self, id: i32) -> Self {
        self.id = Some(id);
        self
    }

    pub fn select_action(mut self, action: impl Fn(String) -> Action + Send + 'static) -> Self {
        self.select_action = Some(Box::new(action));
        self
    }

    pub fn build(self, editor: &Editor) -> Picker {
        let title = self.title;
        let items = self.items;
        let id = self.id;
        let select_action = self.select_action;

        let mut picker = Picker::new(title, editor, &items, id);
        if let Some(select_action) = select_action {
            picker.select_action = Some(select_action);
        }

        picker
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    use crate::{
        buffer::Buffer,
        color::Color,
        config::{Config, KeyAction},
        editor::{Action, Editor, RenderBuffer},
        lsp::LspManager,
        theme::{Style, Theme},
        ui::{Component, Picker, PickerItem, PickerOptions, PickerPreview, PickerUpdate},
        unicode_utils::display_width,
    };

    fn test_editor() -> Editor {
        test_editor_with_theme(Theme::default())
    }

    fn test_editor_with_theme(theme: Theme) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, String::new());

        Editor::with_size(lsp, 80, 24, config, theme, vec![buffer]).unwrap()
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn select(picker: &mut Picker) -> Option<KeyAction> {
        picker.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE))
    }

    fn render_row(buffer: &RenderBuffer, y: usize) -> String {
        buffer.cells[y * buffer.width..(y + 1) * buffer.width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    fn dynamic_item(id: &str, label: &str) -> PickerItem {
        PickerItem {
            id: id.to_string(),
            label: label.to_string(),
            annotation: None,
            detail: None,
            data: json!({ "path": format!("{label}.rs") }),
            matches: vec![],
            detail_matches: vec![],
            preview: None,
        }
    }

    #[test]
    fn ctrl_j_moves_picker_selection_down() {
        let editor = test_editor();
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);

        picker.handle_event(&key(KeyCode::Char('j'), KeyModifiers::CONTROL));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("bravo".to_string(), None),
            ]))
        );
    }

    #[test]
    fn ctrl_k_moves_picker_selection_up() {
        let editor = test_editor();
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);

        picker.handle_event(&key(KeyCode::Down, KeyModifiers::NONE));
        picker.handle_event(&key(KeyCode::Char('k'), KeyModifiers::CONTROL));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("alpha".to_string(), None),
            ]))
        );
    }

    #[test]
    fn ctrl_f_pages_picker_selection_down() {
        let editor = test_editor();
        let items = (0..20)
            .map(|index| format!("item-{index:02}"))
            .collect::<Vec<_>>();
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);

        picker.handle_event(&key(KeyCode::Char('f'), KeyModifiers::CONTROL));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("item-14".to_string(), None),
            ]))
        );
    }

    #[test]
    fn ctrl_b_pages_picker_selection_up() {
        let editor = test_editor();
        let items = (0..20)
            .map(|index| format!("item-{index:02}"))
            .collect::<Vec<_>>();
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);

        picker.handle_event(&key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        picker.handle_event(&key(KeyCode::Char('b'), KeyModifiers::CONTROL));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("item-00".to_string(), None),
            ]))
        );
    }

    #[test]
    fn page_down_key_pages_picker_selection_down() {
        let editor = test_editor();
        let items = (0..20)
            .map(|index| format!("item-{index:02}"))
            .collect::<Vec<_>>();
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);

        picker.handle_event(&key(KeyCode::PageDown, KeyModifiers::NONE));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("item-14".to_string(), None),
            ]))
        );
    }

    #[test]
    fn page_up_key_pages_picker_selection_up() {
        let editor = test_editor();
        let items = (0..20)
            .map(|index| format!("item-{index:02}"))
            .collect::<Vec<_>>();
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);

        picker.handle_event(&key(KeyCode::PageDown, KeyModifiers::NONE));
        picker.handle_event(&key(KeyCode::PageUp, KeyModifiers::NONE));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("item-00".to_string(), None),
            ]))
        );
    }

    #[test]
    fn plain_j_still_filters_picker_items() {
        let editor = test_editor();
        let items = vec!["kay".to_string(), "jay".to_string()];
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);

        picker.handle_event(&key(KeyCode::Char('j'), KeyModifiers::NONE));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("jay".to_string(), None),
            ]))
        );
    }

    #[test]
    fn replace_items_reapplies_current_search() {
        let editor = test_editor();
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &[], None);

        picker.handle_event(&key(KeyCode::Char('s'), KeyModifiers::NONE));
        picker.handle_event(&key(KeyCode::Char('r'), KeyModifiers::NONE));
        picker.handle_event(&key(KeyCode::Char('c'), KeyModifiers::NONE));
        picker.replace_items(vec!["src/main.rs".to_string(), "README.md".to_string()]);

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("src/main.rs".to_string(), None),
            ]))
        );
    }

    #[test]
    fn picker_draws_empty_message_when_no_items_are_visible() {
        let editor = test_editor();
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &[], None);
        picker.set_empty_message(Some("Loading files...".to_string()));
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        assert!(render_row(&buffer, picker.y + 1).contains("Loading files..."));
    }

    #[test]
    fn picker_status_does_not_overwrite_the_query() {
        let editor = test_editor();
        let mut picker = Picker::new(
            /*title*/ Some("Find in Files".to_string()),
            &editor,
            /*items*/ &[],
            /*id*/ None,
        );
        picker.search = "ProjectSearch".to_string();
        picker.status = Some("Searching (0/500) [regex preview]".to_string());
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let separator_y = picker.y + picker.height.saturating_sub(2);
        let prompt_y = picker.y + picker.height.saturating_sub(1);
        assert!(render_row(&buffer, separator_y).contains("Searching (0/500)"));
        assert!(render_row(&buffer, prompt_y).contains("ProjectSearch"));
        assert!(!render_row(&buffer, prompt_y).contains("Searching"));
    }

    #[test]
    fn picker_preview_does_not_overlap_result_rows() {
        let editor = test_editor();
        let mut item = dynamic_item("a", &"result".repeat(20));
        item.preview = Some(PickerPreview::Text {
            text: "preview text".to_string(),
            language: None,
        });
        let picker = Picker::new_dynamic(
            /*title*/ Some("Find in Files".to_string()),
            &editor,
            vec![item],
            /*id*/ 15,
            PickerOptions::default(),
        );
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let divider_x = picker.x + picker.width / 2;
        let result_row = render_row(&buffer, picker.y + 1);
        assert_eq!(result_row.chars().nth(divider_x), Some('│'));
        let preview = result_row.chars().skip(divider_x + 1).collect::<String>();
        assert!(preview.contains("preview text"));
        assert!(!preview.contains("result"));
    }

    #[test]
    fn dynamic_picker_styles_result_parts_and_preserves_selection_background() {
        let file_color = Color::Rgb { r: 1, g: 2, b: 3 };
        let location_color = Color::Rgb { r: 4, g: 5, b: 6 };
        let content_color = Color::Rgb { r: 7, g: 8, b: 9 };
        let selection_color = Color::Rgb {
            r: 10,
            g: 11,
            b: 12,
        };
        let match_color = Color::Rgb {
            r: 13,
            g: 14,
            b: 15,
        };
        let mut theme = Theme::default();
        theme
            .colors
            .insert("peekViewResult.fileForeground".to_string(), file_color);
        theme
            .colors
            .insert("peekViewResult.lineForeground".to_string(), content_color);
        theme.colors.insert(
            "peekViewResult.selectionBackground".to_string(),
            selection_color,
        );
        theme.colors.insert(
            "peekViewResult.matchHighlightBackground".to_string(),
            match_color,
        );
        theme.gutter_style.fg = Some(location_color);
        let editor = test_editor_with_theme(theme.clone());
        let item = PickerItem {
            id: "result".to_string(),
            label: "src/main.rs".to_string(),
            annotation: Some(":7:3".to_string()),
            detail: Some("let needle = 1".to_string()),
            data: json!({}),
            matches: vec![],
            detail_matches: vec![[4, 10]],
            preview: None,
        };
        let picker = Picker::new_dynamic(
            /*title*/ Some("Find in Files".to_string()),
            &editor,
            vec![item],
            /*id*/ 16,
            PickerOptions::default(),
        );
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let row_start = (picker.y + 1) * buffer.width + picker.x + 2;
        let annotation_start = row_start + "src/main.rs".len();
        let detail_start = annotation_start + ":7:3".len() + 2;
        assert_eq!(buffer.cells[row_start].style.fg, Some(file_color));
        assert_eq!(
            buffer.cells[annotation_start].style.fg,
            Some(location_color)
        );
        assert_eq!(buffer.cells[detail_start].style.fg, Some(content_color));
        assert_eq!(buffer.cells[row_start].style.bg, Some(selection_color));
        assert_eq!(buffer.cells[detail_start + 4].style.bg, Some(match_color));
    }

    #[test]
    fn picker_preview_highlights_the_focused_line_and_utf8_byte_match() {
        let line_color = Color::Rgb {
            r: 21,
            g: 22,
            b: 23,
        };
        let match_color = Color::Rgb {
            r: 24,
            g: 25,
            b: 26,
        };
        let mut theme = Theme {
            line_highlight_style: Some(Style {
                bg: Some(line_color),
                ..Style::default()
            }),
            ..Theme::default()
        };
        theme.colors.insert(
            "peekViewEditor.matchHighlightBackground".to_string(),
            match_color,
        );
        let editor = test_editor_with_theme(theme);
        let line = "let caf\u{e9} = needle;";
        let match_start = line.find("needle").unwrap();
        let match_end = match_start + "needle".len();
        let path =
            std::env::temp_dir().join(format!("red-picker-preview-{}.txt", std::process::id()));
        std::fs::write(&path, line).unwrap();
        let mut item = dynamic_item("result", "src/main.rs");
        item.preview = Some(PickerPreview::Location {
            path: path.to_string_lossy().into_owned(),
            line: Some(0),
            column: Some(match_start),
            matches: vec![[match_start, match_end]],
        });
        let picker = Picker::new_dynamic(
            /*title*/ Some("Find in Files".to_string()),
            &editor,
            vec![item],
            /*id*/ 17,
            PickerOptions::default(),
        );
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let preview_x = picker.x + picker.width / 2 + 1;
        let preview_y = picker.y + 1;
        let line_cell = &buffer.cells[preview_y * buffer.width + preview_x];
        let match_x = preview_x + display_width(&line[..match_start]);
        let match_cell = &buffer.cells[preview_y * buffer.width + match_x];
        assert_eq!(line_cell.style.bg, Some(line_color));
        assert_eq!(match_cell.c, 'n');
        assert_eq!(match_cell.style.bg, Some(match_color));

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn live_picker_notifies_when_selection_changes() {
        let editor = test_editor();
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker =
            Picker::new_live(Some("Themes".to_string()), &editor, &items, Some(7), None);

        assert_eq!(
            picker.handle_event(&key(KeyCode::Down, KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::NotifyPlugins(
                "picker:changed:7".to_string(),
                json!("bravo"),
            )))
        );
    }

    #[test]
    fn live_picker_notifies_when_cancelled() {
        let editor = test_editor();
        let items = vec!["alpha".to_string()];
        let mut picker =
            Picker::new_live(Some("Themes".to_string()), &editor, &items, Some(7), None);

        assert_eq!(
            picker.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Multiple(vec![
                Action::NotifyPlugins("picker:cancelled:7".to_string(), json!(null)),
                Action::CloseDialog,
            ]))
        );
    }

    #[test]
    fn live_picker_honors_initial_selection() {
        let editor = test_editor();
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker = Picker::new_live(
            Some("Themes".to_string()),
            &editor,
            &items,
            Some(7),
            Some("bravo"),
        );

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("bravo".to_string(), Some(7)),
            ]))
        );
    }

    #[test]
    fn dynamic_picker_returns_the_structured_item() {
        let editor = test_editor();
        let items = vec![dynamic_item("a", "alpha"), dynamic_item("b", "bravo")];
        let options = PickerOptions {
            initial_selection: Some("b".to_string()),
            ..PickerOptions::default()
        };
        let mut picker = Picker::new_dynamic(
            /*title*/ Some("Files".to_string()),
            &editor,
            items,
            /*id*/ 9,
            options,
        );

        let Some(KeyAction::Multiple(actions)) = select(&mut picker) else {
            panic!("expected selection actions");
        };
        assert_eq!(actions[0], Action::CloseDialog);
        assert_eq!(
            actions[1],
            Action::NotifyPlugins(
                "picker:selected:9".to_string(),
                serde_json::to_value(dynamic_item("b", "bravo")).unwrap(),
            )
        );
    }

    #[test]
    fn external_filter_emits_query_without_filtering_items() {
        let editor = test_editor();
        let items = vec![dynamic_item("a", "alpha"), dynamic_item("b", "bravo")];
        let options = PickerOptions {
            external_filter: true,
            ..PickerOptions::default()
        };
        let mut picker =
            Picker::new_dynamic(/*title*/ None, &editor, items, /*id*/ 11, options);

        assert_eq!(
            picker.handle_event(&key(KeyCode::Char('z'), KeyModifiers::NONE)),
            Some(KeyAction::Single(Action::NotifyPlugins(
                "picker:query:11".to_string(),
                json!("z"),
            )))
        );
        assert_eq!(picker.visible_dynamic_items.len(), 2);
    }

    #[test]
    fn replacing_dynamic_items_preserves_selection_by_id() {
        let editor = test_editor();
        let items = vec![dynamic_item("a", "alpha"), dynamic_item("b", "bravo")];
        let mut picker = Picker::new_dynamic(
            /*title*/ None,
            &editor,
            items,
            /*id*/ 12,
            PickerOptions::default(),
        );
        picker.handle_event(&key(KeyCode::Down, KeyModifiers::NONE));

        picker.apply_update(
            /*id*/ 12,
            PickerUpdate::Items(vec![
                dynamic_item("b", "renamed"),
                dynamic_item("c", "charlie"),
            ]),
        );

        assert_eq!(
            picker.selected_dynamic_item().map(|item| item.id.as_str()),
            Some("b")
        );
    }

    #[test]
    fn dynamic_picker_emits_custom_key_actions() {
        let editor = test_editor();
        let items = vec![dynamic_item("a", "alpha")];
        let options: PickerOptions = serde_json::from_value(json!({
            "actions": [{ "key": "c-o", "id": "openSplit" }]
        }))
        .unwrap();
        let mut picker =
            Picker::new_dynamic(/*title*/ None, &editor, items, /*id*/ 13, options);

        assert_eq!(
            picker.handle_event(&key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(KeyAction::Single(Action::NotifyPlugins(
                "picker:action:13".to_string(),
                json!({
                    "action": "openSplit",
                    "item": dynamic_item("a", "alpha"),
                    "query": "",
                }),
            )))
        );
    }

    #[test]
    fn picker_draw_uses_theme_ui_styles() {
        let mut theme = Theme::default();
        theme.ui_style.popup = Style {
            fg: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
            bg: Some(Color::Rgb { r: 4, g: 5, b: 6 }),
            ..Default::default()
        };
        theme.ui_style.popup_border = Style {
            fg: Some(Color::Rgb { r: 7, g: 8, b: 9 }),
            bg: Some(Color::Rgb {
                r: 10,
                g: 11,
                b: 12,
            }),
            ..Default::default()
        };
        theme.ui_style.picker_item = Style {
            fg: Some(Color::Rgb {
                r: 13,
                g: 14,
                b: 15,
            }),
            bg: Some(Color::Rgb {
                r: 16,
                g: 17,
                b: 18,
            }),
            ..Default::default()
        };
        theme.ui_style.picker_selected_item = Style {
            fg: Some(Color::Rgb {
                r: 19,
                g: 20,
                b: 21,
            }),
            bg: Some(Color::Rgb {
                r: 22,
                g: 23,
                b: 24,
            }),
            ..Default::default()
        };
        theme.ui_style.picker_prompt = Style {
            fg: Some(Color::Rgb {
                r: 25,
                g: 26,
                b: 27,
            }),
            bg: Some(Color::Rgb {
                r: 28,
                g: 29,
                b: 30,
            }),
            ..Default::default()
        };

        let editor = test_editor_with_theme(theme.clone());
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);
        picker.search = "needle".to_string();
        let mut buffer = RenderBuffer::new(80, 24, &theme.style);

        picker.draw(&mut buffer).unwrap();

        let border_cell = &buffer.cells[picker.y * buffer.width + picker.x];
        assert_eq!(border_cell.style, theme.ui_style.popup_border);

        let selected_cell = &buffer.cells[(picker.y + 1) * buffer.width + picker.x + 1];
        assert_eq!(selected_cell.style, theme.ui_style.picker_selected_item);

        let item_cell = &buffer.cells[(picker.y + 2) * buffer.width + picker.x + 1];
        assert_eq!(item_cell.style, theme.ui_style.picker_item);

        let prompt_cell = &buffer.cells
            [(picker.y + picker.height.saturating_sub(1)) * buffer.width + picker.x + 2];
        assert_eq!(prompt_cell.style, theme.ui_style.picker_prompt);
    }
}
