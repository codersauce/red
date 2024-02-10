use std::fs;

use crossterm::style::Color;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{Style, Theme, TokenStyle};

pub fn parse_vscode_theme(file: &str) -> anyhow::Result<Theme> {
    let contents = fs::read_to_string(file)?;
    let vscode_theme: VsCodeTheme = serde_json::from_str(&contents)?;
    let token_styles = vscode_theme
        .token_colors
        .into_iter()
        .map(|tc| tc.try_into())
        .collect::<Result<Vec<TokenStyle>, _>>()?;

    Ok(Theme {
        name: vscode_theme.name.unwrap_or_default(),
        style: Style {
            fg: Some(parse_rgb(
                vscode_theme
                    .colors
                    .get("editor.foreground")
                    .expect("editor.foreground exists")
                    .as_str()
                    .expect("editor.foreground is string"),
            )?),
            bg: Some(parse_rgb(
                vscode_theme
                    .colors
                    .get("editor.background")
                    .expect("editor.background exists")
                    .as_str()
                    .expect("editor.background is string"),
            )?),
            bold: false,
            italic: false,
        },
        token_styles,
    })
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTheme {
    name: Option<String>,
    colors: Map<String, Value>,
    token_colors: Vec<VsCodeTokenColor>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTokenColor {
    name: Option<String>,
    scope: VsCodeScope,
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

        Ok(Self {
            name: tc.name,
            scope: tc.scope.into(),
            style,
        })
    }
}

fn translate_scope(vscode_scope: String) -> String {
    if vscode_scope == "meta.function-call.constructor".to_string() {
        return "constructor".to_string();
    }
    if vscode_scope == "meta.annotation.rust".to_string() {
        return "attribute".to_string();
    }

    return vscode_scope;
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

fn parse_rgb(s: &str) -> anyhow::Result<Color> {
    if !s.starts_with('#') {
        anyhow::bail!("Invalid hex string: {}", s);
    }
    if s.len() != 7 {
        anyhow::bail!("Hex string must be in the format #rrggbb, got: {}", s);
    }

    let r = u8::from_str_radix(&s[1..=2], 16)?;
    let g = u8::from_str_radix(&s[3..=4], 16)?;
    let b = u8::from_str_radix(&s[5..=6], 16)?;

    Ok(Color::Rgb { r, g, b })
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
    fn test_parse_rgb() {
        let rgb = parse_rgb("#08afBB").unwrap();
        assert_eq!(
            rgb,
            Color::Rgb {
                r: 8,
                g: 175,
                b: 187
            }
        );
    }
}
