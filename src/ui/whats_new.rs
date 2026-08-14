//! Branded, responsive release announcements over the existing Markdown renderer.

use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use tokio::sync::oneshot;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    highlighter::{Highlighter, LanguageRegistry},
    log,
    plugin::{
        markdown::{render_markdown_lines_with_highlighter, RenderedTextLine, RenderedTextSpan},
        TextPanelLinkTarget,
    },
    splash::{self, Role},
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
    whats_new::ReleaseNotes,
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    hover_info::hover_span_style,
    paint_rich_text, ActionPriority, Component, UiAction,
};

const MAX_PANEL_WIDTH: usize = 100;
const MAX_PANEL_HEIGHT: usize = 30;
const MIN_PANEL_WIDTH: usize = 26;
const MIN_PANEL_HEIGHT: usize = 9;
const HERO_ROWS: usize = 5;

type ReleaseRefresh = oneshot::Receiver<Result<ReleaseNotes, String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseView {
    Highlights,
    Changelog,
}

/// Theme-aware modal for one exact installed release.
pub struct WhatsNewPanel {
    dialog: Dialog,
    notes: ReleaseNotes,
    view: ReleaseView,
    lines: Vec<RenderedTextLine>,
    scroll: usize,
    selected_link: Option<usize>,
    links: Vec<(u64, TextPanelLinkTarget, usize)>,
    theme: Theme,
    registry: Arc<LanguageRegistry>,
    viewport_width: usize,
    viewport_height: usize,
    refresh: Option<ReleaseRefresh>,
}

impl WhatsNewPanel {
    /// The compact version still needs a readable body and usable close action.
    #[must_use]
    pub(crate) const fn fits(viewport_width: usize, viewport_height: usize) -> bool {
        viewport_width >= MIN_PANEL_WIDTH && viewport_height >= MIN_PANEL_HEIGHT
    }

    pub(crate) fn new(
        editor: &Editor,
        notes: ReleaseNotes,
        refresh: Option<ReleaseRefresh>,
    ) -> Self {
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight();
        let (x, y, width, height) = geometry(viewport_width, viewport_height);
        let theme = editor.theme.clone();
        let style = theme.ui_style.dialog.clone();
        let mut dialog = Dialog::new(
            Some("WHAT’S NEW".to_string()),
            x,
            y,
            width,
            height,
            &style,
            BorderStyle::Rounded,
            &theme,
        )
        .with_surface_theme(&theme, SurfaceRole::Dialog)
        .with_left_aligned_title();
        dialog.set_header_status(Some(format!("v{}", notes.version)));

        let mut panel = Self {
            dialog,
            notes,
            view: ReleaseView::Highlights,
            lines: Vec::new(),
            scroll: 0,
            selected_link: None,
            links: Vec::new(),
            theme,
            registry: editor.language_registry(),
            viewport_width,
            viewport_height,
            refresh,
        };
        panel.reflow();
        panel
    }

    fn body_height(&self) -> usize {
        self.dialog.height.saturating_sub(HERO_ROWS + 1)
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.body_height())
    }

    fn reflow(&mut self) {
        let markdown = match self.view {
            ReleaseView::Highlights => self.notes.highlights_markdown(),
            ReleaseView::Changelog => self.notes.markdown.clone(),
        };
        let width = self.dialog.width.saturating_sub(4);
        let mut highlighter =
            Highlighter::with_registry(&self.theme, Arc::clone(&self.registry)).ok();
        self.lines = render_markdown_lines_with_highlighter(&markdown, width, highlighter.as_mut());
        self.links.clear();
        for (line_index, line) in self.lines.iter().enumerate() {
            for span in &line.spans {
                let Some(link) = &span.link else {
                    continue;
                };
                if self.links.iter().any(|(id, _, _)| *id == link.id) {
                    continue;
                }
                self.links.push((link.id, link.target.clone(), line_index));
            }
        }
        self.selected_link = (!self.links.is_empty()).then_some(0);
        self.scroll = self.scroll.min(self.max_scroll());
        self.update_chrome();
    }

    fn update_chrome(&mut self) {
        let metadata = self.notes.published_at.as_ref().map_or_else(
            || format!("v{}", self.notes.version),
            |date| format!("v{} · {date}", self.notes.version),
        );
        self.dialog.set_header_status(Some(metadata));

        let mut actions = vec![
            UiAction::new("close", "Esc", "Close").with_priority(ActionPriority::Essential),
            UiAction::new("view", "Tab", "Changelog").with_priority(ActionPriority::Essential),
            UiAction::new("github", "o", "GitHub"),
        ];
        if self.max_scroll() > 0 {
            actions.push(UiAction::new("scroll", "j/k", "Scroll"));
        }
        if !self.links.is_empty() {
            actions.push(UiAction::new("link", "Enter", "Open link"));
        }
        self.dialog.set_actions(actions);
    }

    fn switch_view(&mut self) {
        self.view = match self.view {
            ReleaseView::Highlights => ReleaseView::Changelog,
            ReleaseView::Changelog => ReleaseView::Highlights,
        };
        self.scroll = 0;
        self.reflow();
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
    }

    fn select_link(&mut self, delta: isize) {
        if self.links.is_empty() {
            return;
        }
        let current = self.selected_link.unwrap_or(0) as isize;
        let count = self.links.len() as isize;
        let next = (current + delta).rem_euclid(count) as usize;
        self.selected_link = Some(next);
        let line = self.links[next].2;
        let body_height = self.body_height().max(1);
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + body_height {
            self.scroll = line + 1 - body_height;
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn selected_link_id(&self) -> Option<u64> {
        self.selected_link
            .and_then(|selected| self.links.get(selected))
            .map(|(id, _, _)| *id)
    }

    fn open_selected_link(&self) -> Option<KeyAction> {
        let (_, target, _) = self.links.get(self.selected_link?)?;
        match target {
            TextPanelLinkTarget::ExternalUrl(url) => {
                Some(KeyAction::Single(Action::OpenExternalUrl(url.to_string())))
            }
            TextPanelLinkTarget::File { .. } => None,
        }
    }

    fn centered_text(&self, buffer: &mut RenderBuffer, row: usize, text: &str, style: &Style) {
        let available = self.dialog.width.saturating_sub(4);
        let text = truncate_display_width(text, available);
        let x = self.dialog.x + 1 + self.dialog.width.saturating_sub(display_width(&text)) / 2;
        buffer.set_text(x, self.dialog.y + 1 + row, &text, style);
    }

    fn draw_hero(&self, buffer: &mut RenderBuffer) {
        let palette = splash::palette(&self.theme);
        let brand = "red";
        let brand_width = display_width(brand) + 2;
        let x = self.dialog.x + 1 + self.dialog.width.saturating_sub(brand_width) / 2;
        let y = self.dialog.y + 1;
        buffer.set_text(x, y, brand, palette.style(Role::Mark));
        buffer.set_text(
            x + display_width(brand) + 1,
            y,
            "●",
            palette.style(Role::Dot),
        );

        let title_style = Style {
            bold: true,
            ..self.theme.ui_style.dialog_title.clone()
        };
        self.centered_text(buffer, 1, "What’s new in Red", &title_style);
        self.centered_text(
            buffer,
            2,
            "everything your fingers expect — and a little more",
            palette.style(Role::Muted),
        );

        let (first, second) = if self.dialog.width >= 34 {
            (" Highlights ", " Full changelog ")
        } else {
            (" New ", " Changes ")
        };
        let tabs_width = display_width(first) + display_width(second) + 3;
        let tabs_x = self.dialog.x + 1 + self.dialog.width.saturating_sub(tabs_width) / 2;
        let active = palette.style(Role::Key).with_bg(self.dialog.style.bg);
        let inactive = palette.style(Role::Muted).with_bg(self.dialog.style.bg);
        let first_style = if self.view == ReleaseView::Highlights {
            &active
        } else {
            &inactive
        };
        let second_style = if self.view == ReleaseView::Changelog {
            &active
        } else {
            &inactive
        };
        let y = self.dialog.y + 4;
        buffer.set_text(tabs_x, y, first, first_style);
        buffer.set_text(tabs_x + display_width(first), y, " · ", &inactive);
        buffer.set_text(tabs_x + display_width(first) + 3, y, second, second_style);

        let rule_width = self.dialog.width.saturating_sub(4);
        buffer.set_text(
            self.dialog.x + 3,
            self.dialog.y + HERO_ROWS,
            &"─".repeat(rule_width),
            palette.style(Role::Rule),
        );
    }

    fn draw_body(&self, buffer: &mut RenderBuffer) {
        let x = self.dialog.x + 3;
        let y = self.dialog.y + HERO_ROWS + 1;
        let width = self.dialog.width.saturating_sub(4);
        let selected_link_id = self.selected_link_id();

        for (offset, line) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(self.body_height())
            .enumerate()
        {
            paint_rich_text(
                buffer,
                x,
                y + offset,
                width,
                line,
                |span: &RenderedTextSpan| {
                    let mut style = hover_span_style(span, &self.theme);
                    if span
                        .link
                        .as_ref()
                        .is_some_and(|link| Some(link.id) == selected_link_id)
                    {
                        style.bold = true;
                    }
                    style
                },
            );
        }
    }

    fn link_at(&self, column: usize, row: usize) -> Option<usize> {
        let first_x = self.dialog.x + 3;
        let first_y = self.dialog.y + HERO_ROWS + 1;
        if column < first_x || row < first_y {
            return None;
        }
        let line = self.lines.get(self.scroll + row - first_y)?;
        let mut x = first_x;
        for span in &line.spans {
            let end = x + display_width(&span.text);
            if (x..end).contains(&column) {
                let link_id = span.link.as_ref()?.id;
                return self.links.iter().position(|(id, _, _)| *id == link_id);
            }
            x = end;
        }
        None
    }
}

impl Component for WhatsNewPanel {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        self.draw_hero(buffer);
        self.draw_body(buffer);
        Ok(())
    }

    fn tick(&mut self) -> anyhow::Result<bool> {
        let Some(refresh) = &mut self.refresh else {
            return Ok(false);
        };
        match refresh.try_recv() {
            Ok(Ok(notes)) => {
                self.refresh = None;
                self.notes = notes;
                self.reflow();
                Ok(true)
            }
            Ok(Err(error)) => {
                self.refresh = None;
                log!("could not refresh release notes: {error}");
                Ok(false)
            }
            Err(oneshot::error::TryRecvError::Empty) => Ok(false),
            Err(oneshot::error::TryRecvError::Closed) => {
                self.refresh = None;
                Ok(false)
            }
        }
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let refresh = || Some(KeyAction::Single(Action::Refresh));
        match event {
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Char('q'), _) => {
                    Some(KeyAction::Single(Action::CloseDialog))
                }
                (KeyCode::Char('o'), _) => Some(KeyAction::Single(Action::OpenExternalUrl(
                    self.notes.release_url.clone(),
                ))),
                (KeyCode::Tab | KeyCode::BackTab, _) => {
                    self.switch_view();
                    refresh()
                }
                (KeyCode::Down | KeyCode::Char('j'), _) => {
                    self.scroll_by(1);
                    refresh()
                }
                (KeyCode::Up | KeyCode::Char('k'), _) => {
                    self.scroll_by(-1);
                    refresh()
                }
                (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    self.scroll_by(self.body_height().max(1) as isize);
                    refresh()
                }
                (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    self.scroll_by(-(self.body_height().max(1) as isize));
                    refresh()
                }
                (KeyCode::Home | KeyCode::Char('g'), _) => {
                    self.scroll = 0;
                    refresh()
                }
                (KeyCode::End | KeyCode::Char('G'), _) => {
                    self.scroll = self.max_scroll();
                    refresh()
                }
                (KeyCode::Char(']'), _) => {
                    self.select_link(1);
                    refresh()
                }
                (KeyCode::Char('['), _) => {
                    self.select_link(-1);
                    refresh()
                }
                (KeyCode::Enter, _) => self.open_selected_link(),
                _ => None,
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => {
                    self.scroll_by(3);
                    refresh()
                }
                MouseEventKind::ScrollUp => {
                    self.scroll_by(-3);
                    refresh()
                }
                MouseEventKind::Down(_) => {
                    if let Some(link) = self.link_at(mouse.column as usize, mouse.row as usize) {
                        self.selected_link = Some(link);
                        self.open_selected_link()
                    } else if !(self.dialog.x..self.dialog.x + self.dialog.width + 2)
                        .contains(&(mouse.column as usize))
                        || !(self.dialog.y..self.dialog.y + self.dialog.height + 2)
                            .contains(&(mouse.row as usize))
                    {
                        Some(KeyAction::Single(Action::CloseDialog))
                    } else {
                        None
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.viewport_width = viewport_width;
        self.viewport_height = viewport_height;
        let (x, y, width, height) = geometry(viewport_width, viewport_height);
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
        self.reflow();
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.reflow();
    }
}

fn geometry(viewport_width: usize, viewport_height: usize) -> (usize, usize, usize, usize) {
    let width = viewport_width.saturating_sub(4).min(MAX_PANEL_WIDTH);
    let height = viewport_height.saturating_sub(3).min(MAX_PANEL_HEIGHT);
    let x = viewport_width.saturating_sub(width + 2) / 2;
    let y = viewport_height.saturating_sub(height + 2) / 2;
    (x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};

    fn editor(width: usize, height: usize) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        Editor::with_size(
            lsp,
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap()
    }

    fn rendered_text(buffer: &RenderBuffer) -> String {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| row.iter().map(|cell| cell.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn release_panel_renders_brand_version_and_markdown_highlights() {
        let editor = editor(100, 28);
        let notes = ReleaseNotes::bundled(env!("CARGO_PKG_VERSION"), None);
        let panel = WhatsNewPanel::new(&editor, notes, None);
        let mut buffer = RenderBuffer::new(100, 28, &Style::default());

        panel.draw(&mut buffer).unwrap();

        let text = rendered_text(&buffer);
        assert!(text.contains("WHAT’S NEW"));
        assert!(text.contains("What’s new in Red"));
        assert!(text.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("●"));
        assert!(text.contains("Highlights"));
    }

    #[test]
    fn tab_switches_between_highlights_and_the_complete_changelog() {
        let editor = editor(90, 26);
        let notes = ReleaseNotes {
            version: "1.0.0".to_string(),
            markdown:
                "## [1.0.0](https://example.test)\n\n### Features\n\n- Visible feature\n\n### Refactoring\n\n- Full detail"
                    .to_string(),
            release_url: "https://github.com/codersauce/red/releases/tag/v1.0.0".to_string(),
            published_at: None,
        };
        let mut panel = WhatsNewPanel::new(&editor, notes, None);

        assert!(!panel
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.text.contains("Full detail")));
        panel.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )));

        assert!(panel
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.text.contains("Full detail")));
    }

    #[test]
    fn release_panel_reflows_after_resize_without_overflowing() {
        let editor = editor(100, 30);
        let notes = ReleaseNotes::bundled(env!("CARGO_PKG_VERSION"), None);
        let mut panel = WhatsNewPanel::new(&editor, notes, None);

        panel.resize(38, 12);

        assert!(panel.dialog.x + panel.dialog.width + 2 <= 38);
        assert!(panel.dialog.y + panel.dialog.height + 2 <= 12);
        let mut buffer = RenderBuffer::new(38, 12, &Style::default());
        panel.draw(&mut buffer).unwrap();
    }

    #[test]
    fn compact_release_panel_preserves_both_rounded_border_corners() {
        let editor = editor(26, 10);
        let notes = ReleaseNotes::bundled(env!("CARGO_PKG_VERSION"), None);
        let panel = WhatsNewPanel::new(&editor, notes, None);
        let mut buffer = RenderBuffer::new(26, 10, &Style::default());

        panel.draw(&mut buffer).unwrap();

        let first = panel.dialog.y * buffer.width + panel.dialog.x;
        let last = first + panel.dialog.width + 1;
        assert_eq!(buffer.cells[first].c, '╭');
        assert_eq!(buffer.cells[last].c, '╮');
        let text = rendered_text(&buffer);
        assert!(text.contains("New"));
        assert!(text.contains("Changes"));
    }

    #[test]
    fn background_release_refresh_reflows_the_existing_panel() {
        let editor = editor(90, 26);
        let initial = ReleaseNotes::bundled(env!("CARGO_PKG_VERSION"), None);
        let (sender, receiver) = oneshot::channel();
        let mut panel = WhatsNewPanel::new(&editor, initial.clone(), Some(receiver));
        let updated = ReleaseNotes {
            markdown: "### Features\n\n- Fresh GitHub release".to_string(),
            published_at: Some("Aug 13, 2026".to_string()),
            ..initial
        };
        sender.send(Ok(updated)).unwrap();

        assert!(panel.tick().unwrap());
        assert!(panel
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.text.contains("Fresh GitHub release")));
        assert_eq!(panel.notes.published_at.as_deref(), Some("Aug 13, 2026"));
    }
}
