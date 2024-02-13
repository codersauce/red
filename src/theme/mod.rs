use crossterm::style::Color;

mod vscode;

pub use vscode::parse_vscode_theme;

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub style: Style,
    pub gutter_style: Style,
    pub statusline_style: StatuslineStyle,
    pub token_styles: Vec<TokenStyle>,
}

impl Theme {
    pub fn get_style(&self, scope: &str) -> Option<Style> {
        self.token_styles.iter().find_map(|ts| {
            if ts.scope.contains(&scope.to_string()) {
                Some(ts.style.clone())
            } else {
                None
            }
        })
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            style: Style {
                fg: Some(Color::White),
                bg: Some(Color::Black),
                bold: false,
                italic: false,
            },
            gutter_style: Style::default(),
            statusline_style: StatuslineStyle::default(),
            token_styles: vec![],
        }
    }
}

#[derive(Debug, Clone)]
pub struct TokenStyle {
    pub name: Option<String>,
    pub scope: Vec<String>,
    pub style: Style,
}

#[derive(Debug, Default, Clone)]
pub struct StatuslineStyle {
    pub outer_style: Style,
    pub outer_chars: [char; 4],
    pub inner_style: Style,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
}
