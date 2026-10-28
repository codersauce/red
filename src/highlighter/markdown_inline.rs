//! Source-preserving Markdown inline styles layered over the block grammar.

use crate::{
    color::{blend_color, Color},
    theme::{Style, Theme},
};

pub(super) const HIGHLIGHT_QUERY: &str = r#"
(emphasis) @markup.italic.markdown
(strong_emphasis) @markup.bold.markdown
(code_span) @markup.inline.raw.markdown
(emphasis_delimiter) @punctuation.definition.emphasis.markdown
(code_span_delimiter) @punctuation.definition.raw.markdown

[
  (link_text)
  (link_destination)
  (link_label)
  (uri_autolink)
  (email_autolink)
] @markup.underline.link.markdown

(inline_link ["[" "]" "(" ")"] @punctuation.definition.link.markdown)
(full_reference_link ["[" "]"] @punctuation.definition.link.markdown)
(collapsed_reference_link ["[" "]"] @punctuation.definition.link.markdown)
(shortcut_link ["[" "]"] @punctuation.definition.link.markdown)
(link_label ["[" "]"] @punctuation.definition.link.markdown)
(link_destination ["<" ">"] @punctuation.definition.link.markdown)
(image ["!" "[" "]" "(" ")"] @punctuation.definition.link.markdown)
"#;

pub(super) fn capture_style(theme: &Theme, scope: &str) -> Option<Style> {
    let scoped = theme.get_style(scope);
    match scope {
        "markup.bold.markdown" => Some(Style {
            bold: true,
            ..scoped.unwrap_or_default()
        }),
        "markup.italic.markdown" => Some(Style {
            italic: true,
            ..scoped.unwrap_or_default()
        }),
        "markup.underline.link.markdown" => {
            let mut style = scoped.unwrap_or_default();
            style.fg = style
                .fg
                .or_else(|| theme.colors.get("textLink.foreground").copied())
                .or(theme.ui_style.popup_title.fg)
                .or(theme.style.fg);
            style.underline = true;
            Some(style)
        }
        "markup.inline.raw.markdown" => {
            let mut style = scoped
                .or_else(|| theme.get_style("markup.inline.raw.string.markdown"))
                .or_else(|| theme.get_style("markup.raw.block.markdown"))
                .unwrap_or_default();
            style.fg = style.fg.or(theme.style.fg);
            style.bg = style.bg.or_else(|| code_background(theme));
            Some(style)
        }
        "punctuation.definition.emphasis.markdown"
        | "punctuation.definition.raw.markdown"
        | "punctuation.definition.link.markdown" => Some(scoped.unwrap_or(Style {
            fg: theme.ui_style.muted.fg.or(theme.style.fg),
            ..Style::default()
        })),
        _ => scoped,
    }
}

/// Code-block fills often match the cursor row. Keep the fallback inline fill
/// distinct on both surfaces; explicit token backgrounds still take precedence.
fn code_background(theme: &Theme) -> Option<Color> {
    let line_background = theme
        .line_highlight_style
        .as_ref()
        .and_then(|style| style.bg);
    let distinct =
        |color: &Color| Some(*color) != theme.style.bg && Some(*color) != line_background;
    if let Some(background) = theme
        .colors
        .get("textCodeBlock.background")
        .copied()
        .filter(distinct)
    {
        return Some(background);
    }
    let background = theme.style.bg?;
    let neutral = if background.is_light() { 0 } else { 255 };
    [16, 24]
        .into_iter()
        .map(|a| {
            blend_color(
                Color::Rgba {
                    r: neutral,
                    g: neutral,
                    b: neutral,
                    a,
                },
                background,
            )
        })
        .find(distinct)
}

/// The editor selects the narrowest capture instead of merging overlapping
/// styles. Preserve outer emphasis and links on nested content while leaving delimiters
/// independently themeable and subdued.
pub(super) fn inherit_inline_style(
    node: tree_sitter::Node<'_>,
    scope: &str,
    style: &mut Style,
    theme: &Theme,
) {
    if scope.starts_with("punctuation.") {
        return;
    }
    let mut ancestor = Some(node);
    while let Some(node) = ancestor {
        match node.kind() {
            "strong_emphasis" => style.bold = true,
            "emphasis" => style.italic = true,
            "link_text" => {
                style.underline = true;
                if scope != "markup.inline.raw.markdown" {
                    style.fg = capture_style(theme, "markup.underline.link.markdown")
                        .and_then(|link| link.fg)
                        .or(style.fg);
                }
            }
            _ => {}
        }
        ancestor = node.parent();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        editor::StyleInfo,
        highlighter::{Highlighter, LanguageRegistry},
        theme::{parse_vscode_theme, TokenStyle},
    };

    // Match the editor's shortest-range / latest-capture precedence.
    fn style_at<'a>(styles: &'a [StyleInfo], source: &str, text: &str) -> Option<&'a Style> {
        let byte = source.find(text).unwrap();
        styles
            .iter()
            .enumerate()
            .filter(|(_, span)| span.start <= byte && byte < span.end)
            .min_by_key(|(order, span)| (span.end - span.start, std::cmp::Reverse(*order)))
            .map(|(_, span)| &span.style)
    }

    fn highlight(theme: &Theme, source: &str) -> Vec<StyleInfo> {
        Highlighter::new(theme)
            .unwrap()
            .highlight("markdown", source)
            .unwrap()
    }

    fn shape(styles: &[StyleInfo]) -> Vec<(usize, usize, Style)> {
        styles
            .iter()
            .map(|span| (span.start, span.end, span.style.clone()))
            .collect()
    }

    #[test]
    fn emphasis_combines_nested_attributes_and_keeps_delimiters_subdued() {
        let source =
            "Plain **bold** _italic_ ***both*** **outer *inner* end** *outer __nested__ end*";
        let theme = Theme::default();
        let styles = highlight(&theme, source);
        assert!(style_at(&styles, source, "Plain").is_none());
        for (text, bold, italic) in [
            ("bold", true, false),
            ("italic", false, true),
            ("both", true, true),
            ("inner", true, true),
            ("nested", true, true),
            ("end", true, false),
        ] {
            let style = style_at(&styles, source, text).unwrap();
            assert_eq!((style.bold, style.italic), (bold, italic), "{text}");
        }
        let delimiter = style_at(&styles, source, "**bold").unwrap();
        assert_eq!(delimiter.fg, theme.ui_style.muted.fg);
        assert!(!delimiter.bold && !delimiter.italic);
    }

    #[test]
    fn code_spans_keep_literal_markup_and_support_multiple_backticks() {
        let source = "`**literal**` and ``a `tick` *literal*`` and **`strong code`**";
        let styles = highlight(&Theme::default(), source);
        for text in ["literal", "tick", "*literal*"] {
            let style = style_at(&styles, source, text).unwrap();
            assert!(!style.bold && !style.italic, "{text}");
            assert!(style.bg.is_some(), "{text}");
        }
        assert!(style_at(&styles, source, "strong code").unwrap().bold);
    }

    #[test]
    fn markdown_links_style_labels_destinations_references_and_autolinks() {
        let source = "[docs](https://example.test/docs_(v1) \"Title\") [local](./src/main.rs#L10)\n\n[full][ref] [collapsed][] [shortcut]\n\n<https://auto.example.test> <person@example.test>\n\n[ref]: https://reference.example.test\n[collapsed]: ./collapsed.md\n[shortcut]: ./shortcut.md\n";
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let styles = highlight(&theme, source);
        let link = capture_style(&theme, "markup.underline.link.markdown").unwrap();
        for text in [
            "docs",
            "https://example.test",
            "local",
            "./src/main.rs",
            "full",
            "ref]",
            "collapsed",
            "shortcut",
            "https://auto.example.test",
            "person@example.test",
            "https://reference.example.test",
        ] {
            let style = style_at(&styles, source, text).unwrap();
            assert!(style.underline, "{text}");
            assert_eq!(style.fg, link.fg, "{text}");
        }
        for delimiter in ["[docs", "](https", "(https", "[ref]"] {
            let style = style_at(&styles, source, delimiter).unwrap();
            assert!(!style.underline, "{delimiter}");
            assert_ne!(style.fg, link.fg, "{delimiter}");
        }
        assert!(style_at(&styles, source, "Title").is_none());
        let fallback = highlight(&Theme::default(), "[fallback](relative.md)");
        assert!(
            style_at(&fallback, "[fallback](relative.md)", "fallback")
                .unwrap()
                .underline
        );
    }

    #[test]
    fn links_preserve_nested_emphasis_and_code_even_when_capture_ranges_match() {
        let source =
            "[**bold**](a) [*italic*](b) [`code`](c) [plain **strong** and *em*](d) **[outer](e)**";
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let styles = highlight(&theme, source);
        let link = capture_style(&theme, "markup.underline.link.markdown").unwrap();
        for (text, bold, italic) in [
            ("bold", true, false),
            ("italic", false, true),
            ("strong", true, false),
            ("em", false, true),
            ("outer", true, false),
        ] {
            let style = style_at(&styles, source, text).unwrap();
            assert_eq!(
                (style.bold, style.italic, style.underline),
                (bold, italic, true),
                "{text}"
            );
            assert_eq!(style.fg, link.fg, "{text}");
        }
        let code = style_at(&styles, source, "code").unwrap();
        assert!(code.underline);
        assert_eq!(
            code.bg,
            capture_style(&theme, "markup.inline.raw.markdown")
                .unwrap()
                .bg
        );
        assert_ne!(code.fg, link.fg);
    }

    #[test]
    fn link_edits_preserve_fresh_styles_and_leave_code_and_escapes_literal() {
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let source =
            "`[literal](code.md)`\n\n\\[escaped](escape.md)\n\n```text\n[fenced](fence.md)\n```\n";
        let styles = highlight(&theme, source);
        for text in [
            "literal",
            "code.md",
            "escaped",
            "escape.md",
            "fenced",
            "fence.md",
        ] {
            assert!(
                style_at(&styles, source, text).is_none_or(|style| !style.underline),
                "{text}"
            );
        }
        let mut incremental = Highlighter::new(&theme).unwrap();
        for source in [
            "## Before [**café**](one.md) after\r\n",
            "## Before [**café**](two.md) after\r\n",
            "## Before [**café**](two.md after\r\n",
            "## Before [**café**](two.md) after\r\n",
        ] {
            let actual = incremental.highlight("markdown", source).unwrap();
            assert_eq!(
                shape(&actual),
                shape(&highlight(&theme, source)),
                "{source}"
            );
        }
    }

    #[test]
    fn multiline_emphasis_preserves_source_ranges_and_block_markers() {
        let source =
            "**first\nsecond**\n\n> *quoted\n> continuation*\n\n- **listed\n  continuation**\n";
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let styles = highlight(&theme, source);
        assert!(style_at(&styles, source, "second").unwrap().bold);
        assert!(style_at(&styles, source, "continuation*").unwrap().italic);
        assert!(style_at(&styles, source, "continuation**").unwrap().bold);
        assert!(!style_at(&styles, source, "> continuation").unwrap().italic);
    }

    #[test]
    fn source_contexts_receive_inline_styles_without_styling_code_blocks() {
        let source = "# A *heading* with `code`\n\n- **list**\n\n> _quote_\n\n| **header** | value |\n| --- | --- |\n| `cell` | *body* |\n\n```text\n**fenced**\n```\n\n    **indented**\n\nEscaped \\*plain\\* and snake_case_name and **unclosed\n";
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let styles = highlight(&theme, source);
        for text in ["list", "header"] {
            assert!(style_at(&styles, source, text).unwrap().bold, "{text}");
        }
        for text in ["heading", "quote", "body"] {
            assert!(style_at(&styles, source, text).unwrap().italic, "{text}");
        }
        assert!(style_at(&styles, source, "heading").unwrap().bold);
        assert!(style_at(&styles, source, "code").unwrap().bold);
        assert!(style_at(&styles, source, "cell").unwrap().bg.is_some());
        for text in ["fenced", "indented", "plain", "snake_case_name", "unclosed"] {
            assert!(
                style_at(&styles, source, text).is_none_or(|style| !style.bold && !style.italic),
                "{text}"
            );
        }
        assert!(styles.iter().all(|span| {
            source.is_char_boundary(span.start)
                && source.is_char_boundary(span.end)
                && span.end <= source.len()
        }));
    }

    #[test]
    fn inline_code_background_tracks_light_and_dark_themes_and_respects_overrides() {
        for theme_file in ["themes/red.json", "themes/github-light.json"] {
            let theme = parse_vscode_theme(theme_file).unwrap();
            let style = capture_style(&theme, "markup.inline.raw.markdown").unwrap();
            assert_ne!(style.bg, theme.style.bg, "{theme_file}");
            assert_ne!(
                style.bg,
                theme
                    .line_highlight_style
                    .as_ref()
                    .and_then(|style| style.bg),
                "{theme_file}"
            );
            assert!(style.fg.is_some(), "{theme_file}");
        }
        let explicit = Style {
            fg: Some(Color::Rgb {
                r: 200,
                g: 100,
                b: 50,
            }),
            bg: Some(Color::Rgb {
                r: 20,
                g: 30,
                b: 40,
            }),
            ..Style::default()
        };
        for scope in ["markup.inline.raw", "markup.inline.raw.string.markdown"] {
            let theme = Theme {
                token_styles: vec![TokenStyle {
                    name: None,
                    scope: vec![scope.to_string()],
                    style: explicit.clone(),
                }],
                ..Theme::default()
            };
            assert_eq!(
                capture_style(&theme, "markup.inline.raw.markdown"),
                Some(explicit.clone())
            );
        }
    }

    #[test]
    fn inline_edits_and_undo_match_fresh_highlighting() {
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        for sources in [
            vec![
                "## before *word * after\n",
                "## before *word* after\n",
                "## before *word * after\n",
            ],
            vec![
                "Text **世界😀** and `code`\r\n",
                "Text *世界😀* and ``code``\r\n",
                "Text 世界😀 and ``code``\r\n",
            ],
            vec![
                "## **bold** `code`\n",
                "## **bold* `code`\n",
                "## **bold** `code`\n",
            ],
            vec![
                "Plain\n\n```rust\nlet value = 1234;\n```\n\n**after**",
                "Plain\n\n```rust\nlet value = 12934;\n```\n\n**after**",
            ],
        ] {
            let mut incremental = Highlighter::new(&theme).unwrap();
            for source in sources {
                let actual = incremental.highlight("markdown", source).unwrap();
                assert_eq!(
                    shape(&actual),
                    shape(&highlight(&theme, source)),
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn customized_inline_queries_disable_plain_heading_capture_reuse() {
        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("markdown_inline")
            .unwrap()
            .highlight_queries = vec![
            "((inline) @markup.bold.markdown (#match? @markup.bold.markdown \"special\"))"
                .to_string(),
        ];
        let theme = Theme::default();
        let mut highlighter = Highlighter::with_registry(&theme, Arc::new(registry)).unwrap();
        highlighter
            .highlight("markdown", "# a specil heading\n")
            .unwrap();
        let source = "# a special heading\n";
        let styles = highlighter.highlight("markdown", source).unwrap();
        assert!(style_at(&styles, source, "special").unwrap().bold);

        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("markdown_inline")
            .unwrap()
            .injection_query =
            Some("((inline) @injection.content (#set! injection.language \"rust\"))".to_string());
        let registry = Arc::new(registry);
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut incremental = Highlighter::with_registry(&theme, Arc::clone(&registry)).unwrap();
        incremental.highlight("markdown", "# a le value\n").unwrap();
        let source = "# a let value\n";
        let actual = incremental.highlight("markdown", source).unwrap();
        let expected = Highlighter::with_registry(&theme, registry)
            .unwrap()
            .highlight("markdown", source)
            .unwrap();
        assert_eq!(shape(&actual), shape(&expected));
        assert!(!incremental.highlighters["markdown"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }
}
