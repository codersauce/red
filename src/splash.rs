//! Startup splash screen model.
//!
//! Layout and palette resolution are pure so rendering stays a thin paint
//! pass; see `docs/SPLASH.md` for the design spec. The splash is a
//! render-only overlay: it never touches buffer contents.

use crate::color::Color;
use crate::theme::{Style, Theme};

/// Content cells required for the full splash block.
pub const FULL_MIN_WIDTH: usize = 60;
pub const FULL_MIN_HEIGHT: usize = 20;
/// Content cells required for the compact wordmark-only variant.
pub const COMPACT_MIN_WIDTH: usize = 26;
pub const COMPACT_MIN_HEIGHT: usize = 7;

/// Visual role of a splash span; each maps to one theme-derived style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Wordmark strokes.
    Mark,
    /// The dot after the wordmark — always the theme's red.
    Dot,
    /// Version, tagline, hint verbs.
    Muted,
    /// Keystroke column.
    Key,
    /// Hint descriptions.
    Text,
    /// Horizontal rules.
    Rule,
    /// The closing trust-model epigraph.
    Epigraph,
}

#[derive(Debug)]
pub struct Span {
    pub text: String,
    pub role: Role,
}

#[derive(Debug)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    fn new(spans: Vec<Span>) -> Self {
        Self { spans }
    }

    fn blank() -> Self {
        Self { spans: Vec::new() }
    }

    pub fn width(&self) -> usize {
        self.spans
            .iter()
            .map(|span| span.text.chars().count())
            .sum()
    }
}

fn span(text: impl Into<String>, role: Role) -> Span {
    Span {
        text: text.into(),
        role,
    }
}

/// The wordmark rows without the trailing dot; 18 cells wide, 21 with it.
const MARK_ROWS: [&str; 4] = [
    "                 ╷",
    "╭──╮   ╭──╮   ╭──┤",
    "│      ├──╯   │  │",
    "╵      ╰──╴   ╰──╯",
];
const MARK_WIDTH: usize = 21;

const HINTS: [(&str, &str, &str); 6] = [
    ("press", "Space ?", "to discover every command"),
    ("press", "Ctrl-p", "to find a file"),
    ("press", "Space A", "to ask the agent"),
    (
        "type",
        ":AgentReview<Enter>",
        "to review the agent's proposals",
    ),
    ("press", "Space t", "to change the theme"),
    ("type", ":q<Enter>", "to exit"),
];
const HINT_VERB_WIDTH: usize = 7;
const HINT_KEY_WIDTH: usize = 22;

fn centered(text: &str, role: Role, width: usize) -> Line {
    let pad = width.saturating_sub(text.chars().count()) / 2;
    Line::new(vec![span(format!("{}{text}", " ".repeat(pad)), role)])
}

fn mark_lines(width: usize) -> Vec<Line> {
    let pad = " ".repeat(width.saturating_sub(MARK_WIDTH) / 2);
    let mut lines = Vec::new();
    for (row, art) in MARK_ROWS.iter().enumerate() {
        let mut spans = vec![span(format!("{pad}{art}"), Role::Mark)];
        if row == MARK_ROWS.len() - 1 {
            spans.push(span("  ", Role::Mark));
            spans.push(span("●", Role::Dot));
        }
        lines.push(Line::new(spans));
    }
    lines
}

fn full_block(version: &str) -> Vec<Line> {
    let width = FULL_MIN_WIDTH;
    let mut lines = mark_lines(width);
    lines.push(Line::blank());
    lines.push(centered(&format!("red v{version}"), Role::Muted, width));
    lines.push(centered(
        "the modal editor for the agent era",
        Role::Muted,
        width,
    ));
    lines.push(centered("github.com/codersauce/red", Role::Muted, width));
    lines.push(Line::blank());
    lines.push(Line::new(vec![span("─".repeat(width), Role::Rule)]));
    for (verb, key, description) in HINTS {
        lines.push(Line::new(vec![
            span(format!("{verb:<HINT_VERB_WIDTH$}"), Role::Muted),
            span(format!("{key:<HINT_KEY_WIDTH$}"), Role::Key),
            span(description, Role::Text),
        ]));
    }
    lines.push(Line::new(vec![span("─".repeat(width), Role::Rule)]));
    lines.push(Line::blank());
    lines.push(centered(
        "every agent edit is a proposal —",
        Role::Epigraph,
        width,
    ));
    lines.push(centered(
        "nothing touches your files until you accept it",
        Role::Epigraph,
        width,
    ));
    lines
}

fn compact_block(version: &str) -> Vec<Line> {
    let width = COMPACT_MIN_WIDTH;
    let mut lines = mark_lines(width);
    lines.push(Line::blank());
    lines.push(centered(&format!("red v{version}"), Role::Muted, width));
    lines.push(Line::new(vec![
        span("press ", Role::Muted),
        span("Space ?", Role::Key),
        span(" for commands", Role::Muted),
    ]));
    lines
}

/// The splash block that fits a content area, or `None` when the area is too
/// small for even the compact variant.
pub fn block(width: usize, height: usize, version: &str) -> Option<Vec<Line>> {
    if width >= FULL_MIN_WIDTH && height >= FULL_MIN_HEIGHT {
        Some(full_block(version))
    } else if width >= COMPACT_MIN_WIDTH && height >= COMPACT_MIN_HEIGHT {
        Some(compact_block(version))
    } else {
        None
    }
}

/// Theme-derived styles for each [`Role`]; never hardcoded colors, with the
/// fallback chains documented in `docs/SPLASH.md`.
pub struct Palette {
    mark: Style,
    dot: Style,
    muted: Style,
    key: Style,
    text: Style,
    rule: Style,
    epigraph: Style,
}

impl Palette {
    pub fn style(&self, role: Role) -> &Style {
        match role {
            Role::Mark => &self.mark,
            Role::Dot => &self.dot,
            Role::Muted => &self.muted,
            Role::Key => &self.key,
            Role::Text => &self.text,
            Role::Rule => &self.rule,
            Role::Epigraph => &self.epigraph,
        }
    }
}

const FALLBACK_RED: Color = Color::Rgb {
    r: 0xE5,
    g: 0x48,
    b: 0x4D,
};

fn workbench_color(theme: &Theme, keys: &[&str]) -> Option<Color> {
    keys.iter().find_map(|key| theme.colors.get(*key).copied())
}

fn theme_red(theme: &Theme, keys: &[&str]) -> Color {
    workbench_color(theme, keys)
        .or_else(|| theme.error_style.as_ref().and_then(|style| style.fg))
        .unwrap_or(FALLBACK_RED)
}

pub fn palette(theme: &Theme) -> Palette {
    let foreground = theme.style.fg;
    let muted = workbench_color(theme, &["descriptionForeground"])
        .or(theme.gutter_style.fg)
        .or(foreground);
    let plain = Style {
        fg: foreground,
        ..Default::default()
    };
    Palette {
        mark: plain.clone(),
        dot: Style {
            fg: Some(theme_red(
                theme,
                &[
                    "terminal.ansiBrightRed",
                    "terminal.ansiRed",
                    "errorForeground",
                ],
            )),
            bold: true,
            ..Default::default()
        },
        muted: Style {
            fg: muted,
            ..Default::default()
        },
        key: Style {
            fg: Some(theme_red(
                theme,
                &[
                    "terminal.ansiRed",
                    "terminal.ansiBrightRed",
                    "errorForeground",
                ],
            )),
            ..Default::default()
        },
        text: plain,
        rule: Style {
            fg: workbench_color(theme, &["editorGroup.border"]).or(muted),
            ..Default::default()
        },
        epigraph: Style {
            fg: muted,
            italic: true,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn full_block_fits_declared_minimum() {
        let lines = block(FULL_MIN_WIDTH, FULL_MIN_HEIGHT, "0.1.1").unwrap();
        assert_eq!(lines.len(), FULL_MIN_HEIGHT);
        assert!(lines.iter().all(|line| line.width() <= FULL_MIN_WIDTH));
        let text = block_text(&lines);
        assert!(text.contains("red v0.1.1"));
        assert!(text.contains(":AgentReview<Enter>"));
        assert!(text.contains("every agent edit is a proposal"));
    }

    #[test]
    fn hint_columns_align() {
        let lines = block(FULL_MIN_WIDTH, FULL_MIN_HEIGHT, "0.1.1").unwrap();
        let hint_lines: Vec<_> = lines
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.role == Role::Key))
            .collect();
        assert_eq!(hint_lines.len(), HINTS.len());
        for line in hint_lines {
            assert_eq!(line.spans[0].text.chars().count(), HINT_VERB_WIDTH);
            assert_eq!(line.spans[1].text.chars().count(), HINT_KEY_WIDTH);
        }
    }

    #[test]
    fn compact_block_below_full_thresholds() {
        let narrow = block(FULL_MIN_WIDTH - 1, FULL_MIN_HEIGHT, "0.1.1").unwrap();
        assert_eq!(narrow.len(), COMPACT_MIN_HEIGHT);
        assert!(narrow.iter().all(|line| line.width() <= COMPACT_MIN_WIDTH));
        let short = block(FULL_MIN_WIDTH, FULL_MIN_HEIGHT - 1, "0.1.1").unwrap();
        assert_eq!(short.len(), COMPACT_MIN_HEIGHT);
        assert!(block_text(&narrow).contains("Space ?"));
    }

    #[test]
    fn nothing_below_compact_thresholds() {
        assert!(block(COMPACT_MIN_WIDTH - 1, COMPACT_MIN_HEIGHT, "0.1.1").is_none());
        assert!(block(COMPACT_MIN_WIDTH, COMPACT_MIN_HEIGHT - 1, "0.1.1").is_none());
    }

    #[test]
    fn every_mark_line_carries_only_mark_and_dot_roles() {
        let lines = block(FULL_MIN_WIDTH, FULL_MIN_HEIGHT, "0.1.1").unwrap();
        let dot_spans: Vec<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.role == Role::Dot)
            .collect();
        assert_eq!(dot_spans.len(), 1);
        assert_eq!(dot_spans[0].text, "●");
    }

    #[test]
    fn palette_falls_back_to_brand_red_without_theme_colors() {
        let palette = palette(&Theme::default());
        assert_eq!(palette.style(Role::Dot).fg, Some(FALLBACK_RED));
        assert_eq!(palette.style(Role::Key).fg, Some(FALLBACK_RED));
        assert!(palette.style(Role::Dot).bold);
        assert!(palette.style(Role::Epigraph).italic);
    }

    #[test]
    fn palette_prefers_theme_workbench_colors() {
        let mut theme = Theme::default();
        let red = Color::Rgb {
            r: 200,
            g: 40,
            b: 40,
        };
        let muted = Color::Rgb {
            r: 90,
            g: 90,
            b: 100,
        };
        theme.colors.insert("terminal.ansiRed".into(), red);
        theme.colors.insert("descriptionForeground".into(), muted);
        let palette = palette(&theme);
        assert_eq!(palette.style(Role::Key).fg, Some(red));
        assert_eq!(palette.style(Role::Muted).fg, Some(muted));
    }
}
