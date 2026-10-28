//! List and task marker styles that retain every source character and column.

use crate::{
    editor::StyleInfo,
    theme::{Style, Theme},
};

pub(super) fn capture_style(theme: &Theme, scope: &str) -> Option<Style> {
    match scope {
        "punctuation.definition.list.begin.markdown" | "punctuation.definition.task.markdown" => {
            Some(theme.get_style(scope).unwrap_or(Style {
                fg: theme.ui_style.muted.fg.or(theme.style.fg),
                ..Style::default()
            }))
        }
        "constant.numeric.list.markdown" => Some(Style {
            bold: true,
            ..theme.get_style(scope).unwrap_or(Style {
                fg: theme.style.fg,
                ..Style::default()
            })
        }),
        "markup.list.checked.markdown" => {
            let mut style = theme.get_style(scope).unwrap_or_default();
            style.fg = style
                .fg
                .or_else(|| {
                    theme
                        .get_style("markup.inserted")
                        .and_then(|style| style.fg)
                })
                .or_else(|| {
                    theme
                        .colors
                        .get("gitDecoration.addedResourceForeground")
                        .copied()
                })
                .or(theme.ui_style.popup_title.fg)
                .or(theme.style.fg);
            style.bold = true;
            Some(style)
        }
        _ => None,
    }
}

/// The block grammar exposes whole markers; keep emphasis off their punctuation.
pub(super) fn refine_capture(scope: &str, source: &str, span: &mut StyleInfo) {
    let text = &source[span.start..span.end];
    match scope {
        "constant.numeric.list.markdown" => {
            span.end = span.start + text.bytes().take_while(u8::is_ascii_digit).count();
        }
        "markup.list.checked.markdown" if matches!(text, "[x]" | "[X]") => {
            span.start += 1;
            span.end -= 1;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlighter::Highlighter;

    fn style_at<'a>(styles: &'a [StyleInfo], source: &str, needle: &str) -> Option<&'a Style> {
        let byte = source.find(needle).unwrap();
        styles
            .iter()
            .enumerate()
            .filter(|(_, span)| span.start <= byte && byte < span.end)
            .min_by_key(|(order, span)| (span.end - span.start, std::cmp::Reverse(*order)))
            .map(|(_, span)| &span.style)
    }

    #[test]
    fn list_markers_style_numbers_and_task_states_without_styling_item_text() {
        let source = "- bullet\n  + nested\n\n* other\n\n12. numbered\n\n3) parenthesis\n\n- [ ] pending\n- [x] done **bold**\n- [X] upper [link](target.md)\n";
        let theme = Theme::default();
        let styles = Highlighter::new(&theme)
            .unwrap()
            .highlight("markdown", source)
            .unwrap();
        for marker in [
            "- bullet",
            "+ nested",
            "* other",
            ". numbered",
            ") parenthesis",
            "[ ]",
            "[x]",
            "] done",
            "[X]",
            "] upper",
        ] {
            let style = style_at(&styles, source, marker).unwrap();
            assert_eq!(style.fg, theme.ui_style.muted.fg, "{marker}");
            assert!(!style.bold, "{marker}");
        }
        for marker in ["12.", "3)", "x]", "X]"] {
            assert!(style_at(&styles, source, marker).unwrap().bold, "{marker}");
        }
        for text in ["bullet", "nested", "numbered", "pending", "done", "upper"] {
            assert!(style_at(&styles, source, text).is_none(), "{text}");
        }
        assert!(style_at(&styles, source, "bold").unwrap().bold);
        assert!(style_at(&styles, source, "link").unwrap().underline);
    }

    #[test]
    fn literal_task_markers_do_not_receive_checked_styles() {
        let source = "```text\n- [x] fenced\n```\n\n    - [X] indented\n\n`- [x] inline`\n\nplain [x] text\n\n- \\[x] escaped\n- [x]missing-space\n";
        let styles = Highlighter::new(&Theme::default())
            .unwrap()
            .highlight("markdown", source)
            .unwrap();
        for (byte, _) in source.match_indices('x').chain(source.match_indices('X')) {
            assert!(
                !styles
                    .iter()
                    .any(|span| span.start <= byte && byte < span.end && span.style.bold),
                "byte {byte}"
            );
        }
    }

    #[test]
    fn task_and_list_edits_match_fresh_highlighting() {
        let theme = Theme::default();
        let mut incremental = Highlighter::new(&theme).unwrap();
        for source in [
            "- [ ] café\n  - child\n",
            "- [x] café\n  - child\n",
            "- [X] café\n  - child\n",
            "10. [X] café\n    - child\n",
            "10. \\[X] café\n    - child\n",
            "```\n10. [X] café\n    - child\n```\n",
            "- [ ] café\n",
        ] {
            let actual = incremental.highlight("markdown", source).unwrap();
            let expected = Highlighter::new(&theme)
                .unwrap()
                .highlight("markdown", source)
                .unwrap();
            let shape = |styles: Vec<StyleInfo>| {
                styles
                    .into_iter()
                    .map(|span| (span.start, span.end, span.style))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(actual), shape(expected), "{source}");
        }
    }
}
