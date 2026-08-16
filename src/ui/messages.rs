//! Read-only notification history with a stable selection and a full-text detail pane.

use crossterm::event::{Event, KeyCode, KeyModifiers};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    wrap_text, ActionBar, ActionPriority, Component, UiAction,
};
use crate::{
    config::KeyAction,
    editor::{Action, RenderBuffer},
    notification::{MessageAction, NotificationCounts},
    theme::Theme,
    unicode_utils::{fit_display_width, truncate_display_width},
};

pub(crate) struct MessageRow {
    pub summary: String,
    pub metadata: String,
}

pub(crate) struct MessagesView {
    pub rows: Vec<MessageRow>,
    pub selected: usize,
    pub detail: String,
    pub scroll: usize,
    pub query: String,
    pub searching: bool,
    pub filter: &'static str,
    pub counts: NotificationCounts,
    pub feedback: Option<String>,
}

pub(crate) struct MessagesPanel {
    view: MessagesView,
    width: usize,
    height: usize,
    theme: Theme,
}

impl MessagesPanel {
    pub(crate) fn new(view: MessagesView, width: usize, height: usize, theme: &Theme) -> Self {
        Self {
            view,
            width,
            height,
            theme: theme.clone(),
        }
    }

    fn action(action: MessageAction) -> Option<KeyAction> {
        Some(KeyAction::Single(Action::MessageHistory(action)))
    }
}

impl Component for MessagesPanel {
    fn is_message_history(&self) -> bool {
        true
    }

    fn surface_actions(&self) -> Vec<UiAction> {
        let essential =
            |id, key, label| UiAction::new(id, key, label).with_priority(ActionPriority::Essential);
        if self.view.searching {
            return vec![
                essential("done", "Enter", "done"),
                essential("clear", "Esc", "clear"),
            ];
        }
        vec![
            UiAction::new("browse", "j/k", "browse"),
            UiAction::new("search", "/", "search"),
            UiAction::new("filter", "f", "filter"),
            essential("acknowledge", "Enter", "acknowledge"),
            UiAction::new("copy", "y", "copy"),
            UiAction::new("clear", "D", "clear inactive").with_priority(ActionPriority::Secondary),
            UiAction::new("scroll", "Ctrl-d/u", "details").with_priority(ActionPriority::Secondary),
            essential("close", "Esc", "return"),
        ]
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let width = self.width.saturating_sub(4).min(116);
        let height = self.height.saturating_sub(2).min(24);
        if width < 4 || height < 3 {
            return Ok(());
        }
        let x = self.width.saturating_sub(width + 2) / 2;
        let y = self.height.saturating_sub(height + 2) / 2;
        let title = if self.view.searching {
            format!("Messages /{}", self.view.query)
        } else {
            format!(
                "Messages · {} · {} active · {} retained",
                self.view.filter, self.view.counts.active, self.view.counts.total
            )
        };
        let dialog = Dialog::new(
            Some(truncate_display_width(&title, width.saturating_sub(2))),
            x,
            y,
            width,
            height,
            &self.theme.ui_style.dialog,
            BorderStyle::Single,
            &self.theme,
        )
        .with_surface_theme(&self.theme, SurfaceRole::Dialog);
        dialog.draw(buffer)?;

        let body = height.saturating_sub(1);
        let wide = width >= 72;
        let list_width = if wide { (width * 2 / 5).min(48) } else { width };
        let list_height = if wide { body } else { (body / 2).max(1) };
        let visible = (list_height / 2).max(1);
        let first = self.view.selected.saturating_sub(visible.saturating_sub(1));
        if self.view.rows.is_empty() {
            buffer.set_text(
                x + 1,
                y + 1,
                &truncate_display_width("No matching messages", list_width),
                &self.theme.ui_style.muted,
            );
        }
        for (index, row) in self.view.rows.iter().enumerate().skip(first).take(visible) {
            let selected = index == self.view.selected;
            let style = if selected {
                &self.theme.ui_style.picker_selected_item
            } else {
                &self.theme.ui_style.picker_item
            };
            let marker = if selected { "> " } else { "  " };
            let row_y = y + 1 + (index - first) * 2;
            if row_y < y + 1 + list_height {
                buffer.set_text(
                    x + 1,
                    row_y,
                    &fit_display_width(&format!("{marker}{}", row.summary), list_width),
                    style,
                );
            }
            if row_y + 1 < y + 1 + list_height {
                buffer.set_text(
                    x + 1,
                    row_y + 1,
                    &fit_display_width(&format!("  {}", row.metadata), list_width),
                    if selected {
                        style
                    } else {
                        &self.theme.ui_style.muted
                    },
                );
            }
        }
        let (detail_x, detail_y, detail_width, detail_height) = if wide {
            for offset in 0..body {
                buffer.set_text(
                    x + list_width + 1,
                    y + 1 + offset,
                    "│",
                    &self.theme.ui_style.muted,
                );
            }
            (
                x + list_width + 3,
                y + 1,
                width.saturating_sub(list_width + 2),
                body,
            )
        } else {
            (
                x + 1,
                y + 1 + list_height,
                width,
                body.saturating_sub(list_height),
            )
        };
        let detail = wrap_text(&self.view.detail, detail_width.max(1));
        let scroll = self
            .view
            .scroll
            .min(detail.rows.len().saturating_sub(detail_height));
        for (offset, row) in detail
            .rows
            .iter()
            .skip(scroll)
            .take(detail_height)
            .enumerate()
        {
            buffer.set_text(
                detail_x,
                detail_y + offset,
                &truncate_display_width(row, detail_width),
                &self.theme.ui_style.dialog,
            );
        }
        ActionBar::new(&self.surface_actions())
            .with_context(if self.view.searching {
                "SEARCH"
            } else {
                "MESSAGES"
            })
            .with_status(self.view.feedback.as_deref())
            .render(
                buffer,
                x + 1,
                y + height,
                width,
                &self.theme,
                &self.theme.ui_style.dialog,
            );
        Ok(())
    }

    fn resize(&mut self, width: usize, height: usize) -> bool {
        self.width = width;
        self.height = height;
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        if self.view.searching {
            return match event {
                Event::Paste(text) => Self::action(MessageAction::Query(text.clone())),
                Event::Key(key) => match key.code {
                    KeyCode::Esc => Self::action(MessageAction::ClearSearch),
                    KeyCode::Enter => Self::action(MessageAction::EndSearch),
                    KeyCode::Backspace => Self::action(MessageAction::Backspace),
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        Self::action(MessageAction::Query(ch.to_string()))
                    }
                    _ => None,
                },
                _ => None,
            };
        }
        let Event::Key(key) = event else {
            return None;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('d') => Self::action(MessageAction::ScrollDown),
                KeyCode::Char('u') => Self::action(MessageAction::ScrollUp),
                _ => None,
            };
        }
        Self::action(match key.code {
            KeyCode::Char('j') | KeyCode::Down => MessageAction::Next,
            KeyCode::Char('k') | KeyCode::Up => MessageAction::Previous,
            KeyCode::Char('/') => MessageAction::Search,
            KeyCode::Char('f') => MessageAction::CycleFilter,
            KeyCode::Enter | KeyCode::Char('d') => MessageAction::Acknowledge,
            KeyCode::Char('D') => MessageAction::ClearInactive,
            KeyCode::Char('y') => MessageAction::Copy,
            KeyCode::PageDown => MessageAction::ScrollDown,
            KeyCode::PageUp => MessageAction::ScrollUp,
            KeyCode::Esc | KeyCode::Char('q') => MessageAction::Close,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;

    #[test]
    fn selection_fills_both_lines_without_crossing_pane_boundary() {
        let mut theme = Theme::default();
        theme.ui_style.picker_selected_item.bg = Some(Color::Rgb {
            r: 91,
            g: 42,
            b: 73,
        });
        let height = 30;

        for (viewport_width, list_width) in [(120, 46), (64, 60)] {
            let mut panel = MessagesPanel::new(
                MessagesView {
                    rows: vec![
                        MessageRow {
                            summary: "short".into(),
                            metadata: "details".into(),
                        },
                        MessageRow {
                            summary: "界".repeat(80),
                            metadata: "👋".repeat(80),
                        },
                    ],
                    selected: 0,
                    detail: "preview".into(),
                    scroll: 0,
                    query: String::new(),
                    searching: false,
                    filter: "all",
                    counts: NotificationCounts::default(),
                    feedback: None,
                },
                viewport_width,
                height,
                &theme,
            );
            let mut buffer = RenderBuffer::new(viewport_width, height, &theme.style);
            let list_x = 2;
            let list_y = 3;

            for selected in 0..2 {
                panel.view.selected = selected;
                panel.draw(&mut buffer).unwrap();

                for row_y in list_y..list_y + 4 {
                    let is_selected = (row_y - list_y) / 2 == selected;
                    for column in list_x..list_x + list_width {
                        let style = &buffer.cells[row_y * viewport_width + column].style;
                        assert_eq!(
                            style.bg == theme.ui_style.picker_selected_item.bg,
                            is_selected,
                            "viewport={viewport_width}, selected={selected}, cell=({column},{row_y})"
                        );
                    }
                    let boundary = &buffer.cells[row_y * viewport_width + list_x + list_width];
                    assert_eq!(boundary.c, '│');
                    assert_ne!(boundary.style.bg, theme.ui_style.picker_selected_item.bg);
                }
            }
        }
    }
}
