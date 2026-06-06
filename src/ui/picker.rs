use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde_json::json;
use std::cmp::Reverse;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    theme::Theme,
    unicode_utils::{display_width, fit_display_width},
};

use super::{dialog::BorderStyle, Component, Dialog, List};

type SelectAction = Box<dyn Fn(String) -> Action + Send>;

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
        }
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

    pub fn set_empty_message(&mut self, message: Option<String>) {
        self.empty_message = message;
    }

    fn selected_item(&self) -> Option<String> {
        if self.list.items().is_empty() {
            return None;
        }
        Some(self.list.selected_item())
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
            json!(selected),
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
}

impl Component for Picker {
    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(event) => match event.code {
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
                    let previous = self.selected_item();
                    self.list.page_down();
                    self.notify_selection_changed(previous)
                }
                KeyCode::Char('b') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                    let previous = self.selected_item();
                    self.list.page_up();
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
                    self.notify_selection_changed(previous)
                }
                KeyCode::Enter => {
                    if self.list.items().is_empty() {
                        return None;
                    }
                    let action = if let Some(select_action) = &self.select_action {
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
                    self.notify_selection_changed(previous)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        self.list.draw(buffer)?;
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
        buffer.set_text(self.x + 2, dy + 1, &self.search, prompt_style);

        Ok(())
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        let cx = self.x + 2 + display_width(&self.search);
        let cy = self.y + self.height.saturating_sub(1);

        Some((cx, cy))
    }
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
        ui::{Component, Picker},
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
