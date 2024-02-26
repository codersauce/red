use crossterm::{
    event::{self, Event, KeyCode},
    style::Color,
};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    theme::Style,
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
            fg: Some(Color::White),
            bg: Some(Color::Black),
            ..Default::default()
        };
        let selected_style = Style {
            fg: Some(Color::Black),
            bg: Some(Color::White),
            ..Default::default()
        };

        let dialog = Dialog::new(title, x, y, width, height - 1, &style, BorderStyle::Single);
        let list = List::new(
            x + 1,
            y + 1,
            width,
            height - 3,
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
        new_items.sort_by(|a, b| b.1.cmp(&a.1));

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
                    let mut search = self.search.clone();
                    search.truncate(self.search.len().saturating_sub(1));

                    self.filter(&search);
                    self.search = search;
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

        let dy = self.y + self.height - 2;
        buffer.set_char(self.x, dy, '├', &self.style);
        buffer.set_char(self.x + self.width + 1, dy, '┤', &self.style);
        buffer.set_text(self.x + 1, dy, &"─".repeat(self.width), &self.style);
        buffer.set_text(self.x + 2, dy + 1, &self.search, &self.style);

        Ok(())
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        let cx = self.x + 2 + self.search.len();
        let cy = self.y + self.height - 1;

        Some((cx as u16, cy as u16))
    }
}

pub struct PickerBuilder {
    title: Option<String>,
    items: Vec<String>,
    id: Option<i32>,
    select_action: Option<SelectAction>,
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
