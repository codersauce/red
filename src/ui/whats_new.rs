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
const MIN_COMFORTABLE_PANEL_HEIGHT: usize = 20;
const SPACIOUS_CHROME_HEIGHT: usize = 10;

type ReleaseRefresh = oneshot::Receiver<Result<ReleaseNotes, String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseView {
    Highlights,
    Changelog,
}

#[derive(Clone, Copy, Debug)]
struct PanelLayout {
    brand_row: usize,
    title_row: usize,
    tagline_row: Option<usize>,
    tabs_row: usize,
    body_row: usize,
    bottom_padding: usize,
}

impl PanelLayout {
    const fn for_height(height: usize) -> Self {
        if height >= 18 {
            Self {
                brand_row: 2,
                title_row: 3,
                tagline_row: Some(4),
                tabs_row: 6,
                body_row: 8,
                bottom_padding: 2,
            }
        } else if height >= 11 {
            Self {
                brand_row: 1,
                title_row: 2,
                tagline_row: Some(3),
                tabs_row: 4,
                body_row: 6,
                bottom_padding: 1,
            }
        } else {
            Self {
                brand_row: 1,
                title_row: 2,
                tagline_row: None,
                tabs_row: 3,
                body_row: 5,
                bottom_padding: 0,
            }
        }
    }

    const fn body_height(self, height: usize) -> usize {
        height.saturating_sub(self.body_row + self.bottom_padding)
    }
}

#[derive(Clone, Copy, Debug)]
struct TabsLayout {
    x: usize,
    y: usize,
    first_width: usize,
    second_x: usize,
    second_width: usize,
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
        let (x, y, width, height) = geometry(viewport_width, viewport_height, 0);
        let theme = editor.theme.clone();
        let style = theme.ui_style.dialog.clone();
        let mut dialog = Dialog::new(
            Some("RELEASE NOTES".to_string()),
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
        panel.normalize_surface();
        panel.reflow();
        panel
    }

    fn layout(&self) -> PanelLayout {
        PanelLayout::for_height(self.dialog.height)
    }

    fn body_height(&self) -> usize {
        self.layout().body_height(self.dialog.height)
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.body_height())
    }

    fn reflow(&mut self) {
        let width = geometry(self.viewport_width, self.viewport_height, 0).2;
        self.dialog.width = width;
        let markdown = match self.view {
            ReleaseView::Highlights => self.notes.highlights_markdown(),
            ReleaseView::Changelog => self.notes.markdown.clone(),
        };
        let width = width.saturating_sub(4);
        let mut highlighter =
            Highlighter::with_registry(&self.theme, Arc::clone(&self.registry)).ok();
        self.lines = render_markdown_lines_with_highlighter(&markdown, width, highlighter.as_mut());

        let (x, y, width, height) =
            geometry(self.viewport_width, self.viewport_height, self.lines.len());
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;

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

    fn normalize_surface(&mut self) {
        let background = self.dialog.style.bg;
        self.dialog.border_draw_style = self.dialog.border_draw_style.with_bg(background);
        self.dialog.title_style = self.dialog.title_style.with_bg(background);
        self.dialog.footer_style = self.dialog.footer_style.with_bg(background);
    }

    fn surface_style(&self, style: &Style) -> Style {
        style.with_bg(self.dialog.style.bg)
    }

    fn update_chrome(&mut self) {
        let metadata = self.notes.published_at.as_ref().map_or_else(
            || format!("v{}", self.notes.version),
            |date| format!("v{} · {date}", self.notes.version),
        );
        self.dialog.set_header_status(Some(metadata));

        let destination = match self.view {
            ReleaseView::Highlights => "Changelog",
            ReleaseView::Changelog => "Highlights",
        };
        let mut actions = vec![
            UiAction::new("close", "Esc", "close").with_priority(ActionPriority::Essential),
            UiAction::new("view", "Tab", destination).with_priority(ActionPriority::Essential),
            UiAction::new("github", "o", "GitHub"),
        ];
        if self.max_scroll() > 0 {
            actions.push(
                UiAction::new("scroll", "j/k", "scroll").with_priority(ActionPriority::Secondary),
            );
        }
        if !self.links.is_empty() {
            if self.links.len() > 1 {
                actions.push(
                    UiAction::new("links", "[/]", "links").with_priority(ActionPriority::Secondary),
                );
            }
            actions.push(UiAction::new("link", "Enter", "open link"));
        }
        self.dialog.set_actions(actions);
        self.dialog
            .set_action_inset(if self.dialog.width >= 40 { 2 } else { 1 });
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
        buffer.set_text(x, self.dialog.y + row, &text, &self.surface_style(style));
    }

    fn tab_labels(&self) -> (&'static str, &'static str) {
        if self.dialog.width >= 34 {
            (" Highlights ", " Full changelog ")
        } else {
            (" New ", " Changes ")
        }
    }

    fn tabs_layout(&self) -> TabsLayout {
        let (first, second) = self.tab_labels();
        let first_width = display_width(first);
        let second_width = display_width(second);
        let tabs_width = first_width + second_width + 3;
        let x = self.dialog.x + 1 + self.dialog.width.saturating_sub(tabs_width) / 2;
        TabsLayout {
            x,
            y: self.dialog.y + self.layout().tabs_row,
            first_width,
            second_x: x + first_width + 3,
            second_width,
        }
    }

    fn tab_at(&self, column: usize, row: usize) -> Option<ReleaseView> {
        let tabs = self.tabs_layout();
        if row != tabs.y {
            return None;
        }
        if (tabs.x..tabs.x + tabs.first_width).contains(&column) {
            Some(ReleaseView::Highlights)
        } else if (tabs.second_x..tabs.second_x + tabs.second_width).contains(&column) {
            Some(ReleaseView::Changelog)
        } else {
            None
        }
    }

    fn draw_hero(&self, buffer: &mut RenderBuffer) {
        let palette = splash::palette(&self.theme);
        let layout = self.layout();
        let brand = "red";
        let brand_width = display_width(brand) + 2;
        let x = self.dialog.x + 1 + self.dialog.width.saturating_sub(brand_width) / 2;
        let y = self.dialog.y + layout.brand_row;
        buffer.set_text(x, y, brand, &self.surface_style(palette.style(Role::Mark)));
        buffer.set_text(
            x + display_width(brand) + 1,
            y,
            "●",
            &self.surface_style(palette.style(Role::Dot)),
        );

        let title_style = Style {
            bold: true,
            ..self.theme.ui_style.dialog_title.clone()
        };
        self.centered_text(buffer, layout.title_row, "What’s new in Red", &title_style);
        if let Some(tagline_row) = layout.tagline_row {
            self.centered_text(
                buffer,
                tagline_row,
                "the editor that respects your muscle memory",
                palette.style(Role::Muted),
            );
        }

        let (first, second) = self.tab_labels();
        let tabs = self.tabs_layout();
        let active = self.surface_style(palette.style(Role::Key));
        let inactive = self.surface_style(palette.style(Role::Muted));
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
        buffer.set_text(tabs.x, tabs.y, first, first_style);
        buffer.set_text(tabs.x + tabs.first_width, tabs.y, " · ", &inactive);
        buffer.set_text(tabs.second_x, tabs.y, second, second_style);
    }

    fn draw_body(&self, buffer: &mut RenderBuffer) {
        let x = self.dialog.x + 3;
        let y = self.dialog.y + self.layout().body_row;
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

    fn draw_scroll_indicator(&self, buffer: &mut RenderBuffer) {
        if self.max_scroll() == 0 || self.layout().bottom_padding == 0 {
            return;
        }

        let first = self.scroll + 1;
        let last = (self.scroll + self.body_height()).min(self.lines.len());
        let indicator = format!("{first}–{last} of {}", self.lines.len());
        let available = self.dialog.width.saturating_sub(4);
        if display_width(&indicator) > available {
            return;
        }

        let x = self
            .dialog
            .x
            .saturating_add(self.dialog.width)
            .saturating_sub(display_width(&indicator) + 1);
        let y = self.dialog.y + self.dialog.height.saturating_sub(1);
        let palette = splash::palette(&self.theme);
        buffer.set_text(
            x,
            y,
            &indicator,
            &self.surface_style(palette.style(Role::Muted)),
        );
    }

    fn link_at(&self, column: usize, row: usize) -> Option<usize> {
        let first_x = self.dialog.x + 3;
        let first_y = self.dialog.y + self.layout().body_row;
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
    fn surface_actions(&self) -> Vec<UiAction> {
        self.dialog.actions()
    }
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        self.draw_hero(buffer);
        self.draw_body(buffer);
        self.draw_scroll_indicator(buffer);
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
                    let column = mouse.column as usize;
                    let row = mouse.row as usize;
                    if let Some(view) = self.tab_at(column, row) {
                        if self.view != view {
                            self.view = view;
                            self.scroll = 0;
                            self.reflow();
                        }
                        refresh()
                    } else if let Some(link) = self.link_at(column, row) {
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
        self.reflow();
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.normalize_surface();
        self.reflow();
    }
}

fn geometry(
    viewport_width: usize,
    viewport_height: usize,
    content_lines: usize,
) -> (usize, usize, usize, usize) {
    let width = viewport_width.saturating_sub(4).min(MAX_PANEL_WIDTH);
    let available_height = viewport_height.saturating_sub(3).min(MAX_PANEL_HEIGHT);
    let preferred_height = content_lines
        .saturating_add(SPACIOUS_CHROME_HEIGHT)
        .max(MIN_COMFORTABLE_PANEL_HEIGHT);
    let height = preferred_height.min(available_height);
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
        assert!(text.contains("RELEASE NOTES"));
        assert!(text.contains("What’s new in Red"));
        assert!(text.contains("the editor that respects your muscle memory"));
        assert!(text.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("●"));
        assert!(text.contains("Highlights"));
        assert!(text.contains("New features"));
        assert!(text.contains("Fixes"));
        assert!(!text.contains("everything your fingers expect — and a little more"));
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

        let mut buffer = RenderBuffer::new(90, 26, &Style::default());
        panel.draw(&mut buffer).unwrap();
        assert!(rendered_text(&buffer).contains("Tab Highlights"));
    }

    #[test]
    fn mouse_clicks_switch_between_release_views() {
        let editor = editor(100, 35);
        let notes = ReleaseNotes::bundled(env!("CARGO_PKG_VERSION"), None);
        let mut panel = WhatsNewPanel::new(&editor, notes, None);
        let tabs = panel.tabs_layout();

        let result = panel.handle_event(&Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: tabs.second_x as u16,
            row: tabs.y as u16,
            modifiers: KeyModifiers::NONE,
        }));

        assert!(matches!(result, Some(KeyAction::Single(Action::Refresh))));
        assert_eq!(panel.view, ReleaseView::Changelog);

        let tabs = panel.tabs_layout();
        panel.handle_event(&Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: tabs.x as u16,
            row: tabs.y as u16,
            modifiers: KeyModifiers::NONE,
        }));

        assert_eq!(panel.view, ReleaseView::Highlights);
    }

    #[test]
    fn spacious_layout_leaves_balanced_header_and_body_padding() {
        let editor = editor(180, 70);
        let notes = ReleaseNotes::bundled(env!("CARGO_PKG_VERSION"), None);
        let panel = WhatsNewPanel::new(&editor, notes, None);
        let layout = panel.layout();
        let mut buffer = RenderBuffer::new(180, 70, &Style::default());

        panel.draw(&mut buffer).unwrap();

        assert_eq!(panel.dialog.width, MAX_PANEL_WIDTH);
        assert!(panel.dialog.height >= MIN_COMFORTABLE_PANEL_HEIGHT);
        assert!(panel.dialog.height < MAX_PANEL_HEIGHT);
        assert_eq!(layout.brand_row, 2);
        assert!(layout.body_row > layout.tabs_row + 1);
        assert_eq!(layout.bottom_padding, 2);

        let gap_row = panel.dialog.y + layout.tabs_row + 1;
        let first = gap_row * buffer.width + panel.dialog.x + 1;
        let last = first + panel.dialog.width;
        assert!(buffer.cells[first..last].iter().all(|cell| cell.c == ' '));

        let footer_row = panel.dialog.y + panel.dialog.height;
        let last_action_cell = footer_row * buffer.width + panel.dialog.x + panel.dialog.width - 1;
        assert_eq!(buffer.cells[last_action_cell].c, ' ');
    }

    #[test]
    fn every_plain_release_cell_retains_the_dialog_background() {
        let editor = editor(120, 40);
        let notes = ReleaseNotes {
            version: "1.0.0".to_string(),
            markdown: "### Features\n\n- A readable improvement".to_string(),
            release_url: "https://github.com/codersauce/red/releases/tag/v1.0.0".to_string(),
            published_at: Some("Aug 13, 2026".to_string()),
        };
        let mut panel = WhatsNewPanel::new(&editor, notes, None);

        for theme_path in ["themes/red.json", "themes/github-light.json"] {
            let theme = crate::theme::parse_vscode_theme(theme_path).unwrap();
            panel.set_theme(&theme);
            let mut buffer = RenderBuffer::new(120, 40, &Style::default());
            panel.draw(&mut buffer).unwrap();

            for y in panel.dialog.y..=panel.dialog.y + panel.dialog.height + 1 {
                for x in panel.dialog.x..=panel.dialog.x + panel.dialog.width + 1 {
                    let cell = &buffer.cells[y * buffer.width + x];
                    assert_eq!(
                        cell.style.bg, panel.dialog.style.bg,
                        "{theme_path} paints an inconsistent background at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn long_changelogs_grow_within_bounds_and_show_scroll_progress() {
        let editor = editor(160, 70);
        let entries = (1..=45)
            .map(|index| format!("- Improvement number {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let notes = ReleaseNotes {
            version: "1.0.0".to_string(),
            markdown: format!("### Features\n\n{entries}"),
            release_url: "https://github.com/codersauce/red/releases/tag/v1.0.0".to_string(),
            published_at: None,
        };
        let mut panel = WhatsNewPanel::new(&editor, notes, None);
        panel.switch_view();

        assert_eq!(panel.dialog.height, MAX_PANEL_HEIGHT);
        assert!(panel.max_scroll() > 0);

        let mut buffer = RenderBuffer::new(160, 70, &Style::default());
        panel.draw(&mut buffer).unwrap();
        let expected = format!("1–{} of {}", panel.body_height(), panel.lines.len());
        assert!(rendered_text(&buffer).contains(&expected));

        panel.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
        )));
        let mut buffer = RenderBuffer::new(160, 70, &Style::default());
        panel.draw(&mut buffer).unwrap();
        let expected = format!("2–{} of {}", panel.body_height() + 1, panel.lines.len());
        assert!(rendered_text(&buffer).contains(&expected));
    }

    #[test]
    fn multiple_release_links_advertise_keyboard_navigation() {
        let editor = editor(120, 35);
        let notes = ReleaseNotes {
            version: "1.0.0".to_string(),
            markdown: "### Features\n\n- [First change](https://example.test/first)\n- [Second change](https://example.test/second)".to_string(),
            release_url: "https://github.com/codersauce/red/releases/tag/v1.0.0".to_string(),
            published_at: None,
        };
        let mut panel = WhatsNewPanel::new(&editor, notes, None);
        let mut buffer = RenderBuffer::new(120, 35, &Style::default());
        panel.draw(&mut buffer).unwrap();

        assert!(rendered_text(&buffer).contains("[/] links"));
        assert_eq!(panel.selected_link, Some(0));

        panel.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char(']'),
            KeyModifiers::NONE,
        )));

        assert_eq!(panel.selected_link, Some(1));
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
