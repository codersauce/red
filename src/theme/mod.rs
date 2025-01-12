mod vscode;

pub use vscode::parse_vscode_theme;

use crate::color::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    #[allow(unused)]
    pub name: String,
    pub style: Style,
    pub gutter_style: Style,
    pub statusline_style: StatuslineStyle,
    pub token_styles: Vec<TokenStyle>,
    pub line_highlight_style: Option<Style>,
    pub selection_style: Option<Style>,
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

    pub fn get_selection_bg(&self) -> Color {
        self.selection_style
            .as_ref()
            .and_then(|s| s.bg)
            .unwrap_or(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            })
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
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
            token_styles: vec![],
            line_highlight_style: None,
            selection_style: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TokenStyle {
    #[allow(unused)]
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
