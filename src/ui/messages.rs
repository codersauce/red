//! Read-only notification history with a stable selection and a full-text detail pane.

use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    geometry::ScreenRect,
    wrap_text, ActionBar, ActionPriority, Component, UiAction,
};
use crate::{
    config::KeyAction,
    editor::{Action, RenderBuffer},
    keyboard::is_word_backspace,
    notification::{MessageAction, NotificationCounts, NotificationId},
    theme::Theme,
    unicode_utils::{fit_display_width, truncate_display_width},
};

pub(crate) struct MessageRow {
    pub id: NotificationId,
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
    pub can_cancel: bool,
    pub feedback: Option<String>,
}

pub(crate) struct MessagesPanel {
    view: MessagesView,
    width: usize,
    height: usize,
    theme: Theme,
}

struct MessagesLayout {
    dialog: ScreenRect,
    list: ScreenRect,
    detail: ScreenRect,
    visible: usize,
    first: usize,
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

    // Drawing and pointer hit testing must use the same responsive geometry.
    fn layout(&self) -> Option<MessagesLayout> {
        let width = self.width.saturating_sub(4).min(116);
        let height = self.height.saturating_sub(2).min(24);
        if width < 4 || height < 3 {
            return None;
        }
        let x = self.width.saturating_sub(width + 2) / 2;
        let y = self.height.saturating_sub(height + 2) / 2;
        let body = height.saturating_sub(1);
        let wide = width >= 72;
        let list = ScreenRect {
            x: x + 1,
            y: y + 1,
            width: if wide { (width * 2 / 5).min(48) } else { width },
            height: if wide { body } else { (body / 2).max(1) },
        };
        let detail = if wide {
            ScreenRect {
                x: x + list.width + 3,
                y: y + 1,
                width: width.saturating_sub(list.width + 2),
                height: body,
            }
        } else {
            ScreenRect {
                x: x + 1,
                y: y + 1 + list.height,
                width,
                height: body.saturating_sub(list.height),
            }
        };
        let visible = (list.height / 2).max(1);
        Some(MessagesLayout {
            dialog: ScreenRect {
                x,
                y,
                width,
                height,
            },
            list,
            detail,
            visible,
            first: self.view.selected.saturating_sub(visible.saturating_sub(1)),
        })
    }

    fn handle_mouse(&self, event: &MouseEvent) -> Option<KeyAction> {
        let layout = self.layout()?;
        let column = usize::from(event.column);
        let row = usize::from(event.row);
        let action = match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.select_at(&layout, column, row),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => self.scroll_at(
                &layout,
                column,
                row,
                event.kind == MouseEventKind::ScrollDown,
            ),
            _ => None,
        };
        action.and_then(Self::action)
    }

    fn select_at(
        &self,
        layout: &MessagesLayout,
        column: usize,
        row: usize,
    ) -> Option<MessageAction> {
        if !layout.list.contains(column, row) {
            return None;
        }
        let offset = (row - layout.list.y) / 2;
        if offset >= layout.visible {
            return None;
        }
        let index = layout.first + offset;
        Some(MessageAction::Select(self.view.rows.get(index)?.id))
    }

    fn scroll_at(
        &self,
        layout: &MessagesLayout,
        column: usize,
        row: usize,
        down: bool,
    ) -> Option<MessageAction> {
        if layout.list.contains(column, row) {
            return Some(if down {
                MessageAction::Next
            } else {
                MessageAction::Previous
            });
        }
        if !layout.detail.contains(column, row) {
            return None;
        }
        let detail = wrap_text(&self.view.detail, layout.detail.width.max(1));
        let maximum = detail.rows.len().saturating_sub(layout.detail.height);
        // Start from the visible offset, even if keyboard scrolling overshot it.
        let scroll = self.view.scroll.min(maximum);
        Some(MessageAction::ScrollTo(if down {
            scroll.saturating_add(5).min(maximum)
        } else {
            scroll.saturating_sub(5)
        }))
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
        let mut actions = vec![
            UiAction::new("browse", "j/k", "browse"),
            UiAction::new("search", "/", "search"),
            UiAction::new("filter", "f", "filter"),
            essential("acknowledge", "Enter", "acknowledge"),
            UiAction::new("copy", "y", "copy"),
            UiAction::new("clear", "D", "clear inactive").with_priority(ActionPriority::Secondary),
            UiAction::new("scroll", "Ctrl-d/u", "details").with_priority(ActionPriority::Secondary),
            essential("close", "Esc", "return"),
        ];
        if self.view.can_cancel {
            actions.insert(0, essential("cancel", "Ctrl-c", "cancel command"));
        }
        actions
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let Some(layout) = self.layout() else {
            return Ok(());
        };
        let ScreenRect {
            x,
            y,
            width,
            height,
        } = layout.dialog;
        let list = layout.list;
        let detail = layout.detail;
        let visible = layout.visible;
        let first = layout.first;
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

        if self.view.rows.is_empty() {
            buffer.set_text(
                x + 1,
                y + 1,
                &truncate_display_width("No matching messages", list.width),
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
            if row_y < y + 1 + list.height {
                buffer.set_text(
                    x + 1,
                    row_y,
                    &fit_display_width(&format!("{marker}{}", row.summary), list.width),
                    style,
                );
            }
            if row_y + 1 < y + 1 + list.height {
                buffer.set_text(
                    x + 1,
                    row_y + 1,
                    &fit_display_width(&format!("  {}", row.metadata), list.width),
                    if selected {
                        style
                    } else {
                        &self.theme.ui_style.muted
                    },
                );
            }
        }
        if detail.x > list.x {
            for offset in 0..list.height {
                buffer.set_text(
                    list.x + list.width,
                    list.y + offset,
                    "│",
                    &self.theme.ui_style.muted,
                );
            }
        }
        let wrapped = wrap_text(&self.view.detail, detail.width.max(1));
        let scroll = self
            .view
            .scroll
            .min(wrapped.rows.len().saturating_sub(detail.height));
        for (offset, row) in wrapped
            .rows
            .iter()
            .skip(scroll)
            .take(detail.height)
            .enumerate()
        {
            buffer.set_text(
                detail.x,
                detail.y + offset,
                &truncate_display_width(row, detail.width),
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
        if let Event::Mouse(mouse) = event {
            return self.handle_mouse(mouse);
        }
        if matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release) {
            return None;
        }
        if self.view.searching {
            return match event {
                Event::Paste(text) => Self::action(MessageAction::Query(text.clone())),
                Event::Key(key) => match key.code {
                    KeyCode::Esc => Self::action(MessageAction::ClearSearch),
                    KeyCode::Enter => Self::action(MessageAction::EndSearch),
                    KeyCode::Backspace if is_word_backspace(*key) => {
                        Self::action(MessageAction::DeletePreviousWord)
                    }
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
                KeyCode::Char('c') if self.view.can_cancel => {
                    Some(KeyAction::Single(Action::CancelShellCommand))
                }
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
    use crate::{
        color::Color,
        notification::{
            Notice, NotificationCenter, NotificationSource, NotificationTime, Severity,
        },
    };

    fn message_row(
        center: &mut NotificationCenter,
        summary: String,
        metadata: String,
    ) -> MessageRow {
        let id = center
            .publish(
                Notice::new(NotificationSource::Editor, Severity::Info, &summary),
                NotificationTime::now(),
            )
            .unwrap();
        MessageRow {
            id,
            summary,
            metadata,
        }
    }

    fn assert_mouse(
        panel: &mut MessagesPanel,
        kind: MouseEventKind,
        (column, row): (u16, u16),
        expected: Option<MessageAction>,
    ) {
        let event = Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            panel.handle_event(&event),
            expected.and_then(MessagesPanel::action)
        );
    }

    fn mouse_panel() -> MessagesPanel {
        let mut center = NotificationCenter::default();
        MessagesPanel::new(
            MessagesView {
                rows: (0..18)
                    .map(|index| {
                        message_row(&mut center, format!("message {index}"), "details".into())
                    })
                    .collect(),
                selected: 0,
                detail: "line\n".repeat(59) + "line",
                scroll: 0,
                query: String::new(),
                searching: false,
                filter: "all",
                counts: NotificationCounts::default(),
                can_cancel: false,
                feedback: None,
            },
            120,
            30,
            &Theme::default(),
        )
    }

    #[test]
    fn mouse_selection_tracks_visible_rows_and_resizes_in_both_layouts() {
        let mut panel = mouse_panel();
        panel.view.rows.reverse();
        let click = MouseEventKind::Down(MouseButton::Left);
        for (width, selected, row, expected) in [
            (120, 0, 5, 1),
            (120, 14, 3, 4),
            (64, 14, 3, 10),
            (64, 0, 5, 1),
        ] {
            panel.resize(width, 30);
            panel.view.selected = selected;
            let expected = panel.view.rows[expected].id;
            for searching in [false, true] {
                panel.view.searching = searching;
                for line in [row, row + 1] {
                    assert_mouse(
                        &mut panel,
                        click,
                        (2, line),
                        Some(MessageAction::Select(expected)),
                    );
                }
            }
        }
        panel.resize(120, 30);
        // The odd final list line is blank, even when more messages exist.
        assert_mouse(&mut panel, click, (2, 25), None);
        panel.view.rows.truncate(2);
        for width in [120, 64] {
            panel.resize(width, 30);
            for position in [(1, 3), (2, 2), (2, 7), (2, 26), (0, 0), (63, 29)] {
                assert_mouse(&mut panel, click, position, None);
            }
            for kind in [
                MouseEventKind::Down(MouseButton::Right),
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::Moved,
            ] {
                assert_mouse(&mut panel, kind, (2, 3), None);
            }
        }
        panel.resize(120, 30);
        for column in [48, 49, 50] {
            assert_mouse(&mut panel, click, (column, 3), None);
        }
        panel.view.rows.clear();
        assert_mouse(&mut panel, click, (2, 3), None);
        panel.resize(1, 1);
        assert_mouse(&mut panel, MouseEventKind::ScrollDown, (0, 0), None);
    }

    #[test]
    fn mouse_wheel_targets_its_pane_and_scrolls_up_from_the_visible_bottom() {
        let mut panel = mouse_panel();
        for (width, column, row, maximum) in [(120, 50, 3, 37), (64, 2, 14, 48)] {
            panel.resize(width, 30);
            panel.view.searching = width == 64;
            for (kind, expected) in [
                (MouseEventKind::ScrollUp, MessageAction::Previous),
                (MouseEventKind::ScrollDown, MessageAction::Next),
            ] {
                assert_mouse(&mut panel, kind, (2, 3), Some(expected));
            }
            for (scroll, kind, expected) in [
                (0, MouseEventKind::ScrollUp, 0),
                (0, MouseEventKind::ScrollDown, 5),
                (maximum, MouseEventKind::ScrollDown, maximum),
                (usize::MAX, MouseEventKind::ScrollUp, maximum - 5),
            ] {
                panel.view.scroll = scroll;
                assert_mouse(
                    &mut panel,
                    kind,
                    (column, row),
                    Some(MessageAction::ScrollTo(expected)),
                );
            }
            assert_mouse(&mut panel, MouseEventKind::ScrollDown, (2, 26), None);
        }
    }

    #[test]
    fn word_backspace_routes_only_active_message_search() {
        let mut panel = MessagesPanel::new(
            MessagesView {
                rows: vec![],
                selected: 0,
                detail: String::new(),
                scroll: 0,
                query: "one two".into(),
                searching: true,
                filter: "all",
                counts: NotificationCounts::default(),
                can_cancel: false,
                feedback: None,
            },
            80,
            24,
            &Theme::default(),
        );
        for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let key = crossterm::event::KeyEvent::new(KeyCode::Backspace, modifiers);
            assert_eq!(
                panel.handle_event(&Event::Key(key)),
                Some(KeyAction::Single(Action::MessageHistory(
                    MessageAction::DeletePreviousWord
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
        panel.view.searching = false;
        assert_eq!(
            panel.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::ALT,
            ))),
            None
        );
    }

    #[test]
    fn control_c_only_cancels_a_selected_running_shell_command() {
        let mut panel = MessagesPanel::new(
            MessagesView {
                rows: vec![],
                selected: 0,
                detail: String::new(),
                scroll: 0,
                query: String::new(),
                searching: false,
                filter: "all",
                counts: NotificationCounts::default(),
                can_cancel: false,
                feedback: None,
            },
            80,
            24,
            &Theme::default(),
        );
        let event = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(panel.handle_event(&event), None);
        panel.view.can_cancel = true;
        assert_eq!(
            panel.handle_event(&event),
            Some(KeyAction::Single(Action::CancelShellCommand))
        );
        assert!(panel
            .surface_actions()
            .iter()
            .any(|action| action.id == "cancel"));
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
        let mut center = NotificationCenter::default();

        for (viewport_width, list_width) in [(120, 46), (64, 60)] {
            let mut panel = MessagesPanel::new(
                MessagesView {
                    rows: vec![
                        message_row(&mut center, "short".into(), "details".into()),
                        message_row(&mut center, "界".repeat(80), "👋".repeat(80)),
                    ],
                    selected: 0,
                    detail: "preview".into(),
                    scroll: 0,
                    query: String::new(),
                    searching: false,
                    filter: "all",
                    counts: NotificationCounts::default(),
                    can_cancel: false,
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
