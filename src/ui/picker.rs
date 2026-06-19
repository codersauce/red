use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{cell::RefCell, cmp::Reverse, collections::HashMap};

use crate::{
    color::Color,
    config::{KeyAction, PickerInputPosition},
    editor::{Action, Editor, RenderBuffer, StyleInfo},
    highlighter::Highlighter,
    theme::{SelectionForegroundPriority, Style, Theme},
    unicode_utils::{
        byte_to_char, char_slice, display_width, fit_display_width, truncate_display_width,
    },
};

use super::{dialog::BorderStyle, Component, Dialog, List};

type SelectAction = Box<dyn Fn(String) -> Action + Send>;
const MIN_HORIZONTAL_PREVIEW_PANE_WIDTH: usize = 40;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PickerItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
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
#[serde(rename_all = "snake_case", untagged)]
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PickerKeyAction {
    pub key: String,
    #[serde(alias = "id")]
    pub action: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
    #[serde(default)]
    pub presentation: PickerPresentation,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct LegacyPickerOptions {
    #[serde(default)]
    pub initial_selection: Option<String>,
    #[serde(default)]
    pub presentation: PickerPresentation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickerPresentation {
    #[default]
    Default,
    Compact,
}

#[derive(Debug, Clone)]
pub enum PickerUpdate {
    Items(Vec<PickerItem>),
    Query(String),
    Status(Option<String>),
    Preview(Option<PickerPreview>),
}

#[derive(Debug, Clone)]
struct PreviewHighlightSpan {
    start: usize,
    end: usize,
    order: usize,
    style: Style,
}

struct PreviewHighlighter {
    highlighter: RefCell<Option<Highlighter>>,
}

impl PreviewHighlighter {
    fn new(theme: &Theme) -> Self {
        Self {
            highlighter: RefCell::new(Highlighter::new(theme).ok()),
        }
    }

    fn highlight(&self, preview: &PickerPreview, text: &str) -> Vec<PreviewHighlightSpan> {
        let mut highlighter = self.highlighter.borrow_mut();
        let Some(highlighter) = highlighter.as_mut() else {
            return Vec::new();
        };

        let style_info = match preview {
            PickerPreview::Text {
                language: Some(language),
                ..
            } => {
                let Some(language_id) = highlighter.language_id_for_name(language) else {
                    return Vec::new();
                };
                highlighter.highlight(language_id, text)
            }
            PickerPreview::Text { language: None, .. } => Ok(Vec::new()),
            PickerPreview::Location { path, .. } => {
                highlighter.highlight_for_file(Some(path), text)
            }
        }
        .unwrap_or_default();

        preview_highlight_spans(style_info)
    }
}

fn preview_highlight_spans(style_info: Vec<StyleInfo>) -> Vec<PreviewHighlightSpan> {
    let mut spans = style_info
        .into_iter()
        .enumerate()
        .filter_map(|(order, style_info)| {
            (style_info.start < style_info.end).then_some(PreviewHighlightSpan {
                start: style_info.start,
                end: style_info.end,
                order,
                style: style_info.style,
            })
        })
        .collect::<Vec<_>>();

    spans.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.order.cmp(&right.order))
    });
    spans
}

struct PreviewLine<'a> {
    text: &'a str,
    start: usize,
    end: usize,
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
    item_previews: HashMap<String, PickerPreview>,
    placeholder: Option<String>,
    preview_scroll: isize,
    preview_highlighter: PreviewHighlighter,
    history_key: Option<String>,
    history: Vec<String>,
    history_navigation: Option<PickerHistoryNavigation>,
    input_position: PickerInputPosition,
    presentation: PickerPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PickerHistoryNavigation {
    original: String,
    position: usize,
}

#[derive(Debug, Clone, Copy)]
struct PickerRect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

#[derive(Debug, Clone, Copy)]
enum PickerDivider {
    Horizontal { y: usize },
    Vertical { x: usize, y: usize, height: usize },
}

#[derive(Debug, Clone, Copy)]
struct PickerPreviewLayout {
    rect: PickerRect,
    divider: PickerDivider,
}

#[derive(Debug, Clone, Copy)]
struct PickerLayout {
    results: PickerRect,
    preview: Option<PickerPreviewLayout>,
    separator_y: usize,
    query_y: usize,
}

impl Picker {
    fn geometry_for_viewport(
        total_width: usize,
        total_height: usize,
        presentation: PickerPresentation,
    ) -> PickerRect {
        let (width, height, x, y) = match presentation {
            PickerPresentation::Default => {
                let width = total_width * 80 / 100;
                let height = total_height * 80 / 100;
                let x = (total_width / 2).saturating_sub(width / 2);
                let y = (total_height / 2).saturating_sub(height / 2);
                (width, height, x, y)
            }
            PickerPresentation::Compact => {
                let width = (total_width * 45 / 100).clamp(32, 52).min(total_width);
                let height = (total_height * 45 / 100).clamp(8, 14).min(total_height);
                let x = total_width.saturating_sub(width + 2);
                let y = (total_height / 2).saturating_sub(height / 2);
                (width, height, x, y)
            }
        };

        PickerRect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn new(title: Option<String>, editor: &Editor, items: &[String], id: Option<i32>) -> Self {
        let presentation = PickerPresentation::Default;
        let geometry = Self::geometry_for_viewport(editor.vwidth(), editor.vheight(), presentation);

        let style = editor.theme.ui_style.popup.clone();
        let item_style = editor.theme.ui_style.picker_item.clone();
        let selected_style = editor.theme.selected_style(
            &item_style,
            &editor.theme.ui_style.picker_selected_item,
            SelectionForegroundPriority::Selection,
        );
        let border_style = editor.theme.ui_style.popup_border.clone();
        let title_style = editor.theme.ui_style.popup_title.clone();

        let dialog = Dialog::new(
            title,
            geometry.x,
            geometry.y,
            geometry.width,
            geometry.height.saturating_sub(1),
            &style,
            BorderStyle::Single,
            &editor.theme,
        )
        .with_border_draw_style(&border_style)
        .with_title_style(&title_style);
        let list = List::new(
            geometry.x + 1,
            geometry.y + 1,
            geometry.width,
            geometry.height.saturating_sub(3),
            // TODO: remove the clone
            items.to_vec(),
            &item_style,
            &selected_style,
        );

        Picker {
            id,
            x: geometry.x,
            y: geometry.y,
            width: geometry.width,
            height: geometry.height,
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
            item_previews: HashMap::new(),
            placeholder: None,
            preview_scroll: 0,
            preview_highlighter: PreviewHighlighter::new(&editor.theme),
            history_key: None,
            history: Vec::new(),
            history_navigation: None,
            input_position: editor.picker_input_position(),
            presentation,
        }
    }

    fn resize_to_viewport(&mut self, total_width: usize, total_height: usize) {
        let geometry = Self::geometry_for_viewport(total_width, total_height, self.presentation);
        self.x = geometry.x;
        self.y = geometry.y;
        self.width = geometry.width;
        self.height = geometry.height;
        self.dialog.x = geometry.x;
        self.dialog.y = geometry.y;
        self.dialog.width = geometry.width;
        self.dialog.height = geometry.height.saturating_sub(1);
        self.sync_list_bounds();
    }

    fn set_presentation_for_viewport(
        &mut self,
        presentation: PickerPresentation,
        viewport_width: usize,
        viewport_height: usize,
    ) {
        self.presentation = presentation;
        self.resize_to_viewport(viewport_width, viewport_height);
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
        picker.set_presentation_for_viewport(
            options.presentation,
            editor.vwidth(),
            editor.vheight(),
        );
        if !picker.external_filter {
            let query = picker.search.clone();
            picker.filter(&query);
        }
        if let Some(selection) = options.initial_selection {
            picker.select_dynamic_id(&selection);
        }
        picker
    }

    pub fn set_history(&mut self, key: impl Into<String>, history: Vec<String>) {
        self.history_key = Some(key.into());
        self.history = history;
        self.history_navigation = None;
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

    pub fn new_live_with_options(
        title: Option<String>,
        editor: &Editor,
        items: &[String],
        id: Option<i32>,
        options: LegacyPickerOptions,
    ) -> Self {
        let mut picker = Self::new_live(
            title,
            editor,
            items,
            id,
            options.initial_selection.as_deref(),
        );
        picker.set_presentation_for_viewport(
            options.presentation,
            editor.vwidth(),
            editor.vheight(),
        );
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
        self.item_previews.clear();
        self.items = items;
        let search = self.search.clone();
        self.filter(&search);
    }

    pub fn replace_items_with_previews(
        &mut self,
        items: Vec<String>,
        previews: HashMap<String, PickerPreview>,
    ) {
        self.item_previews = previews;
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
                self.reset_history_navigation();
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

    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
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

    fn reset_history_navigation(&mut self) {
        self.history_navigation = None;
    }

    fn set_search(&mut self, search: String) {
        self.search = search;
        let query = self.search.clone();
        self.filter(&query);
    }

    fn record_history_action(&self) -> Option<Action> {
        let key = self.history_key.clone()?;
        if self.search.trim().is_empty() {
            return None;
        }
        Some(Action::RecordPickerHistory {
            key,
            query: self.search.clone(),
        })
    }

    fn navigate_history_back(&mut self) -> Option<KeyAction> {
        if self.history.is_empty() {
            return None;
        }

        let previous = self.selected_item();
        let mut navigation =
            self.history_navigation
                .take()
                .unwrap_or_else(|| PickerHistoryNavigation {
                    original: self.search.clone(),
                    position: self.history.len(),
                });
        navigation.position = navigation.position.saturating_sub(1);
        let search = self.history[navigation.position].clone();
        self.set_search(search);
        self.history_navigation = Some(navigation);
        self.changed_actions(previous)
    }

    fn navigate_history_forward(&mut self) -> Option<KeyAction> {
        let mut navigation = self.history_navigation.take()?;

        let previous = self.selected_item();
        if navigation.position + 1 < self.history.len() {
            navigation.position += 1;
            let search = self.history[navigation.position].clone();
            self.set_search(search);
            self.history_navigation = Some(navigation);
        } else {
            let search = navigation.original.clone();
            navigation.position = self.history.len();
            self.set_search(search);
            self.history_navigation = Some(navigation);
        }
        self.changed_actions(previous)
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

    fn semantic_foreground(&self, base: &Style, semantic: Option<Style>, selected: bool) -> Style {
        let Some(semantic) = semantic else {
            return base.clone();
        };
        let style = Style {
            fg: semantic.fg.or(base.fg),
            bg: base.bg,
            bold: base.bold || semantic.bold,
            italic: base.italic || semantic.italic,
        };
        if selected {
            self.theme.ensure_text_contrast(&style)
        } else {
            style
        }
    }

    fn result_row_style(&self, selected: bool) -> Style {
        let base = self.theme.ui_style.picker_item.clone();
        if !selected {
            return base;
        }
        let selection = Style {
            fg: self
                .theme_color("peekViewResult.selectionForeground")
                .or(self.theme.ui_style.picker_selected_item.fg),
            bg: self
                .theme_color("peekViewResult.selectionBackground")
                .or(self.theme.ui_style.picker_selected_item.bg),
            ..self.theme.ui_style.picker_selected_item.clone()
        };
        self.theme
            .selected_style(&base, &selection, SelectionForegroundPriority::Selection)
    }

    fn result_file_style(&self, base: &Style, selected: bool) -> Style {
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
        self.semantic_foreground(base, semantic, selected)
    }

    fn result_label_style(&self, item: &PickerItem, base: &Style, selected: bool) -> Style {
        let Some(scope) = item.kind.as_deref().and_then(symbol_kind_scope) else {
            return self.result_file_style(base, selected);
        };
        self.semantic_foreground(base, self.theme.get_style(scope), selected)
    }

    fn result_annotation_style(&self, base: &Style, selected: bool) -> Style {
        self.semantic_foreground(base, Some(self.theme.gutter_style.clone()), selected)
    }

    fn result_content_style(&self, base: &Style, selected: bool) -> Style {
        let semantic = self
            .theme_color("peekViewResult.lineForeground")
            .map(|fg| Style {
                fg: Some(fg),
                ..Style::default()
            })
            .or_else(|| Some(self.theme.ui_style.muted.clone()));
        self.semantic_foreground(base, semantic, selected)
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

    fn layout(&self) -> PickerLayout {
        let content_y = match self.input_position {
            PickerInputPosition::Top => self.y + 3,
            PickerInputPosition::Bottom => self.y + 1,
        };
        let content = PickerRect {
            x: self.x + 1,
            y: content_y,
            width: self.width,
            height: self.height.saturating_sub(3),
        };
        let separator_y = match self.input_position {
            PickerInputPosition::Top => self.y + 2,
            PickerInputPosition::Bottom => self.y + self.height.saturating_sub(2),
        };
        let query_y = match self.input_position {
            PickerInputPosition::Top => self.y + 1,
            PickerInputPosition::Bottom => self.y + self.height.saturating_sub(1),
        };

        let Some(preview) = self.preview_layout(content) else {
            return PickerLayout {
                results: content,
                preview: None,
                separator_y,
                query_y,
            };
        };

        let results = match (self.input_position, preview.divider) {
            (_, PickerDivider::Vertical { x, .. }) => PickerRect {
                x: content.x,
                y: content.y,
                width: x.saturating_sub(content.x),
                height: content.height,
            },
            (PickerInputPosition::Top, PickerDivider::Horizontal { y }) => PickerRect {
                x: content.x,
                y: content.y,
                width: content.width,
                height: y.saturating_sub(content.y),
            },
            (PickerInputPosition::Bottom, PickerDivider::Horizontal { y }) => {
                let results_y = y.saturating_add(1);
                PickerRect {
                    x: content.x,
                    y: results_y,
                    width: content.width,
                    height: content
                        .y
                        .saturating_add(content.height)
                        .saturating_sub(results_y),
                }
            }
        };

        PickerLayout {
            results,
            preview: Some(preview),
            separator_y,
            query_y,
        }
    }

    fn preview_layout(&self, content: PickerRect) -> Option<PickerPreviewLayout> {
        self.current_preview()?;
        if content.width / 2 >= MIN_HORIZONTAL_PREVIEW_PANE_WIDTH
            && content.width.saturating_sub(content.width / 2) >= MIN_HORIZONTAL_PREVIEW_PANE_WIDTH
        {
            let divider_x = self.x + self.width / 2;
            let preview_x = divider_x + 1;
            return Some(PickerPreviewLayout {
                rect: PickerRect {
                    x: preview_x,
                    y: content.y,
                    width: (self.x + self.width + 1).saturating_sub(preview_x),
                    height: content.height,
                },
                divider: PickerDivider::Vertical {
                    x: divider_x,
                    y: content.y,
                    height: content.height,
                },
            });
        }

        let split_rows = content.height.saturating_sub(1);
        if split_rows == 0 {
            return Some(PickerPreviewLayout {
                rect: PickerRect {
                    x: content.x,
                    y: content.y,
                    width: content.width,
                    height: 0,
                },
                divider: PickerDivider::Horizontal { y: content.y },
            });
        }

        let results_height = split_rows.div_ceil(2);
        let preview_height = split_rows.saturating_sub(results_height);
        match self.input_position {
            PickerInputPosition::Top => {
                let divider_y = content.y + results_height;
                Some(PickerPreviewLayout {
                    rect: PickerRect {
                        x: content.x,
                        y: divider_y + 1,
                        width: content.width,
                        height: preview_height,
                    },
                    divider: PickerDivider::Horizontal { y: divider_y },
                })
            }
            PickerInputPosition::Bottom => Some(PickerPreviewLayout {
                rect: PickerRect {
                    x: content.x,
                    y: content.y,
                    width: content.width,
                    height: preview_height,
                },
                divider: PickerDivider::Horizontal {
                    y: content.y + preview_height,
                },
            }),
        }
    }

    fn sync_list_bounds(&mut self) {
        let rect = self.layout().results;
        self.list
            .set_bounds(rect.x, rect.y, rect.width, rect.height);
    }

    fn preview_page_height(&self) -> usize {
        self.layout()
            .preview
            .map(|preview| preview.rect.height.max(1))
            .unwrap_or_else(|| self.height.saturating_sub(3).max(1))
    }

    fn draw_separator(&self, buffer: &mut RenderBuffer, y: usize) {
        let border_style = &self.theme.ui_style.popup_border;
        buffer.set_char(self.x, y, '├', border_style, &self.theme);
        buffer.set_char(self.x + self.width + 1, y, '┤', border_style, &self.theme);
        buffer.set_text(self.x + 1, y, &"─".repeat(self.width), border_style);
    }

    fn draw_preview_divider(&self, buffer: &mut RenderBuffer, divider: PickerDivider) {
        match divider {
            PickerDivider::Horizontal { y } => self.draw_separator(buffer, y),
            PickerDivider::Vertical { x, y, height } => {
                for offset in 0..height {
                    buffer.set_char(
                        x,
                        y + offset,
                        '│',
                        &self.theme.ui_style.popup_border,
                        &self.theme,
                    );
                }
            }
        }
    }

    fn draw_prompt(&self, buffer: &mut RenderBuffer, layout: PickerLayout) {
        self.draw_separator(buffer, layout.separator_y);

        if self.search.is_empty() {
            if let Some(placeholder) = &self.placeholder {
                buffer.set_text(
                    self.x + 2,
                    layout.query_y,
                    placeholder,
                    &self.theme.ui_style.picker_item,
                );
            }
        } else {
            buffer.set_text(
                self.x + 2,
                layout.query_y,
                &self.search,
                &self.theme.ui_style.picker_prompt,
            );
        }

        if let Some(status) = &self.status {
            let status = truncate_display_width(status, self.width.saturating_sub(4));
            let status = format!(" {status} ");
            let status_x = self.x + self.width + 1 - display_width(&status);
            buffer.set_text(
                status_x,
                layout.separator_y,
                &status,
                &self.theme.ui_style.picker_prompt,
            );
        }
    }

    fn draw_dynamic_items(&self, buffer: &mut RenderBuffer, rect: PickerRect) {
        let selected = self.list.selected_index();
        let top = self.list.top_index();
        for (offset, item) in self
            .visible_dynamic_items
            .iter()
            .skip(top)
            .take(rect.height)
            .enumerate()
        {
            let item_index = top + offset;
            let is_selected = selected == Some(item_index);
            let row_style = self.result_row_style(is_selected);
            let y = rect.y + offset;
            buffer.set_text(rect.x, y, &" ".repeat(rect.width), &row_style);

            let x = rect.x + 1;
            let content_width = rect.width.saturating_sub(1);
            let detail_separator_width = 2;
            let min_primary_width = content_width.min(8);
            let max_detail_width =
                content_width.saturating_sub(min_primary_width + detail_separator_width);
            let detail_width = item
                .detail
                .as_deref()
                .filter(|detail| !detail.is_empty())
                .map(|detail| display_width(detail).min(max_detail_width))
                .unwrap_or_default();
            let separator_width = usize::from(detail_width > 0) * detail_separator_width;
            let primary_width = content_width.saturating_sub(detail_width + separator_width);
            let annotation_width = item
                .annotation
                .as_deref()
                .filter(|annotation| !annotation.is_empty())
                .map(|annotation| display_width(annotation).min(primary_width))
                .unwrap_or_default();
            let label_width = primary_width.saturating_sub(annotation_width);
            let label_style = self.result_label_style(item, &row_style, is_selected);
            let match_style = self.result_match_style(&label_style);
            let used = self.draw_text_with_matches(
                buffer,
                x,
                y,
                &item.label,
                label_width,
                &label_style,
                &match_style,
                &item.matches,
            );
            let annotation_x = x + used;
            let annotation_remaining = primary_width.saturating_sub(used);

            if let Some(annotation) = item.annotation.as_deref().filter(|value| !value.is_empty()) {
                let annotation_style = self.result_annotation_style(&row_style, is_selected);
                let visible = truncate_display_width(annotation, annotation_remaining);
                buffer.set_text(annotation_x, y, &visible, &annotation_style);
            }

            if let Some(detail) = item.detail.as_deref().filter(|value| !value.is_empty()) {
                let detail_x = x + primary_width + separator_width;

                let content_style = self.result_content_style(&row_style, is_selected);
                let match_style = self.result_match_style(&content_style);
                self.draw_text_with_matches(
                    buffer,
                    detail_x,
                    y,
                    detail,
                    detail_width,
                    &content_style,
                    &match_style,
                    &item.detail_matches,
                );
            }
        }
    }

    fn draw_plain_items(&self, buffer: &mut RenderBuffer, rect: PickerRect) {
        let selected = self.list.selected_index();
        let top = self.list.top_index();

        for (offset, item) in self
            .list
            .items()
            .iter()
            .skip(top)
            .take(rect.height)
            .enumerate()
        {
            let item_index = top + offset;
            let y = rect.y + offset;
            let row_style = self.result_row_style(selected == Some(item_index));
            let visible = fit_display_width(&format!(" {item}"), rect.width);
            buffer.set_text(rect.x, y, &visible, &row_style);
        }
    }

    fn draw_legacy_items_with_preview(&self, buffer: &mut RenderBuffer, rect: PickerRect) {
        let selected = self.list.selected_index();
        let top = self.list.top_index();
        let x = rect.x + 1;
        let content_width = rect.width.saturating_sub(1);

        for (offset, item) in self
            .list
            .items()
            .iter()
            .skip(top)
            .take(rect.height)
            .enumerate()
        {
            let item_index = top + offset;
            let y = rect.y + offset;
            let row_style = self.result_row_style(selected == Some(item_index));
            buffer.set_text(rect.x, y, &" ".repeat(rect.width), &row_style);
            let visible = fit_display_width(item, content_width);
            buffer.set_text(x, y, &visible, &row_style);
        }
    }

    fn draw_preview(
        &self,
        buffer: &mut RenderBuffer,
        preview: &PickerPreview,
        layout: PickerPreviewLayout,
    ) -> anyhow::Result<()> {
        self.draw_preview_divider(buffer, layout.divider);
        let preview_x = layout.rect.x;
        let preview_width = layout.rect.width;
        let preview_height = layout.rect.height;
        if preview_width == 0 || preview_height == 0 {
            return Ok(());
        }

        let blank_line = " ".repeat(preview_width);
        for offset in 0..preview_height {
            buffer.set_text(
                preview_x,
                layout.rect.y + offset,
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
        let lines = preview_lines(&text);
        let highlight_spans = self.preview_highlighter.highlight(preview, &text);
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
                let selection = Style {
                    bg: self
                        .theme
                        .line_highlight_style
                        .as_ref()
                        .and_then(|style| style.bg)
                        .or(self.theme.ui_style.picker_selected_item.bg),
                    ..self.theme.ui_style.picker_selected_item.clone()
                };
                line_style = self.theme.selected_style(
                    &line_style,
                    &selection,
                    SelectionForegroundPriority::Selection,
                );
            }
            let visible = fit_display_width(line.text, preview_width);
            let y = layout.rect.y + offset;
            buffer.set_text(preview_x, y, &visible, &line_style);
            self.draw_preview_syntax(
                buffer,
                preview_x,
                y,
                preview_width,
                line,
                &line_style,
                &highlight_spans,
                focused,
            );

            if focused {
                let match_style = self.preview_match_style(&line_style);
                let char_matches = byte_matches
                    .iter()
                    .map(|[start, end]| {
                        [
                            byte_to_char(line.text, floor_char_boundary(line.text, *start)),
                            byte_to_char(line.text, floor_char_boundary(line.text, *end)),
                        ]
                    })
                    .collect::<Vec<_>>();
                self.draw_preview_match_overlays(
                    buffer,
                    preview_x,
                    y,
                    line.text,
                    preview_width,
                    &match_style,
                    &char_matches,
                );
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_preview_syntax(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        line: &PreviewLine<'_>,
        line_style: &Style,
        spans: &[PreviewHighlightSpan],
        selected: bool,
    ) {
        if spans.is_empty() || line.text.is_empty() {
            return;
        }

        let visible = truncate_display_width(line.text, width);
        let visible_end = visible.len();
        for span in spans
            .iter()
            .filter(|span| span.end > line.start && span.start < line.end)
        {
            let start = span.start.saturating_sub(line.start).min(visible_end);
            let end = span
                .end
                .saturating_sub(line.start)
                .min(line.text.len())
                .min(visible_end);
            if start >= end {
                continue;
            }

            let start = floor_char_boundary(line.text, start);
            let end = floor_char_boundary(line.text, end);
            if start >= end {
                continue;
            }

            let prefix = &line.text[..start];
            let segment = &line.text[start..end];
            let segment_x = display_width(prefix);
            if segment_x >= width {
                continue;
            }
            let segment = truncate_display_width(segment, width - segment_x);
            let mut style = merge_preview_style(line_style, &span.style);
            if selected {
                style = self.theme.ensure_text_contrast(&style);
            }
            buffer.set_text(x + segment_x, y, &segment, &style);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_preview_match_overlays(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        text: &str,
        width: usize,
        match_style: &Style,
        matches: &[[usize; 2]],
    ) {
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

fn symbol_kind_scope(kind: &str) -> Option<&'static str> {
    match kind {
        "Array" | "Object" => Some("variable.other"),
        "Boolean" | "Null" | "Number" | "String" => Some("constant.language"),
        "Class" => Some("entity.name.type.class"),
        "Constructor" => Some("entity.name.function.constructor"),
        "Enum" | "EnumMember" => Some("entity.name.type.enum"),
        "Event" => Some("entity.name.function"),
        "Field" | "Property" => Some("variable.other.member"),
        "File" | "Folder" => Some("string.other.link"),
        "Function" => Some("entity.name.function"),
        "Interface" | "Trait" => Some("entity.name.type.interface"),
        "Key" => Some("support.type.property-name"),
        "Keyword" => Some("keyword"),
        "Method" => Some("entity.name.function.member"),
        "Module" | "Namespace" | "Package" => Some("entity.name.namespace"),
        "Operator" => Some("keyword.operator"),
        "Reference" => Some("variable.other"),
        "Snippet" | "Text" => Some("string"),
        "Struct" => Some("entity.name.type.struct"),
        "Constant" => Some("variable.other.constant"),
        "Unit" | "Value" => Some("constant.other"),
        "Variable" => Some("variable.other"),
        "TypeParameter" => Some("entity.name.type.parameter"),
        _ => None,
    }
}

fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn preview_lines(text: &str) -> Vec<PreviewLine<'_>> {
    let mut lines = Vec::new();
    let mut offset = 0;

    for chunk in text.split_inclusive('\n') {
        let start = offset;
        offset += chunk.len();
        let line = chunk
            .strip_suffix('\n')
            .unwrap_or(chunk)
            .strip_suffix('\r')
            .unwrap_or_else(|| chunk.strip_suffix('\n').unwrap_or(chunk));
        lines.push(PreviewLine {
            text: line,
            start,
            end: start + line.len(),
        });
    }

    if !text.is_empty() && !text.ends_with('\n') && lines.is_empty() {
        lines.push(PreviewLine {
            text,
            start: 0,
            end: text.len(),
        });
    }

    lines
}

fn merge_preview_style(base: &Style, syntax: &Style) -> Style {
    Style {
        fg: syntax.fg.or(base.fg),
        bg: syntax.bg.or(base.bg),
        bold: base.bold || syntax.bold,
        italic: base.italic || syntax.italic,
    }
}

impl Component for Picker {
    fn update_picker(&mut self, id: i32, update: PickerUpdate) -> bool {
        self.apply_update(id, update)
    }

    fn picker_id(&self) -> Option<i32> {
        self.id
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.resize_to_viewport(viewport_width, viewport_height);
        true
    }

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        self.sync_list_bounds();
        match ev {
            Event::Key(event) => {
                if event.modifiers.contains(KeyModifiers::CONTROL) {
                    match event.code {
                        KeyCode::Char('h') => return self.navigate_history_back(),
                        KeyCode::Char('l') => return self.navigate_history_forward(),
                        _ => {}
                    }
                }
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
                            let page_height = self.preview_page_height();
                            self.preview_scroll =
                                self.preview_scroll.saturating_add(page_height as isize);
                            Some(KeyAction::Single(Action::Refresh))
                        } else {
                            let previous = self.selected_item();
                            self.list.page_down();
                            self.notify_selection_changed(previous)
                        }
                    }
                    KeyCode::Char('b') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        if self.dynamic_items.is_some() && self.current_preview().is_some() {
                            let page_height = self.preview_page_height();
                            self.preview_scroll =
                                self.preview_scroll.saturating_sub(page_height as isize);
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
                        self.reset_history_navigation();
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

                        let mut actions = Vec::new();
                        if let Some(record_action) = self.record_history_action() {
                            actions.push(record_action);
                        }
                        actions.push(Action::CloseDialog);
                        actions.push(action);

                        Some(KeyAction::Multiple(actions))
                    }
                    KeyCode::Char(c) => {
                        self.reset_history_navigation();
                        let previous = self.selected_item();
                        let search = format!("{}{}", &self.search, &c);
                        self.set_search(search);
                        self.changed_actions(previous)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let layout = self.layout();
        self.dialog.draw(buffer)?;
        if self.dynamic_items.is_some() {
            self.draw_dynamic_items(buffer, layout.results);
        } else if self.current_preview().is_some() {
            self.draw_legacy_items_with_preview(buffer, layout.results);
        } else {
            self.draw_plain_items(buffer, layout.results);
        }
        if self.list.items().is_empty() {
            if let Some(message) = &self.empty_message {
                let line = fit_display_width(message, layout.results.width.saturating_sub(2));
                buffer.set_text(
                    layout.results.x + 1,
                    layout.results.y,
                    &line,
                    &self.theme.ui_style.picker_item,
                );
            }
        }

        self.draw_prompt(buffer, layout);

        if let (Some(preview), Some(preview_layout)) = (self.current_preview(), layout.preview) {
            self.draw_preview(buffer, preview, preview_layout)?;
        }

        Ok(())
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let cx = self.x + 2 + display_width(&self.search);
        let cy = self.layout().query_y;

        Some((cx, cy))
    }
}

impl Picker {
    fn current_preview(&self) -> Option<&PickerPreview> {
        self.preview.as_ref().or_else(|| {
            self.selected_dynamic_item()
                .and_then(|item| item.preview.as_ref())
                .or_else(|| {
                    self.selected_item()
                        .and_then(|item| self.item_previews.get(&item))
                })
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
    history_key: Option<String>,
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
            history_key: None,
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

    pub fn history_key(mut self, key: impl Into<String>) -> Self {
        self.history_key = Some(key.into());
        self
    }

    pub fn build(self, editor: &Editor) -> Picker {
        let title = self.title;
        let items = self.items;
        let id = self.id;
        let select_action = self.select_action;
        let history_key = self.history_key;

        let mut picker = Picker::new(title, editor, &items, id);
        if let Some(select_action) = select_action {
            picker.select_action = Some(select_action);
        }
        if let Some(history_key) = history_key {
            let history = editor.picker_history(&history_key).to_vec();
            picker.set_history(history_key, history);
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
        color::{contrast_ratio, Color},
        config::{Config, KeyAction, PickerInputPosition},
        editor::{Action, Editor, RenderBuffer},
        lsp::LspManager,
        theme::{SelectionForegroundPriority, Style, Theme, TokenStyle},
        ui::{
            Component, LegacyPickerOptions, Picker, PickerItem, PickerOptions, PickerPresentation,
            PickerPreview, PickerUpdate,
        },
        unicode_utils::display_width,
    };

    fn test_editor() -> Editor {
        test_editor_with_theme(Theme::default())
    }

    fn test_editor_with_theme(theme: Theme) -> Editor {
        let config = Config::default();
        test_editor_with_config_and_size(config, theme, 80, 24)
    }

    fn test_editor_with_theme_and_size(theme: Theme, width: usize, height: usize) -> Editor {
        test_editor_with_config_and_size(Config::default(), theme, width, height)
    }

    fn test_editor_with_config_and_size(
        config: Config,
        theme: Theme,
        width: usize,
        height: usize,
    ) -> Editor {
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, String::new());

        Editor::with_size(lsp, width, height, config, theme, vec![buffer]).unwrap()
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
            kind: None,
            annotation: None,
            detail: None,
            data: json!({ "path": format!("{label}.rs") }),
            matches: vec![],
            detail_matches: vec![],
            preview: None,
        }
    }

    #[test]
    fn ctrl_h_and_ctrl_l_browse_picker_query_history() {
        let editor = test_editor();
        let items = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "README.md".to_string(),
        ];
        let mut picker = Picker::new(Some("Find Files".to_string()), &editor, &items, None);
        picker.set_history("find_files", vec!["src".to_string(), "readme".to_string()]);
        picker.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE));
        picker.handle_event(&key(KeyCode::Char('r'), KeyModifiers::NONE));

        picker.handle_event(&key(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(picker.search, "readme");
        assert_eq!(picker.list.items(), &vec!["README.md".to_string()]);

        picker.handle_event(&key(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(picker.search, "src");
        assert_eq!(
            picker.list.items(),
            &vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]
        );

        picker.handle_event(&key(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(picker.search, "readme");

        picker.handle_event(&key(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(picker.search, "dr");
    }

    #[test]
    fn typing_after_history_navigation_resets_history_browse_state() {
        let editor = test_editor();
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker = Picker::new(Some("Items".to_string()), &editor, &items, None);
        picker.set_history("items", vec!["alpha".to_string()]);

        picker.handle_event(&key(KeyCode::Char('h'), KeyModifiers::CONTROL));
        picker.handle_event(&key(KeyCode::Char('z'), KeyModifiers::NONE));

        assert_eq!(picker.search, "alphaz");
        assert_eq!(
            picker.handle_event(&key(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(picker.search, "alphaz");
    }

    #[test]
    fn history_navigation_notifies_external_filter_picker_query_changes() {
        let editor = test_editor();
        let mut picker = Picker::new_dynamic(
            Some("Symbols".to_string()),
            &editor,
            vec![dynamic_item("alpha", "alpha"), dynamic_item("beta", "beta")],
            11,
            PickerOptions {
                external_filter: true,
                ..PickerOptions::default()
            },
        );
        picker.set_history("picker:11", vec!["needle".to_string()]);

        assert_eq!(
            picker.handle_event(&key(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Some(KeyAction::Single(Action::NotifyPlugins(
                "picker:query:11".to_string(),
                json!("needle"),
            )))
        );
        assert_eq!(picker.search, "needle");
    }

    #[test]
    fn accepting_picker_records_non_empty_query_history() {
        let editor = test_editor();
        let items = vec!["src/main.rs".to_string()];
        let mut picker = Picker::new(Some("Find Files".to_string()), &editor, &items, None);
        picker.set_history("find_files", Vec::new());
        picker.handle_event(&key(KeyCode::Char('s'), KeyModifiers::NONE));

        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::RecordPickerHistory {
                    key: "find_files".to_string(),
                    query: "s".to_string(),
                },
                Action::CloseDialog,
                Action::Picked("src/main.rs".to_string(), None),
            ]))
        );
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
    fn picker_can_place_query_input_at_top() {
        let mut config = Config::default();
        config.picker.input_position = PickerInputPosition::Top;
        let editor = test_editor_with_config_and_size(config, Theme::default(), 80, 24);
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker = Picker::new(Some("Files".to_string()), &editor, &items, None);
        picker.search = "needle".to_string();
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let layout = picker.layout();
        assert!(render_row(&buffer, layout.query_y).contains("needle"));
        assert!(render_row(&buffer, layout.results.y).contains("alpha"));
        assert_eq!(picker.cursor_position().unwrap().1, layout.query_y);
    }

    #[test]
    fn narrow_top_input_picker_stacks_files_then_preview() {
        let mut config = Config::default();
        config.picker.input_position = PickerInputPosition::Top;
        let editor = test_editor_with_config_and_size(config, Theme::default(), 50, 24);
        let mut item = dynamic_item("a", "result.rs");
        item.preview = Some(PickerPreview::Text {
            text: "preview text".to_string(),
            language: None,
        });
        let picker = Picker::new_dynamic(
            Some("Find in Files".to_string()),
            &editor,
            vec![item],
            15,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(50, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let layout = picker.layout();
        let preview = layout.preview.expect("preview layout");
        assert!(matches!(
            preview.divider,
            super::PickerDivider::Horizontal { .. }
        ));
        assert!(layout.results.y < preview.rect.y);
        assert!(render_row(&buffer, layout.results.y).contains("result.rs"));
        assert!(render_row(&buffer, preview.rect.y).contains("preview text"));
    }

    #[test]
    fn narrow_bottom_input_picker_stacks_preview_then_files() {
        let editor = test_editor_with_config_and_size(Config::default(), Theme::default(), 50, 24);
        let mut item = dynamic_item("a", "result.rs");
        item.preview = Some(PickerPreview::Text {
            text: "preview text".to_string(),
            language: None,
        });
        let picker = Picker::new_dynamic(
            Some("Find in Files".to_string()),
            &editor,
            vec![item],
            15,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(50, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let layout = picker.layout();
        let preview = layout.preview.expect("preview layout");
        assert!(matches!(
            preview.divider,
            super::PickerDivider::Horizontal { .. }
        ));
        assert!(preview.rect.y < layout.results.y);
        assert!(render_row(&buffer, preview.rect.y).contains("preview text"));
        assert!(render_row(&buffer, layout.results.y).contains("result.rs"));
    }

    #[test]
    fn picker_resize_preserves_query_and_recomputes_preview_layout() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 120, 24);
        let mut item = dynamic_item("a", "result.rs");
        item.preview = Some(PickerPreview::Text {
            text: "preview text".to_string(),
            language: None,
        });
        let mut picker = Picker::new_dynamic(
            Some("Find in Files".to_string()),
            &editor,
            vec![item],
            15,
            PickerOptions {
                initial_query: "result".to_string(),
                ..PickerOptions::default()
            },
        );

        let wide_layout = picker.layout();
        assert!(matches!(
            wide_layout.preview.unwrap().divider,
            super::PickerDivider::Vertical { .. }
        ));

        assert!(picker.resize(80, 24));

        let narrow_layout = picker.layout();
        assert_eq!(picker.search, "result");
        assert_eq!(picker.width, 64);
        assert!(matches!(
            narrow_layout.preview.unwrap().divider,
            super::PickerDivider::Horizontal { .. }
        ));
    }

    #[test]
    fn compact_picker_uses_smaller_right_aligned_geometry() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 120, 30);
        let items = vec!["Kanso Ink".to_string(), "Mocha".to_string()];
        let mut picker = Picker::new_live_with_options(
            Some("Themes".to_string()),
            &editor,
            &items,
            Some(21),
            LegacyPickerOptions {
                initial_selection: Some("Mocha".to_string()),
                presentation: PickerPresentation::Compact,
            },
        );

        assert_eq!(picker.width, 52);
        assert!((8..=14).contains(&picker.height));
        assert!(picker.height < editor.vheight() * 80 / 100);
        assert_eq!(picker.x, 66);
        assert!(picker.x > editor.vwidth() / 2);
        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Picked("Mocha".to_string(), Some(21)),
            ]))
        );
    }

    #[test]
    fn picker_preview_does_not_overlap_result_rows() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 120, 24);
        let mut item = dynamic_item("a", &"result".repeat(20));
        item.detail = Some("src/main.rs:10:2".to_string());
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
        let result = result_row.chars().take(divider_x).collect::<String>();
        assert!(result.contains("src/main.rs:10:2"));
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
            kind: None,
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
            RenderBuffer::new(/*width*/ 120, /*height*/ 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let row_start = (picker.y + 1) * buffer.width + picker.x + 2;
        let annotation_start = row_start + "src/main.rs".len();
        let detail_start =
            (picker.y + 1) * buffer.width + picker.x + picker.width + 1 - "let needle = 1".len();
        assert_eq!(buffer.cells[row_start].style.fg, Some(file_color));
        assert_eq!(
            buffer.cells[annotation_start].style.fg,
            Some(location_color)
        );
        assert_eq!(buffer.cells[detail_start].style.fg, Some(content_color));
        let selected_bg = buffer.cells[row_start].style.bg.unwrap();
        let surface_bg = theme.ui_style.picker_item.bg.unwrap();
        assert!(contrast_ratio(selected_bg, surface_bg) >= 3.0);
        assert_ne!(selected_bg, selection_color);
        assert_eq!(buffer.cells[detail_start + 4].style.bg, Some(match_color));
    }

    #[test]
    fn dynamic_picker_uses_symbol_kind_theme_scope_for_label() {
        let function_color = Color::Rgb {
            r: 31,
            g: 32,
            b: 33,
        };
        let mut theme = Theme::default();
        theme.token_styles.push(TokenStyle {
            name: Some("functions".to_string()),
            scope: vec!["entity.name.function".to_string()],
            style: Style {
                fg: Some(function_color),
                ..Style::default()
            },
        });
        let editor = test_editor_with_theme(theme);
        let mut item = dynamic_item("render", "󰊕 render");
        item.kind = Some("Function".to_string());
        let picker = Picker::new_dynamic(
            Some("Workspace Symbols".to_string()),
            &editor,
            vec![item],
            18,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let row_start = (picker.y + 1) * buffer.width + picker.x + 2;
        assert_eq!(buffer.cells[row_start].style.fg, Some(function_color));
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
        let editor = test_editor_with_theme_and_size(theme.clone(), 120, 24);
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
            RenderBuffer::new(/*width*/ 120, /*height*/ 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let preview_x = picker.x + picker.width / 2 + 1;
        let preview_y = picker.y + 1;
        let line_cell = &buffer.cells[preview_y * buffer.width + preview_x];
        let match_x = preview_x + display_width(&line[..match_start]);
        let match_cell = &buffer.cells[preview_y * buffer.width + match_x];
        let expected_line_style = theme.selected_style(
            &theme.ui_style.picker_item,
            &Style {
                bg: Some(line_color),
                ..theme.ui_style.picker_selected_item.clone()
            },
            SelectionForegroundPriority::Selection,
        );
        assert_eq!(line_cell.style.bg, expected_line_style.bg);
        assert_eq!(match_cell.c, 'n');
        assert_eq!(match_cell.style.bg, Some(match_color));

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn picker_location_preview_uses_path_for_syntax_highlighting() {
        let keyword_color = Color::Rgb {
            r: 31,
            g: 32,
            b: 33,
        };
        let mut theme = Theme::default();
        theme.token_styles.push(TokenStyle {
            name: Some("keyword".to_string()),
            scope: vec!["keyword".to_string()],
            style: Style {
                fg: Some(keyword_color),
                ..Style::default()
            },
        });
        let editor = test_editor_with_theme_and_size(theme.clone(), 120, 24);
        let path = std::env::temp_dir().join(format!(
            "red-picker-preview-syntax-{}.rs",
            std::process::id()
        ));
        std::fs::write(&path, "let value = 1;").unwrap();
        let mut item = dynamic_item("result", "src/main.rs");
        item.preview = Some(PickerPreview::Location {
            path: path.to_string_lossy().into_owned(),
            line: Some(0),
            column: None,
            matches: vec![],
        });
        let picker = Picker::new_dynamic(
            Some("Find in Files".to_string()),
            &editor,
            vec![item],
            18,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(120, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let preview_x = picker.x + picker.width / 2 + 1;
        let preview_y = picker.y + 1;
        let keyword_cell = &buffer.cells[preview_y * buffer.width + preview_x];
        assert_eq!(keyword_cell.c, 'l');
        assert_eq!(keyword_cell.style.fg, Some(keyword_color));

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn picker_text_preview_uses_explicit_language_for_syntax_highlighting() {
        let keyword_color = Color::Rgb {
            r: 34,
            g: 35,
            b: 36,
        };
        let mut theme = Theme::default();
        theme.token_styles.push(TokenStyle {
            name: Some("keyword".to_string()),
            scope: vec!["keyword".to_string()],
            style: Style {
                fg: Some(keyword_color),
                ..Style::default()
            },
        });
        let editor = test_editor_with_theme_and_size(theme.clone(), 120, 24);
        let mut item = dynamic_item("result", "inline");
        item.preview = Some(PickerPreview::Text {
            text: "fn main() {}".to_string(),
            language: Some("rust".to_string()),
        });
        let picker = Picker::new_dynamic(
            Some("Symbols".to_string()),
            &editor,
            vec![item],
            19,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(120, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let preview_x = picker.x + picker.width / 2 + 1;
        let preview_y = picker.y + 1;
        let keyword_cell = &buffer.cells[preview_y * buffer.width + preview_x];
        assert_eq!(keyword_cell.c, 'f');
        assert_eq!(keyword_cell.style.fg, Some(keyword_color));
    }

    #[test]
    fn picker_text_preview_unknown_language_uses_plain_style() {
        let keyword_color = Color::Rgb {
            r: 37,
            g: 38,
            b: 39,
        };
        let mut theme = Theme::default();
        theme.token_styles.push(TokenStyle {
            name: Some("keyword".to_string()),
            scope: vec!["keyword".to_string()],
            style: Style {
                fg: Some(keyword_color),
                ..Style::default()
            },
        });
        let plain_color = theme.ui_style.picker_item.fg;
        let editor = test_editor_with_theme_and_size(theme, 120, 24);
        let mut item = dynamic_item("result", "inline");
        item.preview = Some(PickerPreview::Text {
            text: "fn main() {}".to_string(),
            language: Some("not-a-language".to_string()),
        });
        let picker = Picker::new_dynamic(
            Some("Symbols".to_string()),
            &editor,
            vec![item],
            20,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(120, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let preview_x = picker.x + picker.width / 2 + 1;
        let preview_y = picker.y + 1;
        let keyword_cell = &buffer.cells[preview_y * buffer.width + preview_x];
        assert_eq!(keyword_cell.c, 'f');
        assert_eq!(keyword_cell.style.fg, plain_color);
    }

    #[test]
    fn picker_preview_match_overlay_preserves_syntax_outside_match() {
        let line_color = Color::Rgb {
            r: 41,
            g: 42,
            b: 43,
        };
        let match_color = Color::Rgb {
            r: 44,
            g: 45,
            b: 46,
        };
        let keyword_color = Color::Rgb {
            r: 47,
            g: 48,
            b: 49,
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
        theme.token_styles.push(TokenStyle {
            name: Some("keyword".to_string()),
            scope: vec!["keyword".to_string()],
            style: Style {
                fg: Some(keyword_color),
                ..Style::default()
            },
        });
        let editor = test_editor_with_theme_and_size(theme.clone(), 120, 24);
        let line = "let value = needle;";
        let match_start = line.find("needle").unwrap();
        let match_end = match_start + "needle".len();
        let path = std::env::temp_dir().join(format!(
            "red-picker-preview-overlay-{}.rs",
            std::process::id()
        ));
        std::fs::write(&path, line).unwrap();
        let mut item = dynamic_item("result", "src/main.rs");
        item.preview = Some(PickerPreview::Location {
            path: path.to_string_lossy().into_owned(),
            line: Some(0),
            column: Some(match_start),
            matches: vec![[match_start, match_end]],
        });
        let picker = Picker::new_dynamic(
            Some("Find in Files".to_string()),
            &editor,
            vec![item],
            21,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(120, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let preview_x = picker.x + picker.width / 2 + 1;
        let preview_y = picker.y + 1;
        let keyword_cell = &buffer.cells[preview_y * buffer.width + preview_x];
        let match_x = preview_x + display_width(&line[..match_start]);
        let match_cell = &buffer.cells[preview_y * buffer.width + match_x];
        let expected_line_style = theme.selected_style(
            &theme.ui_style.picker_item,
            &Style {
                bg: Some(line_color),
                ..theme.ui_style.picker_selected_item.clone()
            },
            SelectionForegroundPriority::Selection,
        );
        assert_eq!(keyword_cell.style.bg, expected_line_style.bg);
        assert!(
            contrast_ratio(
                keyword_cell.style.fg.unwrap(),
                keyword_cell.style.bg.unwrap()
            ) >= 4.5
        );
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
    fn rejects_camel_case_picker_options() {
        let result = serde_json::from_value::<PickerOptions>(json!({
            "externalFilter": true
        }));

        assert!(result.is_err());
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
        assert_eq!(
            selected_cell.style,
            theme.selected_style(
                &theme.ui_style.picker_item,
                &theme.ui_style.picker_selected_item,
                SelectionForegroundPriority::Selection,
            )
        );

        let item_cell = &buffer.cells[(picker.y + 2) * buffer.width + picker.x + 1];
        assert_eq!(item_cell.style, theme.ui_style.picker_item);

        let prompt_cell = &buffer.cells
            [(picker.y + picker.height.saturating_sub(1)) * buffer.width + picker.x + 2];
        assert_eq!(prompt_cell.style, theme.ui_style.picker_prompt);
    }
}
