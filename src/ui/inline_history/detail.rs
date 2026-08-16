//! Structured, read-only History content rendered by the existing Markdown pipeline.

use crate::{
    color::Color,
    highlighter::Highlighter,
    inline_history::HistoryView,
    plugin::{
        markdown::{
            render_code_lines_with_highlighter, render_diff_lines_with_highlighter,
            render_hover_markdown_lines_with_highlighter, wrap_plain_text, wrap_spans,
            RenderedTextLine, RenderedTextSpan, TextPanelSpanSelection, TextPanelSpanStyle,
        },
        TextPanelFileLocation, TextPanelLink, TextPanelLinkTarget,
    },
    theme::{Style, SurfacePalette, Theme},
    unicode_utils::{truncate_display_width_with_marker, TruncationSide},
};
use std::path::{Path, PathBuf};

pub(super) const SOURCE_LINK: u64 = u64::MAX;

#[derive(Clone, Copy, Debug)]
pub(crate) enum HistoryTone {
    Muted,
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub(crate) struct HistoryStatus {
    pub text: String,
    pub tone: HistoryTone,
}

impl HistoryStatus {
    pub(crate) fn new(text: impl Into<String>, tone: HistoryTone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
    fn style(&self, theme: &Theme) -> Style {
        let palette = SurfacePalette::new(theme, &theme.ui_style.dialog);
        let (key, fallback) = match self.tone {
            HistoryTone::Muted => return palette.muted,
            HistoryTone::Info => return palette.accent,
            HistoryTone::Success => (
                "gitDecoration.addedResourceForeground",
                Color::Rgb {
                    r: 75,
                    g: 170,
                    b: 95,
                },
            ),
            HistoryTone::Warning => (
                "notificationsWarningIcon.foreground",
                Color::Rgb {
                    r: 190,
                    g: 140,
                    b: 40,
                },
            ),
            HistoryTone::Error => return palette.error,
        };
        theme.ensure_text_contrast(&Style {
            fg: Some(theme.colors.get(key).copied().unwrap_or(fallback)),
            ..palette.primary
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) enum HistoryBlock {
    Request(String),
    Plain(String),
    Markdown(String),
    Status(HistoryStatus),
    FileLink {
        text: String,
        path: String,
        line: usize,
    },
    Code {
        file: String,
        source: String,
    },
    Diff {
        file: String,
        before: String,
        after: String,
        label: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct HistoryDetail {
    pub location: Option<String>,
    pub can_jump: bool,
    pub cwd: PathBuf,
    pub statuses: Vec<HistoryStatus>,
    pub blocks: Vec<HistoryBlock>,
    pub view: HistoryView,
    pub open_label: &'static str,
}

impl Default for HistoryDetail {
    fn default() -> Self {
        Self {
            location: None,
            can_jump: false,
            cwd: PathBuf::new(),
            statuses: Vec::new(),
            blocks: Vec::new(),
            view: HistoryView::default(),
            open_label: "open",
        }
    }
}

impl From<String> for HistoryDetail {
    fn from(text: String) -> Self {
        Self {
            blocks: vec![HistoryBlock::Plain(text)],
            ..Self::default()
        }
    }
}
impl From<&str> for HistoryDetail {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl HistoryDetail {
    pub(super) fn render(
        &self,
        width: usize,
        compact: bool,
        theme: &Theme,
        mut highlighter: Option<&mut Highlighter>,
    ) -> Vec<RenderedTextLine> {
        if width == 0 {
            return Vec::new();
        }
        let mut lines = Vec::new();
        if let Some(location) = &self.location {
            let text =
                truncate_display_width_with_marker(location, width, "…", TruncationSide::Left);
            let mut line = RenderedTextLine::plain(
                text,
                if self.can_jump {
                    TextPanelSpanStyle::Link
                } else {
                    TextPanelSpanStyle::Muted
                },
            );
            if self.can_jump {
                line.spans[0].link = Some(TextPanelLink {
                    id: SOURCE_LINK,
                    target: TextPanelLinkTarget::File {
                        path: String::new(),
                        location: None,
                    },
                });
            }
            lines.push(line);
        }
        let mut statuses = Vec::new();
        for status in &self.statuses {
            if !statuses.is_empty() {
                statuses.push(
                    RenderedTextLine::plain(" · ".into(), TextPanelSpanStyle::Muted)
                        .spans
                        .remove(0),
                );
            }
            statuses.push(RenderedTextSpan {
                text: status.text.clone(),
                style: TextPanelSpanStyle::Text,
                syntax_style: Some(status.style(theme)),
                link: None,
                selection: TextPanelSpanSelection::Content,
            });
        }
        if !statuses.is_empty() {
            lines.extend(wrap_spans(&statuses, width, &[], &[]));
        }
        for block in &self.blocks {
            if matches!(block, HistoryBlock::Request(text) if compact && !text.contains('\n') && crate::unicode_utils::display_width(text) + 2 <= width)
            {
                continue;
            }
            if !lines.is_empty() {
                lines.push(RenderedTextLine::plain(
                    String::new(),
                    TextPanelSpanStyle::Text,
                ));
            }
            lines.extend(match block {
                HistoryBlock::Request(text) => {
                    wrap_plain_text(&format!("You: {text}"), width, TextPanelSpanStyle::User)
                }
                HistoryBlock::Plain(text) => wrap_plain_text(text, width, TextPanelSpanStyle::Text),
                HistoryBlock::Status(status) => {
                    let mut span =
                        RenderedTextLine::plain(status.text.clone(), TextPanelSpanStyle::Text)
                            .spans
                            .remove(0);
                    span.syntax_style = Some(status.style(theme));
                    wrap_spans(&[span], width, &[], &[])
                }
                HistoryBlock::FileLink { text, path, line } => {
                    let mut span = RenderedTextLine::plain(text.clone(), TextPanelSpanStyle::Link)
                        .spans
                        .remove(0);
                    span.link = Some(TextPanelLink {
                        id: 0,
                        target: TextPanelLinkTarget::File {
                            path: path.clone(),
                            location: Some(TextPanelFileLocation {
                                line: *line,
                                column: 1,
                            }),
                        },
                    });
                    wrap_spans(&[span], width, &[], &[])
                }
                HistoryBlock::Markdown(text) => render_hover_markdown_lines_with_highlighter(
                    text,
                    width,
                    highlighter.as_deref_mut(),
                ),
                HistoryBlock::Code { file, source } => render_code_lines_with_highlighter(
                    file,
                    source,
                    width,
                    highlighter.as_deref_mut(),
                ),
                HistoryBlock::Diff {
                    file,
                    before,
                    after,
                    label,
                } => render_diff_lines_with_highlighter(
                    file,
                    before,
                    after,
                    label,
                    width,
                    theme,
                    highlighter.as_deref_mut(),
                ),
            });
        }
        lines
    }

    pub(super) fn file_target(
        &self,
        path: &str,
        location: &Option<TextPanelFileLocation>,
    ) -> crate::inline_history::HistoryAction {
        let path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        };
        crate::inline_history::HistoryAction::FollowFile {
            path: path.to_string_lossy().into_owned(),
            line: location.as_ref().map(|location| location.line),
            column: location.as_ref().map(|location| location.column),
        }
    }
}
