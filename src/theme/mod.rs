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

    pub fn builder() -> ThemeBuilder {
        ThemeBuilder::default()
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

#[derive(Debug, Default)]
pub struct ThemeBuilder {
    name: Option<String>,
    style: Option<Style>,
    gutter_style: Option<Style>,
    statusline_style: Option<StatuslineStyle>,
    token_styles: Vec<TokenStyle>,
}

#[allow(dead_code)]
impl ThemeBuilder {
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    pub fn style(mut self, fg: &str, bg: &str) -> Self {
        self.style = Some(Style {
            fg: Some(parse_rgb(fg).unwrap()),
            bg: Some(parse_rgb(bg).unwrap()),
            ..Default::default()
        });
        self
    }

    pub fn gutter(mut self, fg: &str, bg: &str) -> Self {
        self.gutter_style = Some(Style {
            fg: Some(parse_rgb(fg).unwrap()),
            bg: Some(parse_rgb(bg).unwrap()),
            ..Default::default()
        });
        self
    }

    pub fn statusline_style(
        mut self,
        outer_fg: &str,
        outer_bg: &str,
        outer_chars: [char; 4],
        inner_fg: &str,
        inner_bg: &str,
    ) -> Self {
        self.statusline_style = Some(StatuslineStyle {
            outer_style: Style {
                fg: Some(parse_rgb(outer_fg).unwrap()),
                bg: Some(parse_rgb(outer_bg).unwrap()),
                ..Default::default()
            },
            outer_chars,
            inner_style: Style {
                fg: Some(parse_rgb(inner_fg).unwrap()),
                bg: Some(parse_rgb(inner_bg).unwrap()),
                ..Default::default()
            },
        });
        self
    }

    pub fn scope(mut self, scope: &str, fg: &str, bg: &str) -> Self {
        self.token_styles.push(TokenStyle {
            name: None,
            scope: vec![scope.to_string()],
            style: Style {
                fg: Some(parse_rgb(fg).unwrap()),
                bg: Some(parse_rgb(bg).unwrap()),
                ..Default::default()
            },
        });
        self
    }

    pub fn build(self) -> Theme {
        Theme {
            name: self.name.unwrap_or_else(|| "default".to_string()),
            style: self.style.unwrap_or_default(),
            gutter_style: self.gutter_style.unwrap_or_default(),
            statusline_style: self.statusline_style.unwrap_or_default(),
            token_styles: self.token_styles,
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

pub fn parse_rgb(s: &str) -> anyhow::Result<Color> {
    if !s.starts_with('#') {
        anyhow::bail!("Invalid hex string: {}", s);
    }
    if s.len() != 7 && s.len() != 9 {
        anyhow::bail!(
            "Hex string must be in the format #rrggbb or #rrggbbaa, got: {}",
            s
        );
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

    #[test]
    fn test_parse_rgb_with_alpha() {
        let rgb = parse_rgb("#d8dee9ff").unwrap();
        assert_eq!(
            rgb,
            Color::Rgb {
                r: 216,
                g: 222,
                b: 233,
            }
        )
    }
}
