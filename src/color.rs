//! Terminal color representation, parsing, blending, and contrast helpers.
//!
//! [`Color`] preserves terminal palette values and true-color RGB values without
//! assuming a background. Theme and plugin code can resolve semantic choices before
//! using these helpers for deterministic color arithmetic.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    if s.eq_ignore_ascii_case("transparent") {
        return Ok(Color::Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        });
    }

    let named = match s.to_ascii_lowercase().as_str() {
        "black" => Some(Color::Rgb { r: 0, g: 0, b: 0 }),
        "white" => Some(Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        }),
        "red" => Some(Color::Rgb { r: 255, g: 0, b: 0 }),
        "green" => Some(Color::Rgb { r: 0, g: 128, b: 0 }),
        "blue" => Some(Color::Rgb { r: 0, g: 0, b: 255 }),
        "yellow" => Some(Color::Rgb {
            r: 255,
            g: 255,
            b: 0,
        }),
        "cyan" => Some(Color::Rgb {
            r: 0,
            g: 255,
            b: 255,
        }),
        "magenta" => Some(Color::Rgb {
            r: 255,
            g: 0,
            b: 255,
        }),
        "gray" | "grey" => Some(Color::Rgb {
            r: 128,
            g: 128,
            b: 128,
        }),
        _ => None,
    };
    if let Some(color) = named {
        return Ok(color);
    }

    if !s.starts_with('#') {
        anyhow::bail!("Invalid hex string: {}", s);
    }

    let hex = s.trim_start_matches('#');
    let expanded;
    let hex = if hex.len() == 3 || hex.len() == 4 {
        expanded = hex.chars().flat_map(|c| [c, c]).collect::<String>();
        expanded.as_str()
    } else {
        hex
    };
    let len = hex.len();

    if len != 6 && len != 8 {
        anyhow::bail!(
            "Hex string must be in the format #RGB, #RGBA, #RRGGBB or #RRGGBBAA, got: {}",
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
    let background = match background {
        Color::Rgba { r, g, b, a } => {
            blend_color(Color::Rgba { r, g, b, a }, Color::Rgb { r: 0, g: 0, b: 0 })
        }
        color => color,
    };

    match foreground {
        Color::Rgba { r, g, b, a } => {
            let Color::Rgb {
                r: bg_r,
                g: bg_g,
                b: bg_b,
            } = background
            else {
                unreachable!("background was normalized to RGB");
            };
            let alpha = a as f32 / 255.0;
            let inv_alpha = 1.0 - alpha;

            let r = (f32::from(r) * alpha + f32::from(bg_r) * inv_alpha) as u8;
            let g = (f32::from(g) * alpha + f32::from(bg_g) * inv_alpha) as u8;
            let b = (f32::from(b) * alpha + f32::from(bg_b) * inv_alpha) as u8;

            Color::Rgb { r, g, b }
        }
        Color::Rgb { .. } => foreground,
    }
}

pub(crate) fn contrast_ratio(foreground: Color, background: Color) -> f32 {
    let background = blend_color(background, Color::Rgb { r: 0, g: 0, b: 0 });
    let foreground = blend_color(foreground, background);
    let foreground_luminance = relative_luminance(foreground);
    let background_luminance = relative_luminance(background);
    let lighter = foreground_luminance.max(background_luminance);
    let darker = foreground_luminance.min(background_luminance);
    (lighter + 0.05) / (darker + 0.05)
}

pub(crate) fn ensure_minimum_contrast(
    foreground: Color,
    background: Color,
    minimum_ratio: f32,
) -> Color {
    let background = blend_color(background, Color::Rgb { r: 0, g: 0, b: 0 });
    let foreground = blend_color(foreground, background);
    let minimum_ratio = minimum_ratio.clamp(1.0, 21.0);
    if contrast_ratio(foreground, background) >= minimum_ratio {
        return foreground;
    }

    let black = Color::Rgb { r: 0, g: 0, b: 0 };
    let white = Color::Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    [black, white]
        .into_iter()
        .filter_map(|target| contrast_candidate(foreground, background, target, minimum_ratio))
        .min_by_key(|candidate| color_distance_squared(foreground, *candidate))
        .unwrap_or_else(|| {
            if contrast_ratio(black, background) >= contrast_ratio(white, background) {
                black
            } else {
                white
            }
        })
}

fn relative_luminance(color: Color) -> f32 {
    let Color::Rgb { r, g, b } = color else {
        return relative_luminance(blend_color(color, Color::Rgb { r: 0, g: 0, b: 0 }));
    };
    let linear = |component: u8| {
        let component = f32::from(component) / 255.0;
        if component <= 0.04045 {
            component / 12.92
        } else {
            ((component + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast_candidate(
    foreground: Color,
    background: Color,
    target: Color,
    minimum_ratio: f32,
) -> Option<Color> {
    if contrast_ratio(target, background) < minimum_ratio {
        return None;
    }

    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..16 {
        let amount = (low + high) / 2.0;
        let candidate = mix_color(foreground, target, amount);
        if contrast_ratio(candidate, background) >= minimum_ratio {
            high = amount;
        } else {
            low = amount;
        }
    }

    let candidate = mix_color(foreground, target, high);
    Some(if contrast_ratio(candidate, background) >= minimum_ratio {
        candidate
    } else {
        target
    })
}

fn mix_color(from: Color, to: Color, amount: f32) -> Color {
    let Color::Rgb {
        r: from_r,
        g: from_g,
        b: from_b,
    } = from
    else {
        unreachable!("contrast colors are normalized to RGB");
    };
    let Color::Rgb {
        r: to_r,
        g: to_g,
        b: to_b,
    } = to
    else {
        unreachable!("contrast targets are RGB");
    };
    let mix = |from: u8, to: u8| {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * amount).round() as u8
    };
    Color::Rgb {
        r: mix(from_r, to_r),
        g: mix(from_g, to_g),
        b: mix(from_b, to_b),
    }
}

fn color_distance_squared(left: Color, right: Color) -> u32 {
    let Color::Rgb {
        r: left_r,
        g: left_g,
        b: left_b,
    } = left
    else {
        unreachable!("contrast colors are normalized to RGB");
    };
    let Color::Rgb {
        r: right_r,
        g: right_g,
        b: right_b,
    } = right
    else {
        unreachable!("contrast candidates are RGB");
    };
    let square = |left: u8, right: u8| u32::from(left.abs_diff(right)).pow(2);
    square(left_r, right_r) + square(left_g, right_g) + square(left_b, right_b)
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
    fn test_parse_short_hex_and_named_colors() {
        assert_eq!(
            parse_rgb("#fff").unwrap(),
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }
        );
        assert_eq!(
            parse_rgb("#08af").unwrap(),
            Color::Rgba {
                r: 0,
                g: 136,
                b: 170,
                a: 255,
            }
        );
        assert_eq!(
            parse_rgb("transparent").unwrap(),
            Color::Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            }
        );
        assert_eq!(
            parse_rgb("white").unwrap(),
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255
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

    #[test]
    fn contrast_ratio_uses_wcag_relative_luminance() {
        let black = Color::Rgb { r: 0, g: 0, b: 0 };
        let white = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };

        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.001);
    }

    #[test]
    fn minimum_contrast_preserves_colors_that_already_pass() {
        let foreground = Color::Rgb {
            r: 242,
            g: 241,
            b: 239,
        };
        let background = Color::Rgb {
            r: 57,
            g: 59,
            b: 68,
        };

        assert_eq!(
            ensure_minimum_contrast(foreground, background, 4.5),
            foreground
        );
    }

    #[test]
    fn minimum_contrast_adjusts_only_as_far_as_needed() {
        let foreground = Color::Rgb {
            r: 139,
            g: 164,
            b: 176,
        };
        let background = Color::Rgb {
            r: 57,
            g: 59,
            b: 68,
        };

        let adjusted = ensure_minimum_contrast(foreground, background, 4.5);

        assert_ne!(adjusted, foreground);
        assert_ne!(
            adjusted,
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert!(contrast_ratio(adjusted, background) >= 4.5);
    }
}
