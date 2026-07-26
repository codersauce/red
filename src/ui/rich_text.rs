//! Grapheme-safe painting for the existing parsed Markdown span model.

use crate::{
    editor::RenderBuffer,
    plugin::markdown::{RenderedTextLine, RenderedTextSpan},
    theme::Style,
    unicode_utils::{display_width, truncate_display_width},
};

/// Paints a styled line without splitting graphemes or crossing its terminal-cell bounds.
///
/// Callers retain ownership of their surface-specific theme, background, link selection,
/// and syntax-highlighting policies.
pub(crate) fn paint_rich_text(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    line: &RenderedTextLine,
    mut resolve_style: impl FnMut(&RenderedTextSpan) -> Style,
) {
    let mut used = 0_usize;
    for span in &line.spans {
        if used >= width {
            break;
        }
        let text = truncate_display_width(&span.text, width.saturating_sub(used));
        if text.is_empty() {
            continue;
        }
        buffer.set_text(x.saturating_add(used), y, &text, &resolve_style(span));
        used = used.saturating_add(display_width(&text));
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        editor::RenderBuffer,
        plugin::markdown::{RenderedTextLine, RenderedTextSpan, TextPanelSpanStyle},
        theme::Style,
    };

    use super::paint_rich_text;

    #[test]
    fn rich_text_painting_clips_at_complete_graphemes() {
        let style = Style::default();
        let mut buffer = RenderBuffer::new(8, 1, &style);
        let line = RenderedTextLine {
            spans: vec![
                RenderedTextSpan {
                    text: "a👨‍👩‍👧".to_string(),
                    style: TextPanelSpanStyle::Text,
                    syntax_style: None,
                    link: None,
                },
                RenderedTextSpan {
                    text: "世z".to_string(),
                    style: TextPanelSpanStyle::Strong,
                    syntax_style: None,
                    link: None,
                },
            ],
        };

        paint_rich_text(&mut buffer, 1, 0, 5, &line, |_| style.clone());

        assert_eq!(buffer.cells[1].c, 'a');
        assert_eq!(buffer.cells[2].c, '👨');
        assert_eq!(buffer.cells[4].c, '世');
        assert_eq!(buffer.cells[6].c, ' ');
    }

    #[test]
    fn zero_width_rich_text_does_not_modify_the_surface() {
        let style = Style::default();
        let mut buffer = RenderBuffer::new(3, 1, &style);
        let line = RenderedTextLine::plain("unchanged".to_string(), TextPanelSpanStyle::Text);

        paint_rich_text(&mut buffer, 0, 0, 0, &line, |_| style.clone());

        assert!(buffer.cells.iter().all(|cell| cell.c == ' '));
    }
}
