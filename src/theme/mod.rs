mod vscode;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
pub use vscode::parse_vscode_theme;
pub use vscode::parse_vscode_theme_contents;

use crate::color::{blend_color, ensure_minimum_contrast, Color};

pub(crate) const MINIMUM_SELECTION_STATE_CONTRAST: f32 = 3.0;
pub(crate) const MINIMUM_SELECTION_TEXT_CONTRAST: f32 = 4.5;
pub(crate) const MINIMUM_CURSOR_STATE_CONTRAST: f32 = 3.0;
pub(crate) const MINIMUM_CURSOR_TEXT_CONTRAST: f32 = 4.5;

#[derive(Clone, Copy)]
pub(crate) enum SelectionForegroundPriority {
    Content,
    Selection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    #[allow(unused)]
    pub name: String,
    #[serde(default)]
    pub colors: BTreeMap<String, Color>,
    pub style: Style,
    pub gutter_style: Style,
    pub statusline_style: StatuslineStyle,
    pub ui_style: UiStyle,
    pub token_styles: Vec<TokenStyle>,
    pub line_highlight_style: Option<Style>,
    pub find_match_style: Option<Style>,
    pub find_match_highlight_style: Option<Style>,
    pub selection_style: Option<Style>,
    pub cursor_style: Option<Style>,
    pub error_style: Option<Style>,
}

/// A theme-derived style requested by a plugin.
///
/// Color references are tried in order. Workbench color keys such as
/// `symbolIcon.functionForeground` resolve from [`Theme::colors`], while
/// `scope:entity.name.function` resolves from TextMate token styles.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeStyleSpec {
    #[serde(default)]
    pub foreground: Vec<String>,
    #[serde(default)]
    pub background: Vec<String>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
}

impl Theme {
    pub fn get_style(&self, scope: &str) -> Option<Style> {
        compatible_scopes(scope).into_iter().find_map(|candidate| {
            self.token_styles.iter().find_map(|ts| {
                if ts.scope.contains(&candidate) {
                    Some(ts.style.clone())
                } else {
                    None
                }
            })
        })
    }

    pub(crate) fn editor_selection_style(&self) -> Style {
        self.selection_style.clone().unwrap_or(Style {
            bg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            ..Style::default()
        })
    }

    pub(crate) fn list_selection_style(&self) -> Style {
        Style {
            fg: self
                .colors
                .get("list.activeSelectionForeground")
                .copied()
                .or(self.ui_style.picker_selected_item.fg),
            bg: self
                .colors
                .get("list.activeSelectionBackground")
                .copied()
                .or(self.ui_style.picker_selected_item.bg),
            ..self.ui_style.picker_selected_item.clone()
        }
    }

    pub(crate) fn selected_style(
        &self,
        content: &Style,
        selection: &Style,
        foreground_priority: SelectionForegroundPriority,
    ) -> Style {
        compose_selection_style(&self.style, content, selection, foreground_priority)
    }

    pub(crate) fn synthetic_cursor_style(&self, content: &Style) -> Style {
        compose_synthetic_cursor_style(&self.style, content, self.cursor_style.as_ref())
    }

    pub(crate) fn ensure_text_contrast(&self, style: &Style) -> Style {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let editor_bg = blend_color(self.style.bg.unwrap_or(black), black);
        let background = blend_color(style.bg.unwrap_or(editor_bg), editor_bg);
        let foreground = blend_color(
            style.fg.or(self.style.fg).unwrap_or(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            background,
        );
        Style {
            fg: Some(ensure_minimum_contrast(
                foreground,
                background,
                MINIMUM_SELECTION_TEXT_CONTRAST,
            )),
            bg: Some(background),
            ..style.clone()
        }
    }

    pub fn resolve_style(&self, spec: &ThemeStyleSpec) -> Style {
        Style {
            fg: self.resolve_color_references(&spec.foreground, StyleColorComponent::Foreground),
            bg: self.resolve_color_references(&spec.background, StyleColorComponent::Background),
            bold: spec.bold.unwrap_or(false),
            italic: spec.italic.unwrap_or(false),
        }
    }

    fn resolve_color_references(
        &self,
        references: &[String],
        component: StyleColorComponent,
    ) -> Option<Color> {
        references
            .iter()
            .find_map(|reference| self.resolve_color_reference(reference, component))
    }

    fn resolve_color_reference(
        &self,
        reference: &str,
        component: StyleColorComponent,
    ) -> Option<Color> {
        if let Some(scope) = reference.strip_prefix("scope:") {
            return self
                .get_style(scope)
                .and_then(|style| component.get(&style));
        }

        match reference {
            "editor.foreground" => self.style.fg,
            "editor.background" => self.style.bg,
            _ => self.colors.get(reference).copied(),
        }
    }
}

pub(crate) fn compose_synthetic_cursor_style(
    editor_style: &Style,
    content: &Style,
    cursor: Option<&Style>,
) -> Style {
    let black = Color::Rgb { r: 0, g: 0, b: 0 };
    let white = Color::Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    let editor_bg = blend_color(editor_style.bg.unwrap_or(black), black);
    let surface_bg = blend_color(content.bg.unwrap_or(editor_bg), editor_bg);
    let requested_bg = cursor
        .and_then(|style| style.fg)
        .or(editor_style.fg)
        .unwrap_or(white);
    let cursor_bg =
        ensure_minimum_contrast(requested_bg, surface_bg, MINIMUM_CURSOR_STATE_CONTRAST);
    let requested_fg = cursor
        .and_then(|style| style.bg)
        .or(editor_style.bg)
        .unwrap_or(black);
    let cursor_fg = ensure_minimum_contrast(requested_fg, cursor_bg, MINIMUM_CURSOR_TEXT_CONTRAST);

    Style {
        fg: Some(cursor_fg),
        bg: Some(cursor_bg),
        bold: false,
        italic: false,
    }
}

pub(crate) fn compose_selection_style(
    editor_style: &Style,
    content: &Style,
    selection: &Style,
    foreground_priority: SelectionForegroundPriority,
) -> Style {
    let black = Color::Rgb { r: 0, g: 0, b: 0 };
    let editor_bg = blend_color(editor_style.bg.unwrap_or(black), black);
    let surface_bg = blend_color(content.bg.unwrap_or(editor_bg), editor_bg);
    let requested_bg = selection.bg.unwrap_or(surface_bg);
    let selected_bg = ensure_minimum_contrast(
        blend_color(requested_bg, surface_bg),
        surface_bg,
        MINIMUM_SELECTION_STATE_CONTRAST,
    );
    let requested_fg = match foreground_priority {
        SelectionForegroundPriority::Content => content.fg.or(selection.fg),
        SelectionForegroundPriority::Selection => selection.fg.or(content.fg),
    }
    .or(editor_style.fg)
    .unwrap_or(Color::Rgb {
        r: 255,
        g: 255,
        b: 255,
    });
    let selected_fg = ensure_minimum_contrast(
        blend_color(requested_fg, selected_bg),
        selected_bg,
        MINIMUM_SELECTION_TEXT_CONTRAST,
    );

    Style {
        fg: Some(selected_fg),
        bg: Some(selected_bg),
        bold: content.bold || selection.bold,
        italic: content.italic || selection.italic,
    }
}

#[derive(Clone, Copy)]
enum StyleColorComponent {
    Foreground,
    Background,
}

impl StyleColorComponent {
    fn get(self, style: &Style) -> Option<Color> {
        match self {
            Self::Foreground => style.fg,
            Self::Background => style.bg,
        }
    }
}

fn compatible_scopes(scope: &str) -> Vec<String> {
    let mut scopes = Vec::new();
    push_scope_with_parents(&mut scopes, scope);

    for alias in markdown_scope_aliases(scope) {
        push_scope_with_parents(&mut scopes, alias);
    }

    scopes
}

fn push_scope_with_parents(scopes: &mut Vec<String>, scope: &str) {
    push_unique_scope(scopes, scope);

    let mut boundary = scope.len();
    while let Some(previous) = scope[..boundary].rfind('.') {
        let parent = &scope[..previous];
        if parent.is_empty() {
            break;
        }
        push_unique_scope(scopes, parent);
        boundary = previous;
    }
}

fn push_unique_scope(scopes: &mut Vec<String>, scope: &str) {
    if !scopes.iter().any(|candidate| candidate == scope) {
        scopes.push(scope.to_string());
    }
}

fn markdown_scope_aliases(scope: &str) -> &'static [&'static str] {
    match scope {
        "heading.1.markdown"
        | "heading.2.markdown"
        | "heading.3.markdown"
        | "heading.4.markdown"
        | "heading.5.markdown"
        | "heading.6.markdown"
        | "markup.heading.setext.1.markdown"
        | "markup.heading.setext.2.markdown"
        | "punctuation.definition.heading.markdown" => &[
            "markup.heading.markdown",
            "markdown.heading",
            "markup.heading",
        ],
        "punctuation.definition.list.begin.markdown" => {
            &["punctuation.definition.list_item.markdown", "markup.list"]
        }
        "markup.raw.block.markdown" => &["markup.raw.block.fenced.markdown", "markup.raw.block"],
        "punctuation.definition.raw.markdown" => &["punctuation.definition.fenced.markdown"],
        "punctuation.definition.quote.begin.markdown" => {
            &["punctuation.definition.blockquote.markdown", "markup.quote"]
        }
        "markup.underline.link.markdown" => {
            &["string.other.link.title.markdown", "markup.underline"]
        }
        _ => &[],
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            colors: BTreeMap::new(),
            style: Style {
                fg: Some(Color::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                bg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
                bold: false,
                italic: false,
            },
            gutter_style: Style::default(),
            statusline_style: StatuslineStyle::default(),
            ui_style: UiStyle::default(),
            token_styles: vec![],
            line_highlight_style: None,
            find_match_style: None,
            find_match_highlight_style: None,
            selection_style: None,
            cursor_style: None,
            error_style: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStyle {
    #[allow(unused)]
    pub name: Option<String>,
    pub scope: Vec<String>,
    pub style: Style,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct StatuslineStyle {
    pub outer_style: Style,
    pub outer_chars: [char; 4],
    pub inner_style: Style,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiStyle {
    pub popup: Style,
    pub popup_border: Style,
    pub popup_title: Style,
    pub dialog: Style,
    pub dialog_border: Style,
    pub dialog_title: Style,
    pub picker_item: Style,
    pub picker_selected_item: Style,
    pub picker_prompt: Style,
    pub muted: Style,
    pub deprecated: Style,
}

impl Default for UiStyle {
    fn default() -> Self {
        let popup = Style {
            fg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            bg: Some(Color::Rgb {
                r: 67,
                g: 70,
                b: 89,
            }),
            ..Default::default()
        };

        Self {
            popup: popup.clone(),
            popup_border: Style {
                fg: Some(Color::Rgb {
                    r: 184,
                    g: 144,
                    b: 243,
                }),
                bg: popup.bg,
                ..Default::default()
            },
            popup_title: popup.clone(),
            dialog: popup.clone(),
            dialog_border: Style {
                fg: Some(Color::Rgb {
                    r: 184,
                    g: 144,
                    b: 243,
                }),
                bg: popup.bg,
                ..Default::default()
            },
            dialog_title: popup.clone(),
            picker_item: popup.clone(),
            picker_selected_item: Style {
                fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
                bg: Some(Color::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                ..Default::default()
            },
            picker_prompt: popup.clone(),
            muted: Style {
                fg: Some(Color::Rgb {
                    r: 128,
                    g: 128,
                    b: 128,
                }),
                bg: popup.bg,
                ..Default::default()
            },
            deprecated: Style {
                fg: Some(Color::Rgb { r: 128, g: 0, b: 0 }),
                bg: popup.bg,
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub fn fallback_bg(&self, fallback_bg: &Style) -> Style {
        let bg = self
            .bg
            .or(fallback_bg.bg)
            .or(Some(Color::Rgb { r: 0, g: 0, b: 0 }));
        self.with_bg(bg)
    }

    pub fn with_bg(&self, bg: Option<Color>) -> Style {
        Style { bg, ..self.clone() }
    }

    pub fn inverted(&self) -> Style {
        Style {
            fg: self.bg,
            bg: self.fg,
            bold: self.bold,
            italic: self.italic,
        }
    }
}

// impl Style {
//     pub fn fg(&self) -> Option<Color> {
//         if let Some(fg) = self.fg {
//             if let Some(bg) = self.bg {
//                 Some(crate::color::blend_color(fg, bg))
//             } else {
//                 Some(fg)
//             }
//         } else {
//             None
//         }
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::contrast_ratio;

    fn style(r: u8, g: u8, b: u8) -> Style {
        Style {
            fg: Some(Color::Rgb { r, g, b }),
            ..Default::default()
        }
    }

    fn theme_with_token_styles(token_styles: Vec<TokenStyle>) -> Theme {
        Theme {
            token_styles,
            ..Theme::default()
        }
    }

    fn cursor_contrast_theme(editor_fg: Color, editor_bg: Color, cursor_fg: Color) -> Theme {
        Theme {
            style: Style {
                fg: Some(editor_fg),
                bg: Some(editor_bg),
                ..Default::default()
            },
            cursor_style: Some(Style {
                fg: Some(cursor_fg),
                ..Default::default()
            }),
            ..Theme::default()
        }
    }

    #[test]
    fn synthetic_cursor_style_repairs_dark_on_dark_cursor_colors() {
        let dark = Color::Rgb {
            r: 34,
            g: 36,
            b: 54,
        };
        let theme = cursor_contrast_theme(
            Color::Rgb {
                r: 200,
                g: 211,
                b: 245,
            },
            dark,
            dark,
        );

        let cursor = theme.synthetic_cursor_style(&theme.style);

        assert!(contrast_ratio(cursor.bg.unwrap(), dark) >= MINIMUM_CURSOR_STATE_CONTRAST);
        assert!(
            contrast_ratio(cursor.fg.unwrap(), cursor.bg.unwrap()) >= MINIMUM_CURSOR_TEXT_CONTRAST
        );
    }

    #[test]
    fn synthetic_cursor_style_repairs_light_on_light_cursor_colors() {
        let light = Color::Rgb {
            r: 250,
            g: 250,
            b: 250,
        };
        let theme = cursor_contrast_theme(
            Color::Rgb {
                r: 56,
                g: 58,
                b: 66,
            },
            light,
            light,
        );

        let cursor = theme.synthetic_cursor_style(&theme.style);

        assert!(contrast_ratio(cursor.bg.unwrap(), light) >= MINIMUM_CURSOR_STATE_CONTRAST);
        assert!(
            contrast_ratio(cursor.fg.unwrap(), cursor.bg.unwrap()) >= MINIMUM_CURSOR_TEXT_CONTRAST
        );
    }

    #[test]
    fn synthetic_cursor_style_preserves_accessible_theme_colors() {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let white = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let mut theme = cursor_contrast_theme(white, black, white);
        theme.cursor_style.as_mut().unwrap().bg = Some(black);

        assert_eq!(
            theme.synthetic_cursor_style(&theme.style),
            Style {
                fg: Some(black),
                bg: Some(white),
                ..Default::default()
            }
        );
    }

    #[test]
    fn synthetic_cursor_style_checks_cursor_block_against_cell_background() {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let white = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let theme = cursor_contrast_theme(white, black, black);
        let content = Style {
            bg: Some(white),
            ..Default::default()
        };

        let cursor = theme.synthetic_cursor_style(&content);

        assert_eq!(cursor.bg, Some(black));
        assert!(contrast_ratio(cursor.bg.unwrap(), white) >= MINIMUM_CURSOR_STATE_CONTRAST);
        assert!(
            contrast_ratio(cursor.fg.unwrap(), cursor.bg.unwrap()) >= MINIMUM_CURSOR_TEXT_CONTRAST
        );
    }

    #[test]
    fn resolve_style_uses_the_first_available_workbench_color() {
        let breadcrumb = Color::Rgb {
            r: 139,
            g: 164,
            b: 176,
        };
        let mut theme = Theme::default();
        theme
            .colors
            .insert("breadcrumb.foreground".to_string(), breadcrumb);

        let resolved = theme.resolve_style(&ThemeStyleSpec {
            foreground: vec![
                "missing.foreground".to_string(),
                "breadcrumb.foreground".to_string(),
                "editor.foreground".to_string(),
            ],
            background: vec![
                "breadcrumb.background".to_string(),
                "editor.background".to_string(),
            ],
            ..Default::default()
        });

        assert_eq!(resolved.fg, Some(breadcrumb));
        assert_eq!(resolved.bg, theme.style.bg);
    }

    #[test]
    fn resolve_style_interleaves_token_scopes_with_workbench_fallbacks() {
        let function = style(203, 166, 247);
        let theme = theme_with_token_styles(vec![TokenStyle {
            name: None,
            scope: vec!["entity.name.function".to_string()],
            style: function.clone(),
        }]);

        let resolved = theme.resolve_style(&ThemeStyleSpec {
            foreground: vec![
                "symbolIcon.functionForeground".to_string(),
                "scope:entity.name.function".to_string(),
                "editor.foreground".to_string(),
            ],
            bold: Some(true),
            ..Default::default()
        });

        assert_eq!(resolved.fg, function.fg);
        assert!(resolved.bold);
    }

    #[test]
    fn resolve_style_can_use_a_token_background() {
        let token_style = Style {
            bg: Some(Color::Rgb {
                r: 24,
                g: 24,
                b: 37,
            }),
            ..Default::default()
        };
        let theme = theme_with_token_styles(vec![TokenStyle {
            name: None,
            scope: vec!["meta.function".to_string()],
            style: token_style.clone(),
        }]);

        let resolved = theme.resolve_style(&ThemeStyleSpec {
            background: vec!["scope:meta.function".to_string()],
            italic: Some(true),
            ..Default::default()
        });

        assert_eq!(resolved.bg, token_style.bg);
        assert!(resolved.italic);
    }

    #[test]
    fn get_style_matches_markdown_textmate_heading_aliases() {
        let markdown_heading = style(139, 164, 176);
        let generic_heading = style(138, 154, 123);
        let theme = theme_with_token_styles(vec![
            TokenStyle {
                name: None,
                scope: vec!["markup.heading".to_string()],
                style: generic_heading,
            },
            TokenStyle {
                name: None,
                scope: vec!["markup.heading.markdown".to_string()],
                style: markdown_heading.clone(),
            },
        ]);

        assert_eq!(
            theme.get_style("heading.1.markdown"),
            Some(markdown_heading)
        );
    }

    #[test]
    fn get_style_matches_markdown_textmate_list_and_fence_aliases() {
        let list_marker = style(197, 201, 199);
        let fence = style(92, 96, 102);
        let theme = theme_with_token_styles(vec![
            TokenStyle {
                name: None,
                scope: vec!["punctuation.definition.list_item.markdown".to_string()],
                style: list_marker.clone(),
            },
            TokenStyle {
                name: None,
                scope: vec!["punctuation.definition.fenced.markdown".to_string()],
                style: fence.clone(),
            },
        ]);

        assert_eq!(
            theme.get_style("punctuation.definition.list.begin.markdown"),
            Some(list_marker)
        );
        assert_eq!(
            theme.get_style("punctuation.definition.raw.markdown"),
            Some(fence)
        );
    }
}
