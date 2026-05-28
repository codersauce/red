use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::cmp::Reverse;

use crate::{
    color::Color,
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::display_width,
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
    style: Style,
    list: List,
    dialog: Dialog,
    matcher: SkimMatcherV2,
    select_action: Option<SelectAction>,
    search: String,
    theme: Theme,
}

impl Picker {
    pub fn new(title: Option<String>, editor: &Editor, items: &[String], id: Option<i32>) -> Self {
        let total_width = editor.vwidth();
        let total_height = editor.vheight();

        let width = total_width * 80 / 100;
        let height = total_height * 80 / 100;
        let x = (total_width / 2) - (width / 2);
        let y = (total_height / 2) - (height / 2);

        let style = Style {
            fg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            bg: Some(Color::Rgb {
                r: 67,
                g: 70,
                b: 89,
            }),
            ..Default::default()
        };
        let selected_style = Style {
            fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            ..Default::default()
        };

        let dialog = Dialog::new(
            title,
            x,
            y,
            width,
            height.saturating_sub(1),
            &style,
            BorderStyle::Single,
            &editor.theme,
        );
        let list = List::new(
            x + 1,
            y + 1,
            width,
            height.saturating_sub(3),
            // TODO: remove the clone
            items.to_vec(),
            &style,
            &selected_style,
        );

        Picker {
            id,
            x,
            y,
            width,
            height,
            style,
            items: items.to_vec(),
            list,
            dialog,
            matcher: SkimMatcherV2::default(),
            select_action: None,
            search: String::new(),
            theme: editor.theme.clone(),
        }
    }

    pub fn builder() -> PickerBuilder {
        PickerBuilder::new()
    }

    pub fn filter(&mut self, term: &str) {
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
}

impl Component for Picker {
    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(event) => match event.code {
                KeyCode::Char('j') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.list.move_down();
                    None
                }
                KeyCode::Char('k') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.list.move_up();
                    None
                }
                KeyCode::Down => {
                    self.list.move_down();
                    None
                }
                KeyCode::Up => {
                    self.list.move_up();
                    None
                }
                KeyCode::Esc => Some(KeyAction::Single(Action::CloseDialog)),
                KeyCode::Backspace => {
                    self.search.pop();
                    let search = self.search.clone();
                    self.filter(&search);
                    None
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
                    let search = format!("{}{}", &self.search, &c);
                    self.filter(&search);
                    self.search = search;
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        self.list.draw(buffer)?;

        let dy = self.y + self.height.saturating_sub(2);
        buffer.set_char(self.x, dy, '├', &self.style, &self.theme);
        buffer.set_char(self.x + self.width + 1, dy, '┤', &self.style, &self.theme);
        buffer.set_text(self.x + 1, dy, &"─".repeat(self.width), &self.style);
        buffer.set_text(self.x + 2, dy + 1, &self.search, &self.style);

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

    use crate::{
        buffer::Buffer,
        config::{Config, KeyAction},
        editor::{Action, Editor},
        lsp::LspManager,
        theme::Theme,
        ui::{Component, Picker},
    };

    fn test_editor() -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let theme = Theme::default();
        let buffer = Buffer::new(None, String::new());

        Editor::with_size(lsp, 80, 24, config, theme, vec![buffer]).unwrap()
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn select(picker: &mut Picker) -> Option<KeyAction> {
        picker.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE))
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
}
