//! Cursor-anchored, severity-aware diagnostics for the active buffer line.

use std::path::Path;

use crossterm::event::{Event, KeyCode, MouseEventKind};
use textwrap::Options;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    lsp::{normalized_file_path, Diagnostic, DiagnosticSeverity},
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    geometry::anchored_popup_geometry,
    Component,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiagnosticSpan {
    text: String,
    style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiagnosticLine {
    spans: Vec<DiagnosticSpan>,
}

impl DiagnosticLine {
    fn plain(text: impl Into<String>, style: &Style) -> Self {
        Self {
            spans: vec![DiagnosticSpan {
                text: text.into(),
                style: style.clone(),
            }],
        }
    }
}

pub struct DiagnosticInfo {
    diagnostics: Vec<Diagnostic>,
    viewport_y_offset: usize,
    anchor: (usize, usize),
    viewport_width: usize,
    viewport_height: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    scroll: usize,
    lines: Vec<DiagnosticLine>,
    theme: Theme,
    dialog: Dialog,
}

impl DiagnosticInfo {
    pub fn new(editor: &Editor, diagnostics: Vec<Diagnostic>) -> Self {
        let theme = editor.theme.clone();
        let local_anchor = editor.cursor_position();
        let anchor = editor.render_cursor_position().unwrap_or(local_anchor);
        let viewport_y_offset = anchor.1.saturating_sub(local_anchor.1);
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight().saturating_add(viewport_y_offset);
        let width = diagnostic_width(&diagnostics).min(viewport_width.saturating_sub(2));
        let lines = diagnostic_lines(&diagnostics, width, &theme);
        let (x, y, height) =
            anchored_popup_geometry(anchor, viewport_width, viewport_height, width, lines.len());
        let style = theme.ui_style.dialog.clone();
        let dialog = Dialog::new(
            None,
            x,
            y,
            width,
            height,
            &style,
            BorderStyle::Rounded,
            &theme,
        )
        .with_surface_theme(&theme, SurfaceRole::Dialog);

        Self {
            diagnostics,
            viewport_y_offset,
            anchor,
            viewport_width,
            viewport_height,
            x,
            y,
            width,
            height,
            scroll: 0,
            lines,
            theme,
            dialog,
        }
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
    }

    fn reflow(&mut self, viewport_width: usize, viewport_height: usize) {
        let viewport_height = viewport_height.saturating_add(self.viewport_y_offset);
        let width = diagnostic_width(&self.diagnostics).min(viewport_width.saturating_sub(2));
        let lines = diagnostic_lines(&self.diagnostics, width, &self.theme);
        let (x, y, height) = anchored_popup_geometry(
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
    }
}

impl Component for DiagnosticInfo {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        for (row, line) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(self.height)
            .enumerate()
        {
            let mut x = self.x.saturating_add(1);
            let y = self.y.saturating_add(1).saturating_add(row);
            let right = x.saturating_add(self.width);
            for span in &line.spans {
                if x >= right {
                    break;
                }
                let text = truncate_display_width(&span.text, right.saturating_sub(x));
                buffer.set_text(x, y, &text, &span.style);
                x = x.saturating_add(display_width(&text));
            }
        }
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let redraw = || Some(KeyAction::Single(Action::Refresh));
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Some(KeyAction::Single(Action::CloseDialog)),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll_by(-1);
                    redraw()
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll_by(1);
                    redraw()
                }
                KeyCode::PageUp => {
                    self.scroll_by(-(self.height.max(1) as isize));
                    redraw()
                }
                KeyCode::PageDown => {
                    self.scroll_by(self.height.max(1) as isize);
                    redraw()
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.scroll = 0;
                    redraw()
                }
                KeyCode::End | KeyCode::Char('G') => {
                    self.scroll = self.max_scroll();
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
                MouseEventKind::Down(_) => {
                    let inside = (self.x..self.x.saturating_add(self.width).saturating_add(2))
                        .contains(&(mouse.column as usize))
                        && (self.y..self.y.saturating_add(self.height).saturating_add(2))
                            .contains(&(mouse.row as usize));
                    (!inside).then_some(KeyAction::Single(Action::CloseDialog))
                }
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
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.reflow(self.viewport_width, self.viewport_height);
    }
}

fn diagnostic_width(diagnostics: &[Diagnostic]) -> usize {
    let mut width = display_width("Diagnostics:");
    for (index, diagnostic) in diagnostics.iter().enumerate() {
        let prefix = format!("{}. ", index + 1);
        let suffix = diagnostic
            .code
            .as_ref()
            .map(|code| format!(" [{}]", code.as_string()))
            .unwrap_or_default();
        let message_lines = diagnostic.message.split('\n').collect::<Vec<_>>();
        for (line_index, line) in message_lines.iter().enumerate() {
            let line_suffix = if line_index + 1 == message_lines.len() {
                suffix.as_str()
            } else {
                ""
            };
            width = width.max(
                display_width(&prefix)
                    .saturating_add(display_width(line.trim_end_matches('\r')))
                    .saturating_add(display_width(line_suffix)),
            );
        }
        for related in diagnostic.related_information.iter().flatten() {
            width = width.max(
                display_width(&prefix)
                    .saturating_add(display_width(&related_information_text(related))),
            );
        }
    }
    width
}

fn diagnostic_lines(
    diagnostics: &[Diagnostic],
    width: usize,
    theme: &Theme,
) -> Vec<DiagnosticLine> {
    if width == 0 {
        return Vec::new();
    }

    let normal = theme.ui_style.dialog.clone();
    let mut header = theme.ui_style.dialog_title.clone();
    header.bg = normal.bg;
    header.bold = true;
    let mut lines = vec![DiagnosticLine::plain("Diagnostics:", &header)];

    for (index, diagnostic) in diagnostics.iter().enumerate() {
        let prefix = format!("{}. ", index + 1);
        let continuation = " ".repeat(display_width(&prefix));
        let suffix = diagnostic
            .code
            .as_ref()
            .map(|code| format!(" [{}]", code.as_string()))
            .unwrap_or_default();
        let severity = diagnostic_style(theme, diagnostic.severity.as_ref());
        let message_lines = diagnostic.message.split('\n').collect::<Vec<_>>();

        for (message_index, message) in message_lines.iter().enumerate() {
            let first_indent = if message_index == 0 {
                prefix.as_str()
            } else {
                continuation.as_str()
            };
            let last_message = message_index + 1 == message_lines.len();
            let body = if last_message {
                format!("{}{}", message.trim_end_matches('\r'), suffix)
            } else {
                message.trim_end_matches('\r').to_string()
            };
            let mut wrapped = textwrap::wrap(
                &body,
                Options::new(width)
                    .initial_indent(first_indent)
                    .subsequent_indent(&continuation),
            );
            if wrapped.is_empty() {
                wrapped.push(first_indent.into());
            }
            let wrapped_len = wrapped.len();
            for (wrapped_index, wrapped) in wrapped.into_iter().enumerate() {
                let indent = if wrapped_index == 0 {
                    first_indent
                } else {
                    continuation.as_str()
                };
                let text = wrapped.into_owned();
                let body = text.strip_prefix(indent).unwrap_or(&text);
                let has_suffix = last_message
                    && wrapped_index + 1 == wrapped_len
                    && !suffix.is_empty()
                    && body.ends_with(&suffix);
                let message = if has_suffix {
                    &body[..body.len().saturating_sub(suffix.len())]
                } else {
                    body
                };
                let mut spans = vec![DiagnosticSpan {
                    text: indent.to_string(),
                    style: normal.clone(),
                }];
                if !message.is_empty() {
                    spans.push(DiagnosticSpan {
                        text: message.to_string(),
                        style: severity.clone(),
                    });
                }
                if has_suffix {
                    spans.push(DiagnosticSpan {
                        text: suffix.clone(),
                        style: normal.clone(),
                    });
                }
                lines.push(DiagnosticLine { spans });
            }
        }

        for related in diagnostic.related_information.iter().flatten() {
            let text = related_information_text(related);
            for wrapped in textwrap::wrap(
                &text,
                Options::new(width)
                    .initial_indent(&continuation)
                    .subsequent_indent(&continuation),
            ) {
                lines.push(DiagnosticLine::plain(wrapped.into_owned(), &normal));
            }
        }
    }
    lines
}

fn related_information_text(related: &crate::lsp::DiagnosticRelatedInformation) -> String {
    let file = normalized_file_path(&related.location.uri)
        .ok()
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| related.location.uri.clone());
    format!(
        "{}:{}:{}: {}",
        file,
        related.location.range.start.line.saturating_add(1),
        related.location.range.start.character.saturating_add(1),
        related.message
    )
}

fn diagnostic_style(theme: &Theme, severity: Option<&DiagnosticSeverity>) -> Style {
    let color = |keys: &[&str]| keys.iter().find_map(|key| theme.colors.get(*key).copied());
    let fg = match severity {
        Some(DiagnosticSeverity::Warning) => color(&[
            "editorWarning.foreground",
            "list.warningForeground",
            "terminal.ansiYellow",
        ]),
        Some(DiagnosticSeverity::Information) => color(&[
            "editorInfo.foreground",
            "notificationsInfoIcon.foreground",
            "terminal.ansiBlue",
        ]),
        Some(DiagnosticSeverity::Hint) => color(&[
            "editorHint.foreground",
            "editorInlayHint.foreground",
            "descriptionForeground",
        ]),
        Some(DiagnosticSeverity::Error) | None => color(&[
            "editorError.foreground",
            "errorForeground",
            "terminal.ansiRed",
        ])
        .or_else(|| theme.error_style.as_ref().and_then(|style| style.fg)),
    }
    .or(theme.ui_style.dialog.fg)
    .or(theme.style.fg);
    Style {
        fg,
        bg: theme.ui_style.dialog.bg,
        ..Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        color::Color,
        config::Config,
        lsp::LspManager,
        lsp::{DiagnosticCode, Position, Range},
    };

    fn diagnostic(
        severity: DiagnosticSeverity,
        message: &str,
        code: Option<DiagnosticCode>,
    ) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            severity: Some(severity),
            code,
            message: message.to_string(),
            related_information: None,
            data: None,
            tags: None,
        }
    }

    fn line_text(line: &DiagnosticLine) -> String {
        line.spans.iter().map(|span| span.text.as_str()).collect()
    }

    fn test_editor(width: usize, height: usize) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        Editor::with_size(
            lsp,
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(None, "value".to_string())],
        )
        .unwrap()
    }

    #[test]
    fn formats_numbered_diagnostics_codes_and_multiline_indentation() {
        let diagnostics = vec![
            diagnostic(
                DiagnosticSeverity::Error,
                "missing import",
                Some(DiagnosticCode::String("reportMissingImports".to_string())),
            ),
            diagnostic(DiagnosticSeverity::Warning, "first\nsecond", None),
        ];

        let lines = diagnostic_lines(&diagnostics, 80, &Theme::default());
        let text = lines.iter().map(line_text).collect::<Vec<_>>();

        assert_eq!(text[0], "Diagnostics:");
        assert_eq!(text[1], "1. missing import [reportMissingImports]");
        assert_eq!(text[2], "2. first");
        assert_eq!(text[3], "   second");
    }

    #[test]
    fn colors_only_the_message_with_the_diagnostic_severity() {
        let mut theme = Theme::default();
        let error = Color::Rgb { r: 9, g: 8, b: 7 };
        theme
            .colors
            .insert("editorError.foreground".to_string(), error);
        let lines = diagnostic_lines(
            &[diagnostic(
                DiagnosticSeverity::Error,
                "problem",
                Some(DiagnosticCode::String("E1".to_string())),
            )],
            80,
            &theme,
        );

        assert_eq!(lines[1].spans.len(), 3);
        assert_eq!(lines[1].spans[1].text, "problem");
        assert_eq!(lines[1].spans[1].style.fg, Some(error));
        assert_eq!(lines[1].spans[0].style, theme.ui_style.dialog);
        assert_eq!(lines[1].spans[2].style, theme.ui_style.dialog);
    }

    #[test]
    fn wraps_unicode_content_with_aligned_continuations() {
        let lines = diagnostic_lines(
            &[diagnostic(
                DiagnosticSeverity::Information,
                "alpha 👋 beta gamma",
                None,
            )],
            12,
            &Theme::default(),
        );
        let text = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(text.len() > 2);
        assert!(text[1].starts_with("1. "));
        assert!(text[2].starts_with("   "));
        assert!(text.iter().all(|line| display_width(line) <= 12));
    }

    #[test]
    fn popup_uses_rounded_chrome_scrolls_and_closes_with_q() {
        let editor = test_editor(30, 8);
        let diagnostics = (0..8)
            .map(|index| {
                diagnostic(
                    DiagnosticSeverity::Warning,
                    &format!("warning {index}"),
                    None,
                )
            })
            .collect();
        let mut popup = DiagnosticInfo::new(&editor, diagnostics);
        let mut buffer = RenderBuffer::new(30, 8, &Style::default());

        popup.draw(&mut buffer).unwrap();
        assert_eq!(buffer.cells[popup.y * buffer.width + popup.x].c, '╭');
        assert!(popup.max_scroll() > 0);
        assert_eq!(
            popup.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ))),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(popup.scroll, 1);
        assert_eq!(
            popup.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE,
            ))),
            Some(KeyAction::Single(Action::CloseDialog))
        );
        assert!(popup.resize(16, 6));
        assert!(popup.x.saturating_add(popup.width).saturating_add(2) <= 16);
    }
}
