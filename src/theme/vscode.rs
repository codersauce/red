use std::{collections::HashMap, fs};

use json_comments::StripComments;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::color::{parse_rgb, Color};

use super::{StatuslineStyle, Style, Theme, TokenStyle};

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
    let contents = StripComments::new(contents.as_bytes());
    let vscode_theme: VsCodeTheme = serde_json::from_reader(contents)?;

    let error_style = vscode_theme.style_from("editorError.foreground", "editorError.background");

    // partition token_colors into a collection of the ones that have scope and the ones that don't
    let (token_colors_with_scope, token_colors_without_scope): (
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

    let statusline_style = StatuslineStyle {
        outer_style: Style {
            fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bg: Some(Color::Rgb {
                r: 184,
                g: 144,
                b: 243,
            }),
            bold: true,
            ..Default::default()
        },
        outer_chars: [' ', '', '', ' '],
        inner_style: Style {
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
        },
    };

    let foreground_token_color = token_colors_without_scope
        .iter()
        .find(|tc| tc.settings.contains_key("foreground"));
    let background_token_color = token_colors_without_scope
        .iter()
        .find(|tc| tc.settings.contains_key("background"));

    let fg = match foreground_token_color {
        Some(tc) => tc.settings.get("foreground"),
        None => vscode_theme.colors.get("editor.foreground"),
    };
    let bg = match background_token_color {
        Some(tc) => tc.settings.get("background"),
        None => vscode_theme.colors.get("editor.background"),
    };

    Ok(Theme {
        name: vscode_theme.name.unwrap_or_default(),
        style: Style {
            fg: Some(parse_rgb(
                fg.expect("foreground color exists").as_str().expect(""),
            )?),
            bg: Some(parse_rgb(
                bg.expect("background color exists").as_str().expect(""),
            )?),
            bold: false,
            italic: false,
        },
        token_styles,
        gutter_style,
        statusline_style,
        line_highlight_style,
        selection_style,
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
    fn style_from(&self, fg_key: &str, bg_key: &str) -> Option<Style> {
        let fg = self
            .colors
            .get(fg_key)
            .map(|v| parse_rgb(v.as_str().expect("")).unwrap());
        let bg = self
            .colors
            .get(bg_key)
            .map(|v| parse_rgb(v.as_str().expect("")).unwrap());

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
    fn test_token_color_with_no_scope() {
        parse_vscode_theme("src/fixtures/token-color-with-no-scope.json").unwrap();
    }

    #[test]
    fn test_theme_with_comments() {
        parse_vscode_theme("src/fixtures/nord.json").unwrap();
    }
}
