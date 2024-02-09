use std::fs;

use crossterm::style::Color;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Default)]
struct Style {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    italic: bool,
}

#[derive(Debug)]
struct TokenStyle {
    name: Option<String>,
    scope: Vec<String>,
    style: Style,
}

#[derive(Debug)]
struct Theme {
    name: String,
    style: Style,
    token_styles: Vec<TokenStyle>,
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
            VsCodeScope::Single(s) => vec![s],
            VsCodeScope::Multiple(v) => v,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTokenColor {
    name: Option<String>,
    scope: VsCodeScope,
    settings: Map<String, Value>,
}

// FIXME: this actually needs to be a TryFrom, since the parsing of rgb values can fail
impl From<VsCodeTokenColor> for TokenStyle {
    fn from(tc: VsCodeTokenColor) -> Self {
        let mut style = Style::default();

        if let Some(fg) = tc.settings.get("foreground") {
            style.fg =
                Some(parse_rgb(fg.as_str().expect("fg is string")).expect("parsing rgb works"));
        }

        if let Some(bg) = tc.settings.get("backgrounbg") {
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

        Self {
            name: tc.name,
            scope: tc.scope.into(),
            style,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTheme {
    name: Option<String>,
    #[serde(rename = "type")]
    typ: Option<String>,
    colors: Map<String, Value>,
    token_colors: Vec<VsCodeTokenColor>,
}

fn parse_rgb(s: &str) -> anyhow::Result<Color> {
    if !s.starts_with('#') {
        anyhow::bail!("Invalid hex string");
    }

    let r = u8::from_str_radix(&s[1..=2], 16)?;
    let g = u8::from_str_radix(&s[3..=4], 16)?;
    let b = u8::from_str_radix(&s[5..=6], 16)?;

    Ok(Color::Rgb { r, g, b })
}

pub fn parse_vscode_theme(file: &str) -> anyhow::Result<Theme> {
    let contents = fs::read_to_string(file)?;
    let vscode_theme: VsCodeTheme = serde_json::from_str(&contents)?;

    let mut token_styles = Vec::new();
    for token_color in vscode_theme.token_colors {
        token_styles.push(token_color.into());
    }

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
        let rgb = parse_rgb("#08afBB");
        println!("{rgb:#?}");
    }
}
