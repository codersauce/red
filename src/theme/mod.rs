//! Editor theme model, semantic style lookup, defaults, and VS Code theme import.
//!
//! A [`Theme`] contains the concrete styles used by the renderer and a semantic lookup
//! surface used by plugins. [`ThemeStyleSpec`] resolves an ordered list of semantic
//! foreground candidates before callers apply explicit style overrides. Parsing accepts
//! comments through the VS Code adapter but produces the same internal model as bundled
//! native themes.

mod surface;
mod vscode;
pub(crate) use surface::{DiffPalette, SurfaceCardColors, SurfaceCardPalette, SurfacePalette};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
pub use vscode::parse_vscode_theme;
pub use vscode::parse_vscode_theme_contents;

use crate::color::{blend_color, contrast_ratio, ensure_minimum_contrast, Color};

pub(crate) const MINIMUM_SELECTION_STATE_CONTRAST: f32 = 3.0;
pub(crate) const MINIMUM_SELECTION_TEXT_CONTRAST: f32 = 4.5;
pub(crate) const MINIMUM_CURSOR_STATE_CONTRAST: f32 = 3.0;
pub(crate) const MINIMUM_CURSOR_TEXT_CONTRAST: f32 = 4.5;

#[derive(Clone, Copy)]
pub(crate) enum SelectionForegroundPriority {
    Content,
    Selection,
}

/// Overall appearance of the active editor theme, independent of the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
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
    pub bracket_match_style: Option<Style>,
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
#[serde(rename_all = "snake_case")]
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
    /// Classifies the editor's resolved background by perceived luminance.
    pub fn mode(&self) -> ThemeMode {
        if self.style.bg.is_some_and(Color::is_light) {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        }
    }

    fn inline_comment_background(&self) -> Color {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let editor_background = blend_color(self.style.bg.unwrap_or(black), black);
        if let Some(background) = self.colors.get("red.inlineCommentBackground") {
            return blend_color(*background, editor_background);
        }
        let light = self.mode() == ThemeMode::Light;
        let background = if light {
            Color::Rgb {
                r: 228,
                g: 228,
                b: 231,
            }
        } else {
            Color::Rgb {
                r: 39,
                g: 39,
                b: 43,
            }
        };
        if contrast_ratio(background, editor_background) >= 1.08 {
            background
        } else if light {
            Color::Rgb {
                r: 212,
                g: 212,
                b: 216,
            }
        } else {
            Color::Rgb {
                r: 58,
                g: 58,
                b: 64,
            }
        }
    }

    pub(crate) fn inline_comment_style(&self) -> Style {
        let background = self.inline_comment_background();
        let foreground = self
            .colors
            .get("red.inlineCommentForeground")
            .or_else(|| self.colors.get("editorInfo.foreground"))
            .copied()
            .or_else(|| self.get_style("comment").and_then(|style| style.fg))
            .or(self.ui_style.muted.fg)
            .or(self.style.fg)
            .unwrap_or(Color::Rgb {
                r: 128,
                g: 128,
                b: 128,
            });
        Style {
            fg: Some(ensure_minimum_contrast(foreground, background, 4.5)),
            bg: Some(background),
            italic: true,
            ..Style::default()
        }
    }

    pub(crate) fn inline_comment_guide_style(&self) -> Style {
        let mut style = self.inline_comment_style();
        let background = self.style.bg.unwrap_or_default();
        if let Some(foreground) = style.fg {
            let (Color::Rgb { r, g, b } | Color::Rgba { r, g, b, .. }) = foreground;
            style.fg = Some(blend_color(Color::Rgba { r, g, b, a: 80 }, background));
        }
        style.bg = self.style.bg;
        style.italic = false;
        style
    }

    pub(crate) fn inline_comment_arrow_style(&self) -> Style {
        let mut style = self.inline_comment_style();
        let background = self.style.bg.unwrap_or_default();
        style.fg = style
            .fg
            .map(|foreground| ensure_minimum_contrast(foreground, background, 4.5));
        style.bg = self.style.bg;
        style.italic = false;
        style
    }

    pub(crate) fn current_line_number_style(&self) -> Style {
        let mut style = self.gutter_style.fallback_bg(&self.style);
        style.fg = self
            .colors
            .get("editorLineNumber.activeForeground")
            .copied()
            .or(self.style.fg);
        style
    }

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

    pub(crate) fn editor_bracket_match_style(&self) -> Style {
        self.bracket_match_style
            .clone()
            .or_else(|| self.find_match_highlight_style.clone())
            .or_else(|| self.find_match_style.clone())
            .or_else(|| self.selection_style.clone())
            .unwrap_or_else(|| self.editor_selection_style())
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

    pub(crate) fn terminal_cursor_color(&self, content: &Style) -> Color {
        self.synthetic_cursor_style(content)
            .bg
            .expect("synthetic cursor styles always have a background")
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

    /// Resolves a visible drag accent without assuming the theme is dark.
    pub(crate) fn active_divider_style(&self, inactive: &Style, surface: &Style) -> Style {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let white = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let editor_background = blend_color(self.style.bg.unwrap_or(black), black);
        let background = blend_color(
            inactive.bg.or(surface.bg).unwrap_or(editor_background),
            editor_background,
        );
        let inactive_foreground = blend_color(
            inactive
                .fg
                .or(surface.fg)
                .or(self.style.fg)
                .unwrap_or(white),
            background,
        );

        let accent = [
            self.colors.get("sash.hoverBorder").copied(),
            self.colors.get("panelTitle.activeBorder").copied(),
            self.colors.get("focusBorder").copied(),
            self.ui_style.picker_prompt.fg,
            self.colors.get("editorCursor.foreground").copied(),
            self.cursor_style.as_ref().and_then(|style| style.fg),
            self.ui_style.popup_border.fg,
        ]
        .into_iter()
        .flatten()
        .find_map(|candidate| {
            if matches!(candidate, Color::Rgba { a: 0, .. }) {
                return None;
            }

            let candidate = blend_color(candidate, background);
            (candidate != inactive_foreground
                && contrast_ratio(candidate, background) >= MINIMUM_SELECTION_STATE_CONTRAST)
                .then_some(candidate)
        })
        .unwrap_or_else(|| {
            let adjusted = ensure_minimum_contrast(
                inactive_foreground,
                background,
                MINIMUM_SELECTION_STATE_CONTRAST,
            );
            if adjusted != inactive_foreground {
                return adjusted;
            }

            let strongest =
                if contrast_ratio(black, background) >= contrast_ratio(white, background) {
                    black
                } else {
                    white
                };
            let alternative = if strongest == inactive_foreground {
                if strongest == black {
                    white
                } else {
                    black
                }
            } else {
                strongest
            };

            ensure_minimum_contrast(alternative, background, MINIMUM_SELECTION_STATE_CONTRAST)
        });

        Style {
            fg: Some(accent),
            bold: true,
            ..inactive.clone()
        }
    }

    pub fn resolve_style(&self, spec: &ThemeStyleSpec) -> Style {
        Style {
            fg: self.resolve_color_references(&spec.foreground, StyleColorComponent::Foreground),
            bg: self.resolve_color_references(&spec.background, StyleColorComponent::Background),
            bold: spec.bold.unwrap_or(false),
            italic: spec.italic.unwrap_or(false),
            underline: false,
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
        underline: false,
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
        underline: content.underline || selection.underline,
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
                underline: false,
            },
            gutter_style: Style::default(),
            statusline_style: StatuslineStyle::default(),
            ui_style: UiStyle::default(),
            token_styles: vec![],
            line_highlight_style: None,
            bracket_match_style: None,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
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
            underline: self.underline,
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

    #[test]
    fn underline_style_is_backward_compatible_and_survives_selection() {
        let legacy = r#"{"fg":null,"bg":null,"bold":false,"italic":false}"#;
        let plain: Style = serde_json::from_str(legacy).unwrap();
        assert!(!plain.underline);
        assert!(!serde_json::to_string(&plain).unwrap().contains("underline"));
        let linked = Style {
            underline: true,
            ..plain
        };
        assert!(
            serde_json::from_str::<Style>(&serde_json::to_string(&linked).unwrap())
                .unwrap()
                .underline
        );
        assert!(linked.inverted().underline);
        assert!(
            Theme::default()
                .selected_style(
                    &linked,
                    &Style::default(),
                    SelectionForegroundPriority::Content
                )
                .underline
        );
    }

    #[test]
    fn theme_mode_uses_perceived_background_luminance() {
        let mut theme = Theme::default();
        theme.style.bg = Some(Color::Rgb {
            r: 20,
            g: 20,
            b: 24,
        });
        assert_eq!(theme.mode(), ThemeMode::Dark);
        theme.style.bg = Some(Color::Rgb {
            r: 250,
            g: 250,
            b: 247,
        });
        assert_eq!(theme.mode(), ThemeMode::Light);
        theme.style.bg = None;
        assert_eq!(theme.mode(), ThemeMode::Dark);
    }

    #[test]
    fn inline_comment_background_uses_gray_unless_explicitly_overridden() {
        let mut theme = Theme::default();
        theme.colors.clear();
        theme.style.bg = Some(Color::Rgb {
            r: 16,
            g: 16,
            b: 20,
        });
        let terminal = Color::Rgb { r: 0, g: 0, b: 0 };
        theme
            .colors
            .insert("terminal.background".to_string(), terminal);
        assert_eq!(
            theme.inline_comment_style().bg,
            Some(Color::Rgb {
                r: 39,
                g: 39,
                b: 43
            })
        );
        let custom = Color::Rgb {
            r: 45,
            g: 55,
            b: 65,
        };
        theme
            .colors
            .insert("red.inlineCommentBackground".to_string(), custom);
        assert_eq!(theme.inline_comment_style().bg, Some(custom));
    }

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

    #[test]
    fn current_line_number_style_prefers_theme_active_foreground() {
        let active = Color::Rgb {
            r: 180,
            g: 190,
            b: 200,
        };
        let gutter_background = Color::Rgb {
            r: 20,
            g: 21,
            b: 22,
        };
        let mut theme = Theme {
            gutter_style: Style {
                fg: Some(Color::Rgb {
                    r: 70,
                    g: 71,
                    b: 72,
                }),
                bg: Some(gutter_background),
                italic: true,
                ..Style::default()
            },
            ..Theme::default()
        };
        theme
            .colors
            .insert("editorLineNumber.activeForeground".to_string(), active);

        let style = theme.current_line_number_style();

        assert_eq!(style.fg, Some(active));
        assert_eq!(style.bg, Some(gutter_background));
        assert!(style.italic);
    }

    #[test]
    fn current_line_number_style_falls_back_to_editor_foreground() {
        let editor_foreground = Color::Rgb {
            r: 210,
            g: 211,
            b: 212,
        };
        let theme = Theme {
            style: Style {
                fg: Some(editor_foreground),
                ..Style::default()
            },
            gutter_style: Style {
                fg: Some(Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                }),
                ..Style::default()
            },
            ..Theme::default()
        };

        assert_eq!(
            theme.current_line_number_style().fg,
            Some(editor_foreground)
        );
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
    fn active_divider_prefers_the_theme_sash_accent() {
        let sash = Color::Rgb {
            r: 203,
            g: 166,
            b: 247,
        };
        let panel = Color::Rgb {
            r: 137,
            g: 220,
            b: 235,
        };
        let mut theme = Theme::default();
        theme.colors.insert("sash.hoverBorder".to_string(), sash);
        theme
            .colors
            .insert("panelTitle.activeBorder".to_string(), panel);
        let inactive = Style {
            fg: Some(Color::Rgb {
                r: 100,
                g: 100,
                b: 100,
            }),
            bg: theme.style.bg,
            italic: true,
            ..Style::default()
        };

        let active = theme.active_divider_style(&inactive, &theme.style);

        assert_eq!(active.fg, Some(sash));
        assert_eq!(active.bg, inactive.bg);
        assert!(active.bold);
        assert!(active.italic);
    }

    #[test]
    fn active_divider_skips_invisible_identical_and_low_contrast_accents() {
        let idle = Color::Rgb {
            r: 100,
            g: 100,
            b: 100,
        };
        let cursor = Color::Rgb {
            r: 250,
            g: 208,
            b: 0,
        };
        let mut theme = Theme::default();
        theme.colors.insert(
            "sash.hoverBorder".to_string(),
            Color::Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 0,
            },
        );
        theme
            .colors
            .insert("panelTitle.activeBorder".to_string(), idle);
        theme.colors.insert(
            "focusBorder".to_string(),
            Color::Rgb {
                r: 20,
                g: 20,
                b: 20,
            },
        );
        theme.ui_style.picker_prompt.fg = Some(Color::Rgb {
            r: 25,
            g: 25,
            b: 25,
        });
        theme
            .colors
            .insert("editorCursor.foreground".to_string(), cursor);
        let inactive = Style {
            fg: Some(idle),
            ..Style::default()
        };

        let active = theme.active_divider_style(&inactive, &theme.style);

        assert_eq!(active.fg, Some(cursor));
        assert!(active.bold);
    }

    #[test]
    fn active_divider_fallback_remains_distinct_on_dark_and_light_themes() {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let white = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };

        for (background, idle) in [
            (
                black,
                Color::Rgb {
                    r: 25,
                    g: 25,
                    b: 25,
                },
            ),
            (
                white,
                Color::Rgb {
                    r: 245,
                    g: 245,
                    b: 245,
                },
            ),
            (black, white),
            (white, black),
        ] {
            let mut theme = Theme::default();
            theme.style.bg = Some(background);
            theme.ui_style.picker_prompt.fg = None;
            theme.ui_style.popup_border.fg = None;
            let inactive = Style {
                fg: Some(idle),
                bg: Some(background),
                ..Style::default()
            };

            let active = theme.active_divider_style(&inactive, &theme.style);
            let foreground = active.fg.expect("active dividers have a foreground");

            assert_ne!(foreground, idle);
            assert!(contrast_ratio(foreground, background) >= MINIMUM_SELECTION_STATE_CONTRAST);
            assert_eq!(active.bg, Some(background));
            assert!(active.bold);
        }
    }

    #[test]
    fn active_divider_checks_accents_against_the_actual_surface() {
        let white = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let blue = Color::Rgb {
            r: 3,
            g: 73,
            b: 180,
        };
        let mut theme = Theme::default();
        theme.colors.insert(
            "sash.hoverBorder".to_string(),
            Color::Rgb {
                r: 240,
                g: 240,
                b: 210,
            },
        );
        theme
            .colors
            .insert("panelTitle.activeBorder".to_string(), blue);
        let surface = Style {
            bg: Some(white),
            ..Style::default()
        };
        let inactive = Style {
            fg: Some(Color::Rgb {
                r: 100,
                g: 100,
                b: 100,
            }),
            ..Style::default()
        };

        let active = theme.active_divider_style(&inactive, &surface);

        assert_eq!(active.fg, Some(blue));
        assert!(contrast_ratio(blue, white) >= MINIMUM_SELECTION_STATE_CONTRAST);
        assert_eq!(active.bg, inactive.bg);
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
    fn terminal_cursor_color_checks_the_actual_surface() {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let white = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let theme = cursor_contrast_theme(white, black, black);
        let picker_surface = Style {
            bg: Some(white),
            ..Default::default()
        };

        let cursor = theme.terminal_cursor_color(&picker_surface);

        assert!(contrast_ratio(cursor, white) >= MINIMUM_CURSOR_STATE_CONTRAST);
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
