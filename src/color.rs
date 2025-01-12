use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Rgb { r: u8, g: u8, b: u8 },
    Rgba { r: u8, g: u8, b: u8, a: u8 },
}

impl Default for Color {
    fn default() -> Self {
        Color::Rgb { r: 0, g: 0, b: 0 }
    }
}

impl From<Color> for crossterm::style::Color {
    fn from(color: Color) -> Self {
        match color {
            Color::Rgb { r, g, b } => crossterm::style::Color::Rgb { r, g, b },
            Color::Rgba { r, g, b, a: _ } => crossterm::style::Color::Rgb { r, g, b },
        }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Color::Rgb { r, g, b } => write!(f, "#{:02x}{:02x}{:02x}", r, g, b),
            Color::Rgba { r, g, b, a } => write!(f, "#{:02x}{:02x}{:02x}{:02x}", r, g, b, a),
        }
    }
}

pub fn parse_rgb(s: &str) -> anyhow::Result<Color> {
    if !s.starts_with('#') {
        anyhow::bail!("Invalid hex string: {}", s);
    }

    let hex = s.trim_start_matches('#');
    let len = hex.len();

    if len != 6 && len != 8 {
        anyhow::bail!(
            "Hex string must be in the format #RRGGBB or #RRGGBBAA, got: {}",
            s
        );
    }

    let r = u8::from_str_radix(&hex[0..2], 16)?;
    let g = u8::from_str_radix(&hex[2..4], 16)?;
    let b = u8::from_str_radix(&hex[4..6], 16)?;

    if len == 8 {
        let a = u8::from_str_radix(&hex[6..8], 16)?;
        Ok(Color::Rgba { r, g, b, a })
    } else {
        Ok(Color::Rgb { r, g, b })
    }
}

pub fn blend_color(foreground: Color, background: Color) -> Color {
    match (foreground, background) {
        (
            Color::Rgba { r, g, b, a },
            Color::Rgb {
                r: bg_r,
                g: bg_g,
                b: bg_b,
            },
        ) => {
            let alpha = a as f32 / 255.0;
            let inv_alpha = 1.0 - alpha;

            let r = (r as f32 * alpha + bg_r as f32 * inv_alpha) as u8;
            let g = (g as f32 * alpha + bg_g as f32 * inv_alpha) as u8;
            let b = (b as f32 * alpha + bg_b as f32 * inv_alpha) as u8;

            Color::Rgb { r, g, b }
        }
        _ => foreground, // Fallback if blending isn't needed or possible
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_rgb() {
        assert_eq!(
            parse_rgb("#08afBB").unwrap(),
            Color::Rgb {
                r: 8,
                g: 175,
                b: 187
            }
        );
    }

    #[test]
    fn test_parse_rgba() {
        assert_eq!(
            parse_rgb("#d8dee9ff").unwrap(),
            Color::Rgba {
                r: 216,
                g: 222,
                b: 233,
                a: 255
            }
        );
    }

    #[test]
    fn test_blend_color() {
        let fg = Color::Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 128,
        };
        let bg = Color::Rgb { r: 0, g: 0, b: 255 };
        let blended = blend_color(fg, bg);
        assert_eq!(
            blended,
            Color::Rgb {
                r: 128,
                g: 0,
                b: 126
            }
        );
    }
}
