//! Terminal cursor state shared by native and detached renderers.
//!
//! `NO_COLOR` still suppresses color in streamed CLI output. The editor theme
//! is the interface, so interactive painting opts back into ANSI color.

use std::io::{self, Write};

use crossterm::{cursor, QueueableCommand as _};
use serde::{Deserialize, Serialize};

use crate::{color::Color, config::CursorShape};

/// Paint theme colors even when `NO_COLOR` is set.
///
/// Crossterm treats a non-empty `NO_COLOR` as a process-wide ban on
/// foreground and background sequences. That is right for `red update` and
/// other utility output, and wrong for a full-screen editor: without those
/// sequences the theme, syntax highlighting, and edit tint all disappear.
/// Call this before the first interactive frame. It does not change
/// [`crate::cli_style`], which keeps honoring `NO_COLOR`.
pub fn enable_theme_color() {
    crossterm::style::force_color_output(true);
}

/// The native cursor to display after a complete frame. `None` means hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    /// Zero-based terminal coordinates, or no native cursor.
    pub position: Option<(usize, usize)>,
    /// Shape requested by the focused editing surface.
    pub shape: CursorShape,
    /// Optional theme-derived cursor color.
    pub color: Option<Color>,
}

impl CursorState {
    /// Compatibility state for owners that only send a cursor position.
    pub fn visible(position: (usize, usize)) -> Self {
        Self {
            position: Some(position),
            shape: CursorShape::Default,
            color: None,
        }
    }

    /// Queue the final cursor without making its intermediate position visible.
    pub fn queue(self, output: &mut impl Write) -> io::Result<()> {
        let position = self
            .position
            .and_then(|(x, y)| Some((u16::try_from(x).ok()?, u16::try_from(y).ok()?)));
        let Some((x, y)) = position else {
            output.queue(cursor::Hide)?;
            return Ok(());
        };
        if let Some(color) = self.color {
            write!(output, "\x1b]12;{color}\x1b\\")?;
        }
        let shape = match self.shape {
            CursorShape::Default => cursor::SetCursorStyle::DefaultUserShape,
            CursorShape::BlinkingBlock => cursor::SetCursorStyle::BlinkingBlock,
            CursorShape::SteadyBlock => cursor::SetCursorStyle::SteadyBlock,
            CursorShape::BlinkingUnderscore => cursor::SetCursorStyle::BlinkingUnderScore,
            CursorShape::SteadyUnderscore => cursor::SetCursorStyle::SteadyUnderScore,
            CursorShape::BlinkingBar => cursor::SetCursorStyle::BlinkingBar,
            CursorShape::SteadyBar => cursor::SetCursorStyle::SteadyBar,
        };
        output
            .queue(shape)?
            .queue(cursor::MoveTo(x, y))?
            .queue(cursor::Show)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_color_is_painted_when_no_color_is_set() {
        // Crossterm's color switch is process-global. Leave it enabled so a
        // later paint does not straddle the transition. Do not change the
        // `NO_COLOR` environment variable; CLI styling reads that directly.
        crossterm::style::force_color_output(false);
        let suppressed = crossterm::style::Colored::ForegroundColor(crossterm::style::Color::Rgb {
            r: 12,
            g: 34,
            b: 56,
        })
        .to_string();
        assert!(
            suppressed.is_empty(),
            "NO_COLOR should suppress streamed color, got {suppressed}"
        );
        enable_theme_color();
        let painted = crossterm::style::Colored::ForegroundColor(crossterm::style::Color::Rgb {
            r: 12,
            g: 34,
            b: 56,
        })
        .to_string();
        assert_eq!(painted, "38;2;12;34;56");
    }

    #[test]
    fn hidden_cursor_emits_no_style_or_position_commands() {
        let mut output = Vec::new();
        CursorState {
            position: None,
            shape: CursorShape::SteadyBar,
            color: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
        }
        .queue(&mut output)
        .unwrap();
        assert_eq!(output, b"\x1b[?25l");
        output.clear();
        CursorState::visible((usize::MAX, 0))
            .queue(&mut output)
            .unwrap();
        assert_eq!(output, b"\x1b[?25l");
    }
}
