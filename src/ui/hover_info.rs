use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    highlighter::Highlighter,
    lsp::{Command as LspCommand, CommandLinkGroup},
    plugin::markdown::{
        render_hover_markdown_lines_with_highlighter, wrap_plain_text, RenderedTextLine,
        RenderedTextSpan, TextPanelSpanStyle,
    },
    theme::{SelectionForegroundPriority, Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog},
    Component,
};

const MAX_PROSE_HOVER_WIDTH: usize = 80;
const MAX_CODE_HOVER_WIDTH: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverInfoFormat {
    Markdown,
    Plaintext,
}

pub struct HoverInfo {
    source: String,
    format: HoverInfoFormat,
    actions: Vec<HoverAction>,
    line_actions: Vec<Option<usize>>,
    selected_action: Option<usize>,
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

#[derive(Clone)]
struct HoverAction {
    label: String,
    command: LspCommand,
}

impl HoverInfo {
    pub fn new(
        editor: &Editor,
        source: String,
        format: HoverInfoFormat,
        action_groups: Vec<CommandLinkGroup>,
    ) -> Self {
        let theme = editor.theme.clone();
        let anchor = editor.cursor_position();
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight();
        let actions = hover_actions(action_groups);
        let (lines, line_actions, width) = render_lines(
            &source,
            format,
            hover_width_limit(&source, format, viewport_width),
            &theme,
            &actions,
        );
        let (x, y, height) =
            hover_geometry(anchor, viewport_width, viewport_height, width, lines.len());
        let style = theme.ui_style.dialog.clone();
        let mut info = Self {
            source,
            format,
            selected_action: (!actions.is_empty()).then_some(0),
            actions,
            line_actions,
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
            .with_title_style(&theme.ui_style.dialog_title)
            .with_footer_style(&theme.ui_style.muted),
            theme,
        };
        info.update_chrome();
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
        self.update_chrome();
    }

    fn update_chrome(&mut self) {
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
        let footer = match (!self.actions.is_empty(), self.max_scroll() > 0) {
            (true, true) => "Tab actions · Enter open · ↑↓ scroll",
            (true, false) => "Tab actions · Enter open",
            (false, true) => "↑↓ scroll",
            (false, false) => "Esc close",
        };
        self.dialog.set_footer(Some(footer.to_string()));
    }

    fn reflow(&mut self, viewport_width: usize, viewport_height: usize) {
        let (lines, line_actions, width) = render_lines(
            &self.source,
            self.format,
            hover_width_limit(&self.source, self.format, viewport_width),
            &self.theme,
            &self.actions,
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
        self.line_actions = line_actions;
        self.scroll = self.scroll.min(self.max_scroll());
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
        self.ensure_selected_action_visible();
        self.update_chrome();
    }

    fn select_action_by(&mut self, delta: isize) {
        if self.actions.is_empty() {
            return;
        }
        let count = self.actions.len() as isize;
        let current = self.selected_action.unwrap_or(0) as isize;
        self.selected_action = Some((current + delta).rem_euclid(count) as usize);
        self.ensure_selected_action_visible();
        self.update_chrome();
    }

    fn ensure_selected_action_visible(&mut self) {
        let Some(selected) = self.selected_action else {
            return;
        };
        let Some(line) = self
            .line_actions
            .iter()
            .position(|action| *action == Some(selected))
        else {
            return;
        };
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll.saturating_add(self.height) {
            self.scroll = line.saturating_sub(self.height.saturating_sub(1));
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn activate_action(&self, index: usize) -> Option<KeyAction> {
        let command = self.actions.get(index)?.command.clone();
        Some(KeyAction::Multiple(vec![
            Action::CloseDialog,
            Action::ExecuteLspCommand(Box::new(command)),
        ]))
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
            let line_index = self.scroll + row;
            let selected = self
                .line_actions
                .get(line_index)
                .copied()
                .flatten()
                .is_some_and(|action| Some(action) == self.selected_action);
            render_line(
                buffer,
                self.x + 1,
                self.y + 1 + row,
                self.width,
                line,
                selected,
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
                    self.update_chrome();
                    redraw()
                }
                (KeyCode::End | KeyCode::Char('G'), _) => {
                    self.scroll = self.max_scroll();
                    self.update_chrome();
                    redraw()
                }
                (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => {
                    self.select_action_by(-1);
                    redraw()
                }
                (KeyCode::Tab, _) => {
                    self.select_action_by(1);
                    redraw()
                }
                (KeyCode::Enter, _) => self
                    .selected_action
                    .and_then(|index| self.activate_action(index)),
                (KeyCode::Char(number @ '1'..='9'), KeyModifiers::NONE) => {
                    self.activate_action(number as usize - '1' as usize)
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
                    let content_x = self.x.saturating_add(1);
                    let content_y = self.y.saturating_add(1);
                    if (content_x..content_x.saturating_add(self.width))
                        .contains(&(mouse.column as usize))
                        && (content_y..content_y.saturating_add(self.height))
                            .contains(&(mouse.row as usize))
                    {
                        let line = self.scroll.saturating_add(mouse.row as usize - content_y);
                        if let Some(Some(action)) = self.line_actions.get(line) {
                            self.selected_action = Some(*action);
                            return redraw();
                        }
                        None
                    } else {
                        Some(KeyAction::Single(Action::CloseDialog))
                    }
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
        self.dialog.style = theme.ui_style.dialog.clone();
        self.dialog.border_draw_style = theme.ui_style.dialog_border.clone();
        self.dialog.title_style = theme.ui_style.dialog_title.clone();
        self.dialog.footer_style = theme.ui_style.muted.clone();
        self.dialog.theme = theme.clone();
        self.reflow(self.viewport_width, self.viewport_height);
    }
}

fn render_lines(
    source: &str,
    format: HoverInfoFormat,
    available_width: usize,
    theme: &Theme,
    actions: &[HoverAction],
) -> (Vec<RenderedTextLine>, Vec<Option<usize>>, usize) {
    if available_width == 0 {
        return (Vec::new(), Vec::new(), 0);
    }
    let mut highlighter = Highlighter::new(theme).ok();
    let content_lines = match format {
        HoverInfoFormat::Markdown => render_hover_markdown_lines_with_highlighter(
            source,
            available_width,
            highlighter.as_mut(),
        ),
        HoverInfoFormat::Plaintext => {
            wrap_plain_text(source, available_width, TextPanelSpanStyle::Text)
        }
    };
    let action_lines = actions
        .iter()
        .enumerate()
        .flat_map(|(index, action)| {
            wrap_plain_text(
                &format!("{}. {}", index + 1, action.label),
                available_width,
                TextPanelSpanStyle::Link,
            )
            .into_iter()
            .map(move |line| (line, Some(index)))
        })
        .collect::<Vec<_>>();
    let width = action_lines
        .iter()
        .map(|(line, _)| line_width(line))
        .chain(content_lines.iter().map(line_width))
        .max()
        .unwrap_or(0)
        .max(display_width("Hover"))
        .min(available_width);
    let mut lines = Vec::new();
    let mut line_actions = Vec::new();
    for (line, action) in action_lines {
        lines.push(line);
        line_actions.push(action);
    }
    if !actions.is_empty() {
        lines.push(RenderedTextLine::plain(
            "─".repeat(width),
            TextPanelSpanStyle::Muted,
        ));
        line_actions.push(None);
    }
    for line in content_lines {
        lines.push(line);
        line_actions.push(None);
    }
    (lines, line_actions, width)
}

fn hover_actions(groups: Vec<CommandLinkGroup>) -> Vec<HoverAction> {
    groups
        .into_iter()
        .flat_map(|group| {
            let group_title = group.title.filter(|title| !title.trim().is_empty());
            group.commands.into_iter().map(move |command| {
                let label = group_title.as_ref().map_or_else(
                    || command.title.clone(),
                    |title| format!("{title}: {}", command.title),
                );
                HoverAction {
                    label,
                    command: command.into(),
                }
            })
        })
        .collect()
}

fn hover_width_limit(source: &str, format: HoverInfoFormat, viewport_width: usize) -> usize {
    let code_heavy =
        format == HoverInfoFormat::Markdown && (source.contains("```") || source.contains("~~~"));
    viewport_width.saturating_sub(2).min(if code_heavy {
        MAX_CODE_HOVER_WIDTH
    } else {
        MAX_PROSE_HOVER_WIDTH
    })
}

fn hover_geometry(
    anchor: (usize, usize),
    viewport_width: usize,
    viewport_height: usize,
    content_width: usize,
    content_height: usize,
) -> (usize, usize, usize) {
    let width = content_width.min(viewport_width.saturating_sub(2));
    let max_x = viewport_width.saturating_sub(width.saturating_add(2));
    let wide = width.saturating_add(2) >= viewport_width.saturating_mul(2) / 3;
    let x = if wide {
        usize::from(max_x > 0)
    } else {
        anchor.0.min(max_x)
    };
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
    selected: bool,
    theme: &Theme,
) {
    if selected {
        let selection = theme.list_selection_style();
        let selected_style = theme.selected_style(
            &theme.ui_style.dialog,
            &selection,
            SelectionForegroundPriority::Selection,
        );
        buffer.set_text(x, y, &" ".repeat(width), &selected_style);
    }
    let mut used = 0;
    for span in &line.spans {
        if used >= width {
            break;
        }
        let text = truncate_display_width(&span.text, width - used);
        if text.is_empty() {
            continue;
        }
        let mut style = hover_span_style(span, theme);
        if selected {
            let selection = theme.list_selection_style();
            style = theme.selected_style(&style, &selection, SelectionForegroundPriority::Content);
        }
        buffer.set_text(x + used, y, &text, &style);
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
            Vec::new(),
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
        let rendered = info
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains("┌─"));
        assert!(!rendered.contains("└─"));
        assert!(!rendered.contains("│ "));
        assert!(!rendered.contains("rust"));
    }

    #[test]
    fn hover_actions_render_as_selected_rows_and_execute_the_server_command() {
        let editor = test_editor(Theme::default(), 100, 24);
        let mut info = HoverInfo::new(
            &editor,
            "Documentation".to_string(),
            HoverInfoFormat::Markdown,
            vec![CommandLinkGroup {
                title: None,
                commands: vec![crate::lsp::CommandLink {
                    title: "Go to Error (anyhow::Error)".to_string(),
                    command: "rust-analyzer.gotoLocation".to_string(),
                    arguments: Some(vec![serde_json::json!({"uri": "file:///tmp/lib.rs"})]),
                    tooltip: Some("Open the type definition".to_string()),
                }],
            }],
        );

        assert_eq!(info.selected_action, Some(0));
        assert_eq!(info.line_actions.first(), Some(&Some(0)));
        assert!(info.lines[0]
            .spans
            .iter()
            .any(|span| span.text.contains("1. Go to Error")));

        let action = info.handle_event(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        assert!(matches!(
            action,
            Some(KeyAction::Multiple(actions))
                if matches!(actions.as_slice(), [
                    Action::CloseDialog,
                    Action::ExecuteLspCommand(command)
                ] if command.command == "rust-analyzer.gotoLocation")
        ));
    }

    #[test]
    fn signature_heavy_hover_uses_the_wider_edge_aligned_layout() {
        let mut editor = test_editor(Theme::default(), 160, 30);
        editor.test_set_viewport_cursor(70, 0, 10);
        let signature = format!("fn long_signature({})", "argument: usize, ".repeat(8));
        let info = HoverInfo::new(
            &editor,
            format!("```rust\n{signature}\n```"),
            HoverInfoFormat::Markdown,
            Vec::new(),
        );

        assert_eq!(info.x, 1);
        assert!(info.width > MAX_PROSE_HOVER_WIDTH);
        assert!(info.width <= MAX_CODE_HOVER_WIDTH);
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
            Vec::new(),
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
            Vec::new(),
        );
        let wide_lines = info.lines.len();

        assert!(info.resize(24, 12));
        assert!(info.lines.len() > wide_lines);
        assert!(info.width <= 22);
    }
}
