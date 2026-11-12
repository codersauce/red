//! Theme selection surfaces built on the standard compact picker.

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use super::*;
use crate::{
    config::AppearanceConfig, editor::ThemeBrowserAction as ThemeAction, theme::ThemeMode,
    ui::ActionBarRole,
};

const THEME_PICKER_ID: i32 = -997;
const SETTINGS_TITLE: &str = "Light/dark themes";

pub(crate) struct ThemeCatalogEntry {
    pub name: String,
    pub file: String,
    pub mode: Option<ThemeMode>,
    pub detail: String,
}

pub(crate) struct ThemeBrowser {
    picker: Picker,
    catalog: Vec<ThemeCatalogEntry>,
    pair: AppearanceConfig,
    slot: Option<ThemeMode>,
    show_all: bool,
    return_to_settings: bool,
    handle: Option<PickerHandle>,
}

fn theme_action(action: ThemeAction) -> Option<KeyAction> {
    Some(KeyAction::Single(Action::ThemeBrowser(action)))
}

impl ThemeBrowser {
    pub(crate) fn new(
        editor: &Editor,
        mut catalog: Vec<ThemeCatalogEntry>,
        slot: Option<ThemeMode>,
        return_to_settings: bool,
        current: &str,
        pair: AppearanceConfig,
        handle: Option<PickerHandle>,
    ) -> Self {
        catalog.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.file.clone()));
        let mut browser = Self {
            picker: Picker::new_dynamic(
                Some("Themes".into()),
                editor,
                Vec::new(),
                THEME_PICKER_ID,
                PickerOptions {
                    placeholder: Some("Filter themes".into()),
                    presentation: PickerPresentation::Compact,
                    item_layout: PickerItemLayout::LabelFirst,
                    ..Default::default()
                },
            ),
            catalog,
            pair,
            slot,
            show_all: slot.is_none(),
            return_to_settings,
            handle,
        };
        browser.picker.live = false;
        browser.refresh_list(Some(current));
        browser
    }

    fn refresh_list(&mut self, current: Option<&str>) {
        let items: Vec<_> = self
            .catalog
            .iter()
            .filter(|entry| self.show_all || entry.mode == self.slot)
            .map(|entry| PickerItem {
                id: entry.file.clone(),
                label: entry.name.clone(),
                kind: Some("Theme".into()),
                annotation: if self.slot.is_some() {
                    entry.mode.map(|mode| mode.label().to_string())
                } else {
                    Some(entry.file.clone())
                },
                icon: None,
                detail: self.slot.is_none().then(|| entry.detail.clone()),
                data: Value::Null,
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            })
            .collect();
        self.picker.reserved_annotation_width = if self.slot.is_some() {
            "Light".len()
        } else {
            0
        };
        self.picker
            .set_status(Some(format!("{} themes", items.len())));
        self.picker.set_badges(
            self.slot
                .map(|slot| format!("Editing {}", slot.label()))
                .into_iter()
                .collect(),
        );
        self.picker.set_empty_message(Some(
            if self.show_all {
                "No themes found"
            } else {
                "No matching themes · Ctrl+r shows all"
            }
            .into(),
        ));
        self.picker
            .apply_update(THEME_PICKER_ID, PickerUpdate::Items(items));

        let preference = match self.slot {
            Some(ThemeMode::Light) => self.pair.light_theme.as_deref(),
            Some(ThemeMode::Dark) => self.pair.dark_theme.as_deref(),
            None => current,
        };
        if let Some(preference) = preference {
            self.picker.select_dynamic_id(preference);
        }

        self.picker.native_actions = vec![UiAction::new("select", "Enter", "select")
            .with_compact_label("")
            .with_priority(ActionPriority::Essential)];
        if let Some(slot) = self.slot {
            self.picker.native_actions.push(
                UiAction::new("slot", "Tab", slot.opposite().label())
                    .with_description(
                        "Switch the preference being edited. Shift+Tab also switches.",
                    )
                    .with_priority(ActionPriority::Essential),
            );
        }
        self.picker.native_actions.extend([
            UiAction::new("settings", "Ctrl+t", SETTINGS_TITLE)
                .with_description("Use different light and dark themes")
                .with_priority(if self.slot.is_some() {
                    ActionPriority::Secondary
                } else {
                    ActionPriority::Essential
                }),
            UiAction::new(
                "cancel",
                "Esc",
                if self.return_to_settings {
                    "back"
                } else {
                    "cancel"
                },
            )
            .with_compact_label("")
            .with_priority(ActionPriority::Essential),
        ]);
        if self.slot.is_some() {
            self.picker.native_actions.push(
                UiAction::new(
                    "filter",
                    "Ctrl+r",
                    if self.show_all {
                        "Matching themes"
                    } else {
                        "All themes"
                    },
                )
                .with_priority(ActionPriority::Secondary),
            );
        }
        self.picker
            .native_actions
            .extend(crate::ui::picker_reference_actions());
        self.picker.sync_list_bounds();
    }
}

impl Component for ThemeBrowser {
    fn picker_handle(&self) -> Option<PickerHandle> {
        self.handle
    }

    fn picker_id(&self) -> Option<i32> {
        self.handle.map(PickerHandle::get)
    }

    fn shortcut_context(&self) -> &str {
        self.picker.shortcut_context()
    }

    fn surface_actions(&self) -> Vec<UiAction> {
        self.picker.surface_actions()
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.picker.draw(buffer)
    }

    fn resize(&mut self, width: usize, height: usize) -> bool {
        self.picker.resize(width, height)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.picker.set_theme(theme);
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        self.picker.cursor_position()
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        match event {
            Event::Key(KeyEvent {
                kind: KeyEventKind::Release,
                ..
            }) => return None,
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                    return theme_action(ThemeAction::Settings)
                }
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    return theme_action(ThemeAction::Cancel {
                        return_to_settings: self.return_to_settings,
                    });
                }
                (KeyCode::Enter, _) => {
                    return theme_action(ThemeAction::Save {
                        name: self.picker.selected_item()?,
                        slot: self.slot,
                        return_to_settings: self.return_to_settings,
                    });
                }
                (KeyCode::Tab, KeyModifiers::NONE | KeyModifiers::SHIFT)
                | (KeyCode::BackTab, KeyModifiers::NONE | KeyModifiers::SHIFT)
                    if self.slot.is_some() =>
                {
                    self.slot = self.slot.map(ThemeMode::opposite);
                    self.refresh_list(None);
                    return theme_action(ThemeAction::Preview(self.picker.selected_item()));
                }
                (KeyCode::Char('r'), KeyModifiers::CONTROL) if self.slot.is_some() => {
                    self.show_all = !self.show_all;
                    let selected = self.picker.selected_item();
                    self.refresh_list(None);
                    if let Some(selected) = selected {
                        self.picker.select_dynamic_id(&selected);
                    }
                    return theme_action(ThemeAction::Preview(self.picker.selected_item()));
                }
                _ => {}
            },
            Event::Mouse(mouse) => {
                if let Some(action) = clicked_footer(&self.picker, mouse) {
                    return if action == "help" {
                        Some(KeyAction::Single(Action::KeyboardShortcuts))
                    } else {
                        self.activate_surface_action(&action)
                    };
                }
                let index = clicked_row(&self.picker, mouse)? + self.picker.list.top_index();
                self.picker.visible_dynamic_items.get(index)?;
                let previous = self.picker.selected_item();
                self.picker.list.set_selected_index(index);
                let selected = self.picker.selected_item();
                return (selected != previous).then(|| {
                    KeyAction::Single(Action::ThemeBrowser(ThemeAction::Preview(selected)))
                });
            }
            _ => {}
        }
        let previous = self.picker.selected_item();
        let action = self.picker.handle_event(event);
        let selected = self.picker.selected_item();
        if selected != previous {
            theme_action(ThemeAction::Preview(selected))
        } else {
            action
        }
    }
}

pub(crate) struct ThemeSettings {
    picker: Picker,
    enabled: bool,
    light: String,
    dark: String,
    row: usize,
    handle: Option<PickerHandle>,
}

impl ThemeSettings {
    pub(crate) fn new(
        editor: &Editor,
        enabled: bool,
        light: String,
        dark: String,
        appearance: Option<ThemeMode>,
        handle: Option<PickerHandle>,
    ) -> Self {
        let mut picker = Picker::new(Some(SETTINGS_TITLE.into()), editor, &[], None);
        picker.set_presentation_for_viewport(
            PickerPresentation::Compact,
            editor.vwidth(),
            editor.vheight(),
        );
        let mut settings = Self {
            picker,
            enabled,
            light,
            dark,
            row: 0,
            handle,
        };
        settings.focus_row(0);
        settings.set_terminal_appearance(appearance);
        settings
    }

    fn focus_row(&mut self, row: usize) {
        self.row = row;
        let action = if row == 0 {
            UiAction::new("toggle", "Space", "toggle")
        } else {
            UiAction::new("choose", "Enter", "choose")
        };
        self.picker.native_actions = vec![
            action.with_priority(ActionPriority::Essential),
            UiAction::new("back", "Esc", "back").with_priority(ActionPriority::Essential),
        ];
        if self.enabled {
            self.picker.native_actions.push(
                UiAction::new("move", "↑ / ↓", "Move between settings")
                    .with_priority(ActionPriority::Reference),
            );
        }
    }

    fn choose_row(&self) -> Option<KeyAction> {
        match self.row {
            0 => theme_action(ThemeAction::ToggleAutomatic),
            1 => theme_action(ThemeAction::Choose(ThemeMode::Light)),
            2 => theme_action(ThemeAction::Choose(ThemeMode::Dark)),
            _ => None,
        }
    }
}

impl Component for ThemeSettings {
    fn picker_handle(&self) -> Option<PickerHandle> {
        self.handle
    }

    fn picker_id(&self) -> Option<i32> {
        self.handle.map(PickerHandle::get)
    }

    fn shortcut_context(&self) -> &str {
        SETTINGS_TITLE
    }

    fn surface_actions(&self) -> Vec<UiAction> {
        self.picker.surface_actions()
    }

    fn set_terminal_appearance(&mut self, appearance: Option<ThemeMode>) {
        let label = appearance.map_or("Unknown", ThemeMode::label);
        self.picker.set_status(Some(format!("Terminal: {label}")));
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let picker = &self.picker;
        let layout = picker.layout();
        picker.dialog.draw(buffer)?;
        let rows = [
            "Use different light and dark themes.".to_string(),
            format!(
                "[{}] Enable automatic switching",
                if self.enabled { "x" } else { " " }
            ),
            format!("Light theme   {}", self.light),
            format!("Dark theme    {}", self.dark),
        ];
        for (index, text) in rows.into_iter().take(layout.results.height).enumerate() {
            let selected = index == self.row + 1;
            let mut style = picker.result_row_style(selected);
            if index >= 2 && !self.enabled {
                style.fg = picker.theme.ui_style.muted.fg;
            }
            let y = layout.results.y + index;
            let x = layout.results.x;
            let width = layout.results.width;
            buffer.fill_ascii_spaces(x, y, width, &style);
            if selected {
                buffer.set_text(x, y, "›", &style);
            }
            buffer.set_text(
                x + 2,
                y,
                &fit_display_width(&text, width.saturating_sub(2)),
                &style,
            );
        }
        picker.draw_prompt(buffer, layout);
        if let Some(y) = layout.action_y {
            ActionBar::new(&picker.native_actions).render(
                buffer,
                picker.x + 1,
                y,
                picker.width,
                &picker.theme,
                &picker.theme.ui_style.popup,
            );
        }
        Ok(())
    }

    fn resize(&mut self, width: usize, height: usize) -> bool {
        self.picker.resize(width, height)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.picker.set_theme(theme);
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        match event {
            Event::Key(KeyEvent {
                kind: KeyEventKind::Release,
                ..
            }) => None,
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    theme_action(ThemeAction::BackToList)
                }
                (KeyCode::Enter, _) if self.row != 0 || key.kind == KeyEventKind::Press => {
                    self.choose_row()
                }
                (KeyCode::Char(' '), KeyModifiers::NONE)
                    if self.row == 0 && key.kind == KeyEventKind::Press =>
                {
                    self.choose_row()
                }
                (KeyCode::Up, _) if self.enabled => {
                    self.focus_row(self.row.saturating_sub(1));
                    Some(KeyAction::Single(Action::Refresh))
                }
                (KeyCode::Down, _) if self.enabled => {
                    self.focus_row((self.row + 1).min(2));
                    Some(KeyAction::Single(Action::Refresh))
                }
                _ => None,
            },
            Event::Mouse(mouse) => {
                if let Some(action) = clicked_footer(&self.picker, mouse) {
                    return if action == "help" {
                        Some(KeyAction::Single(Action::KeyboardShortcuts))
                    } else {
                        self.activate_surface_action(&action)
                    };
                }
                let row = clicked_row(&self.picker, mouse)?.checked_sub(1)?;
                if row == 0 || (self.enabled && row <= 2) {
                    self.focus_row(row);
                    self.choose_row()
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

fn clicked_row(picker: &Picker, mouse: &MouseEvent) -> Option<usize> {
    let rect = picker.layout().results;
    let x = usize::from(mouse.column);
    let y = usize::from(mouse.row);
    (mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && (rect.x..rect.x + rect.width).contains(&x)
        && (rect.y..rect.y + rect.height).contains(&y))
    .then(|| y - rect.y)
}

fn clicked_footer(picker: &Picker, mouse: &MouseEvent) -> Option<String> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || picker.layout().action_y != Some(usize::from(mouse.row))
    {
        return None;
    }
    let layout = ActionBar::new(&picker.native_actions).layout(picker.width);
    let mut x = picker.x + 1;
    let column = usize::from(mouse.column);
    let mut action = None;
    for span in layout.spans {
        if span.role == ActionBarRole::Key {
            action = picker
                .native_actions
                .iter()
                .find(|action| action.key == span.text);
        }
        let end = x + display_width(&span.text);
        if (x..end).contains(&column) {
            return match span.role {
                ActionBarRole::Overflow => Some("help".into()),
                ActionBarRole::Key | ActionBarRole::Label => action.map(|action| action.id.clone()),
                _ => None,
            };
        }
        x = end;
    }
    None
}
