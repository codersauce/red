use crossterm::style::Color;

mod vscode;

pub use vscode::parse_vscode_theme;

#[derive(Debug)]
struct Theme {
    name: String,
    style: Style,
    token_styles: Vec<TokenStyle>,
}

#[derive(Debug)]
struct TokenStyle {
    name: Option<String>,
    scope: Vec<String>,
    style: Style,
}

#[derive(Debug, Default)]
struct Style {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    italic: bool,
}
