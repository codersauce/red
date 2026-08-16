//! Foreground-first styles for persistent UI surfaces and structured diffs.

use crate::color::{blend_color, ensure_minimum_contrast, Color};

use super::{
    Style, Theme, ThemeMode, MINIMUM_SELECTION_STATE_CONTRAST, MINIMUM_SELECTION_TEXT_CONTRAST,
};

/// Text roles whose backgrounds always belong to the surrounding surface.
#[derive(Debug, Clone)]
pub(crate) struct SurfacePalette {
    pub surface: Style,
    pub primary: Style,
    pub secondary: Style,
    pub muted: Style,
    pub accent: Style,
    pub error: Style,
    pub divider: Style,
}

impl SurfacePalette {
    /// Keeps the surface's visual roles legible on a different, opaque background.
    pub fn on_background(&self, background: Color) -> Self {
        let mut palette = self.clone();
        for style in [
            &mut palette.surface,
            &mut palette.primary,
            &mut palette.secondary,
            &mut palette.accent,
            &mut palette.error,
        ] {
            style.bg = Some(background);
            style.fg = style.fg.map(|foreground| {
                ensure_minimum_contrast(foreground, background, MINIMUM_SELECTION_TEXT_CONTRAST)
            });
        }
        for style in [&mut palette.muted, &mut palette.divider] {
            style.bg = Some(background);
            style.fg = style.fg.map(|foreground| {
                ensure_minimum_contrast(foreground, background, MINIMUM_SELECTION_STATE_CONTRAST)
            });
        }
        palette
    }

    pub fn new(theme: &Theme, surface: &Style) -> Self {
        let background = blend_color(
            surface.bg.or(theme.style.bg).unwrap_or_default(),
            Color::default(),
        );
        let primary = surface.fg.or(theme.style.fg).unwrap_or(Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        let color = |keys: &[&str], fallback: Color| {
            keys.iter()
                .find_map(|key| theme.colors.get(*key).copied())
                .unwrap_or(fallback)
        };
        let secondary = color(&["descriptionForeground"], primary);
        let muted = color(
            &["editorLineNumber.foreground", "descriptionForeground"],
            theme.ui_style.muted.fg.unwrap_or(secondary),
        );
        let accent = color(
            &[
                "textLink.foreground",
                "editorInfo.foreground",
                "focusBorder",
            ],
            theme.ui_style.popup_title.fg.unwrap_or(primary),
        );
        let error = color(&["editorError.foreground", "list.errorForeground"], primary);
        let role = |foreground, minimum, bold| Style {
            fg: Some(ensure_minimum_contrast(foreground, background, minimum)),
            bg: Some(background),
            bold,
            italic: false,
        };
        Self {
            surface: surface.with_bg(Some(background)),
            primary: role(primary, MINIMUM_SELECTION_TEXT_CONTRAST, false),
            secondary: role(secondary, MINIMUM_SELECTION_TEXT_CONTRAST, false),
            muted: role(muted, 3.0, false),
            accent: role(accent, MINIMUM_SELECTION_TEXT_CONTRAST, true),
            error: role(error, MINIMUM_SELECTION_TEXT_CONTRAST, false),
            divider: role(muted, 3.0, false),
        }
    }
}

/// Resolved, opaque diff colors. Text colors never carry another surface's background.
pub(crate) struct DiffPalette {
    pub added: Style,
    pub removed: Style,
    pub added_text: Color,
    pub removed_text: Color,
    pub added_marker: Color,
    pub removed_marker: Color,
    pub hunk: Style,
}

impl DiffPalette {
    pub fn new(theme: &Theme) -> Self {
        let surface = SurfacePalette::new(theme, &theme.style);
        let background = surface.surface.bg.unwrap_or_default();
        let accent = |added: bool| {
            let (keys, scope, fallback) = if added {
                (
                    [
                        "gitDecoration.addedResourceForeground",
                        "editorGutter.addedBackground",
                        "terminal.ansiGreen",
                    ],
                    "markup.inserted.diff",
                    Color::Rgb {
                        r: 80,
                        g: 160,
                        b: 100,
                    },
                )
            } else {
                (
                    [
                        "gitDecoration.deletedResourceForeground",
                        "editorGutter.deletedBackground",
                        "terminal.ansiRed",
                    ],
                    "markup.deleted.diff",
                    Color::Rgb {
                        r: 210,
                        g: 85,
                        b: 95,
                    },
                )
            };
            keys[..2]
                .iter()
                .find_map(|key| theme.colors.get(*key).copied())
                .or_else(|| theme.get_style(scope).and_then(|style| style.fg))
                .or_else(|| theme.colors.get(keys[2]).copied())
                .unwrap_or(fallback)
        };
        let added = accent(true);
        let removed = accent(false);
        let resolve = |line_key: &str, text_key: &str, accent: Color| {
            let line = theme
                .colors
                .get(line_key)
                .copied()
                .or_else(|| theme.colors.get(text_key).copied())
                .unwrap_or_else(|| tint(accent, 32));
            let line = blend_color(line, background);
            let text = theme
                .colors
                .get(text_key)
                .copied()
                .unwrap_or_else(|| tint(accent, 72));
            (theme.style.with_bg(Some(line)), blend_color(text, line))
        };
        let (added_style, added_text) = resolve(
            "diffEditor.insertedLineBackground",
            "diffEditor.insertedTextBackground",
            added,
        );
        let (removed_style, removed_text) = resolve(
            "diffEditor.removedLineBackground",
            "diffEditor.removedTextBackground",
            removed,
        );
        let hunk_background = theme
            .colors
            .get("multiDiffEditor.headerBackground")
            .copied()
            .or_else(|| {
                theme
                    .line_highlight_style
                    .as_ref()
                    .and_then(|style| style.bg)
            })
            .unwrap_or_else(|| {
                tint(
                    surface.accent.fg.unwrap_or(added),
                    if theme.mode() == ThemeMode::Light {
                        20
                    } else {
                        28
                    },
                )
            });
        Self {
            added: added_style,
            removed: removed_style,
            added_text,
            removed_text,
            added_marker: ensure_minimum_contrast(added, background, 3.0),
            removed_marker: ensure_minimum_contrast(removed, background, 3.0),
            hunk: surface
                .secondary
                .with_bg(Some(blend_color(hunk_background, background))),
        }
    }
}

fn tint(color: Color, alpha: u8) -> Color {
    let (Color::Rgb { r, g, b } | Color::Rgba { r, g, b, .. }) = color;
    Color::Rgba { r, g, b, a: alpha }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_dark_has_distinct_opaque_diff_surfaces() {
        let theme = crate::theme::parse_vscode_theme("themes/one-dark-pro.json").unwrap();
        let palette = DiffPalette::new(&theme);
        assert_ne!(palette.added.bg, theme.style.bg);
        assert_ne!(palette.removed.bg, theme.style.bg);
        assert_ne!(palette.added.bg, palette.removed.bg);
        assert!(matches!(palette.added.bg, Some(Color::Rgb { .. })));
        assert!(matches!(palette.removed.bg, Some(Color::Rgb { .. })));
    }

    #[test]
    fn all_bundled_themes_have_resolved_diff_backgrounds() {
        for entry in std::fs::read_dir("themes").unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let theme = crate::theme::parse_vscode_theme(path.to_str().unwrap()).unwrap();
            let palette = DiffPalette::new(&theme);
            for color in [palette.added.bg, palette.removed.bg, palette.hunk.bg] {
                assert!(
                    matches!(color, Some(Color::Rgb { .. })),
                    "{}",
                    path.display()
                );
            }
        }
    }
}
