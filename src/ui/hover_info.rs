use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    highlighter::Highlighter,
    plugin::markdown::{
        render_markdown_lines_with_highlighter, wrap_plain_text, RenderedTextLine,
        RenderedTextSpan, TextPanelSpanStyle,
    },
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog},
    Component,
};

const MAX_HOVER_WIDTH: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverInfoFormat {
    Markdown,
    Plaintext,
}

pub struct HoverInfo {
    source: String,
    format: HoverInfoFormat,
    anchor: (usize, usize),
    viewport_width: usize,
    viewport_height: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    scroll: usize,
    lines: Vec<RenderedTextLine>,
    theme: Theme,
    dialog: Dialog,
}

impl HoverInfo {
    pub fn new(editor: &Editor, source: String, format: HoverInfoFormat) -> Self {
        let theme = editor.theme.clone();
        let anchor = editor.cursor_position();
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight();
        let (lines, width) = render_lines(
            &source,
            format,
            viewport_width.saturating_sub(2).min(MAX_HOVER_WIDTH),
            &theme,
        );
        let (x, y, height) =
            hover_geometry(anchor, viewport_width, viewport_height, width, lines.len());
        let style = theme.ui_style.dialog.clone();
        let mut info = Self {
            source,
            format,
            anchor,
            viewport_width,
            viewport_height,
            x,
            y,
            width,
            height,
            scroll: 0,
            lines,
            dialog: Dialog::new(
                Some("Hover".to_string()),
                x,
                y,
                width,
                height,
                &style,
                BorderStyle::Single,
                &theme,
            )
            .with_border_draw_style(&theme.ui_style.dialog_border)
            .with_title_style(&theme.ui_style.dialog_title),
            theme,
        };
        info.update_title();
        info
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
        self.update_title();
    }

    fn update_title(&mut self) {
        let title = if self.max_scroll() == 0 {
            "Hover".to_string()
        } else {
            format!(
                "Hover · {}/{}",
                self.scroll.saturating_add(1),
                self.max_scroll().saturating_add(1)
            )
        };
        self.dialog.set_title(Some(title));
    }

    fn reflow(&mut self, viewport_width: usize, viewport_height: usize) {
        let (lines, width) = render_lines(
            &self.source,
            self.format,
            viewport_width.saturating_sub(2).min(MAX_HOVER_WIDTH),
            &self.theme,
        );
        let (x, y, height) = hover_geometry(
            self.anchor,
            viewport_width,
            viewport_height,
            width,
            lines.len(),
        );
        self.viewport_width = viewport_width;
        self.viewport_height = viewport_height;
        self.x = x;
        self.y = y;
        self.width = width;
        self.height = height;
        self.lines = lines;
        self.scroll = self.scroll.min(self.max_scroll());
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
        self.update_title();
    }
}

impl Component for HoverInfo {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;

        for (row, line) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(self.height)
            .enumerate()
        {
            render_line(
                buffer,
                self.x + 1,
                self.y + 1 + row,
                self.width,
                line,
                &self.theme,
            );
        }
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let redraw = || Some(KeyAction::Single(Action::ShowDialog));
        match event {
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Char('q'), _) => {
                    Some(KeyAction::Single(Action::CloseDialog))
                }
                (KeyCode::Up | KeyCode::Char('k'), _) => {
                    self.scroll_by(-1);
                    redraw()
                }
                (KeyCode::Down | KeyCode::Char('j'), _) => {
                    self.scroll_by(1);
                    redraw()
                }
                (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    self.scroll_by(-(self.height.max(1) as isize));
                    redraw()
                }
                (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    self.scroll_by(self.height.max(1) as isize);
                    redraw()
                }
                (KeyCode::Home | KeyCode::Char('g'), _) => {
                    self.scroll = 0;
                    self.update_title();
                    redraw()
                }
                (KeyCode::End | KeyCode::Char('G'), _) => {
                    self.scroll = self.max_scroll();
                    self.update_title();
                    redraw()
                }
                _ => None,
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_by(-3);
                    redraw()
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_by(3);
                    redraw()
                }
                MouseEventKind::Down(_) => Some(KeyAction::Single(Action::CloseDialog)),
                _ => None,
            },
            _ => None,
        }
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.reflow(viewport_width, viewport_height);
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.dialog.style = theme.ui_style.dialog.clone();
        self.dialog.border_draw_style = theme.ui_style.dialog_border.clone();
        self.dialog.title_style = theme.ui_style.dialog_title.clone();
        self.dialog.theme = theme.clone();
        self.reflow(self.viewport_width, self.viewport_height);
    }
}

fn render_lines(
    source: &str,
    format: HoverInfoFormat,
    available_width: usize,
    theme: &Theme,
) -> (Vec<RenderedTextLine>, usize) {
    if available_width == 0 {
        return (Vec::new(), 0);
    }
    let mut highlighter = Highlighter::new(theme).ok();
    let lines = match format {
        HoverInfoFormat::Markdown => {
            render_markdown_lines_with_highlighter(source, available_width, highlighter.as_mut())
        }
        HoverInfoFormat::Plaintext => {
            wrap_plain_text(source, available_width, TextPanelSpanStyle::Text)
        }
    };
    let width = lines
        .iter()
        .map(line_width)
        .max()
        .unwrap_or(0)
        .max(display_width("Hover"))
        .min(available_width);
    (lines, width)
}

fn hover_geometry(
    anchor: (usize, usize),
    viewport_width: usize,
    viewport_height: usize,
    content_width: usize,
    content_height: usize,
) -> (usize, usize, usize) {
    let width = content_width.min(viewport_width.saturating_sub(2));
    let x = anchor
        .0
        .min(viewport_width.saturating_sub(width.saturating_add(2)));
    let below = viewport_height.saturating_sub(anchor.1.saturating_add(3));
    let above = anchor.1.saturating_sub(2);
    let capacity = if below >= content_height || below >= above {
        below
    } else {
        above
    };
    let height = content_height.min(capacity);
    let y = if capacity == above && above > below {
        anchor.1.saturating_sub(height.saturating_add(2))
    } else {
        anchor.1.saturating_add(1)
    };
    (x, y, height)
}

fn line_width(line: &RenderedTextLine) -> usize {
    line.spans
        .iter()
        .map(|span| display_width(&span.text))
        .sum()
}

fn render_line(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    line: &RenderedTextLine,
    theme: &Theme,
) {
    let mut used = 0;
    for span in &line.spans {
        if used >= width {
            break;
        }
        let text = truncate_display_width(&span.text, width - used);
        if text.is_empty() {
            continue;
        }
        buffer.set_text(x + used, y, &text, &hover_span_style(span, theme));
        used += display_width(&text);
    }
}

fn hover_span_style(span: &RenderedTextSpan, theme: &Theme) -> Style {
    let base = &theme.ui_style.dialog;
    let code_background = theme
        .colors
        .get("textCodeBlock.background")
        .copied()
        .or(base.bg);
    let requested = if let Some(style) = &span.syntax_style {
        style.clone()
    } else {
        let scoped = |scope: &str| theme.get_style(scope).unwrap_or_else(|| base.clone());
        match span.style {
            TextPanelSpanStyle::User | TextPanelSpanStyle::Agent | TextPanelSpanStyle::Text => {
                base.clone()
            }
            TextPanelSpanStyle::Error => theme.ui_style.deprecated.clone(),
            TextPanelSpanStyle::Heading => {
                let mut style = scoped("heading.1.markdown");
                style.bold = true;
                style
            }
            TextPanelSpanStyle::Strong => Style {
                bold: true,
                ..base.clone()
            },
            TextPanelSpanStyle::Emphasis => Style {
                italic: true,
                ..base.clone()
            },
            TextPanelSpanStyle::Strikethrough => scoped("markup.strikethrough.markdown"),
            TextPanelSpanStyle::InlineCode | TextPanelSpanStyle::Code => {
                scoped("markup.raw.block.markdown")
            }
            TextPanelSpanStyle::Link => scoped("markup.underline.link.markdown"),
            TextPanelSpanStyle::Quote | TextPanelSpanStyle::Muted => theme.ui_style.muted.clone(),
        }
    };
    Style {
        fg: requested.fg.or(base.fg),
        bg: if matches!(
            span.style,
            TextPanelSpanStyle::InlineCode | TextPanelSpanStyle::Code
        ) {
            code_background
        } else {
            base.bg
        },
        bold: requested.bold,
        italic: requested.italic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer::Buffer, color::Color, config::Config, lsp::LspManager};

    fn test_editor(theme: Theme, width: usize, height: usize) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        Editor::with_size(
            lsp,
            width,
            height,
            config,
            theme,
            vec![Buffer::new(None, String::new())],
        )
        .unwrap()
    }

    #[test]
    fn markdown_hover_renders_semantics_and_syntax_styles() {
        let mut theme = Theme::default();
        let keyword = Style {
            fg: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
            ..Default::default()
        };
        theme.token_styles.push(crate::theme::TokenStyle {
            name: None,
            scope: vec!["keyword".to_string()],
            style: keyword.clone(),
        });
        let editor = test_editor(theme, 80, 24);
        let info = HoverInfo::new(
            &editor,
            "# Summary\n\n```rust\nfn main() {}\n```".to_string(),
            HoverInfoFormat::Markdown,
        );

        assert!(info
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style == TextPanelSpanStyle::Heading));
        assert!(
            info.lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.text.contains("fn") && span.syntax_style == Some(keyword.clone())),
            "{:?}",
            info.lines
        );
    }

    #[test]
    fn tall_hover_uses_space_above_and_scrolls() {
        let mut editor = test_editor(Theme::default(), 40, 10);
        editor.test_set_viewport_cursor(0, 0, 7);
        let mut info = HoverInfo::new(
            &editor,
            (0..20)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            HoverInfoFormat::Plaintext,
        );

        assert!(info.max_scroll() > 0);
        assert_eq!(info.y, 0);
        info.scroll_by(1);
        assert_eq!(info.scroll, 1);
        assert!(info.x + info.width + 2 <= 40);
        assert!(info.y + info.height + 2 <= editor.vheight());
    }

    #[test]
    fn resize_reflows_instead_of_closing_hover() {
        let editor = test_editor(Theme::default(), 80, 24);
        let mut info = HoverInfo::new(
            &editor,
            "A sentence that should wrap onto several lines in a narrow viewport.".to_string(),
            HoverInfoFormat::Markdown,
        );
        let wide_lines = info.lines.len();

        assert!(info.resize(24, 12));
        assert!(info.lines.len() > wide_lines);
        assert!(info.width <= 22);
    }
}
