//! Bottom-docked, read-only browser for retained inline conversations.

use std::time::Instant;

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    spinner_frame, wrap_text, ActionBar, ActionPriority, Component, UiAction,
    SPINNER_FRAME_INTERVAL_MS,
};
use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    inline_history::HistoryAction,
    keyboard::is_word_backspace,
    theme::Theme,
    unicode_utils::{fit_display_width, truncate_display_width},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

pub(crate) struct InlineHistoryRow {
    pub text: String,
    pub running: bool,
}

pub(crate) struct InlineHistoryPanel {
    rows: Vec<InlineHistoryRow>,
    selected: usize,
    detail: String,
    scroll: usize,
    searching: bool,
    query: String,
    confirm_forget: bool,
    can_restore: bool,
    animation_started: Instant,
    animation_frame: u64,
    title: String,
    width: usize,
    height: usize,
    theme: Theme,
}

impl InlineHistoryPanel {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        editor: &Editor,
        rows: Vec<InlineHistoryRow>,
        selected: usize,
        detail: String,
        scroll: usize,
        searching: bool,
        query: String,
        confirm_forget: bool,
        title: String,
        can_restore: bool,
        animation_started: Instant,
    ) -> Self {
        Self {
            rows,
            selected,
            detail,
            scroll,
            searching,
            query,
            confirm_forget,
            can_restore,
            animation_started,
            animation_frame: animation_started.elapsed().as_millis() as u64
                / SPINNER_FRAME_INTERVAL_MS,
            title,
            width: editor.vwidth(),
            height: editor.inline_history_viewport_height(),
            theme: editor.theme.clone(),
        }
    }
    fn action(action: HistoryAction) -> Option<KeyAction> {
        Some(KeyAction::Single(Action::InlineHistoryAction(action)))
    }

    fn list_item_at(&self, column: usize, row: usize) -> Option<usize> {
        let width = self.width.saturating_sub(2);
        let height = (self.height / 2)
            .clamp(5, 16)
            .min(self.height.saturating_sub(2));
        let y = self.height.saturating_sub(height + 2);
        let body = height.saturating_sub(1);
        let wide = width >= 70;
        let list_width = if wide { (width * 2 / 5).min(72) } else { width };
        let list_height = if wide { body } else { body.min(2) };
        if column < 1 || column > list_width || row <= y || row >= y + 1 + list_height {
            return None;
        }
        let first = self
            .selected
            .saturating_sub((list_height / 2).max(1).saturating_sub(1));
        let index = first + (row - y - 1) / 2;
        (index < self.rows.len()).then_some(index)
    }
}

impl Component for InlineHistoryPanel {
    fn is_inline_history(&self) -> bool {
        true
    }

    fn tick(&mut self) -> anyhow::Result<bool> {
        if !self.rows.iter().any(|row| row.running) {
            return Ok(false);
        }
        let frame = self.animation_started.elapsed().as_millis() as u64 / SPINNER_FRAME_INTERVAL_MS;
        if frame == self.animation_frame {
            return Ok(false);
        }
        self.animation_frame = frame;
        Ok(true)
    }

    fn surface_actions(&self) -> Vec<UiAction> {
        let essential =
            |id, key, label| UiAction::new(id, key, label).with_priority(ActionPriority::Essential);
        if self.confirm_forget {
            return vec![
                essential("confirm", "y", "forget conversation"),
                essential("cancel", "Esc", "cancel"),
            ];
        }
        if self.searching {
            return vec![
                essential("done", "Enter", "done"),
                essential("clear", "Esc", "clear"),
            ];
        }
        vec![
            UiAction::new("browse", "j/k", "browse"),
            UiAction::new("turns", "l/h", "turns"),
            UiAction::new("search", "/", "search"),
            UiAction::new("workspace", "w", "workspace"),
            UiAction::new("view", "v", "view"),
            essential("open", "Enter", "open"),
            UiAction::new("pin", "p", "pin annotations").with_enabled(self.can_restore),
            UiAction::new("continue", "r", "continue"),
            UiAction::new("recheck", "R", "recheck").with_priority(ActionPriority::Secondary),
            UiAction::new("resolve", "d", "resolve").with_priority(ActionPriority::Secondary),
            UiAction::new("forget", "D", "forget…").with_priority(ActionPriority::Secondary),
            UiAction::new("jump", "g", "jump"),
            essential("return", "Esc", "return"),
        ]
    }
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
        let list_width = if wide { (width * 2 / 5).min(72) } else { width };
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
            let mut lines = row.text.lines();
            let spinner = if row.running {
                format!(
                    "{} ",
                    spinner_frame(
                        self.animation_frame
                            .saturating_mul(SPINNER_FRAME_INTERVAL_MS)
                    )
                )
            } else {
                String::new()
            };
            let text = fit_display_width(
                &format!("{marker}{spinner}{}", lines.next().unwrap_or_default()),
                list_width,
            );
            if row_y < y + 1 + list_height {
                buffer.set_text(1, row_y, &text, style);
            }
            if row_y + 1 < y + 1 + list_height {
                buffer.set_text(
                    1,
                    row_y + 1,
                    &fit_display_width(
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
        if height > 0 {
            ActionBar::new(&self.surface_actions())
                .with_context(if self.confirm_forget {
                    "CONFIRM"
                } else if self.searching {
                    "SEARCH"
                } else {
                    "HISTORY"
                })
                .render(
                    buffer,
                    1,
                    y + height,
                    width,
                    &self.theme,
                    &self.theme.ui_style.dialog,
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
        if matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release) {
            return None;
        }
        if self.searching {
            return match event {
                Event::Paste(text) => Self::action(HistoryAction::Query(text.clone())),
                Event::Key(key) => match key.code {
                    KeyCode::Esc => Self::action(HistoryAction::ClearSearch),
                    KeyCode::Enter => Self::action(HistoryAction::EndSearch),
                    KeyCode::Backspace if is_word_backspace(*key) => {
                        Self::action(HistoryAction::DeletePreviousWord)
                    }
                    KeyCode::Backspace => Self::action(HistoryAction::Backspace),
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        Self::action(HistoryAction::Query(ch.to_string()))
                    }
                    _ => None,
                },
                _ => None,
            };
        }
        if !self.confirm_forget {
            if let Event::Mouse(mouse) = event {
                let index = self.list_item_at(mouse.column as usize, mouse.row as usize)?;
                return match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) if index == self.selected => {
                        Self::action(HistoryAction::Open)
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        Self::action(HistoryAction::Select(index))
                    }
                    MouseEventKind::ScrollDown => Self::action(HistoryAction::Next),
                    MouseEventKind::ScrollUp => Self::action(HistoryAction::Previous),
                    _ => None,
                };
            }
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
            KeyCode::Enter => HistoryAction::Open,
            KeyCode::Char('g') => HistoryAction::Jump,
            KeyCode::Char('p' | 's') if self.can_restore => HistoryAction::ShowAnnotations,
            KeyCode::Esc | KeyCode::Char('q') => HistoryAction::Close,
            KeyCode::Char('r') => HistoryAction::Continue,
            KeyCode::Char('R') => HistoryAction::Recheck,
            KeyCode::Char('d') => HistoryAction::Resolve,
            KeyCode::Char('D') => HistoryAction::Forget,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};
    use crossterm::event::{KeyEvent, MouseEvent};

    #[test]
    fn word_backspace_routes_only_active_inline_history_search() {
        let mut panel = InlineHistoryPanel {
            rows: vec![],
            selected: 0,
            detail: String::new(),
            scroll: 0,
            searching: true,
            query: "one two".into(),
            confirm_forget: false,
            title: "History".into(),
            width: 80,
            height: 24,
            theme: Theme::default(),
        };
        for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let key = crossterm::event::KeyEvent::new(KeyCode::Backspace, modifiers);
            assert_eq!(
                panel.handle_event(&Event::Key(key)),
                Some(KeyAction::Single(Action::InlineHistoryAction(
                    HistoryAction::DeletePreviousWord
                )))
            );
            assert_eq!(
                panel.handle_event(&Event::Key(crossterm::event::KeyEvent {
                    kind: KeyEventKind::Release,
                    ..key
                })),
                None
            );
        }
        panel.searching = false;
        assert_eq!(
            panel.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::CONTROL,
            ))),
            None
        );
    }

    #[test]
    fn selection_fills_both_lines_without_crossing_pane_boundary() {
        let mut theme = Theme::default();
        theme.ui_style.picker_selected_item.bg = Some(Color::Rgb {
            r: 91,
            g: 42,
            b: 73,
        });
        let height = 30;

        for (viewport_width, list_width) in [(120, 47), (64, 62)] {
            let mut panel = InlineHistoryPanel {
                rows: vec![
                    InlineHistoryRow { text: "short\ndetails".into(), running: false },
                    InlineHistoryRow { text: format!("{}\n{}", "界".repeat(80), "👋".repeat(80)), running: false },
                ],
                selected: 0,
                detail: "preview".into(),
                scroll: 0,
                searching: false,
                query: String::new(),
                confirm_forget: false,
                can_restore: false,
                animation_started: Instant::now(),
                animation_frame: 0,
                title: "History".into(),
                width: viewport_width,
                height,
                theme: theme.clone(),
            };
            let mut buffer = RenderBuffer::new(viewport_width, height, &theme.style);

            for selected in 0..2 {
                panel.selected = selected;
                panel.draw(&mut buffer).unwrap();

                let row_y = 14
                    + if viewport_width >= 72 {
                        selected * 2
                    } else {
                        0
                    };
                for row_y in row_y..row_y + 2 {
                    for column in 1..=list_width {
                        assert_eq!(
                            buffer.cells[row_y * viewport_width + column].style,
                            theme.ui_style.picker_selected_item,
                            "viewport={viewport_width}, selected={selected}, cell=({column},{row_y})"
                        );
                    }
                    let boundary = &buffer.cells[row_y * viewport_width + list_width + 1];
                    assert_eq!(boundary.c, '│');
                    assert_ne!(boundary.style.bg, theme.ui_style.picker_selected_item.bg);
                }
            }
        }
    }

    #[test]
    fn inline_history_exposes_open_jump_restore_and_mouse_selection() {
        let config = Config::default();
        let editor = Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            100,
            30,
            config,
            Theme::default(),
            vec![Buffer::new(None, "alpha\n".into())],
        )
        .unwrap();
        let mut panel = InlineHistoryPanel::new(
            &editor,
            vec![
                InlineHistoryRow {
                    text: "First\nfile.c:1 · running".into(),
                    running: true,
                },
                InlineHistoryRow {
                    text: "Second\nfile.c:2 · kept".into(),
                    running: false,
                },
            ],
            0,
            "Detail".into(),
            0,
            false,
            String::new(),
            false,
            "Inline history".into(),
            true,
            Instant::now(),
        );
        assert!(panel.is_inline_history());
        for (key, action) in [
            (KeyCode::Enter, HistoryAction::Open),
            (KeyCode::Char('g'), HistoryAction::Jump),
            (KeyCode::Char('s'), HistoryAction::ShowAnnotations),
            (KeyCode::Char('p'), HistoryAction::ShowAnnotations),
        ] {
            assert_eq!(
                panel.handle_event(&Event::Key(KeyEvent::new(key, KeyModifiers::NONE))),
                Some(KeyAction::Single(Action::InlineHistoryAction(action)))
            );
        }
        let row = (0..panel.height)
            .find(|row| panel.list_item_at(1, *row) == Some(1))
            .unwrap();
        assert_eq!(
            panel.handle_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: row as u16,
                modifiers: KeyModifiers::NONE
            })),
            Some(KeyAction::Single(Action::InlineHistoryAction(
                HistoryAction::Select(1)
            )))
        );
        panel.animation_started =
            Instant::now() - std::time::Duration::from_millis(SPINNER_FRAME_INTERVAL_MS);
        panel.animation_frame = 0;
        assert!(panel.tick().unwrap());
        panel.rows[0].running = false;
        assert!(!panel.tick().unwrap());
        panel.can_restore = false;
        assert_eq!(
            panel.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::NONE
            ))),
            None
        );
    }
}
