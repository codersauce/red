use crossterm::style::{Attribute, Attributes, Color, ContentStyle};

mod vscode;

pub use vscode::parse_vscode_theme;

#[derive(Debug)]
pub struct Theme {
    pub name: String,
    pub style: Style,
    pub gutter_style: Style,
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

#[derive(Debug)]
pub struct TokenStyle {
    pub name: Option<String>,
    pub scope: Vec<String>,
    pub style: Style,
}

#[derive(Debug, Default, Clone)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub fn to_content_style(&self, fallback_style: &Style) -> ContentStyle {
        let foreground_color = match self.fg {
            Some(fg) => Some(fg),
            None => fallback_style.fg,
        };
        let background_color = match self.bg {
            Some(bg) => Some(bg),
            None => fallback_style.bg,
        };
        let mut attributes = Attributes::default();
        if self.italic {
            attributes.set(Attribute::Italic);
        }
        if self.bold {
            attributes.set(Attribute::Bold);
        }

        ContentStyle {
            foreground_color,
            background_color,
            attributes,
            ..Default::default()
        }
    }
}
