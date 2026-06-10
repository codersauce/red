use std::{
    collections::{BTreeMap, HashMap},
    fs,
};

use json_comments::StripComments;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::color::{parse_rgb, Color};

use super::{StatuslineStyle, Style, Theme, TokenStyle, UiStyle};

static SYNTAX_HIGHLIGHTING_MAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();

    m.insert("constant", "constant");
    m.insert("entity.name.type", "type");
    m.insert("support.type", "type");
    m.insert("entity.name.function.constructor", "constructor");
    m.insert("variable.other.enummember", "constructor");
    m.insert("entity.name.function", "function");
    m.insert("meta.function-call", "function");
    m.insert("entity.name.function.member", "function.method");
    m.insert("variable.function", "function.method");
    m.insert("entity.name.function.macro", "function.macro");
    m.insert("support.function.macro", "function.macro");
    m.insert("variable.other.member", "property");
    m.insert("variable.other.property", "property");
    m.insert("variable.parameter", "variable.parameter");
    m.insert("entity.name.label", "label");
    m.insert("comment", "comment");
    m.insert("punctuation.definition.comment", "comment");
    m.insert("punctuation.section.block", "punctuation.bracket");
    m.insert("punctuation.definition.brackets", "punctuation.bracket");
    m.insert("punctuation.separator", "punctuation.delimiter");
    m.insert("punctuation.accessor", "punctuation.delimiter");
    m.insert("keyword", "keyword");
    m.insert("keyword.control", "keyword");
    m.insert("support.type.primitive", "type.builtin");
    m.insert("keyword.type", "type.builtin");
    m.insert("variable.language", "variable.builtin");
    m.insert("support.variable", "variable.builtin");
    m.insert("string.quoted.double", "string");
    m.insert("string.quoted.single", "string");
    m.insert("constant.language", "constant.builtin");
    m.insert("constant.numeric", "constant.builtin");
    m.insert("constant.character", "constant.builtin");
    m.insert("constant.character.escape", "escape");
    m.insert("keyword.operator", "operator");
    m.insert("storage.modifier.attribute", "attribute");
    m.insert("meta.attribute", "attribute");

    m
});

pub fn parse_vscode_theme(file: &str) -> anyhow::Result<Theme> {
    let contents = &fs::read_to_string(file)?;
    parse_vscode_theme_contents(contents)
}

pub fn parse_vscode_theme_contents(contents: &str) -> anyhow::Result<Theme> {
    let contents = StripComments::new(contents.as_bytes());
    let vscode_theme: VsCodeTheme = serde_json::from_reader(contents)?;

    let error_style = vscode_theme.style_from("editorError.foreground", "editorError.background");
    let cursor_style = vscode_theme
        .style_from("editorCursor.foreground", "editorCursor.background")
        .or_else(|| {
            vscode_theme.style_from("terminalCursor.foreground", "terminalCursor.background")
        });

    let gutter_style = Style {
        fg: vscode_theme
            .colors
            .iter()
            .find(|(c, _)| **c == "editorLineNumber.foreground")
            .map(|(_, hex)| parse_rgb(hex.as_str().expect("colors are an hex string")).unwrap()),
        bg: vscode_theme
            .colors
            .iter()
            .find(|(c, _)| **c == "editorLineNumber.background")
            .map(|(_, hex)| parse_rgb(hex.as_str().expect("colors are an hex string")).unwrap()),
        ..Default::default()
    };

    let line_highlight_style = vscode_theme
        .colors
        .iter()
        .find(|(c, _)| **c == "editor.lineHighlightBackground")
        .map(|(_, hex)| Style {
            bg: Some(parse_rgb(hex.as_str().expect("colors are an hex string")).unwrap()),
            ..Default::default()
        });

    let selection_style = vscode_theme
        .colors
        .iter()
        .find(|(c, _)| **c == "editor.selectionBackground")
        .map(|(_, hex)| Style {
            bg: Some(parse_rgb(hex.as_str().expect("colors are an hex string")).unwrap()),
            ..Default::default()
        });

    let find_match_style = vscode_theme
        .colors
        .iter()
        .find(|(c, _)| **c == "editor.findMatchBackground")
        .map(|(_, hex)| Style {
            bg: Some(parse_rgb(hex.as_str().expect("colors are an hex string")).unwrap()),
            ..Default::default()
        });

    let find_match_highlight_style = vscode_theme
        .colors
        .iter()
        .find(|(c, _)| **c == "editor.findMatchHighlightBackground")
        .map(|(_, hex)| Style {
            bg: Some(parse_rgb(hex.as_str().expect("colors are an hex string")).unwrap()),
            ..Default::default()
        });

    let statusline_style = vscode_theme.statusline_style(selection_style.as_ref());

    let foreground_token_color = vscode_theme
        .token_colors
        .iter()
        .filter(|tc| tc.scope.is_none())
        .find(|tc| tc.settings.contains_key("foreground"));
    let background_token_color = vscode_theme
        .token_colors
        .iter()
        .filter(|tc| tc.scope.is_none())
        .find(|tc| tc.settings.contains_key("background"));

    let fg = match foreground_token_color {
        Some(tc) => tc.settings.get("foreground"),
        None => vscode_theme.colors.get("editor.foreground"),
    };
    let bg = match background_token_color {
        Some(tc) => tc.settings.get("background"),
        None => vscode_theme.colors.get("editor.background"),
    };

    let editor_style = Style {
        fg: Some(parse_rgb(
            fg.expect("foreground color exists").as_str().expect(""),
        )?),
        bg: Some(parse_rgb(
            bg.expect("background color exists").as_str().expect(""),
        )?),
        bold: false,
        italic: false,
    };
    let ui_style = vscode_theme.ui_style(&editor_style, selection_style.as_ref());

    // partition token_colors into a collection of the ones that have scope and the ones that don't
    let (token_colors_with_scope, _token_colors_without_scope): (
        Vec<VsCodeTokenColor>,
        Vec<VsCodeTokenColor>,
    ) = vscode_theme
        .token_colors
        .into_iter()
        .partition(|tc| tc.scope.is_some());

    let token_styles = token_colors_with_scope
        .into_iter()
        .map(|tc| tc.try_into())
        .collect::<Result<Vec<TokenStyle>, _>>()?;
    let colors = vscode_theme
        .colors
        .iter()
        .filter_map(|(key, value)| {
            let hex = value.as_str()?;
            parse_rgb(hex).ok().map(|color| (key.to_string(), color))
        })
        .collect::<BTreeMap<_, _>>();

    Ok(Theme {
        name: vscode_theme.name.unwrap_or_default(),
        colors,
        style: editor_style,
        ui_style,
        token_styles,
        gutter_style,
        statusline_style,
        line_highlight_style,
        find_match_style,
        find_match_highlight_style,
        selection_style,
        cursor_style,
        error_style,
    })
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTheme {
    name: Option<String>,
    colors: Map<String, Value>,
    token_colors: Vec<VsCodeTokenColor>,
}

impl VsCodeTheme {
    fn color_from(&self, key: &str) -> Option<Color> {
        self.colors
            .get(key)
            .map(|v| parse_rgb(v.as_str().expect("colors are an hex string")).unwrap())
    }

    fn style_from(&self, fg_key: &str, bg_key: &str) -> Option<Style> {
        let fg = self.color_from(fg_key);
        let bg = self.color_from(bg_key);

        if fg.is_none() && bg.is_none() {
            return None;
        }

        Some(Style {
            fg,
            bg,
            bold: false,
            italic: false,
        })
    }

    fn statusline_style(&self, selection_style: Option<&Style>) -> StatuslineStyle {
        let fallback_outer_bg = Color::Rgb {
            r: 184,
            g: 144,
            b: 243,
        };
        let fallback_inner_fg = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let fallback_inner_bg = Color::Rgb {
            r: 67,
            g: 70,
            b: 89,
        };

        let inner_fg = self
            .color_from("statusBar.foreground")
            .unwrap_or(fallback_inner_fg);
        let inner_bg = self
            .color_from("statusBar.background")
            .filter(|color| !is_transparent(*color))
            .unwrap_or(fallback_inner_bg);

        let (outer_bg, outer_fg) = self
            .statusline_accent_from(
                "statusBarItem.prominentBackground",
                "statusBarItem.prominentForeground",
            )
            .or_else(|| {
                self.statusline_accent_from(
                    "statusBarItem.remoteBackground",
                    "statusBarItem.remoteForeground",
                )
            })
            .or_else(|| {
                selection_style
                    .and_then(|style| style.bg)
                    .filter(|color| !is_transparent(*color))
                    .map(|bg| {
                        (
                            bg,
                            self.color_from("statusBar.foreground").unwrap_or(inner_fg),
                        )
                    })
            })
            .unwrap_or((fallback_outer_bg, Color::Rgb { r: 0, g: 0, b: 0 }));

        StatuslineStyle {
            outer_style: Style {
                fg: Some(outer_fg),
                bg: Some(outer_bg),
                bold: true,
                ..Default::default()
            },
            outer_chars: [' ', '', '', ' '],
            inner_style: Style {
                fg: Some(inner_fg),
                bg: Some(inner_bg),
                ..Default::default()
            },
        }
    }

    fn statusline_accent_from(&self, bg_key: &str, fg_key: &str) -> Option<(Color, Color)> {
        let bg = self
            .color_from(bg_key)
            .filter(|color| !is_transparent(*color))?;
        let fg = self
            .color_from(fg_key)
            .or_else(|| self.color_from("statusBar.foreground"))
            .unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });
        Some((bg, fg))
    }

    fn ui_style(&self, editor_style: &Style, selection_style: Option<&Style>) -> UiStyle {
        let editor_fg = editor_style.fg.unwrap_or(Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        let editor_bg = editor_style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });

        let popup_bg = self
            .color_from("quickInput.background")
            .or_else(|| self.color_from("editorWidget.background"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or_else(|| adjust_color(editor_bg, 8));
        let popup_fg = self
            .color_from("quickInput.foreground")
            .or_else(|| self.color_from("editorWidget.foreground"))
            .unwrap_or(editor_fg);
        let border_fg = self
            .color_from("quickInputTitle.background")
            .or_else(|| self.color_from("focusBorder"))
            .or_else(|| self.color_from("input.border"))
            .or_else(|| self.color_from("editorWidget.border"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or_else(|| adjust_color(popup_bg, 18));
        let selected_bg = self
            .color_from("quickInputList.focusBackground")
            .or_else(|| self.color_from("list.activeSelectionBackground"))
            .or_else(|| selection_style.and_then(|style| style.bg))
            .filter(|color| !is_transparent(*color))
            .unwrap_or_else(|| adjust_color(popup_bg, 16));
        let selected_fg = self
            .color_from("quickInputList.focusForeground")
            .or_else(|| self.color_from("list.activeSelectionForeground"))
            .unwrap_or_else(|| readable_foreground(selected_bg, popup_fg));
        let prompt_bg = self
            .color_from("input.background")
            .filter(|color| !is_transparent(*color))
            .unwrap_or(popup_bg);
        let prompt_fg = self.color_from("input.foreground").unwrap_or(popup_fg);
        let muted_fg = self
            .color_from("input.placeholderForeground")
            .or_else(|| self.color_from("descriptionForeground"))
            .or_else(|| self.color_from("editorLineNumber.foreground"))
            .unwrap_or_else(|| adjust_color(popup_fg, -30));
        let deprecated_fg = self
            .color_from("list.warningForeground")
            .or_else(|| self.color_from("editorWarning.foreground"))
            .or_else(|| self.color_from("editorError.foreground"))
            .unwrap_or_else(|| adjust_color(popup_fg, -45));
        let dialog_bg = self
            .color_from("editorHoverWidget.background")
            .or_else(|| self.color_from("editorWidget.background"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or(popup_bg);
        let dialog_fg = self
            .color_from("editorHoverWidget.foreground")
            .or_else(|| self.color_from("editorWidget.foreground"))
            .unwrap_or(popup_fg);
        let dialog_border_fg = self
            .color_from("editorHoverWidget.border")
            .or_else(|| self.color_from("editorWidget.border"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or(border_fg);

        UiStyle {
            popup: Style {
                fg: Some(popup_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            popup_border: Style {
                fg: Some(border_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            popup_title: Style {
                fg: Some(popup_fg),
                bg: Some(popup_bg),
                bold: true,
                ..Default::default()
            },
            dialog: Style {
                fg: Some(dialog_fg),
                bg: Some(dialog_bg),
                ..Default::default()
            },
            dialog_border: Style {
                fg: Some(dialog_border_fg),
                bg: Some(dialog_bg),
                ..Default::default()
            },
            dialog_title: Style {
                fg: Some(dialog_fg),
                bg: Some(dialog_bg),
                bold: true,
                ..Default::default()
            },
            picker_item: Style {
                fg: Some(popup_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            picker_selected_item: Style {
                fg: Some(selected_fg),
                bg: Some(selected_bg),
                ..Default::default()
            },
            picker_prompt: Style {
                fg: Some(prompt_fg),
                bg: Some(prompt_bg),
                ..Default::default()
            },
            muted: Style {
                fg: Some(muted_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            deprecated: Style {
                fg: Some(deprecated_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
        }
    }
}

fn is_transparent(color: Color) -> bool {
    matches!(color, Color::Rgba { a: 0, .. })
}

fn adjust_color(color: Color, percentage: i32) -> Color {
    let Color::Rgb { r, g, b } = color else {
        return color;
    };

    let adjust = |component: u8| -> u8 {
        let delta = (255.0 * (percentage as f32 / 100.0)) as i32;
        (component as i32 + delta).clamp(0, 255) as u8
    };

    Color::Rgb {
        r: adjust(r),
        g: adjust(g),
        b: adjust(b),
    }
}

fn readable_foreground(background: Color, fallback: Color) -> Color {
    let Color::Rgb { r, g, b } = background else {
        return fallback;
    };

    let luminance = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if luminance > 140.0 {
        Color::Rgb { r: 0, g: 0, b: 0 }
    } else {
        Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTokenColor {
    name: Option<String>,
    scope: Option<VsCodeScope>,
    settings: Map<String, Value>,
}

impl TryFrom<VsCodeTokenColor> for TokenStyle {
    type Error = anyhow::Error;

    fn try_from(tc: VsCodeTokenColor) -> Result<Self, Self::Error> {
        let mut style = Style::default();

        if let Some(fg) = tc.settings.get("foreground") {
            style.fg =
                Some(parse_rgb(fg.as_str().expect("fg is string")).expect("parsing rgb works"));
        }

        if let Some(bg) = tc.settings.get("background") {
            style.bg =
                Some(parse_rgb(bg.as_str().expect("bg is string")).expect("parsing rgb works"));
        }

        if let Some(font_style) = tc.settings.get("fontStyle") {
            style.bold = font_style
                .as_str()
                .expect("fontStyle is string")
                .contains("bold");
            style.italic = font_style
                .as_str()
                .expect("fontStyle is string")
                .contains("italic");
        }

        let Some(scope) = tc.scope else {
            return Err(anyhow::anyhow!("TokenColor has no scope"));
        };

        Ok(Self {
            name: tc.name,
            scope: scope.into(),
            style,
        })
    }
}

fn translate_scope(vscode_scope: String) -> String {
    SYNTAX_HIGHLIGHTING_MAP
        .get(&vscode_scope.as_str())
        .map(|s| s.to_string())
        .unwrap_or(vscode_scope)
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum VsCodeScope {
    Single(String),
    Multiple(Vec<String>),
}

impl From<VsCodeScope> for Vec<String> {
    fn from(scope: VsCodeScope) -> Self {
        match scope {
            VsCodeScope::Single(s) => vec![translate_scope(s)],
            VsCodeScope::Multiple(v) => v.into_iter().map(translate_scope).collect(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_vscode() {
        let theme = parse_vscode_theme("./src/fixtures/frappe.json").unwrap();
        println!("{:#?}", theme);
    }

    #[test]
    fn test_statusline_uses_vscode_statusbar_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.statusline_style.inner_style.fg,
            Some(Color::Rgb {
                r: 205,
                g: 214,
                b: 244,
            })
        );
        assert_eq!(
            theme.statusline_style.inner_style.bg,
            Some(Color::Rgb {
                r: 17,
                g: 17,
                b: 27,
            })
        );
        assert_eq!(
            theme.statusline_style.outer_style.bg,
            Some(Color::Rgb {
                r: 137,
                g: 180,
                b: 250,
            })
        );
        assert_eq!(
            theme.statusline_style.outer_style.fg,
            Some(Color::Rgb {
                r: 17,
                g: 17,
                b: 27,
            })
        );
    }

    #[test]
    fn test_ui_style_uses_vscode_quick_input_colors() {
        let theme = parse_vscode_theme("./src/fixtures/nord.json").unwrap();

        assert_eq!(
            theme.ui_style.popup.bg,
            Some(Color::Rgb {
                r: 46,
                g: 52,
                b: 64,
            })
        );
        assert_eq!(
            theme.ui_style.popup_border.fg,
            Some(Color::Rgb {
                r: 59,
                g: 66,
                b: 82,
            })
        );
        assert_eq!(
            theme.ui_style.picker_selected_item.bg,
            Some(Color::Rgb {
                r: 136,
                g: 192,
                b: 208,
            })
        );
        assert_eq!(
            theme.ui_style.picker_selected_item.fg,
            Some(Color::Rgb {
                r: 46,
                g: 52,
                b: 64,
            })
        );
    }

    #[test]
    fn test_ui_style_uses_vscode_hover_widget_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.ui_style.dialog.bg,
            Some(Color::Rgb {
                r: 24,
                g: 24,
                b: 37,
            })
        );
        assert_eq!(
            theme.ui_style.dialog.fg,
            Some(Color::Rgb {
                r: 205,
                g: 214,
                b: 244,
            })
        );
        assert_eq!(
            theme.ui_style.dialog_border.fg,
            Some(Color::Rgb {
                r: 88,
                g: 91,
                b: 112,
            })
        );
        assert_eq!(theme.ui_style.dialog_title.bg, theme.ui_style.dialog.bg);
    }

    #[test]
    fn test_search_styles_use_vscode_find_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.find_match_style.and_then(|style| style.bg),
            Some(Color::Rgb {
                r: 94,
                g: 63,
                b: 83,
            })
        );
        assert_eq!(
            theme.find_match_highlight_style.and_then(|style| style.bg),
            Some(Color::Rgb {
                r: 62,
                g: 87,
                b: 103,
            })
        );
    }

    #[test]
    fn test_ui_style_uses_vscode_input_and_list_fallbacks() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.ui_style.popup.bg,
            Some(Color::Rgb {
                r: 24,
                g: 24,
                b: 37,
            })
        );
        assert_eq!(
            theme.ui_style.picker_prompt.bg,
            Some(Color::Rgb {
                r: 49,
                g: 50,
                b: 68,
            })
        );
        assert_eq!(
            theme.ui_style.picker_selected_item.bg,
            Some(Color::Rgb {
                r: 49,
                g: 50,
                b: 68,
            })
        );
    }

    #[test]
    fn test_cursor_uses_vscode_editor_cursor_colors() {
        let theme = parse_vscode_theme("./src/fixtures/latte.json").unwrap();

        assert_eq!(
            theme.cursor_style,
            Some(Style {
                fg: Some(Color::Rgb {
                    r: 220,
                    g: 138,
                    b: 120,
                }),
                bg: Some(Color::Rgb {
                    r: 239,
                    g: 241,
                    b: 245,
                }),
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_exposes_raw_vscode_workbench_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.colors.get("gitDecoration.modifiedResourceForeground"),
            Some(&Color::Rgb {
                r: 249,
                g: 226,
                b: 175,
            })
        );
        assert_eq!(
            theme.colors.get("symbolIcon.folderForeground"),
            Some(&Color::Rgb {
                r: 203,
                g: 166,
                b: 247,
            })
        );
    }

    #[test]
    fn test_statusline_falls_back_without_vscode_statusbar_colors() {
        let theme = parse_vscode_theme("src/fixtures/token-color-with-no-scope.json").unwrap();

        assert_eq!(
            theme.statusline_style.inner_style.fg,
            Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            })
        );
        assert_eq!(
            theme.statusline_style.inner_style.bg,
            Some(Color::Rgb {
                r: 67,
                g: 70,
                b: 89,
            })
        );
        assert_eq!(
            theme.statusline_style.outer_style.bg,
            Some(Color::Rgb {
                r: 184,
                g: 144,
                b: 243,
            })
        );
    }

    #[test]
    fn test_token_color_with_no_scope() {
        parse_vscode_theme("src/fixtures/token-color-with-no-scope.json").unwrap();
    }

    #[test]
    fn test_theme_with_comments() {
        parse_vscode_theme("src/fixtures/nord.json").unwrap();
    }

    #[test]
    fn test_bundled_themes_parse() {
        let themes_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
        let mut theme_files = std::fs::read_dir(&themes_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        theme_files.sort();

        assert!(!theme_files.is_empty());

        for theme_file in theme_files {
            let theme_file = theme_file.to_string_lossy();
            match std::panic::catch_unwind(|| parse_vscode_theme(&theme_file)) {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("failed to parse {theme_file}: {error}"),
                Err(error) => {
                    let message = error
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| error.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("parser panicked");
                    panic!("failed to parse {theme_file}: {message}");
                }
            }
        }
    }
}
