//! Terminal cursor state shared by native and detached renderers.

use std::io::{self, Write};

use crossterm::{cursor, QueueableCommand as _};
use serde::{Deserialize, Serialize};

use crate::{color::Color, config::CursorShape};

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
