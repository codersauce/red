//! Structured fuzzy picker with query editing, history, previews, status, and live updates.
//!
//! [`Picker`] separates authoritative [`PickerItem`] identity and metadata from rendered
//! rows. Filtering may be local or supplied by a live plugin callback; update IDs prevent
//! responses for an older picker instance from mutating the active dialog.
//!
//! Query and cursor operations are grapheme-aware, while alignment and clipping use
//! terminal display columns. Selection actions are returned to the editor or plugin
//! owner and never applied directly by this module.

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    cmp::Reverse,
    collections::VecDeque,
    io::{self, BufRead as _, BufReader, Read as _, Seek as _, SeekFrom},
    path::PathBuf,
    sync::Arc,
    time::SystemTime,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    color::Color,
    config::{KeyAction, PickerIconStyle, PickerIconsConfig, PickerInputPosition},
    editor::{Action, Editor, RenderBuffer, StyleInfo},
    highlighter::Highlighter,
    theme::{SelectionForegroundPriority, Style, Theme},
    unicode_utils::{
        byte_to_char, char_slice, display_width, fit_display_width, truncate_display_width,
    },
};

use super::{dialog::BorderStyle, Component, Dialog, List};

type SelectAction = Box<dyn Fn(String) -> Action + Send>;
type FilterAction = Box<dyn Fn(&PickerItem, &str) -> Option<i64> + Send>;
const MIN_HORIZONTAL_PREVIEW_PANE_WIDTH: usize = 40;
const MAX_PREVIEW_HIGHLIGHT_BYTES: usize = 64 * 1024;
const MAX_UNFOCUSED_PREVIEW_BYTES: u64 = 256 * 1024;
const MAX_LOCATION_PREVIEW_SCAN_BYTES: usize = 8 * 1024 * 1024;
const LOCATION_PREVIEW_CACHE_CAPACITY: usize = 8;
const COMMAND_COLUMN_GAP: usize = 2;
const PICKER_ICON_WIDTH: usize = 2;
const PICKER_ITEM_PREFIX_WIDTH: usize = 2 + PICKER_ICON_WIDTH;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PickerItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PickerIcon>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", untagged)]
pub enum PickerIcon {
    Text(String),
    Styled {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
    },
}

impl PickerIcon {
    fn text(&self) -> &str {
        match self {
            Self::Text(text) | Self::Styled { text, .. } => text,
        }
    }

    fn role(&self) -> Option<&str> {
        match self {
            Self::Text(_) => None,
            Self::Styled { role, .. } => role.as_deref(),
        }
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

struct CachedLocationPreview {
    path: String,
    modified: Option<SystemTime>,
    len: u64,
    text: Arc<str>,
    line_starts: Vec<usize>,
    first_line: usize,
    source_offset: u64,
    requested_start: usize,
    requested_height: usize,
    complete: bool,
}

pub struct Picker {
    id: Option<i32>,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    items: Vec<String>,
    list: List,
    list_bounds: Option<PickerRect>,
    dialog: Dialog,
    matcher: SkimMatcherV2,
    select_action: Option<SelectAction>,
    filter_action: Option<FilterAction>,
    search: String,
    empty_message: Option<String>,
    theme: Theme,
    live: bool,
    dynamic_items: Option<Vec<PickerItem>>,
    visible_dynamic_items: Vec<usize>,
    command_column_widths: Cell<Option<CommandColumns>>,
    external_filter: bool,
    status: Option<String>,
    key_actions: Vec<PickerKeyAction>,
    preview: Option<PickerPreview>,
    item_preview_root: Option<PathBuf>,
    placeholder: Option<String>,
    preview_scroll: isize,
    preview_highlighter: PreviewHighlighter,
    preview_text_cache: RefCell<VecDeque<Arc<CachedLocationPreview>>>,
    history_key: Option<String>,
    history: Vec<String>,
    history_navigation: Option<PickerHistoryNavigation>,
    input_position: PickerInputPosition,
    icons: PickerIconsConfig,
    presentation: PickerPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PickerHistoryNavigation {
    original: String,
    position: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CommandColumns {
    category: usize,
    title: usize,
    shortcut: usize,
    colon: usize,
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
                let width = (total_width * 45 / 100)
                    .clamp(32, 52)
                    .min(total_width.saturating_sub(2));
                let height = (total_height * 45 / 100)
                    .clamp(8, 14)
                    .min(total_height.saturating_sub(1));
                let x = total_width.saturating_sub(width + 2);
                let y = total_height.saturating_sub(height + 1) / 2;
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
            list_bounds: None,
            dialog,
            matcher: SkimMatcherV2::default(),
            select_action: None,
            filter_action: None,
            search: String::new(),
            empty_message: None,
            theme: editor.theme.clone(),
            live: false,
            dynamic_items: None,
            visible_dynamic_items: Vec::new(),
            command_column_widths: Cell::new(None),
            external_filter: false,
            status: None,
            key_actions: Vec::new(),
            preview: None,
            item_preview_root: None,
            placeholder: None,
            preview_scroll: 0,
            preview_highlighter: PreviewHighlighter::new(&editor.theme),
            preview_text_cache: RefCell::new(VecDeque::new()),
            history_key: None,
            history: Vec::new(),
            history_navigation: None,
            input_position: editor.picker_input_position(),
            icons: editor.picker_icons(),
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

    pub(crate) fn apply_theme(&mut self, theme: &Theme) {
        let item_style = theme.ui_style.picker_item.clone();
        let selected_style = theme.selected_style(
            &item_style,
            &theme.ui_style.picker_selected_item,
            SelectionForegroundPriority::Selection,
        );
        self.list.set_styles(&item_style, &selected_style);
        self.dialog.style = theme.ui_style.popup.clone();
        self.dialog.border_draw_style = theme.ui_style.popup_border.clone();
        self.dialog.title_style = theme.ui_style.popup_title.clone();
        self.dialog.theme = theme.clone();
        self.theme = theme.clone();
        self.preview_highlighter = PreviewHighlighter::new(theme);
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
        let mut picker = Self::new(title, editor, &[], Some(id));
        picker.live = true;
        picker.visible_dynamic_items = (0..items.len()).collect();
        picker.list.set_item_count(items.len());
        picker.dynamic_items = Some(items);
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
                self.visible_dynamic_items = (0..items.len()).collect();
            } else {
                let mut matches = items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        self.filter_action
                            .as_ref()
                            .map_or_else(
                                || self.matcher.fuzzy_match(&item.label, term),
                                |filter| filter(item, term),
                            )
                            .map(|score| (index, score))
                    })
                    .collect::<Vec<_>>();
                matches.sort_unstable_by_key(|(index, score)| (Reverse(*score), *index));
                self.visible_dynamic_items = matches.into_iter().map(|(index, _)| index).collect();
            }
            self.list.set_item_count(self.visible_dynamic_items.len());
            self.command_column_widths.set(None);
            return;
        }
        if term.is_empty() {
            self.list.set_items(self.items.clone());
            return;
        }

        let mut new_items = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                self.matcher
                    .fuzzy_match(item, term)
                    .map(|score| (index, score))
            })
            .collect::<Vec<_>>();
        new_items.sort_unstable_by_key(|(index, score)| (Reverse(*score), *index));

        let new_items = new_items
            .into_iter()
            .map(|(index, _)| self.items[index].clone())
            .collect::<Vec<_>>();
        self.list.set_items(new_items);
    }

    pub fn replace_items(&mut self, items: Vec<String>) {
        self.item_preview_root = None;
        self.dynamic_items = None;
        self.visible_dynamic_items.clear();
        self.items = items;
        let search = self.search.clone();
        self.filter(&search);
    }

    pub fn replace_items_with_preview_root(&mut self, items: Vec<String>, root: PathBuf) {
        self.item_preview_root = Some(root);
        self.dynamic_items = None;
        self.visible_dynamic_items.clear();
        self.items = items;
        let search = self.search.clone();
        self.filter(&search);
    }

    pub fn replace_structured_items(&mut self, items: Vec<PickerItem>) {
        self.item_preview_root = None;
        self.items.clear();
        self.dynamic_items = Some(items);
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
            .and_then(|index| self.dynamic_items.as_ref()?.get(*index))
    }

    fn select_dynamic_id(&mut self, id: &str) {
        let Some(items) = self.dynamic_items.as_ref() else {
            return;
        };
        if let Some(index) = self
            .visible_dynamic_items
            .iter()
            .position(|index| items[*index].id == id)
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

    pub(crate) fn query(&self) -> &str {
        &self.search
    }

    fn selected_item(&self) -> Option<String> {
        if let Some(item) = self.selected_dynamic_item() {
            return Some(item.id.clone());
        }
        if self.list.is_empty() {
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

    fn result_label_style(&self, base: &Style) -> Style {
        base.clone()
    }

    fn color_style(&self, keys: &[&str]) -> Option<Style> {
        keys.iter().find_map(|key| {
            self.theme_color(key).map(|fg| Style {
                fg: Some(fg),
                ..Style::default()
            })
        })
    }

    fn role_style(&self, role: &str) -> Option<Style> {
        match role.to_ascii_lowercase().as_str() {
            "file" | "filepath" | "filematch" | "folder" | "buffer" => self
                .color_style(&["peekViewResult.fileForeground", "symbolIcon.fileForeground"])
                .or_else(|| self.theme.get_style("string.other.link")),
            "command" | "action" | "codeaction" | "theme" | "accent" => {
                Some(self.theme.ui_style.picker_prompt.clone())
            }
            "success" | "preferred" | "proceed" | "added" | "created" | "staged" => self
                .color_style(&[
                    "gitDecoration.addedResourceForeground",
                    "editorGutter.addedBackground",
                    "testing.iconPassed",
                ])
                .or_else(|| self.theme.get_style("markup.inserted")),
            "warning" | "warn" | "modified" | "amend" | "permission" => self
                .color_style(&[
                    "editorWarning.foreground",
                    "notificationsWarningIcon.foreground",
                    "gitDecoration.modifiedResourceForeground",
                ])
                .or_else(|| self.theme.get_style("markup.changed")),
            "error" | "failed" | "deleted" | "conflict" | "destructive" => self
                .color_style(&[
                    "editorError.foreground",
                    "errorForeground",
                    "gitDecoration.deletedResourceForeground",
                ])
                .or_else(|| self.theme.get_style("markup.deleted")),
            "info" | "reference" | "match" | "search" => self
                .color_style(&[
                    "editorInfo.foreground",
                    "notificationsInfoIcon.foreground",
                    "peekViewResult.fileForeground",
                ])
                .or_else(|| Some(self.theme.ui_style.picker_prompt.clone())),
            "gitbranch" | "gitremote" => self
                .color_style(&[
                    "gitDecoration.submoduleResourceForeground",
                    "editorInfo.foreground",
                    "peekViewResult.fileForeground",
                ])
                .or_else(|| Some(self.theme.ui_style.picker_prompt.clone())),
            "gittag" | "gitstash" => self
                .color_style(&[
                    "gitDecoration.modifiedResourceForeground",
                    "editorWarning.foreground",
                ])
                .or_else(|| self.theme.get_style("markup.changed")),
            "gitcommit" => self
                .color_style(&[
                    "gitDecoration.untrackedResourceForeground",
                    "descriptionForeground",
                ])
                .or_else(|| Some(self.theme.ui_style.muted.clone())),
            "gitworktree" => self
                .color_style(&["peekViewResult.fileForeground", "symbolIcon.fileForeground"])
                .or_else(|| self.theme.get_style("string.other.link")),
            "muted" | "cancel" | "close" => Some(self.theme.ui_style.muted.clone()),
            _ => symbol_kind_scope(role).and_then(|scope| self.theme.get_style(scope)),
        }
    }

    fn result_icon_style(&self, item: &PickerItem, base: &Style, selected: bool) -> Style {
        if !self.icons.color {
            return base.clone();
        }
        if item.icon.is_none() {
            if let Some(path) = item_file_path(item) {
                let semantic = picker_file_icon_color(path)
                    .map(|fg| Style {
                        fg: Some(fg),
                        ..Style::default()
                    })
                    .or_else(|| self.role_style("file"));
                return self.semantic_foreground(base, semantic, selected);
            }
        }
        let role = item
            .icon
            .as_ref()
            .and_then(PickerIcon::role)
            .or(item.kind.as_deref());
        self.semantic_foreground(base, role.and_then(|role| self.role_style(role)), selected)
    }

    fn item_icon<'a>(&self, item: &'a PickerItem) -> &'a str {
        if self.icons.style == PickerIconStyle::None {
            return "";
        }
        if item.icon.is_none() {
            if let Some(path) = item_file_path(item) {
                return picker_file_icon(path, self.icons.style);
            }
        }
        item.icon.as_ref().map(PickerIcon::text).unwrap_or_else(|| {
            item.kind
                .as_deref()
                .map(|kind| picker_kind_icon(kind, self.icons.style))
                .unwrap_or_default()
        })
    }

    fn draw_item_prefix(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        item: &PickerItem,
        row_style: &Style,
        selected: bool,
    ) {
        if selected {
            let marker_style = self.semantic_foreground(
                row_style,
                Some(self.theme.ui_style.picker_prompt.clone()),
                true,
            );
            buffer.set_text(x, y, "›", &marker_style);
        }

        let icon = fit_display_width(self.item_icon(item), PICKER_ICON_WIDTH);
        let icon_style = self.result_icon_style(item, row_style, selected);
        buffer.set_text(x + 2, y, &icon, &icon_style);
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
        if !self.has_preview() {
            return None;
        }
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
        if self.list_bounds == Some(rect) {
            return;
        }
        self.list
            .set_bounds(rect.x, rect.y, rect.width, rect.height);
        self.list_bounds = Some(rect);
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
        let query_width = self.width.saturating_sub(2);
        buffer.set_text(
            self.x + 1,
            layout.query_y,
            "›",
            &self.theme.ui_style.picker_prompt,
        );

        if self.search.is_empty() {
            if let Some(placeholder) = &self.placeholder {
                let placeholder = truncate_display_width(placeholder, query_width);
                buffer.set_text(
                    self.x + 3,
                    layout.query_y,
                    &placeholder,
                    &self.theme.ui_style.picker_item,
                );
            }
        } else {
            let visible_query = display_width_tail(&self.search, query_width);
            buffer.set_text(
                self.x + 3,
                layout.query_y,
                visible_query,
                &self.theme.ui_style.picker_prompt,
            );
        }

        let command_status = self
            .selected_dynamic_item()
            .filter(|item| item.kind.as_deref() == Some("Command"))
            .map(|item| {
                let description = item
                    .data
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let total = self.dynamic_items.as_ref().map_or(0, Vec::len);
                let visible = self.visible_dynamic_items.len();
                if description.is_empty() {
                    format!("{visible}/{total} commands")
                } else {
                    format!("{description} · {visible}/{total} commands")
                }
            });
        let file_status = self
            .selected_dynamic_item()
            .filter(|item| item.kind.as_deref() == Some("FilePath"))
            .map(|_| {
                let total = self.dynamic_items.as_ref().map_or(0, Vec::len);
                let visible = self.visible_dynamic_items.len();
                format!("{visible}/{total}")
            });
        if let Some(status) = command_status
            .as_deref()
            .or(self.status.as_deref())
            .or(file_status.as_deref())
        {
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
        let Some(items) = self.dynamic_items.as_ref() else {
            return;
        };
        let content_width = rect.width.saturating_sub(PICKER_ITEM_PREFIX_WIDTH);
        let command_columns = self.command_columns(items, content_width);
        let selected = self.list.selected_index();
        let top = self.list.top_index();
        for (offset, index) in self
            .visible_dynamic_items
            .iter()
            .skip(top)
            .take(rect.height)
            .enumerate()
        {
            let item = &items[*index];
            let item_index = top + offset;
            let is_selected = selected == Some(item_index);
            let row_style = self.result_row_style(is_selected);
            let y = rect.y + offset;
            buffer.set_text(rect.x, y, &" ".repeat(rect.width), &row_style);

            self.draw_item_prefix(buffer, rect.x, y, item, &row_style, is_selected);
            let x = rect.x + PICKER_ITEM_PREFIX_WIDTH;
            if item.kind.as_deref() == Some("Command") {
                let category = item.annotation.as_deref().unwrap_or_default().trim_end();
                if command_columns.category > 0 {
                    let annotation_style = self.result_annotation_style(&row_style, is_selected);
                    let visible = truncate_display_width(category, command_columns.category);
                    buffer.set_text(x, y, &visible, &annotation_style);
                }

                let category_gap = usize::from(command_columns.category > 0) * COMMAND_COLUMN_GAP;
                let label_x = x + command_columns.category + category_gap;
                let label_style = self.result_label_style(&row_style);
                let match_style = self.result_match_style(&label_style);
                self.draw_text_with_matches(
                    buffer,
                    label_x,
                    y,
                    &item.label,
                    command_columns.title,
                    &label_style,
                    &match_style,
                    &item.matches,
                );

                let content_style = self.result_content_style(&row_style, is_selected);
                let detail_match_style = self.result_match_style(&content_style);
                let mut detail_x = label_x + command_columns.title;
                if command_columns.shortcut > 0 {
                    detail_x += COMMAND_COLUMN_GAP;
                    let shortcut = item
                        .data
                        .get("primary_shortcut")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.draw_text_with_matches(
                        buffer,
                        detail_x,
                        y,
                        shortcut,
                        command_columns.shortcut,
                        &content_style,
                        &detail_match_style,
                        &item.detail_matches,
                    );
                    detail_x += command_columns.shortcut;
                }
                if command_columns.colon > 0 {
                    detail_x += COMMAND_COLUMN_GAP;
                    let colon = item
                        .data
                        .get("colon")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.draw_text_with_matches(
                        buffer,
                        detail_x,
                        y,
                        colon,
                        command_columns.colon,
                        &content_style,
                        &detail_match_style,
                        &item.detail_matches,
                    );
                }
                continue;
            }

            let detail_separator_width = 2;
            let min_primary_width = if item.kind.as_deref() == Some("FileMatch") {
                let max_primary_width = content_width.saturating_sub(detail_separator_width + 8);
                (content_width * 2 / 5)
                    .max(display_width(&item.label))
                    .min(max_primary_width)
            } else {
                content_width.min(8)
            };
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
            let label_x = x;
            let label_width = display_width(&item.label).min(primary_width);
            let label_style = self.result_label_style(&row_style);
            let match_style = self.result_match_style(&label_style);
            let used = self.draw_text_with_matches(
                buffer,
                label_x,
                y,
                &item.label,
                label_width,
                &label_style,
                &match_style,
                &item.matches,
            );
            let annotation_remaining = primary_width.saturating_sub(used);

            if annotation_remaining > 1 {
                if let Some(annotation) =
                    item.annotation.as_deref().filter(|value| !value.is_empty())
                {
                    let annotation_style = self.result_annotation_style(&row_style, is_selected);
                    let annotation_x = label_x + used + 1;
                    let visible =
                        truncate_display_width(annotation, annotation_remaining.saturating_sub(1));
                    buffer.set_text(annotation_x, y, &visible, &annotation_style);
                }
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

    fn command_columns(&self, items: &[PickerItem], content_width: usize) -> CommandColumns {
        let mut columns = self.command_column_widths.get().unwrap_or_else(|| {
            let mut columns = CommandColumns::default();
            for item in self
                .visible_dynamic_items
                .iter()
                .filter_map(|index| items.get(*index))
                .filter(|item| item.kind.as_deref() == Some("Command"))
            {
                let category = item.annotation.as_deref().unwrap_or_default().trim_end();
                let shortcut = item
                    .data
                    .get("primary_shortcut")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let colon = item
                    .data
                    .get("colon")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                columns.category = columns.category.max(display_width(category));
                columns.title = columns.title.max(display_width(&item.label));
                columns.shortcut = columns.shortcut.max(display_width(shortcut));
                columns.colon = columns.colon.max(display_width(colon));
            }
            self.command_column_widths.set(Some(columns));
            columns
        });

        let category_gap = usize::from(columns.category > 0) * COMMAND_COLUMN_GAP;
        let primary_width = columns.category + category_gap + columns.title;
        if primary_width > content_width {
            let remaining = content_width.saturating_sub(columns.title);
            columns.category = if remaining > COMMAND_COLUMN_GAP {
                columns.category.min(remaining - COMMAND_COLUMN_GAP)
            } else {
                0
            };
        }

        let category_gap = usize::from(columns.category > 0) * COMMAND_COLUMN_GAP;
        columns.title = columns
            .title
            .min(content_width.saturating_sub(columns.category + category_gap));
        let mut remaining =
            content_width.saturating_sub(columns.category + category_gap + columns.title);
        let has_shortcut = columns.shortcut > 0;
        if columns.shortcut > 0 {
            let required = COMMAND_COLUMN_GAP + columns.shortcut;
            if required <= remaining {
                remaining -= required;
            } else {
                columns.shortcut = 0;
            }
        }
        if columns.colon > 0 {
            let required = COMMAND_COLUMN_GAP + columns.colon;
            if required > remaining || (has_shortcut && columns.shortcut == 0) {
                columns.colon = 0;
            }
        }
        columns
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
            let is_selected = selected == Some(item_index);
            let row_style = self.result_row_style(is_selected);
            buffer.set_text(rect.x, y, &" ".repeat(rect.width), &row_style);
            if is_selected {
                let marker_style = self.semantic_foreground(
                    &row_style,
                    Some(self.theme.ui_style.picker_prompt.clone()),
                    true,
                );
                buffer.set_text(rect.x, y, "›", &marker_style);
            }
            let visible = fit_display_width(item, rect.width.saturating_sub(2));
            buffer.set_text(rect.x + 2, y, &visible, &row_style);
        }
    }

    fn draw_legacy_items_with_preview(&self, buffer: &mut RenderBuffer, rect: PickerRect) {
        let selected = self.list.selected_index();
        let top = self.list.top_index();
        let x = rect.x + 2;
        let content_width = rect.width.saturating_sub(2);

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
            let is_selected = selected == Some(item_index);
            let row_style = self.result_row_style(is_selected);
            buffer.set_text(rect.x, y, &" ".repeat(rect.width), &row_style);
            if is_selected {
                let marker_style = self.semantic_foreground(
                    &row_style,
                    Some(self.theme.ui_style.picker_prompt.clone()),
                    true,
                );
                buffer.set_text(rect.x, y, "›", &marker_style);
            }
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

        let location_preview;
        let (text, focus_line, byte_matches, cached_line_starts, window_first_line) = match preview
        {
            PickerPreview::Text { text, .. } => (text.as_str(), None, &[][..], None, None),
            PickerPreview::Location {
                path,
                line,
                matches,
                ..
            } => {
                location_preview =
                    self.location_preview(path, *line, self.preview_scroll, preview_height);
                (
                    location_preview.text.as_ref(),
                    *line,
                    matches.as_slice(),
                    Some(location_preview.line_starts.as_slice()),
                    (!location_preview.complete).then_some(location_preview.first_line),
                )
            }
        };
        let line_count = cached_line_starts.map_or_else(|| text.lines().count(), <[usize]>::len);
        let centered_start = focus_line
            .unwrap_or_default()
            .saturating_sub(preview_height / 2)
            .min(line_count.saturating_sub(preview_height));
        let max_start = line_count.saturating_sub(preview_height);
        let start = if window_first_line.is_some() {
            0
        } else {
            centered_start
                .saturating_add_signed(self.preview_scroll)
                .min(max_start)
        };
        let lines = cached_line_starts.map_or_else(
            || preview_lines(text, start, preview_height),
            |line_starts| preview_lines_with_starts(text, line_starts, start, preview_height),
        );
        let highlight_spans = self.preview_highlight_spans(preview, text, &lines);
        for (offset, line) in lines.iter().enumerate() {
            let line_index = window_first_line.unwrap_or_default() + start + offset;
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

    fn location_preview(
        &self,
        path: &str,
        focus_line: Option<usize>,
        preview_scroll: isize,
        preview_height: usize,
    ) -> Arc<CachedLocationPreview> {
        let metadata = std::fs::metadata(path).ok();
        let modified = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok());
        let len = metadata.as_ref().map_or(0, std::fs::Metadata::len);
        let requested_start = location_preview_start(
            focus_line,
            preview_scroll,
            preview_height,
            /*line_count*/ None,
        );
        let mut cache = self.preview_text_cache.borrow_mut();
        let cached_index = cache.iter().position(|cached| {
            cached.path == path
                && cached.modified == modified
                && cached.len == len
                && (cached.complete
                    || (cached.requested_start == requested_start
                        && cached.requested_height == preview_height))
        });
        if let Some(cached) = cached_index.and_then(|index| cache.remove(index)) {
            let result = Arc::clone(&cached);
            cache.push_front(cached);
            return result;
        }

        let complete = len <= MAX_UNFOCUSED_PREVIEW_BYTES;
        let checkpoint = cache
            .iter()
            .filter(|cached| {
                modified.is_some()
                    && cached.path == path
                    && cached.modified == modified
                    && cached.len == len
                    && cached.first_line <= requested_start
                    && (cached.first_line == 0 || cached.source_offset > 0)
            })
            .max_by_key(|cached| cached.first_line)
            .map(|cached| (cached.first_line, cached.source_offset));
        let (text, first_line, source_offset) = read_location_preview(
            path,
            complete,
            focus_line,
            preview_scroll,
            preview_height,
            checkpoint,
        )
        .unwrap_or_else(|error| {
            (
                format!("Unable to preview {path}: {error}"),
                requested_start,
                0,
            )
        });
        let text = Arc::<str>::from(text);
        let line_starts = preview_line_starts(&text);
        let preview = Arc::new(CachedLocationPreview {
            path: path.to_string(),
            modified,
            len,
            text,
            line_starts,
            first_line,
            source_offset,
            requested_start,
            requested_height: preview_height,
            complete,
        });
        if metadata.is_some() {
            cache.retain(|cached| {
                cached.path != path || (cached.modified == modified && cached.len == len)
            });
            cache.push_front(Arc::clone(&preview));
            cache.truncate(LOCATION_PREVIEW_CACHE_CAPACITY);
        }
        preview
    }

    fn preview_highlight_spans(
        &self,
        preview: &PickerPreview,
        text: &str,
        lines: &[PreviewLine<'_>],
    ) -> Vec<PreviewHighlightSpan> {
        let Some(first) = lines.first() else {
            return Vec::new();
        };
        let Some(last) = lines.last() else {
            return Vec::new();
        };
        let start = first.start;
        let end = floor_char_boundary(
            text,
            last.end
                .min(start.saturating_add(MAX_PREVIEW_HIGHLIGHT_BYTES)),
        );
        if start >= end {
            return Vec::new();
        }

        let mut spans = self
            .preview_highlighter
            .highlight(preview, &text[start..end]);
        for span in &mut spans {
            span.start += start;
            span.end += start;
        }
        spans
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

fn picker_kind_icon(kind: &str, style: PickerIconStyle) -> &'static str {
    match style {
        PickerIconStyle::Unicode => unicode_picker_kind_icon(kind),
        PickerIconStyle::NerdFont => nerd_font_picker_kind_icon(kind),
        PickerIconStyle::Ascii => ascii_picker_kind_icon(kind),
        PickerIconStyle::None => "",
    }
}

fn item_file_path(item: &PickerItem) -> Option<&str> {
    match item.kind.as_deref()? {
        "FilePath" => Some(item.id.as_str()),
        "FileMatch" => item
            .data
            .get("location")
            .and_then(|location| location.get("path"))
            .and_then(Value::as_str)
            .or(match item.preview.as_ref() {
                Some(PickerPreview::Location { path, .. }) => Some(path.as_str()),
                _ => None,
            }),
        _ => None,
    }
}

fn picker_file_icon(path: &str, style: PickerIconStyle) -> &'static str {
    let extension = picker_file_extension(path);
    match style {
        PickerIconStyle::Unicode => match extension {
            "c" | "h" => "Ⓒ",
            "cpp" | "cc" | "cxx" | "hpp" => "C+",
            "css" | "scss" | "sass" => "♯",
            "fish" | "sh" | "zsh" => "$",
            "html" | "htm" | "xml" => "<>",
            "js" | "cjs" | "mjs" => "JS",
            "json" | "jsonc" => "{}",
            "lua" => "☾",
            "md" | "markdown" | "mdx" => "M↓",
            "py" => "Py",
            "rs" => "ⓡ",
            "toml" => "ⓣ",
            "ts" => "TS",
            "tsx" | "jsx" => "TX",
            "yaml" | "yml" => "Y",
            "lock" => "▣",
            _ => "▤",
        },
        PickerIconStyle::NerdFont => picker_file_devicon(path).0,
        PickerIconStyle::Ascii => match extension {
            "c" | "h" => "C",
            "cpp" | "cc" | "cxx" | "hpp" => "C+",
            "css" | "scss" | "sass" => "#",
            "fish" | "sh" | "zsh" => "$",
            "html" | "htm" | "xml" => "<>",
            "js" | "cjs" | "mjs" => "JS",
            "json" | "jsonc" => "{}",
            "lua" => "L",
            "md" | "markdown" | "mdx" => "MD",
            "py" => "Py",
            "rs" => "Rs",
            "toml" => "T",
            "ts" => "TS",
            "tsx" | "jsx" => "TX",
            "yaml" | "yml" => "Y",
            "lock" => "L",
            _ => "F",
        },
        PickerIconStyle::None => "",
    }
}

fn picker_file_icon_color(path: &str) -> Option<Color> {
    picker_file_devicon(path)
        .1
        .map(|(r, g, b)| Color::Rgb { r, g, b })
}

/// The filename-first, extension-second subset of nvim-web-devicons used by
/// Red's common picker file types. Unknown files use Snacks' generic fallback.
fn picker_file_devicon(path: &str) -> (&'static str, Option<(u8, u8, u8)>) {
    let filename = picker_file_name(path);
    let filename_icon = if matches_ignore_ascii_case(filename, &["readme", "readme.md"]) {
        Some(("󰂺", (237, 237, 237)))
    } else if matches_ignore_ascii_case(filename, &["license", "license.md", "unlicense"]) {
        Some(("", (208, 191, 65)))
    } else if matches_ignore_ascii_case(filename, &["copying", "copying.lesser"]) {
        Some(("", (203, 203, 65)))
    } else if matches_ignore_ascii_case(filename, &[".gitignore", ".gitattributes"]) {
        Some(("", (245, 77, 39)))
    } else if filename.eq_ignore_ascii_case("dockerfile") {
        Some(("󰡨", (69, 142, 230)))
    } else if matches_ignore_ascii_case(filename, &["makefile", "gnumakefile"]) {
        Some(("", (109, 128, 134)))
    } else if filename.eq_ignore_ascii_case("package.json") {
        Some(("", (232, 39, 75)))
    } else if filename.eq_ignore_ascii_case("tsconfig.json") {
        Some(("", (81, 154, 186)))
    } else if matches_ignore_ascii_case(filename, &["go.mod", "go.sum"]) {
        Some(("", (0, 173, 216)))
    } else if filename.eq_ignore_ascii_case("gemfile") {
        Some(("", (112, 21, 22)))
    } else if filename.eq_ignore_ascii_case("justfile") {
        Some(("", (109, 128, 134)))
    } else {
        None
    };
    if let Some((icon, color)) = filename_icon {
        return (icon, Some(color));
    }

    let (icon, color) = match picker_file_extension(path) {
        "c" => ("", (89, 158, 255)),
        "h" | "hpp" => ("", (160, 116, 196)),
        "cc" => ("", (243, 75, 125)),
        "cpp" | "cxx" => ("", (81, 154, 186)),
        "conf" | "ini" => ("", (109, 128, 134)),
        "cs" => ("󰌛", (89, 103, 6)),
        "css" => ("", (102, 51, 153)),
        "sass" | "scss" => ("", (245, 83, 133)),
        "erl" | "hrl" => ("", (184, 57, 152)),
        "ex" | "exs" => ("", (160, 116, 196)),
        "env" => ("", (250, 247, 67)),
        "fish" | "sh" => ("", (77, 90, 94)),
        "zsh" => ("", (137, 224, 81)),
        "fs" | "fsx" => ("", (81, 154, 186)),
        "gif" | "jpeg" | "jpg" | "png" | "webp" => ("", (160, 116, 196)),
        "go" => ("", (0, 173, 216)),
        "gql" | "graphql" => ("", (229, 53, 171)),
        "htm" => ("", (227, 76, 38)),
        "html" => ("", (228, 77, 38)),
        "java" => ("", (204, 62, 68)),
        "js" | "cjs" => ("", (203, 203, 65)),
        "mjs" => ("", (241, 224, 90)),
        "json" | "jsonc" => ("", (203, 203, 65)),
        "jsx" => ("", (32, 194, 227)),
        "kt" | "kts" => ("", (127, 82, 255)),
        "lock" => ("", (187, 187, 187)),
        "log" => ("󰌱", (221, 221, 221)),
        "lua" => ("", (81, 160, 207)),
        "markdown" => ("", (221, 221, 221)),
        "md" => ("", (221, 221, 221)),
        "mdx" => ("", (81, 154, 186)),
        "pdf" => ("", (179, 11, 0)),
        "php" => ("", (160, 116, 196)),
        "py" => ("", (255, 188, 3)),
        "rb" => ("", (112, 21, 22)),
        "rs" => ("", (222, 165, 132)),
        "sql" => ("", (218, 216, 216)),
        "svelte" => ("", (255, 62, 0)),
        "swift" => ("", (227, 121, 51)),
        "toml" => ("", (156, 66, 33)),
        "ts" => ("", (81, 154, 186)),
        "tsx" => ("", (19, 84, 191)),
        "txt" => ("󰈙", (137, 224, 81)),
        "vue" => ("", (141, 193, 73)),
        "xml" => ("󰗀", (227, 121, 51)),
        "yaml" | "yml" => ("", (215, 0, 0)),
        "zig" => ("", (246, 154, 27)),
        _ => return ("󰈔", None),
    };
    (icon, Some(color))
}

fn picker_file_extension(path: &str) -> &str {
    let file = picker_file_name(path);
    file.rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or_default()
}

fn picker_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn unicode_picker_kind_icon(kind: &str) -> &'static str {
    match kind {
        "Array" => "[]",
        "Boolean" => "◐",
        "Class" => "○",
        "Color" => "◉",
        "Constant" => "π",
        "Constructor" => "◇",
        "Enum" => "ℰ",
        "EnumMember" => "ℯ",
        "Event" => "↯",
        "Field" => "◆",
        "File" | "FilePath" | "FileMatch" => "▤",
        "Folder" => "▸",
        "Function" => "λ",
        "Interface" | "Trait" => "◌",
        "Key" => "⌁",
        "Keyword" => "κ",
        "Method" => "ƒ",
        "Module" => "□",
        "Namespace" => "§",
        "Null" | "Unit" => "∅",
        "Number" => "#",
        "Object" => "◈",
        "Operator" => "±",
        "Package" | "Struct" => "▦",
        "Property" => "◇",
        "Reference" => "→",
        "Snippet" => "✂",
        "String" => "″",
        "Text" => "≡",
        "TypeParameter" => "𝑇",
        "Value" => "=",
        "Variable" => "𝑥",
        "Buffer" => "▣",
        "Command" => "⌘",
        "Theme" => "◐",
        "Match" | "Search" => "⌕",
        "CodeAction" | "Action" => "◇",
        "Preferred" | "Proceed" | "Success" | "Added" | "Created" | "Staged" => "✓",
        "Warning" | "Warn" | "Modified" | "Permission" | "Amend" => "⚠",
        "Error" | "Failed" | "Deleted" | "Conflict" | "Destructive" => "✗",
        "Cancel" | "Close" => "×",
        "GitCommit" => "●",
        "GitBranch" => "⌁",
        "GitTag" => "◆",
        "GitStash" => "≡",
        "GitRemote" => "↗",
        "GitWorktree" => "▦",
        "Unknown" => "?",
        _ => "",
    }
}

fn nerd_font_picker_kind_icon(kind: &str) -> &'static str {
    match kind {
        "Array" => "",
        "Boolean" => "󰨙",
        "Class" => "",
        "Color" => "",
        "Constant" => "󰏿",
        "Constructor" => "",
        "Enum" | "EnumMember" => "",
        "Event" => "",
        "Field" | "Property" => "",
        "File" | "FilePath" | "FileMatch" => "",
        "Folder" => "",
        "Function" | "Method" => "󰊕",
        "Interface" => "",
        "Key" | "Text" | "Value" => "",
        "Keyword" => "",
        "Module" | "Package" => "",
        "Namespace" => "󰦮",
        "Null" => "",
        "Number" => "󰎠",
        "Object" => "",
        "Operator" => "",
        "Reference" => "",
        "Snippet" => "󱄽",
        "String" => "",
        "Struct" => "󰆼",
        "TypeParameter" => "",
        "Unit" => "",
        "Variable" => "󰀫",
        "Buffer" => "󰓩",
        "Command" => "",
        "Theme" => "",
        "Match" | "Search" => "",
        "CodeAction" | "Action" => "",
        "Preferred" | "Proceed" | "Success" | "Added" | "Created" | "Staged" => "",
        "Warning" | "Warn" | "Modified" | "Permission" | "Amend" => "",
        "Error" | "Failed" | "Deleted" | "Conflict" | "Destructive" => "",
        "Cancel" | "Close" => "󰅖",
        "GitCommit" => "",
        "GitBranch" => "",
        "GitTag" => "",
        "GitStash" => "",
        "GitRemote" => "",
        "GitWorktree" => "",
        "Trait" => "",
        "Unknown" => "",
        _ => "",
    }
}

fn ascii_picker_kind_icon(kind: &str) -> &'static str {
    match kind {
        "File" | "FilePath" | "FileMatch" => "F",
        "Folder" => "D",
        "Buffer" => "B",
        "Command" => ">",
        "Theme" => "T",
        "Match" | "Search" => "/",
        "Reference" => "->",
        "Function" | "Method" => "fn",
        "Class" => "C",
        "Interface" | "Trait" => "I",
        "Struct" => "S",
        "Enum" | "EnumMember" => "E",
        "Module" | "Namespace" | "Package" => "M",
        "Variable" | "Field" | "Property" => "v",
        "Constant" => "c",
        "TypeParameter" => "T",
        "CodeAction" | "Action" => "*",
        "Preferred" | "Proceed" | "Success" | "Added" | "Created" | "Staged" => "+",
        "Warning" | "Warn" | "Modified" | "Permission" | "Amend" => "!",
        "Error" | "Failed" | "Deleted" | "Conflict" | "Destructive" => "x",
        "Cancel" | "Close" => "x",
        "GitCommit" => "o",
        "GitBranch" => "b",
        "GitTag" => "t",
        "GitStash" => "s",
        "GitRemote" => "r",
        "GitWorktree" => "w",
        "Unknown" => "?",
        _ => "",
    }
}

fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn preview_lines(text: &str, start_line: usize, max_lines: usize) -> Vec<PreviewLine<'_>> {
    let line_starts = preview_line_starts(text);
    preview_lines_with_starts(text, &line_starts, start_line, max_lines)
}

fn preview_line_starts(text: &str) -> Vec<usize> {
    if text.is_empty() {
        return Vec::new();
    }

    std::iter::once(0)
        .chain(
            text.match_indices('\n')
                .map(|(index, _)| index + 1)
                .filter(|index| *index < text.len()),
        )
        .collect()
}

fn preview_lines_with_starts<'a>(
    text: &'a str,
    line_starts: &[usize],
    start_line: usize,
    max_lines: usize,
) -> Vec<PreviewLine<'a>> {
    line_starts
        .iter()
        .copied()
        .enumerate()
        .skip(start_line)
        .take(max_lines)
        .map(|(line_index, start)| {
            let end = line_starts
                .get(line_index + 1)
                .copied()
                .unwrap_or(text.len());
            let line = text[start..end]
                .strip_suffix('\n')
                .unwrap_or(&text[start..end]);
            let line = line.strip_suffix('\r').unwrap_or(line);
            PreviewLine {
                text: line,
                start,
                end: start + line.len(),
            }
        })
        .collect()
}

fn location_preview_start(
    focus_line: Option<usize>,
    preview_scroll: isize,
    preview_height: usize,
    line_count: Option<usize>,
) -> usize {
    let max_start = line_count.map(|line_count| line_count.saturating_sub(preview_height));
    let centered = focus_line
        .unwrap_or_default()
        .saturating_sub(preview_height / 2);
    let centered = max_start.map_or(centered, |max_start| centered.min(max_start));
    let start = centered.saturating_add_signed(preview_scroll);
    max_start.map_or(start, |max_start| start.min(max_start))
}

fn read_location_preview(
    path: &str,
    complete: bool,
    focus_line: Option<usize>,
    preview_scroll: isize,
    preview_height: usize,
    checkpoint: Option<(usize, u64)>,
) -> io::Result<(String, usize, u64)> {
    if complete {
        let mut bytes = Vec::with_capacity(MAX_UNFOCUSED_PREVIEW_BYTES as usize);
        open_location_preview(path)?
            .take(MAX_UNFOCUSED_PREVIEW_BYTES)
            .read_to_end(&mut bytes)?;
        return String::from_utf8(bytes)
            .map(|text| (text, 0, 0))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    }

    let requested_start = location_preview_start(
        focus_line,
        preview_scroll,
        preview_height,
        /*line_count*/ None,
    );
    let scan_until = focus_line.map_or(0, |line| {
        line.saturating_add(preview_height / 2).saturating_add(1)
    });
    let mut scan_remaining = MAX_LOCATION_PREVIEW_SCAN_BYTES;
    let (text, line_count, source_offset) = read_location_window(
        path,
        requested_start,
        preview_height,
        scan_until,
        checkpoint,
        &mut scan_remaining,
    )?;
    let Some(line_count) = line_count else {
        return Ok((text, requested_start, source_offset));
    };
    let actual_start =
        location_preview_start(focus_line, preview_scroll, preview_height, Some(line_count));
    if actual_start == requested_start {
        return Ok((text, actual_start, source_offset));
    }
    // The corrected window reopens from the nearest checkpoint and needs a fresh scan budget.
    let mut scan_remaining = MAX_LOCATION_PREVIEW_SCAN_BYTES;
    read_location_window(
        path,
        actual_start,
        preview_height,
        /*scan_until*/ 0,
        checkpoint.filter(|(line, _)| *line <= actual_start),
        &mut scan_remaining,
    )
    .map(|(text, _, source_offset)| (text, actual_start, source_offset))
}

fn read_location_window(
    path: &str,
    start_line: usize,
    preview_height: usize,
    scan_until: usize,
    checkpoint: Option<(usize, u64)>,
    scan_remaining: &mut usize,
) -> io::Result<(String, Option<usize>, u64)> {
    let mut reader = BufReader::new(open_location_preview(path)?);
    let (checkpoint_line, checkpoint_offset) = checkpoint
        .filter(|(line, _)| *line <= start_line)
        .unwrap_or((0, 0));
    reader.seek(SeekFrom::Start(checkpoint_offset))?;
    let skipped = checkpoint_line
        + skip_location_lines(&mut reader, start_line - checkpoint_line, scan_remaining)?;
    if skipped < start_line {
        return Ok((String::new(), Some(skipped), reader.stream_position()?));
    }
    let source_offset = reader.stream_position()?;

    let max_lines = preview_height
        .max(1)
        .min(MAX_UNFOCUSED_PREVIEW_BYTES as usize);
    let max_line_bytes = (MAX_UNFOCUSED_PREVIEW_BYTES as usize / max_lines).saturating_sub(1);
    let mut text = String::new();
    let mut line_count = skipped;
    for _ in 0..max_lines {
        let Some(line) = read_location_line(&mut reader, max_line_bytes, scan_remaining)? else {
            return Ok((text, Some(line_count), source_offset));
        };
        text.push_str(&line);
        line_count += 1;
    }

    let remaining = scan_until.saturating_sub(line_count);
    if remaining == 0 {
        return Ok((text, None, source_offset));
    }
    let scanned = skip_location_lines(&mut reader, remaining, scan_remaining)?;
    if scanned < remaining {
        return Ok((text, Some(line_count + scanned), source_offset));
    }
    Ok((text, None, source_offset))
}

fn open_location_preview(path: &str) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(nix::libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("location preview target is not a regular file: {path}"),
        ));
    }
    Ok(file)
}

fn location_preview_scan_limit() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("location preview scan exceeds {MAX_LOCATION_PREVIEW_SCAN_BYTES} bytes"),
    )
}

fn skip_location_lines(
    reader: &mut BufReader<std::fs::File>,
    target: usize,
    scan_remaining: &mut usize,
) -> io::Result<usize> {
    let mut skipped = 0;
    let mut partial = false;
    while skipped < target {
        if *scan_remaining == 0 {
            return Err(location_preview_scan_limit());
        }
        let (consumed, lines, ends_line) = {
            let bytes = reader.fill_buf()?;
            if bytes.is_empty() {
                return Ok(skipped + usize::from(partial));
            }
            let bytes = &bytes[..bytes.len().min(*scan_remaining)];
            let mut lines = 0;
            let mut consumed = bytes.len();
            for (index, byte) in bytes.iter().enumerate() {
                if *byte == b'\n' {
                    lines += 1;
                    if skipped + lines == target {
                        consumed = index + 1;
                        break;
                    }
                }
            }
            (consumed, lines, bytes[consumed - 1] == b'\n')
        };
        reader.consume(consumed);
        *scan_remaining -= consumed;
        skipped += lines;
        partial = !ends_line;
    }
    Ok(skipped)
}

fn read_location_line(
    reader: &mut BufReader<std::fs::File>,
    max_line_bytes: usize,
    scan_remaining: &mut usize,
) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut has_bytes = false;
    loop {
        if *scan_remaining == 0 {
            return Err(location_preview_scan_limit());
        }
        let (consumed, line_end, copy) = {
            let bytes = reader.fill_buf()?;
            if bytes.is_empty() {
                return has_bytes.then(|| valid_preview_text(line)).transpose();
            }
            let bytes = &bytes[..bytes.len().min(*scan_remaining)];
            let line_end = bytes.iter().position(|byte| *byte == b'\n');
            let content_end = line_end.unwrap_or(bytes.len());
            let copy = content_end.min(max_line_bytes.saturating_sub(line.len()));
            (line_end.map_or(bytes.len(), |end| end + 1), line_end, copy)
        };
        if copy > 0 {
            line.extend_from_slice(&reader.buffer()[..copy]);
        }
        reader.consume(consumed);
        *scan_remaining -= consumed;
        has_bytes = true;
        if line_end.is_some() {
            let mut line = valid_preview_text(line)?;
            line.push('\n');
            return Ok(Some(line));
        }
    }
}

fn valid_preview_text(bytes: Vec<u8>) -> io::Result<String> {
    match std::str::from_utf8(&bytes) {
        Ok(_) => String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
                .map(str::to_string)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        }
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
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

    fn set_theme(&mut self, theme: &Theme) {
        self.apply_theme(theme);
    }

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        self.sync_list_bounds();
        match ev {
            Event::Paste(text) => {
                self.reset_history_navigation();
                let previous = self.selected_item();
                let pasted = text
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .split('\n')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                self.set_search(format!("{}{}", self.search, pasted));
                self.changed_actions(previous)
            }
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
                        if self.dynamic_items.is_some() && self.has_preview() {
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
                        if self.dynamic_items.is_some() && self.has_preview() {
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
                    KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.notify_cancelled()
                    }
                    KeyCode::Esc => self.notify_cancelled(),
                    KeyCode::Backspace => {
                        self.reset_history_navigation();
                        let previous = self.selected_item();
                        if let Some((start, _)) = self.search.grapheme_indices(true).next_back() {
                            self.search.truncate(start);
                        }
                        let search = self.search.clone();
                        self.filter(&search);
                        self.changed_actions(previous)
                    }
                    KeyCode::Enter => {
                        if self.list.is_empty() {
                            return None;
                        }
                        let action = if let Some(select_action) = &self.select_action {
                            let item = self
                                .selected_dynamic_item()
                                .map_or_else(|| self.list.selected_item(), |item| item.id.clone());
                            select_action(item)
                        } else if self.dynamic_items.is_some() {
                            Action::NotifyPlugins(
                                format!("picker:selected:{}", self.id.unwrap_or_default()),
                                self.selected_value().unwrap_or(Value::Null),
                            )
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
                    KeyCode::Char(c)
                        if !event
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.reset_history_navigation();
                        let previous = self.selected_item();
                        let search = format!("{}{c}", self.search);
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
        } else if self.has_preview() {
            self.draw_legacy_items_with_preview(buffer, layout.results);
        } else {
            self.draw_plain_items(buffer, layout.results);
        }
        if self.list.is_empty() {
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
            self.draw_preview(buffer, preview.as_ref(), preview_layout)?;
        }

        Ok(())
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let query_width = self.width.saturating_sub(2);
        let visible_query = display_width_tail(&self.search, query_width);
        let cx = self.x + 3 + display_width(visible_query).min(query_width.saturating_sub(1));
        let cy = self.layout().query_y;

        Some((cx, cy))
    }
}

fn display_width_tail(text: &str, max_width: usize) -> &str {
    let mut width = 0;
    let mut start = text.len();
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        let grapheme_width = display_width(grapheme);
        if width + grapheme_width > max_width {
            break;
        }
        width += grapheme_width;
        start = index;
    }
    &text[start..]
}

impl Picker {
    fn has_preview(&self) -> bool {
        self.preview.is_some()
            || self
                .selected_dynamic_item()
                .is_some_and(|item| item.preview.is_some())
            || (self.item_preview_root.is_some() && self.list.selected_index().is_some())
    }

    fn current_preview(&self) -> Option<Cow<'_, PickerPreview>> {
        if let Some(preview) = self.preview.as_ref() {
            return Some(Cow::Borrowed(preview));
        }
        if let Some(preview) = self
            .selected_dynamic_item()
            .and_then(|item| item.preview.as_ref())
        {
            return Some(Cow::Borrowed(preview));
        }

        let root = self.item_preview_root.as_ref()?;
        let selected = self.list.selected_index()?;
        let item = self.list.items().get(selected)?;
        Some(Cow::Owned(PickerPreview::Location {
            path: root.join(item).to_string_lossy().into_owned(),
            line: None,
            column: None,
            matches: Vec::new(),
        }))
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
    structured_items: Option<Vec<PickerItem>>,
    id: Option<i32>,
    select_action: Option<SelectAction>,
    filter_action: Option<FilterAction>,
    placeholder: Option<String>,
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
            structured_items: None,
            id: None,
            select_action: None,
            filter_action: None,
            placeholder: None,
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

    /// Supplies precomputed picker rows with stable IDs and structured display fields.
    pub fn structured_items(mut self, items: Vec<PickerItem>) -> Self {
        self.structured_items = Some(items);
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

    /// Sets the scorer used to filter structured picker rows for the current query.
    pub fn filter_action(
        mut self,
        filter: impl Fn(&PickerItem, &str) -> Option<i64> + Send + 'static,
    ) -> Self {
        self.filter_action = Some(Box::new(filter));
        self
    }

    /// Sets the prompt hint shown while the picker query is empty.
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    pub fn history_key(mut self, key: impl Into<String>) -> Self {
        self.history_key = Some(key.into());
        self
    }

    pub fn build(self, editor: &Editor) -> Picker {
        let title = self.title;
        let structured_items = self.structured_items;
        let items = if structured_items.is_some() {
            Vec::new()
        } else {
            self.items
        };
        let id = self.id;
        let select_action = self.select_action;
        let filter_action = self.filter_action;
        let placeholder = self.placeholder;
        let history_key = self.history_key;

        let mut picker = Picker::new(title, editor, &items, id);
        if let Some(structured_items) = structured_items {
            picker.visible_dynamic_items = (0..structured_items.len()).collect();
            picker.list.set_item_count(structured_items.len());
            picker.dynamic_items = Some(structured_items);
        }
        if let Some(select_action) = select_action {
            picker.select_action = Some(select_action);
        }
        picker.filter_action = filter_action;
        picker.placeholder = placeholder;
        if let Some(history_key) = history_key {
            let history = editor.picker_history(&history_key).to_vec();
            picker.set_history(history_key, history);
        }

        picker
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    use super::{picker_file_icon, picker_file_icon_color};
    use crate::{
        buffer::Buffer,
        color::{contrast_ratio, Color},
        config::{Config, KeyAction, PickerIconStyle, PickerInputPosition},
        editor::{Action, Editor, RenderBuffer},
        lsp::LspManager,
        theme::{SelectionForegroundPriority, Style, Theme, TokenStyle},
        ui::{
            Component, LegacyPickerOptions, Picker, PickerIcon, PickerItem, PickerOptions,
            PickerPresentation, PickerPreview, PickerUpdate,
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

    fn display_column(row: &str, needle: &str) -> Option<usize> {
        row.find(needle).map(|index| display_width(&row[..index]))
    }

    fn dynamic_item(id: &str, label: &str) -> PickerItem {
        PickerItem {
            id: id.to_string(),
            icon: None,
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
    fn paste_updates_picker_query_once_without_accepting_newline() {
        let editor = test_editor();
        let items = vec!["alpha".to_string(), "bravo".to_string()];
        let mut picker = Picker::new(Some("Items".to_string()), &editor, &items, None);

        picker.handle_event(&Event::Paste("alp\r\nbravo".to_string()));

        assert_eq!(picker.search, "alp");
        assert_eq!(picker.list.items(), &vec!["alpha".to_string()]);
    }

    #[test]
    fn backspace_removes_one_complete_grapheme_from_picker_query() {
        let editor = test_editor();
        let mut picker = Picker::new(Some("Items".to_string()), &editor, &[], None);
        picker.search = "prefix👨‍👩‍👧".to_string();

        picker.handle_event(&key(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(picker.search, "prefix");
    }

    #[test]
    fn control_c_cancels_and_unhandled_alt_keys_do_not_append_to_picker_query() {
        let editor = test_editor();
        let mut picker = Picker::new(Some("Items".to_string()), &editor, &[], None);
        picker.search = "prefix".to_string();

        assert_eq!(
            picker.handle_event(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(KeyAction::Single(Action::CloseDialog))
        );
        assert_eq!(
            picker.handle_event(&key(KeyCode::Char('x'), KeyModifiers::ALT)),
            None
        );

        assert_eq!(picker.search, "prefix");
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
    fn compact_picker_long_query_keeps_tail_cursor_and_right_border_visible() {
        let editor = test_editor();
        let mut picker = Picker::new_dynamic(
            Some("Items".to_string()),
            &editor,
            vec![dynamic_item("item", "item")],
            22,
            PickerOptions {
                initial_query: "prefix-0123456789-ABCDEFGHIJ-漢字-TAIL".to_string(),
                presentation: PickerPresentation::Compact,
                ..PickerOptions::default()
            },
        );
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let query_y = picker.layout().query_y;
        let row = render_row(&buffer, query_y);
        assert!(row.contains("TAIL"), "query row was: {row:?}");
        assert_eq!(row.chars().last(), Some('│'));
        let (cursor_x, cursor_y) = picker.cursor_position().unwrap();
        assert_eq!(cursor_y, query_y);
        assert!(cursor_x < 80);
        assert!(cursor_x <= picker.x + picker.width);

        assert!(picker.resize(48, 14));
        let mut buffer = RenderBuffer::new(48, 14, &Style::default());
        picker.draw(&mut buffer).unwrap();
        let row = render_row(&buffer, picker.layout().query_y);
        assert!(row.contains("TAIL"), "resized query row was: {row:?}");
        assert_eq!(row.chars().last(), Some('│'));
        assert!(picker.cursor_position().unwrap().0 < 48);
    }

    #[test]
    fn compact_picker_reserves_border_space_in_tiny_viewports() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 32, 10);
        let picker = Picker::new_dynamic(
            Some("Items".to_string()),
            &editor,
            vec![dynamic_item("item", "item")],
            23,
            PickerOptions {
                presentation: PickerPresentation::Compact,
                ..PickerOptions::default()
            },
        );
        let mut buffer = RenderBuffer::new(32, 10, &Style::default());

        picker.draw(&mut buffer).unwrap();

        assert!(picker.x + picker.width + 2 <= 32);
        assert!(picker.y + picker.height < 10);
        let top = render_row(&buffer, picker.y);
        let bottom = render_row(&buffer, picker.y + picker.height);
        assert_eq!(top.chars().last(), Some('┐'));
        assert_eq!(bottom.chars().last(), Some('┘'));
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
            icon: None,
            label: "src/main.rs".to_string(),
            kind: Some("File".to_string()),
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
            vec![dynamic_item("selected", "selected"), item],
            /*id*/ 16,
            PickerOptions::default(),
        );
        let mut buffer =
            RenderBuffer::new(/*width*/ 120, /*height*/ 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let selected_row_start = (picker.y + 1) * buffer.width + picker.x + 5;
        let row_start = (picker.y + 2) * buffer.width + picker.x + 5;
        let icon_start = (picker.y + 2) * buffer.width + picker.x + 3;
        let annotation_start = row_start + "src/main.rs ".len();
        let detail_start =
            (picker.y + 2) * buffer.width + picker.x + picker.width + 1 - "let needle = 1".len();
        assert_eq!(buffer.cells[icon_start].style.fg, Some(file_color));
        assert_eq!(
            buffer.cells[row_start].style.fg,
            theme.ui_style.picker_item.fg
        );
        assert_eq!(
            buffer.cells[annotation_start].style.fg,
            Some(location_color)
        );
        assert_eq!(buffer.cells[detail_start].style.fg, Some(content_color));
        let selected_bg = buffer.cells[selected_row_start].style.bg.unwrap();
        let surface_bg = theme.ui_style.picker_item.bg.unwrap();
        assert!(contrast_ratio(selected_bg, surface_bg) >= 3.0);
        assert_ne!(selected_bg, selection_color);
        assert_eq!(buffer.cells[detail_start + 4].style.bg, Some(match_color));
    }

    #[test]
    fn dynamic_picker_preserves_label_before_truncating_annotation() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 100, 30);
        let mut item = dynamic_item("macchiato", "Catppuccin Macchiato");
        item.annotation = Some("catppuccin-macchiato.json".to_string());
        item.detail = Some("embedded".to_string());
        let picker = Picker::new_dynamic(
            Some("Themes".to_string()),
            &editor,
            vec![item],
            17,
            PickerOptions {
                presentation: PickerPresentation::Compact,
                ..PickerOptions::default()
            },
        );
        let mut buffer = RenderBuffer::new(100, 30, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let row = render_row(&buffer, picker.y + 1);
        assert!(row.contains("Catppuccin Macchiato "));
        assert!(!row.contains("Catppuccin Macchiatocatppuccin"));
        assert!(!row.contains("catppuccin-macchiato.json"));
        assert!(row.contains("embedded"));
    }

    #[test]
    fn file_match_rows_keep_the_filename_visible_before_match_text() {
        let editor = test_editor();
        let mut item = dynamic_item("match", "default_config.toml");
        item.kind = Some("FileMatch".to_string());
        item.annotation = Some("crates/config/:10:2".to_string());
        item.detail = Some(
            "# Name of the VSCode theme to use. The theme file should remain readable".to_string(),
        );
        item.data = json!({
            "location": {
                "path": "crates/config/default_config.toml"
            }
        });
        let picker = Picker::new_dynamic(
            Some("Find in Files".to_string()),
            &editor,
            vec![item],
            20,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let row = render_row(&buffer, picker.y + 1);
        assert!(row.contains("default_config.toml"), "{row:?}");
        assert!(row.contains("# Name of"), "{row:?}");
    }

    #[test]
    fn structured_command_picker_aligns_fields_filters_noise_and_selects_by_id() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 120, 24);
        let items = vec![
            PickerItem {
                id: "git.open".to_string(),
                icon: None,
                label: "Open Git dashboard".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("Git   ".to_string()),
                detail: Some("Space G    :GitDashboard".to_string()),
                data: json!({
                    "description": "Inspect workspace changes",
                    "aliases": ["source control"],
                    "shortcuts": ["Space G"],
                    "primary_shortcut": "Space G",
                    "colon": ":GitDashboard",
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
            PickerItem {
                id: "other.open".to_string(),
                icon: None,
                label: "Open dashboard".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("Other ".to_string()),
                detail: Some("           :Other".to_string()),
                data: json!({
                    "description": "Get information together",
                    "aliases": [],
                    "shortcuts": [],
                    "primary_shortcut": "",
                    "colon": ":Other",
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
            PickerItem {
                id: "tree.toggle".to_string(),
                icon: None,
                label: "Toggle file tree".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("File  ".to_string()),
                detail: Some("Ctrl-e".to_string()),
                data: json!({
                    "description": "Show or hide the workspace file tree",
                    "aliases": [],
                    "shortcuts": ["Ctrl-e"],
                    "primary_shortcut": "Ctrl-e",
                    "colon": null,
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
        ];
        let mut picker = Picker::builder()
            .title("Commands")
            .structured_items(items)
            .filter_action(crate::command_palette::filter_score)
            .select_action(Action::Print)
            .placeholder("Type a command")
            .build(&editor);
        let mut buffer = RenderBuffer::new(120, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();
        let first = render_row(&buffer, picker.y + 1);
        let second = render_row(&buffer, picker.y + 2);
        let third = render_row(&buffer, picker.y + 3);
        assert!(first.contains("Git    Open Git dashboard"), "{first:?}");
        assert!(second.contains("Other  Open dashboard"), "{second:?}");
        assert!(third.contains("File   Toggle file tree"), "{third:?}");
        assert!(first.contains("Space G  :GitDashboard"), "{first:?}");
        assert_eq!(
            display_column(&first, ":GitDashboard"),
            display_column(&second, ":Other")
        );
        assert_eq!(
            display_column(&first, "Space G"),
            display_column(&third, "Ctrl-e")
        );
        assert!(render_row(&buffer, picker.layout().separator_y)
            .contains("Inspect workspace changes · 3/3 commands"));

        picker.set_search("other".to_string());
        picker.draw(&mut buffer).unwrap();
        let other = render_row(&buffer, picker.y + 1);
        assert!(other.contains("Other  Open dashboard  :Other"), "{other:?}");

        picker.set_search("git".to_string());
        picker.draw(&mut buffer).unwrap();

        assert_eq!(picker.visible_dynamic_items.len(), 1);
        assert!(render_row(&buffer, picker.y + 1).contains("Open Git dashboard"));
        assert!(!render_row(&buffer, picker.y + 2).contains("Open dashboard"));
        assert_eq!(
            select(&mut picker),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::Print("git.open".to_string()),
            ]))
        );
    }

    #[test]
    fn structured_command_picker_drops_colon_commands_before_truncating_actions() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 80, 24);
        let items = vec![
            PickerItem {
                id: "agent.cancel".to_string(),
                icon: None,
                label: "Cancel agent request".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("Agent ".to_string()),
                detail: Some("Space A         :AgentCancel".to_string()),
                data: json!({
                    "shortcuts": ["Space A"],
                    "primary_shortcut": "Space A",
                    "colon": ":AgentCancel",
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
            PickerItem {
                id: "buffer.next".to_string(),
                icon: None,
                label: "Next buffer".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("Buffer".to_string()),
                detail: Some("Space Space +1  :bn".to_string()),
                data: json!({
                    "shortcuts": ["Space Space", "Space n"],
                    "primary_shortcut": "Space Space +1",
                    "colon": ":bn",
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
            PickerItem {
                id: "edit.join-preserve".to_string(),
                icon: None,
                label: "Join lines without trimming".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("Edit  ".to_string()),
                detail: Some("g J             :join!".to_string()),
                data: json!({
                    "shortcuts": ["g J"],
                    "primary_shortcut": "g J",
                    "colon": ":join!",
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
        ];
        let picker = Picker::builder()
            .title("Commands")
            .structured_items(items)
            .build(&editor);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let first = render_row(&buffer, picker.y + 1);
        let second = render_row(&buffer, picker.y + 2);
        let third = render_row(&buffer, picker.y + 3);
        assert!(first.contains("Agent   Cancel agent request"), "{first:?}");
        assert!(second.contains("Buffer  Next buffer"), "{second:?}");
        assert!(
            third.contains("Edit    Join lines without trimming"),
            "{third:?}"
        );
        assert!(first.contains("Space A"), "{first:?}");
        assert!(second.contains("Space Space +1"), "{second:?}");
        assert!(third.contains("g J"), "{third:?}");
        assert_eq!(
            display_column(&first, "Space A"),
            display_column(&second, "Space Space +1")
        );
        assert_eq!(
            display_column(&first, "Space A"),
            display_column(&third, "g J")
        );
        assert!(!first.contains(":AgentCancel"), "{first:?}");
        assert!(!second.contains(":bn"), "{second:?}");
        assert!(!third.contains(":join!"), "{third:?}");
    }

    #[test]
    fn structured_command_picker_hides_shortcuts_when_actions_need_the_space() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 64, 24);
        let items = vec![
            PickerItem {
                id: "buffer.next".to_string(),
                icon: None,
                label: "Next buffer".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("Buffer".to_string()),
                detail: Some("Space Space +1  :bn".to_string()),
                data: json!({
                    "shortcuts": ["Space Space", "Space n"],
                    "primary_shortcut": "Space Space +1",
                    "colon": ":bn",
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
            PickerItem {
                id: "edit.join-preserve".to_string(),
                icon: None,
                label: "Join lines without trimming".to_string(),
                kind: Some("Command".to_string()),
                annotation: Some("Edit  ".to_string()),
                detail: Some("g J             :join!".to_string()),
                data: json!({
                    "shortcuts": ["g J"],
                    "primary_shortcut": "g J",
                    "colon": ":join!",
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            },
        ];
        let picker = Picker::builder()
            .title("Commands")
            .structured_items(items)
            .build(&editor);
        let mut buffer = RenderBuffer::new(64, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let first = render_row(&buffer, picker.y + 1);
        let second = render_row(&buffer, picker.y + 2);
        assert!(first.contains("Buffer  Next buffer"), "{first:?}");
        assert!(
            second.contains("Edit    Join lines without trimming"),
            "{second:?}"
        );
        assert!(!first.contains("Space Space +1"), "{first:?}");
        assert!(!second.contains("g J"), "{second:?}");
        assert!(!first.contains(":bn"), "{first:?}");
        assert!(!second.contains(":join!"), "{second:?}");
    }

    #[test]
    fn picker_theme_update_restyles_open_picker_without_losing_query() {
        let editor = test_editor();
        let mut picker = Picker::new_dynamic(
            Some("Themes".to_string()),
            &editor,
            vec![dynamic_item("theme", "Lackluster")],
            18,
            PickerOptions::default(),
        );
        picker.search = "lack".to_string();
        let popup_bg = Color::Rgb { r: 1, g: 2, b: 3 };
        let border_fg = Color::Rgb { r: 4, g: 5, b: 6 };
        let prompt_fg = Color::Rgb { r: 7, g: 8, b: 9 };
        let mut theme = Theme::default();
        theme.ui_style.popup.bg = Some(popup_bg);
        theme.ui_style.popup_border.fg = Some(border_fg);
        theme.ui_style.picker_prompt.fg = Some(prompt_fg);

        picker.set_theme(&theme);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());
        picker.draw(&mut buffer).unwrap();

        let border = &buffer.cells[picker.y * buffer.width + picker.x];
        let prompt = &buffer.cells[picker.layout().query_y * buffer.width + picker.x + 3];
        assert_eq!(picker.search, "lack");
        assert_eq!(border.style.fg, Some(border_fg));
        assert_eq!(prompt.style.fg, Some(prompt_fg));
        assert_eq!(picker.dialog.style.bg, Some(popup_bg));
    }

    #[test]
    fn dynamic_picker_uses_symbol_kind_theme_scope_for_icon() {
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
        let mut item = dynamic_item("render", "render");
        item.kind = Some("Function".to_string());
        let picker = Picker::new_dynamic(
            Some("Workspace Symbols".to_string()),
            &editor,
            vec![dynamic_item("selected", "selected"), item],
            18,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let icon_start = (picker.y + 2) * buffer.width + picker.x + 3;
        let label_start = (picker.y + 2) * buffer.width + picker.x + 5;
        assert_eq!(buffer.cells[icon_start].text, "󰊕");
        assert_eq!(buffer.cells[icon_start].style.fg, Some(function_color));
        assert_eq!(
            buffer.cells[label_start].style.fg,
            editor.theme.ui_style.picker_item.fg
        );
    }

    #[test]
    fn picker_icon_style_switches_between_unicode_ascii_and_hidden() {
        for (style, expected, unexpected) in [
            (PickerIconStyle::Unicode, "λ", "fn"),
            (PickerIconStyle::Ascii, "fn", "λ"),
            (PickerIconStyle::None, "render", "λ"),
        ] {
            let mut config = Config::default();
            config.picker.icons.style = style;
            let editor = test_editor_with_config_and_size(config, Theme::default(), 80, 24);
            let mut item = dynamic_item("render", "render");
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

            let row = render_row(&buffer, picker.y + 1);
            assert!(row.contains(expected), "{style:?}: {row:?}");
            assert!(!row.contains(unexpected), "{style:?}: {row:?}");
        }
    }

    #[test]
    fn file_icons_follow_extension_and_configured_icon_style() {
        assert_eq!(
            picker_file_icon("crates/husk/src/lib.rs", PickerIconStyle::Unicode),
            "ⓡ"
        );
        assert_eq!(
            picker_file_icon("default_config.toml", PickerIconStyle::NerdFont),
            ""
        );
        assert_eq!(
            picker_file_icon("Cargo.lock", PickerIconStyle::NerdFont),
            ""
        );
        assert_eq!(
            picker_file_icon("README.md", PickerIconStyle::NerdFont),
            "󰂺"
        );
        assert_eq!(
            picker_file_icon("AGENTS.md", PickerIconStyle::NerdFont),
            ""
        );
        assert_eq!(picker_file_icon("LICENSE", PickerIconStyle::NerdFont), "");
        assert_eq!(
            picker_file_icon("src/plugin.hk", PickerIconStyle::NerdFont),
            "󰈔"
        );
        assert_eq!(
            picker_file_icon("docs/README.md", PickerIconStyle::Ascii),
            "MD"
        );
        assert_eq!(picker_file_icon("LICENSE", PickerIconStyle::Unicode), "▤");
        assert_eq!(
            picker_file_icon_color("src/lib.rs"),
            Some(Color::Rgb {
                r: 222,
                g: 165,
                b: 132,
            })
        );
    }

    #[test]
    fn explicit_picker_icon_uses_its_role_without_coloring_the_label() {
        let error_color = Color::Rgb {
            r: 220,
            g: 40,
            b: 50,
        };
        let mut theme = Theme::default();
        theme
            .colors
            .insert("editorError.foreground".to_string(), error_color);
        let editor = test_editor_with_theme(theme.clone());
        let mut item = dynamic_item("danger", "Discard changes");
        item.icon = Some(PickerIcon::Styled {
            text: "−".to_string(),
            role: Some("error".to_string()),
        });
        let picker = Picker::new_dynamic(
            Some("Confirm".to_string()),
            &editor,
            vec![dynamic_item("selected", "Cancel"), item],
            19,
            PickerOptions::default(),
        );
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        let icon_start = (picker.y + 2) * buffer.width + picker.x + 3;
        let label_start = (picker.y + 2) * buffer.width + picker.x + 5;
        assert_eq!(buffer.cells[icon_start].text, "−");
        assert_eq!(buffer.cells[icon_start].style.fg, Some(error_color));
        assert_eq!(
            buffer.cells[label_start].style.fg,
            theme.ui_style.picker_item.fg
        );
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
    fn location_preview_reuses_cache_and_invalidates_changed_files() {
        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let path = std::env::temp_dir().join(format!(
            "red-picker-preview-cache-{}.txt",
            uuid::Uuid::new_v4()
        ));
        let path_text = path.to_string_lossy();
        std::fs::write(&path, "first preview").unwrap();

        let first = picker.location_preview(
            &path_text, /*focus_line*/ None, /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        let cached = picker.location_preview(
            &path_text, /*focus_line*/ None, /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert!(Arc::ptr_eq(&first, &cached));

        std::fs::write(&path, "updated preview text").unwrap();
        let updated = picker.location_preview(
            &path_text, /*focus_line*/ None, /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert!(!Arc::ptr_eq(&first, &updated));
        assert_eq!(updated.text.as_ref(), "updated preview text");

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn complete_location_preview_rejects_an_incomplete_trailing_utf8_sequence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("invalid-utf8.txt");
        let mut contents = vec![b'x'; super::MAX_UNFOCUSED_PREVIEW_BYTES as usize - 1];
        contents.push(0xc3);
        std::fs::write(&path, contents).unwrap();

        let error = super::read_location_preview(
            &path.to_string_lossy(),
            /*complete*/ true,
            /*focus_line*/ None,
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
            /*checkpoint*/ None,
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn location_preview_keeps_recent_files_and_evicts_the_oldest() {
        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let root =
            std::env::temp_dir().join(format!("red-picker-preview-lru-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = (0..=super::LOCATION_PREVIEW_CACHE_CAPACITY)
            .map(|index| root.join(format!("preview-{index}.txt")))
            .collect::<Vec<_>>();
        for (index, path) in paths.iter().enumerate() {
            std::fs::write(path, format!("preview {index}\nsecond line")).unwrap();
        }

        let first_path = paths[0].to_string_lossy();
        let first = picker.location_preview(
            &first_path,
            /*focus_line*/ None,
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert_eq!(first.line_starts, vec![0, "preview 0\n".len()]);
        for path in paths
            .iter()
            .skip(1)
            .take(super::LOCATION_PREVIEW_CACHE_CAPACITY - 1)
        {
            picker.location_preview(
                &path.to_string_lossy(),
                /*focus_line*/ None,
                /*preview_scroll*/ 0,
                /*preview_height*/ 8,
            );
        }
        let cached = picker.location_preview(
            &first_path,
            /*focus_line*/ None,
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert!(Arc::ptr_eq(&first, &cached));

        for path in paths.iter().skip(1) {
            picker.location_preview(
                &path.to_string_lossy(),
                /*focus_line*/ None,
                /*preview_scroll*/ 0,
                /*preview_height*/ 8,
            );
        }
        let evicted = picker.location_preview(
            &first_path,
            /*focus_line*/ None,
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert!(!Arc::ptr_eq(&first, &evicted));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn focused_location_preview_keeps_large_utf8_files_bounded() {
        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let path = std::env::temp_dir().join(format!(
            "red-picker-preview-prefix-{}.txt",
            uuid::Uuid::new_v4()
        ));
        let mut contents = "top line\n".to_string();
        contents.push_str(
            &"x".repeat(super::MAX_UNFOCUSED_PREVIEW_BYTES as usize - contents.len() - 1),
        );
        contents.push('é');
        contents.push_str("\nfocused tail\n");
        std::fs::write(&path, &contents).unwrap();
        let path_text = path.to_string_lossy();

        let prefix = picker.location_preview(
            &path_text, /*focus_line*/ None, /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert!(!prefix.complete);
        assert!(prefix.text.len() < super::MAX_UNFOCUSED_PREVIEW_BYTES as usize);
        assert!(prefix.text.starts_with("top line\n"));

        let focused = picker.location_preview(
            &path_text,
            /*focus_line*/ Some(2),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert!(!focused.complete);
        assert!(focused.text.len() <= super::MAX_UNFOCUSED_PREVIEW_BYTES as usize);
        assert!(std::str::from_utf8(focused.text.as_bytes()).is_ok());
        assert!(focused.text.contains("focused tail"));

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn scrolling_an_unfocused_location_preview_keeps_large_files_bounded() {
        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let path = std::env::temp_dir().join(format!(
            "red-picker-preview-scroll-{}.txt",
            uuid::Uuid::new_v4()
        ));
        let contents = format!(
            "{}\nscrolled tail\n",
            "x".repeat(super::MAX_UNFOCUSED_PREVIEW_BYTES as usize)
        );
        std::fs::write(&path, &contents).unwrap();
        let path_text = path.to_string_lossy();

        let prefix = picker.location_preview(
            &path_text, /*focus_line*/ None, /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        let scrolled = picker.location_preview(
            &path_text, /*focus_line*/ None, /*preview_scroll*/ 1,
            /*preview_height*/ 8,
        );

        assert!(!prefix.complete);
        assert!(!scrolled.complete);
        assert!(scrolled.text.len() <= super::MAX_UNFOCUSED_PREVIEW_BYTES as usize);
        assert!(scrolled.text.contains("scrolled tail"));
        assert_eq!(scrolled.first_line, 0);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn focused_location_preview_preserves_absolute_lines_scroll_and_cache_window() {
        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let path = std::env::temp_dir().join(format!(
            "red-picker-preview-window-{}.txt",
            uuid::Uuid::new_v4()
        ));
        let mut contents = "é".repeat(super::MAX_UNFOCUSED_PREVIEW_BYTES as usize / 2);
        contents.push('\n');
        for line in 1..=64 {
            contents.push_str(&format!("line {line}\n"));
        }
        std::fs::write(&path, contents).unwrap();
        let path_text = path.to_string_lossy();

        let focused = picker.location_preview(
            &path_text,
            /*focus_line*/ Some(40),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        let cached = picker.location_preview(
            &path_text,
            /*focus_line*/ Some(40),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        let scrolled = picker.location_preview(
            &path_text,
            /*focus_line*/ Some(40),
            /*preview_scroll*/ 8,
            /*preview_height*/ 8,
        );
        let near_end = picker.location_preview(
            &path_text,
            /*focus_line*/ Some(64),
            /*preview_scroll*/ -8,
            /*preview_height*/ 8,
        );
        let utf8_prefix = picker.location_preview(
            &path_text,
            /*focus_line*/ Some(0),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );

        assert!(Arc::ptr_eq(&focused, &cached));
        assert!(!Arc::ptr_eq(&focused, &scrolled));
        assert!(std::str::from_utf8(utf8_prefix.text.as_bytes()).is_ok());
        assert!(utf8_prefix.text.len() <= super::MAX_UNFOCUSED_PREVIEW_BYTES as usize);
        assert_eq!(focused.first_line, 36);
        assert_eq!(focused.text.lines().next(), Some("line 36"));
        assert!(focused.text.lines().any(|line| line == "line 40"));
        assert_eq!(scrolled.first_line, 44);
        assert_eq!(scrolled.text.lines().next(), Some("line 44"));
        assert_eq!(near_end.first_line, 49);
        assert_eq!(near_end.text.lines().next(), Some("line 49"));
        assert!(near_end.text.len() <= super::MAX_UNFOCUSED_PREVIEW_BYTES as usize);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn scrolling_a_large_location_preview_reuses_bounded_source_offsets() {
        const LINE_BYTES: usize = 1024;
        const LINE_COUNT: usize = 10 * 1024;
        const FOCUS_LINE: usize = 7 * 1024;

        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("offset-cache.txt");
        let mut contents = String::with_capacity(LINE_BYTES * LINE_COUNT);
        for line in 0..LINE_COUNT {
            let label = format!("line {line:05} ");
            contents.push_str(&label);
            contents.push_str(&"x".repeat(LINE_BYTES - label.len() - 1));
            contents.push('\n');
        }
        std::fs::write(&path, contents).unwrap();
        let path = path.to_string_lossy();

        let first = picker.location_preview(
            &path,
            /*focus_line*/ Some(FOCUS_LINE),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        let next = picker.location_preview(
            &path,
            /*focus_line*/ Some(FOCUS_LINE),
            /*preview_scroll*/ 1024,
            /*preview_height*/ 8,
        );
        let later = picker.location_preview(
            &path,
            /*focus_line*/ Some(FOCUS_LINE),
            /*preview_scroll*/ 2048,
            /*preview_height*/ 8,
        );
        let back = picker.location_preview(
            &path,
            /*focus_line*/ Some(FOCUS_LINE),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );

        assert_eq!(first.first_line, FOCUS_LINE - 4);
        assert_eq!(first.source_offset, (first.first_line * LINE_BYTES) as u64);
        assert_eq!(next.first_line, FOCUS_LINE + 1020);
        assert_eq!(next.source_offset, (next.first_line * LINE_BYTES) as u64);
        assert_eq!(later.first_line, FOCUS_LINE + 2044);
        assert_eq!(later.source_offset, (later.first_line * LINE_BYTES) as u64);
        assert!(later.text.starts_with("line 09212 "));
        assert!(!later.text.contains("location preview scan exceeds"));
        assert!(Arc::ptr_eq(&first, &back));
    }

    #[cfg(unix)]
    #[test]
    fn location_preview_rejects_fifos_without_blocking_and_keeps_regular_symlinks() {
        use nix::{sys::stat::Mode, unistd::mkfifo};
        use std::os::unix::fs::symlink;

        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("blocked.fifo");
        let fifo_link = root.path().join("blocked-link.fifo");
        let regular = root.path().join("regular.txt");
        let regular_link = root.path().join("regular-link.txt");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        symlink(&fifo, &fifo_link).unwrap();
        std::fs::write(&regular, "regular preview\n").unwrap();
        symlink(&regular, &regular_link).unwrap();

        for path in [&fifo, &fifo_link] {
            let preview = picker.location_preview(
                &path.to_string_lossy(),
                /*focus_line*/ Some(0),
                /*preview_scroll*/ 0,
                /*preview_height*/ 8,
            );
            assert!(preview.text.contains("not a regular file"));
        }

        let regular = picker.location_preview(
            &regular_link.to_string_lossy(),
            /*focus_line*/ Some(0),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );
        assert_eq!(regular.text.as_ref(), "regular preview\n");
    }

    #[test]
    fn location_preview_bounds_huge_lines_and_extreme_line_indexes() {
        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("huge-line.txt");
        std::fs::write(
            &path,
            "x".repeat(super::MAX_LOCATION_PREVIEW_SCAN_BYTES + 1),
        )
        .unwrap();
        let path = path.to_string_lossy();

        for focus_line in [0, usize::MAX] {
            let preview = picker.location_preview(
                &path,
                Some(focus_line),
                /*preview_scroll*/ 0,
                /*preview_height*/ 8,
            );
            assert!(preview.text.contains("location preview scan exceeds"));
            assert!(preview.text.contains("8388608 bytes"));
        }
    }

    #[test]
    fn location_preview_retries_a_bounded_near_eof_window_with_a_fresh_scan_budget() {
        const LINE_BYTES: usize = 1024;
        const LINE_COUNT: usize = 5 * 1024;

        let editor = test_editor();
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("near-eof.txt");
        let mut contents = String::with_capacity(LINE_BYTES * LINE_COUNT);
        for line in 0..LINE_COUNT {
            let label = format!("line {line:04} ");
            contents.push_str(&label);
            contents.push_str(&"x".repeat(LINE_BYTES - label.len() - 1));
            contents.push('\n');
        }
        assert_eq!(contents.len(), 5 * 1024 * 1024);
        std::fs::write(&path, contents).unwrap();

        let preview = picker.location_preview(
            &path.to_string_lossy(),
            /*focus_line*/ Some(LINE_COUNT - 1),
            /*preview_scroll*/ 0,
            /*preview_height*/ 8,
        );

        assert_eq!(preview.first_line, LINE_COUNT - 8);
        assert!(preview.text.starts_with("line 5112 "));
        assert!(preview
            .text
            .lines()
            .any(|line| line.starts_with("line 5119 ")));
        assert!(!preview.text.contains("location preview scan exceeds"));
        assert!(preview.text.len() <= super::MAX_UNFOCUSED_PREVIEW_BYTES as usize);
    }

    #[test]
    fn preview_lines_only_materializes_the_requested_window() {
        let text = "zero\r\none\ntwé\nthree\nfour";

        let lines = super::preview_lines(text, /*start_line*/ 1, /*max_lines*/ 2);

        assert_eq!(
            lines.iter().map(|line| line.text).collect::<Vec<_>>(),
            vec!["one", "twé"]
        );
        assert_eq!(lines[0].start, "zero\r\n".len());
        assert_eq!(lines[1].end, "zero\r\none\ntwé".len());
    }

    #[test]
    fn preview_syntax_highlighting_is_bounded_for_a_long_visible_line() {
        let mut theme = Theme::default();
        theme.token_styles.push(TokenStyle {
            name: Some("keyword".to_string()),
            scope: vec!["keyword".to_string()],
            style: Style {
                bold: true,
                ..Style::default()
            },
        });
        let editor = test_editor_with_theme_and_size(theme, 120, 24);
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let text = format!(
            "let value = \"{}\";",
            "x".repeat(super::MAX_PREVIEW_HIGHLIGHT_BYTES * 2)
        );
        let preview = PickerPreview::Text {
            text: text.clone(),
            language: Some("rust".to_string()),
        };
        let lines = super::preview_lines(&text, /*start_line*/ 0, /*max_lines*/ 1);

        let spans = picker.preview_highlight_spans(&preview, &text, &lines);

        assert!(!spans.is_empty());
        assert!(spans
            .iter()
            .all(|span| span.end <= super::MAX_PREVIEW_HIGHLIGHT_BYTES));
    }

    #[test]
    fn preview_syntax_highlighting_rebases_visible_window_offsets() {
        let mut theme = Theme::default();
        theme.token_styles.push(TokenStyle {
            name: Some("keyword".to_string()),
            scope: vec!["keyword".to_string()],
            style: Style {
                bold: true,
                ..Style::default()
            },
        });
        let editor = test_editor_with_theme_and_size(theme, 120, 24);
        let picker = Picker::new(/*title*/ None, &editor, &[], /*id*/ None);
        let text = "// above the preview\nlet value = true;\n";
        let preview = PickerPreview::Text {
            text: text.to_string(),
            language: Some("rust".to_string()),
        };
        let lines = super::preview_lines(text, /*start_line*/ 1, /*max_lines*/ 1);

        let spans = picker.preview_highlight_spans(&preview, text, &lines);

        let keyword_start = text.find("let").unwrap();
        assert!(spans
            .iter()
            .any(|span| span.start == keyword_start && span.end == keyword_start + "let".len()));
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
    fn filtering_dynamic_items_keeps_references_to_the_original_items() {
        let editor = test_editor();
        let items = vec![dynamic_item("a", "alpha"), dynamic_item("b", "bravo")];
        let mut picker = Picker::new_dynamic(
            /*title*/ None,
            &editor,
            items,
            /*id*/ 22,
            PickerOptions::default(),
        );
        let original_label = picker.dynamic_items.as_ref().unwrap()[1].label.as_ptr();

        picker.filter("br");

        let selected = picker.selected_dynamic_item().unwrap();
        assert_eq!(selected.id, "b");
        assert_eq!(selected.label.as_ptr(), original_label);
        assert!(picker.list.items().is_empty());
    }

    #[test]
    fn structured_command_columns_stay_aligned_across_scrolled_pages() {
        let editor = test_editor_with_theme_and_size(Theme::default(), 200, 12);
        let mut items = (0..24)
            .map(|index| {
                let mut item =
                    dynamic_item(&format!("command-{index}"), &format!("Action {index}"));
                item.kind = Some("Command".to_string());
                item.annotation = Some("Edit".to_string());
                item.data = json!({ "primary_shortcut": "Ctrl-x", "colon": ":Action" });
                item
            })
            .collect::<Vec<_>>();
        items[20].label = "The much longer action that only appears on the next page".to_string();
        let mut picker = Picker::builder().structured_items(items).build(&editor);
        let content_width = picker.layout().results.width.saturating_sub(1);

        let first_page =
            picker.command_columns(picker.dynamic_items.as_ref().unwrap(), content_width);
        assert!(picker.command_column_widths.get().is_some());
        picker.list.set_selected_index(20);
        let next_page =
            picker.command_columns(picker.dynamic_items.as_ref().unwrap(), content_width);

        assert_eq!(first_page, next_page);
        assert_eq!(
            first_page.title,
            display_width("The much longer action that only appears on the next page")
        );

        picker.filter("Action 1");
        assert!(picker.command_column_widths.get().is_none());
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

        let selected_cell = &buffer.cells[(picker.y + 1) * buffer.width + picker.x + 3];
        assert_eq!(
            selected_cell.style,
            theme.selected_style(
                &theme.ui_style.picker_item,
                &theme.ui_style.picker_selected_item,
                SelectionForegroundPriority::Selection,
            )
        );

        let item_cell = &buffer.cells[(picker.y + 2) * buffer.width + picker.x + 3];
        assert_eq!(item_cell.style, theme.ui_style.picker_item);

        let prompt_cell = &buffer.cells
            [(picker.y + picker.height.saturating_sub(1)) * buffer.width + picker.x + 3];
        assert_eq!(prompt_cell.style, theme.ui_style.picker_prompt);
    }
}
