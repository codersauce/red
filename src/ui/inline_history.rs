//! Bottom-docked, read-only browser for retained inline conversations.

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    wrap_text, Component,
};
use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    inline_history::HistoryAction,
    theme::Theme,
    unicode_utils::truncate_display_width,
};
use crossterm::event::{Event, KeyCode, KeyModifiers};

pub(crate) struct InlineHistoryPanel {
    rows: Vec<String>,
    selected: usize,
    detail: String,
    scroll: usize,
    searching: bool,
    query: String,
    confirm_forget: bool,
    title: String,
    width: usize,
    height: usize,
    theme: Theme,
}

impl InlineHistoryPanel {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        editor: &Editor,
        rows: Vec<String>,
        selected: usize,
        detail: String,
        scroll: usize,
        searching: bool,
        query: String,
        confirm_forget: bool,
        title: String,
    ) -> Self {
        Self {
            rows,
            selected,
            detail,
            scroll,
            searching,
            query,
            confirm_forget,
            title,
            width: editor.vwidth(),
            height: editor.inline_history_viewport_height(),
            theme: editor.theme.clone(),
        }
    }
    fn action(action: HistoryAction) -> Option<KeyAction> {
        Some(KeyAction::Single(Action::InlineHistoryAction(action)))
    }
}

impl Component for InlineHistoryPanel {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let width = self.width.saturating_sub(2);
        let height = (self.height / 2)
            .clamp(5, 16)
            .min(self.height.saturating_sub(2));
        let y = self.height.saturating_sub(height + 2);
        let title = if self.searching {
            format!("Inline history /{}", self.query)
        } else {
            self.title.clone()
        };
        let dialog = Dialog::new(
            Some(title),
            0,
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
        let wide = width >= 70;
        let list_width = if wide { (width * 2 / 5).min(48) } else { width };
        let list_height = if wide { body } else { body.min(2) };
        let visible_items = (list_height / 2).max(1);
        let first = self
            .selected
            .saturating_sub(visible_items.saturating_sub(1));
        for (offset, row) in self.rows.iter().enumerate().skip(first).take(visible_items) {
            let style = if offset == self.selected {
                &self.theme.ui_style.picker_selected_item
            } else {
                &self.theme.ui_style.picker_item
            };
            let marker = if offset == self.selected { "> " } else { "  " };
            let row_y = y + 1 + (offset - first) * 2;
            let mut lines = row.lines();
            let text = truncate_display_width(
                &format!("{marker}{}", lines.next().unwrap_or_default()),
                list_width,
            );
            if row_y < y + 1 + list_height {
                buffer.set_text(1, row_y, &text, style);
            }
            if row_y + 1 < y + 1 + list_height {
                buffer.set_text(
                    1,
                    row_y + 1,
                    &truncate_display_width(
                        &format!("  {}", lines.next().unwrap_or_default()),
                        list_width,
                    ),
                    if offset == self.selected {
                        style
                    } else {
                        &self.theme.ui_style.muted
                    },
                );
            }
        }
        let (detail_x, detail_y, detail_width, detail_height) = if wide {
            for row in 0..body {
                buffer.set_text(list_width + 1, y + 1 + row, "│", &self.theme.ui_style.muted);
            }
            (
                list_width + 3,
                y + 1,
                width.saturating_sub(list_width + 2),
                body,
            )
        } else {
            (
                1,
                y + 1 + list_height,
                width,
                body.saturating_sub(list_height),
            )
        };
        for (offset, row) in wrap_text(&self.detail, detail_width.max(1))
            .rows
            .iter()
            .skip(self.scroll)
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
        let help = if self.confirm_forget {
            "Forget this entire conversation? y confirm · Esc cancel"
        } else if self.searching {
            "Type to search · Enter done · Esc clear"
        } else {
            "j/k browse · l/h turns · / search · w workspace · v view · r continue · R recheck · d resolve · D forget · Enter jump · Esc return"
        };
        if height > 0 {
            buffer.set_text(
                1,
                y + height,
                &truncate_display_width(help, width),
                &self.theme.ui_style.muted,
            );
        }
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
        if self.searching {
            return match event {
                Event::Paste(text) => Self::action(HistoryAction::Query(text.clone())),
                Event::Key(key) => match key.code {
                    KeyCode::Esc => Self::action(HistoryAction::ClearSearch),
                    KeyCode::Enter => Self::action(HistoryAction::EndSearch),
                    KeyCode::Backspace => Self::action(HistoryAction::Backspace),
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        Self::action(HistoryAction::Query(ch.to_string()))
                    }
                    _ => None,
                },
                _ => None,
            };
        }
        let Event::Key(key) = event else {
            return None;
        };
        if self.confirm_forget {
            return match key.code {
                KeyCode::Char('y') => Self::action(HistoryAction::ConfirmForget),
                KeyCode::Esc | KeyCode::Char('n') => Self::action(HistoryAction::Forget),
                _ => None,
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('d') => Self::action(HistoryAction::ScrollDown),
                KeyCode::Char('u') => Self::action(HistoryAction::ScrollUp),
                _ => None,
            };
        }
        Self::action(match key.code {
            KeyCode::Char('j') | KeyCode::Down => HistoryAction::Next,
            KeyCode::Char('k') | KeyCode::Up => HistoryAction::Previous,
            KeyCode::Char('l') | KeyCode::Right => HistoryAction::Expand,
            KeyCode::Char('h') | KeyCode::Left => HistoryAction::Collapse,
            KeyCode::Char('w') => HistoryAction::ToggleWorkspace,
            KeyCode::Char('/') => HistoryAction::Search,
            KeyCode::Char('v') => HistoryAction::CycleView,
            KeyCode::PageDown => HistoryAction::ScrollDown,
            KeyCode::PageUp => HistoryAction::ScrollUp,
            KeyCode::Enter => HistoryAction::Jump,
            KeyCode::Esc | KeyCode::Char('q') => HistoryAction::Close,
            KeyCode::Char('r') => HistoryAction::Continue,
            KeyCode::Char('R') => HistoryAction::Recheck,
            KeyCode::Char('d') => HistoryAction::Resolve,
            KeyCode::Char('D') => HistoryAction::Forget,
            _ => return None,
        })
    }
}
