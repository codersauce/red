//! Bottom-docked, read-only browser for retained inline conversations.

use std::{sync::Arc, time::Instant};

mod detail;
pub(crate) use detail::{HistoryBlock, HistoryDetail, HistoryStatus, HistoryTone};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    spinner_frame, ActionBar, ActionPriority, Component, UiAction, SPINNER_FRAME_INTERVAL_MS,
};
use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    highlighter::{Highlighter, LanguageRegistry},
    inline_history::HistoryAction,
    keyboard::is_word_backspace,
    plugin::{markdown::RenderedTextLine, TextPanelLinkTarget},
    theme::Theme,
    unicode_utils::{display_width, fit_display_width},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

pub(crate) struct InlineHistoryRow {
    pub text: String,
    pub running: bool,
}

pub(crate) struct InlineHistoryPanel {
    rows: Vec<InlineHistoryRow>,
    selected: usize,
    detail: HistoryDetail,
    rendered: Vec<RenderedTextLine>,
    registry: Arc<LanguageRegistry>,
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
        detail: HistoryDetail,
        scroll: usize,
        searching: bool,
        query: String,
        confirm_forget: bool,
        title: String,
        can_restore: bool,
        animation_started: Instant,
    ) -> Self {
        let mut panel = Self {
            rows,
            selected,
            detail,
            rendered: Vec::new(),
            registry: editor.language_registry(),
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
        };
        panel.reflow();
        panel
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
        let list_height = if wide { body } else { body.min(1) };
        let item_height = if wide { 2 } else { 1 };
        if column < 1 || column > list_width || row <= y || row >= y + 1 + list_height {
            return None;
        }
        let first = self
            .selected
            .saturating_sub((list_height / item_height).max(1).saturating_sub(1));
        let index = first + (row - y - 1) / item_height;
        (index < self.rows.len()).then_some(index)
    }

    fn detail_geometry(&self) -> (usize, usize, usize, usize) {
        let width = self.width.saturating_sub(2);
        let height = (self.height / 2)
            .clamp(5, 16)
            .min(self.height.saturating_sub(2));
        let y = self.height.saturating_sub(height + 2);
        let body = height.saturating_sub(1);
        if width >= 70 {
            let list_width = (width * 2 / 5).min(72);
            (
                list_width + 3,
                y + 1,
                width.saturating_sub(list_width + 2),
                body,
            )
        } else {
            let list_height = body.min(1);
            (
                1,
                y + 1 + list_height,
                width,
                body.saturating_sub(list_height),
            )
        }
    }

    fn reflow(&mut self) {
        let (_, _, width, height) = self.detail_geometry();
        let mut highlighter =
            Highlighter::with_registry(&self.theme, Arc::clone(&self.registry)).ok();
        self.rendered = self.detail.render(
            width,
            self.width.saturating_sub(2) < 70,
            &self.theme,
            highlighter.as_mut(),
        );
        self.scroll = self.scroll.min(self.rendered.len().saturating_sub(height));
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    fn detail_link_at(&self, column: usize, row: usize) -> Option<KeyAction> {
        let (x, y, width, height) = self.detail_geometry();
        if !(x..x + width).contains(&column) || !(y..y + height).contains(&row) {
            return None;
        }
        let line = self.rendered.get(self.scroll + row - y)?;
        let mut start = x;
        for span in &line.spans {
            let end = (start + display_width(&span.text)).min(x + width);
            if (start..end).contains(&column) {
                let link = span.link.as_ref()?;
                if link.id == detail::SOURCE_LINK {
                    return Self::action(HistoryAction::Jump);
                }
                return match &link.target {
                    TextPanelLinkTarget::File { path, location } => {
                        Self::action(self.detail.file_target(path, location))
                    }
                    TextPanelLinkTarget::ExternalUrl(url) => {
                        Some(KeyAction::Single(Action::OpenExternalUrl(url.clone())))
                    }
                };
            }
            start = end;
        }
        None
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
            UiAction::new("view", "v", self.detail.view.label()),
            essential("open", "Enter", self.detail.open_label),
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
        } else if width < 70 && !self.rows.is_empty() {
            format!("Inline history · {}/{}", self.selected + 1, self.rows.len())
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
        let list_height = if wide { body } else { body.min(1) };
        let item_height = if wide { 2 } else { 1 };
        let visible_items = (list_height / item_height).max(1);
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
            let row_y = y + 1 + (offset - first) * item_height;
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
            if wide && row_y + 1 < y + 1 + list_height {
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
        if wide {
            for row in 0..body {
                buffer.set_text(list_width + 1, y + 1 + row, "│", &self.theme.ui_style.muted);
            }
        }
        let (detail_x, detail_y, detail_width, detail_height) = self.detail_geometry();
        for (offset, row) in self
            .rendered
            .iter()
            .skip(self.scroll)
            .take(detail_height)
            .enumerate()
        {
            super::hover_info::render_line(
                buffer,
                detail_x,
                detail_y + offset,
                detail_width,
                row,
                false,
                &self.theme,
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
        self.reflow();
        true
    }
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.reflow();
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
                let column = mouse.column as usize;
                let row = mouse.row as usize;
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    if let Some(action) = self.detail_link_at(column, row) {
                        return Some(action);
                    }
                }
                let (x, y, width, height) = self.detail_geometry();
                if (x..x + width).contains(&column) && (y..y + height).contains(&row) {
                    return match mouse.kind {
                        MouseEventKind::ScrollDown => Self::action(HistoryAction::ScrollDown),
                        MouseEventKind::ScrollUp => Self::action(HistoryAction::ScrollUp),
                        _ => None,
                    };
                }
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

    fn rich_panel(width: usize, theme: Theme) -> InlineHistoryPanel {
        let config = Config::default();
        let editor = Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            width,
            40,
            config,
            theme,
            vec![Buffer::new(None, String::new())],
        )
        .unwrap();
        InlineHistoryPanel::new(
            &editor,
            vec![InlineHistoryRow {
                text: "Explain this\nmetadata".into(),
                running: false,
            }],
            0,
            HistoryDetail {
                location: Some("src/main.rs:10–12".into()),
                can_jump: true,
                cwd: "/workspace".into(),
                statuses: vec![
                    HistoryStatus::new("✓ Applied", HistoryTone::Success),
                    HistoryStatus::new("Unsaved", HistoryTone::Warning),
                    HistoryStatus::new("source unchanged", HistoryTone::Muted),
                ],
                blocks: vec![
                    HistoryBlock::Request("Explain this".into()),
                    HistoryBlock::Markdown(
                        "**Useful answer** with [another file](src/other.rs:7:2).".into(),
                    ),
                    HistoryBlock::Code {
                        file: "main.rs".into(),
                        source: "fn demo() { return 1; }".into(),
                    },
                    HistoryBlock::Diff {
                        file: "main.rs".into(),
                        before: "fn old() {}\n".into(),
                        after: "fn new() {}\n".into(),
                        label: "after".into(),
                    },
                ],
                open_label: "review changes",
                ..HistoryDetail::default()
            },
            0,
            false,
            String::new(),
            false,
            "Inline history".into(),
            true,
            Instant::now(),
        )
    }

    fn line_text(line: &RenderedTextLine) -> String {
        line.spans.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn inline_history_renders_semantic_status_markdown_code_and_diffs() {
        use crate::plugin::markdown::TextPanelSpanStyle;
        use crate::theme::{DiffPalette, Style, TokenStyle};
        for name in ["themes/one-dark-pro.json", "themes/atom-one-light.json"] {
            let mut theme = crate::theme::parse_vscode_theme(name).unwrap();
            let keyword = Color::Rgb {
                r: 19,
                g: 87,
                b: 143,
            };
            theme.token_styles.insert(
                0,
                TokenStyle {
                    name: None,
                    scope: vec!["keyword".into()],
                    style: Style {
                        fg: Some(keyword),
                        ..Style::default()
                    },
                },
            );
            let panel = rich_panel(120, theme.clone());
            let spans = || panel.rendered.iter().flat_map(|line| &line.spans);
            assert!(spans().any(|span| span.style == TextPanelSpanStyle::Strong));
            assert!(spans().any(|span| span.style == TextPanelSpanStyle::Code
                && span
                    .syntax_style
                    .as_ref()
                    .is_some_and(|style| style.fg == Some(keyword))));
            let palette = DiffPalette::new(&theme);
            for (needle, background) in [
                ("-fn old", palette.removed.bg),
                ("+fn new", palette.added.bg),
            ] {
                let line = panel
                    .rendered
                    .iter()
                    .find(|line| line_text(line).starts_with(needle))
                    .unwrap();
                assert!(line
                    .spans
                    .iter()
                    .all(
                        |span| super::super::hover_info::hover_span_style(span, &theme).bg
                            == background
                    ));
            }
            let applied = spans().find(|span| span.text.contains("Applied")).unwrap();
            let unsaved = spans().find(|span| span.text == "Unsaved").unwrap();
            assert_ne!(
                applied.syntax_style.as_ref().unwrap().fg,
                unsaved.syntax_style.as_ref().unwrap().fg
            );
            let source = &panel.rendered[0].spans[0];
            assert!(super::super::hover_info::hover_span_style(source, &theme).underline);
        }
    }

    #[test]
    fn inline_history_links_and_scroll_hit_testing_follow_the_rendered_layout() {
        let mut panel = rich_panel(120, Theme::default());
        for width in [120, 64, 34, 8, 3] {
            panel.resize(width, 40);
            panel.scroll = 0;
            let (x, y, available, height) = panel.detail_geometry();
            assert!(panel
                .rendered
                .iter()
                .all(|line| display_width(&line_text(line)) <= available));
            assert_eq!(
                panel.detail_link_at(x, y),
                InlineHistoryPanel::action(HistoryAction::Jump)
            );
            let mut frame = RenderBuffer::new(width, 40, &panel.theme.style);
            panel.draw(&mut frame).unwrap();
            assert!(frame.cells[y * width + x].style.underline);
            if (34..72).contains(&width) {
                assert!(!panel
                    .rendered
                    .iter()
                    .any(|line| line_text(line).contains("You: Explain this")));
            }
            let (row, column) = panel
                .rendered
                .iter()
                .enumerate()
                .find_map(|(row, line)| {
                    let mut column = 0;
                    for span in &line.spans {
                        if span
                            .link
                            .as_ref()
                            .is_some_and(|link| link.id != detail::SOURCE_LINK)
                        {
                            return Some((row, column));
                        }
                        column += display_width(&span.text);
                    }
                    None
                })
                .unwrap();
            panel.scroll = row.min(panel.rendered.len().saturating_sub(height));
            assert_eq!(
                panel.detail_link_at(x + column, y + row - panel.scroll),
                InlineHistoryPanel::action(HistoryAction::FollowFile {
                    path: "/workspace/src/other.rs".into(),
                    line: Some(7),
                    column: Some(2)
                })
            );
            assert_eq!(
                panel.handle_event(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: x as u16,
                    row: y as u16,
                    modifiers: KeyModifiers::NONE
                })),
                InlineHistoryPanel::action(HistoryAction::ScrollDown)
            );
            panel.scroll = usize::MAX;
            panel.reflow();
            assert_eq!(panel.scroll, panel.rendered.len().saturating_sub(height));
        }
        panel.detail.can_jump = false;
        panel.reflow();
        panel.scroll = 0;
        let (x, y, _, _) = panel.detail_geometry();
        assert_eq!(panel.detail_link_at(x, y), None);
    }

    #[test]
    fn word_backspace_routes_only_active_inline_history_search() {
        let mut panel = rich_panel(80, Theme::default());
        panel.searching = true;
        panel.query = "one two".into();
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
                    InlineHistoryRow {
                        text: "short\ndetails".into(),
                        running: false,
                    },
                    InlineHistoryRow {
                        text: format!("{}\n{}", "界".repeat(80), "👋".repeat(80)),
                        running: false,
                    },
                ],
                selected: 0,
                detail: "preview".into(),
                rendered: Vec::new(),
                registry: Arc::new(LanguageRegistry::bundled()),
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
                let selected_height = if viewport_width >= 72 { 2 } else { 1 };
                for row_y in row_y..row_y + selected_height {
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
