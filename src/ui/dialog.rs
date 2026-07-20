//! Type-erased wrapper around the currently active modal [`Component`].
//!
//! A dialog delegates drawing and input while preserving component-specific update hooks
//! for pickers. The editor owns at most one dialog and decides whether unhandled input
//! continues to the normal action pipeline.

use crate::{
    editor::RenderBuffer,
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::Component;

pub struct Dialog {
    title: Option<String>,
    footer: Option<String>,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub style: Style,
    pub border_draw_style: Style,
    pub title_style: Style,
    pub footer_style: Style,
    pub border_style: BorderStyle,
    pub theme: Theme,
}

#[derive(PartialEq)]
pub enum BorderStyle {
    None,
    Single,
}

impl Dialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        title: Option<String>,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        style: &Style,
        border_style: BorderStyle,
        theme: &Theme,
    ) -> Self {
        Self {
            title,
            footer: None,
            x,
            y,
            width,
            height,
            style: style.clone(),
            border_draw_style: style.clone(),
            title_style: style.clone(),
            footer_style: style.clone(),
            border_style,
            theme: theme.clone(),
        }
    }

    pub fn with_border_draw_style(mut self, style: &Style) -> Self {
        self.border_draw_style = style.clone();
        self
    }

    pub fn with_title_style(mut self, style: &Style) -> Self {
        self.title_style = style.clone();
        self
    }

    pub fn with_footer_style(mut self, style: &Style) -> Self {
        self.footer_style = style.clone();
        self
    }

    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
    }

    pub fn set_footer(&mut self, footer: Option<String>) {
        self.footer = footer;
    }
}

impl Component for Dialog {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let mut height = self.height;
        let mut width = self.width;

        if self.border_style != BorderStyle::None {
            height += 2;
        }
        if self.border_style != BorderStyle::None {
            width += 2;
        }

        // Draw the dialog box
        buffer.fill_rect(self.x, self.y, width, height, ' ', &self.style, &self.theme);

        // Draw the border
        if self.border_style != BorderStyle::None {
            let border_style = match self.border_style {
                BorderStyle::Single => "─│┌┐└┘",
                BorderStyle::None => unreachable!(),
            };

            let mut char_indices = border_style.char_indices();
            let top = char_indices.next().unwrap().1;
            let bottom = top;
            let left = char_indices.next().unwrap().1;
            let right = left;
            let top_left = char_indices.next().unwrap().1;
            let top_right = char_indices.next().unwrap().1;
            let bottom_left = char_indices.next().unwrap().1;
            let bottom_right = char_indices.next().unwrap().1;

            buffer.fill_rect(
                self.x,
                self.y,
                width,
                1,
                top,
                &self.border_draw_style,
                &self.theme,
            );
            buffer.fill_rect(
                self.x,
                self.y + height - 1,
                width,
                1,
                bottom,
                &self.border_draw_style,
                &self.theme,
            );
            buffer.fill_rect(
                self.x,
                self.y,
                1,
                height,
                left,
                &self.border_draw_style,
                &self.theme,
            );
            buffer.fill_rect(
                self.x + width - 1,
                self.y,
                1,
                height,
                right,
                &self.border_draw_style,
                &self.theme,
            );

            buffer.set_char(
                self.x,
                self.y,
                top_left,
                &self.border_draw_style,
                &self.theme,
            );
            buffer.set_char(
                self.x + width - 1,
                self.y,
                top_right,
                &self.border_draw_style,
                &self.theme,
            );
            buffer.set_char(
                self.x,
                self.y + height - 1,
                bottom_left,
                &self.border_draw_style,
                &self.theme,
            );
            buffer.set_char(
                self.x + width - 1,
                self.y + height - 1,
                bottom_right,
                &self.border_draw_style,
                &self.theme,
            );
        }

        if let Some(ref title) = self.title {
            let title = format!(" {} ", title);
            let title = truncate_display_width(&title, width);
            let title_width = display_width(&title);
            let cx = self.x + width.saturating_sub(title_width) / 2;
            buffer.set_text(cx, self.y, &title, &self.title_style);
        }

        if let Some(ref footer) = self.footer {
            let footer = format!(" {} ", footer);
            let footer = truncate_display_width(&footer, width.saturating_sub(2));
            let footer_width = display_width(&footer);
            let cx = self
                .x
                .saturating_add(width.saturating_sub(footer_width).saturating_sub(1));
            buffer.set_text(
                cx,
                self.y + height.saturating_sub(1),
                &footer,
                &self.footer_style,
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_cells(buffer: &RenderBuffer, y: usize, x: usize, width: usize) -> Vec<char> {
        buffer.cells[y * buffer.width + x..y * buffer.width + x + width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[test]
    fn long_title_does_not_underflow_when_centered() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(10, 4, &style);
        let dialog = Dialog::new(
            Some("very long title".to_string()),
            0,
            0,
            3,
            1,
            &style,
            BorderStyle::Single,
            &theme,
        );

        dialog.draw(&mut buffer).unwrap();

        assert_eq!(rendered_cells(&buffer, 0, 0, 5).len(), 5);
    }

    #[test]
    fn title_placement_uses_display_width() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(12, 4, &style);
        let dialog = Dialog::new(
            Some("👋".to_string()),
            0,
            0,
            8,
            1,
            &style,
            BorderStyle::Single,
            &theme,
        );

        dialog.draw(&mut buffer).unwrap();

        assert_eq!(rendered_cells(&buffer, 0, 3, 4), vec![' ', '👋', ' ', ' ']);
    }

    #[test]
    fn footer_is_right_aligned_inside_the_border() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(14, 4, &style);
        let mut dialog = Dialog::new(None, 0, 0, 10, 1, &style, BorderStyle::Single, &theme);
        dialog.set_footer(Some("Esc".to_string()));

        dialog.draw(&mut buffer).unwrap();

        assert_eq!(
            rendered_cells(&buffer, 2, 6, 6),
            vec![' ', 'E', 's', 'c', ' ', '┘']
        );
    }
}
