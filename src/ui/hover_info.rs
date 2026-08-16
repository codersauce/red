use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use std::sync::Arc;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    highlighter::{Highlighter, LanguageRegistry},
    lsp::{Command as LspCommand, CommandLinkGroup},
    plugin::markdown::{
        render_hover_markdown_lines_with_highlighter, wrap_plain_text, RenderedTextLine,
        RenderedTextSpan, TextPanelSpanStyle,
    },
    theme::{SelectionForegroundPriority, Style, Theme},
    unicode_utils::display_width,
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    geometry::anchored_popup_geometry,
    paint_rich_text, ActionPriority, Component, UiAction,
};

const MAX_PROSE_HOVER_WIDTH: usize = 80;
const MAX_CODE_HOVER_WIDTH: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverInfoFormat {
    Markdown,
    Plaintext,
}

pub struct HoverInfo {
    label: String,
    close_action: Action,
    inline_navigation: Option<uuid::Uuid>,
    confirm_action: Option<(String, Action)>,
    source: String,
    format: HoverInfoFormat,
    actions: Vec<HoverAction>,
    line_actions: Vec<Option<usize>>,
    selected_action: Option<usize>,
    viewport_y_offset: usize,
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
    registry: Arc<LanguageRegistry>,
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
        let local_anchor = editor.cursor_position();
        let anchor = editor.render_cursor_position().unwrap_or(local_anchor);
        let viewport_y_offset = anchor.1.saturating_sub(local_anchor.1);
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight().saturating_add(viewport_y_offset);
        let actions = hover_actions(action_groups);
        let (lines, line_actions, width) = render_lines(
            &source,
            format,
            hover_width_limit(&source, format, viewport_width),
            &theme,
            &editor.language_registry(),
            &actions,
        );
        let (x, y, height) = anchored_popup_geometry(
            anchor,
            viewport_width,
            viewport_height,
            width,
            lines.len().saturating_add(1),
        );
        let style = theme.ui_style.dialog.clone();
        let mut info = Self {
            label: "Hover".to_string(),
            close_action: Action::CloseDialog,
            inline_navigation: None,
            confirm_action: None,
            source,
            format,
            selected_action: (!actions.is_empty()).then_some(0),
            actions,
            line_actions,
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
            .with_surface_theme(&theme, SurfaceRole::Dialog)
            .with_footer_style(&theme.ui_style.muted),
            theme,
            registry: editor.language_registry(),
        };
        info.update_chrome();
        info
    }

    pub(crate) fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self.update_chrome();
        self
    }

    pub(crate) fn with_close_action(mut self, action: Action) -> Self {
        self.close_action = action;
        self.update_chrome();
        self
    }

    pub(crate) fn with_inline_navigation(mut self, id: uuid::Uuid) -> Self {
        self.inline_navigation = Some(id);
        self.update_chrome();
        self
    }

    pub(crate) fn with_confirm_action(mut self, label: impl Into<String>, action: Action) -> Self {
        self.confirm_action = Some((label.into(), action));
        self.update_chrome();
        self
    }

    fn content_height(&self) -> usize {
        self.height.saturating_sub(1)
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.content_height())
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
            self.label.clone()
        } else {
            format!(
                "{} · {}/{}",
                self.label,
                self.scroll.saturating_add(1),
                self.max_scroll().saturating_add(1)
            )
        };
        self.dialog.set_title(Some(title));
        let mut actions = vec![UiAction::new(
            "close",
            "Esc",
            if matches!(self.close_action, Action::CloseDialog) {
                "close"
            } else {
                "back"
            },
        )
        .with_priority(ActionPriority::Essential)];
        if let Some((label, _)) = &self.confirm_action {
            actions.push(
                UiAction::new("confirm", "Enter", label).with_priority(ActionPriority::Essential),
            );
        }
        if self.inline_navigation.is_some() {
            actions.push(UiAction::new("previous-inline", "[", "previous inline"));
            actions.push(UiAction::new("next-inline", "]", "next inline"));
        }
        if !self.actions.is_empty() {
            actions.push(
                UiAction::new("open", "Enter", "open").with_priority(ActionPriority::Essential),
            );
            actions.push(UiAction::new("actions", "Tab", "actions"));
        }
        if self.max_scroll() > 0 {
            actions.push(UiAction::new("scroll", "↑↓", "scroll"));
        }
        self.dialog.set_actions(actions);
    }

    fn reflow(&mut self, viewport_width: usize, viewport_height: usize) {
        let viewport_height = viewport_height.saturating_add(self.viewport_y_offset);
        let (lines, line_actions, width) = render_lines(
            &self.source,
            self.format,
            hover_width_limit(&self.source, self.format, viewport_width),
            &self.theme,
            &self.registry,
            &self.actions,
        );
        let (x, y, height) = anchored_popup_geometry(
            self.anchor,
            viewport_width,
            viewport_height,
            width,
            lines.len().saturating_add(1),
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
        let content_height = self.content_height().max(1);
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll.saturating_add(content_height) {
            self.scroll = line.saturating_sub(content_height.saturating_sub(1));
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn activate_action(&self, index: usize) -> Option<KeyAction> {
        let command = self.actions.get(index)?.command.clone();
        Some(KeyAction::Multiple(vec![
            self.close_action.clone(),
            Action::ExecuteLspCommand(Box::new(command)),
        ]))
    }
}

impl Component for HoverInfo {
    fn surface_actions(&self) -> Vec<UiAction> {
        self.dialog.actions()
    }
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;

        for (row, line) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(self.content_height())
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
        if let (Some((_, action)), Event::Key(key)) = (&self.confirm_action, event) {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                return Some(KeyAction::Single(action.clone()));
            }
        }
        if let (Some(id), Event::Key(key)) = (self.inline_navigation, event) {
            if matches!(key.code, KeyCode::Char('[' | ']')) && key.modifiers.is_empty() {
                return Some(KeyAction::Single(
                    Action::NavigateOverlappingInlineComment {
                        id,
                        backwards: key.code == KeyCode::Char('['),
                        open: true,
                    },
                ));
            }
        }
        let redraw = || Some(KeyAction::Single(Action::Refresh));
        match event {
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Char('q'), _) => {
                    Some(KeyAction::Single(self.close_action.clone()))
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
                    self.scroll_by(-(self.content_height().max(1) as isize));
                    redraw()
                }
                (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    self.scroll_by(self.content_height().max(1) as isize);
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
                        && (content_y..content_y.saturating_add(self.content_height()))
                            .contains(&(mouse.row as usize))
                    {
                        let line = self.scroll.saturating_add(mouse.row as usize - content_y);
                        if let Some(Some(action)) = self.line_actions.get(line) {
                            self.selected_action = Some(*action);
                            return redraw();
                        }
                        None
                    } else {
                        Some(KeyAction::Single(self.close_action.clone()))
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
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.reflow(self.viewport_width, self.viewport_height);
    }
}

fn render_lines(
    source: &str,
    format: HoverInfoFormat,
    available_width: usize,
    theme: &Theme,
    registry: &Arc<LanguageRegistry>,
    actions: &[HoverAction],
) -> (Vec<RenderedTextLine>, Vec<Option<usize>>, usize) {
    if available_width == 0 {
        return (Vec::new(), Vec::new(), 0);
    }
    let mut highlighter = Highlighter::with_registry(theme, Arc::clone(registry)).ok();
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
        .chain(
            content_lines
                .iter()
                .map(|line| markdown_rule_prefix_width(line).unwrap_or_else(|| line_width(line))),
        )
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
    for mut line in content_lines {
        if let Some(prefix_width) = markdown_rule_prefix_width(&line) {
            if let Some(rule) = line.spans.last_mut() {
                rule.text = "─".repeat(width.saturating_sub(prefix_width));
            }
        }
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

fn line_width(line: &RenderedTextLine) -> usize {
    line.spans
        .iter()
        .map(|span| display_width(&span.text))
        .sum()
}

fn markdown_rule_prefix_width(line: &RenderedTextLine) -> Option<usize> {
    let rule = line.spans.last()?;
    if rule.style != TextPanelSpanStyle::Muted
        || rule.text.is_empty()
        || !rule.text.chars().all(|character| character == '─')
    {
        return None;
    }
    Some(
        line.spans[..line.spans.len().saturating_sub(1)]
            .iter()
            .map(|span| display_width(&span.text))
            .sum(),
    )
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
    paint_rich_text(buffer, x, y, width, line, |span| {
        let mut style = hover_span_style(span, theme);
        if selected {
            let selection = theme.list_selection_style();
            style = theme.selected_style(&style, &selection, SelectionForegroundPriority::Content);
        }
        style
    });
}

pub(crate) fn hover_span_style(span: &RenderedTextSpan, theme: &Theme) -> Style {
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
    fn hover_footer_reserves_an_interior_row_without_overwriting_content_or_border() {
        let editor = test_editor(Theme::default(), 80, 24);
        let info = HoverInfo::new(
            &editor,
            "The first documentation line\nThe final documentation line".to_string(),
            HoverInfoFormat::Plaintext,
            Vec::new(),
        );
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        info.draw(&mut buffer).unwrap();

        let first_start = (info.y + 1) * buffer.width + info.x + 1;
        let last_start = (info.y + 2) * buffer.width + info.x + 1;
        let footer_y = info.y + info.height;
        let footer_start = footer_y * buffer.width + info.x + 1;
        let first = buffer.cells[first_start..first_start + info.width]
            .iter()
            .map(|cell| cell.c)
            .collect::<String>();
        let last = buffer.cells[last_start..last_start + info.width]
            .iter()
            .map(|cell| cell.c)
            .collect::<String>();
        let footer = buffer.cells[footer_start..footer_start + info.width]
            .iter()
            .map(|cell| cell.c)
            .collect::<String>();

        assert!(first.contains("The first documentation line"), "{first:?}");
        assert!(last.contains("The final documentation line"), "{last:?}");
        assert!(footer.contains("Esc close"), "{footer:?}");
        assert_eq!(buffer.cells[(footer_y + 1) * buffer.width + info.x].c, '└');
        assert_eq!(
            buffer.cells[(footer_y + 1) * buffer.width + info.x + info.width + 1].c,
            '┘'
        );
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
    fn markdown_rules_expand_to_content_width_without_defining_it() {
        let editor = test_editor(Theme::default(), 160, 30);
        let info = HoverInfo::new(
            &editor,
            "```rust\nlet language_name: &str\n```\n\n---\n\nsize = 16, align = 8".to_string(),
            HoverInfoFormat::Markdown,
            Vec::new(),
        );

        assert!(info.width < MAX_PROSE_HOVER_WIDTH);
        let rule = info
            .lines
            .iter()
            .find(|line| markdown_rule_prefix_width(line).is_some())
            .unwrap();
        assert_eq!(line_width(rule), info.width);
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
