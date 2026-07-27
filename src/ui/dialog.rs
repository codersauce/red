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

use super::{ActionBar, Component, UiAction};

pub struct Dialog {
    title: Option<String>,
    header_status: Option<String>,
    footer: Option<String>,
    actions: Vec<UiAction>,
    action_width: Option<usize>,
    left_aligned_title: bool,
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
    surface_role: Option<SurfaceRole>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Single,
    Rounded,
}

impl BorderStyle {
    /// Returns horizontal, vertical, and the four clockwise frame-corner glyphs.
    #[must_use]
    pub(crate) const fn glyphs(self) -> Option<[char; 6]> {
        match self {
            Self::None => None,
            Self::Single => Some(['─', '│', '┌', '┐', '└', '┘']),
            Self::Rounded => Some(['─', '│', '╭', '╮', '╰', '╯']),
        }
    }
}

/// Selects the theme-owned styles for a shared dialog or popup surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceRole {
    Dialog,
    Popup,
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
            header_status: None,
            footer: None,
            actions: Vec::new(),
            action_width: None,
            left_aligned_title: false,
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
            surface_role: None,
        }
    }

    /// Associates this surface with its canonical theme role.
    #[must_use]
    pub(crate) fn with_surface_theme(mut self, theme: &Theme, role: SurfaceRole) -> Self {
        self.apply_surface_theme(theme, role);
        self
    }

    /// Refreshes all theme-derived chrome together.
    pub(crate) fn apply_surface_theme(&mut self, theme: &Theme, role: SurfaceRole) {
        let styles = &theme.ui_style;
        let (surface, border, title) = match role {
            SurfaceRole::Dialog => (&styles.dialog, &styles.dialog_border, &styles.dialog_title),
            SurfaceRole::Popup => (&styles.popup, &styles.popup_border, &styles.popup_title),
        };
        self.style = surface.clone();
        self.border_draw_style = border.clone();
        self.title_style = title.clone();
        self.footer_style = styles.muted.clone();
        self.theme = theme.clone();
        self.surface_role = Some(role);
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

    /// Places a modal title inside the left edge of its top border.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn with_left_aligned_title(mut self) -> Self {
        self.left_aligned_title = true;
        self
    }

    /// Attaches complete, responsive footer actions to this dialog.
    #[must_use]
    #[cfg(test)]
    pub fn with_actions(mut self, actions: Vec<UiAction>) -> Self {
        self.set_actions(actions);
        self
    }

    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
    }

    /// Sets compact, right-aligned metadata in the dialog header.
    #[cfg(test)]
    pub(crate) fn set_header_status(&mut self, status: Option<String>) {
        self.header_status = status;
    }

    pub fn set_footer(&mut self, footer: Option<String>) {
        self.footer = footer;
        self.actions.clear();
    }

    /// Replaces the responsive footer actions without changing legacy footers.
    pub fn set_actions(&mut self, actions: Vec<UiAction>) {
        self.set_footer(None);
        self.actions = actions;
    }
}

impl Component for Dialog {
    fn set_theme(&mut self, theme: &Theme) {
        if let Some(role) = self.surface_role {
            self.apply_surface_theme(theme, role);
        } else {
            self.theme = theme.clone();
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let mut height = self.height;
        let mut width = self.width;

        if self.border_style != BorderStyle::None {
            height = height.saturating_add(2);
        }
        if self.border_style != BorderStyle::None {
            width = width.saturating_add(2);
        }

        // Draw the dialog box
        buffer.fill_rect(self.x, self.y, width, height, ' ', &self.style, &self.theme);

        // Draw the border
        if self.border_style != BorderStyle::None {
            let [top, left, top_left, top_right, bottom_left, bottom_right] = self
                .border_style
                .glyphs()
                .expect("a visible border has frame glyphs");
            let bottom = top;
            let right = left;

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

        let inset = usize::from(self.border_style != BorderStyle::None);
        let inner_width = width.saturating_sub(inset.saturating_mul(2));

        if let Some(ref title) = self.title {
            let title = format!(" {} ", title);
            let status_width = self
                .header_status
                .as_ref()
                .map_or(0, |status| display_width(status).saturating_add(3));
            let title_width_limit = inner_width.saturating_sub(status_width);
            let title = truncate_display_width(&title, title_width_limit);
            let title_width = display_width(&title);
            let cx = if self.left_aligned_title {
                self.x.saturating_add(inset)
            } else {
                self.x
                    .saturating_add(inset)
                    .saturating_add(inner_width.saturating_sub(title_width) / 2)
            };
            buffer.set_text(cx, self.y, &title, &self.title_style);
        }

        let footer_style = self.footer_style.with_bg(self.style.bg);

        if let Some(status) = self.header_status.as_deref() {
            let status = format!(" {status} ");
            let status = truncate_display_width(&status, inner_width);
            let status_x = self
                .x
                .saturating_add(width.saturating_sub(inset))
                .saturating_sub(display_width(&status));
            buffer.set_text(status_x, self.y, &status, &footer_style);
        }

        let footer_x = self.x.saturating_add(inset);
        let footer_y = self
            .y
            .saturating_add(height.saturating_sub(inset.saturating_add(1)));
        if !self.actions.is_empty() && inner_width > 0 {
            let action_width = self.action_width.unwrap_or(inner_width).min(inner_width);
            let action_x = footer_x.saturating_add(inner_width.saturating_sub(action_width));
            ActionBar::new(&self.actions).render_right_aligned(
                buffer,
                action_x,
                footer_y,
                action_width,
                &self.theme,
                &footer_style,
            );
        } else if let Some(ref footer) = self.footer {
            let footer = format!(" {} ", footer);
            let footer = truncate_display_width(&footer, inner_width);
            let footer_width = display_width(&footer);
            let cx = footer_x.saturating_add(inner_width.saturating_sub(footer_width));
            buffer.set_text(cx, footer_y, &footer, &footer_style);
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

        let header = rendered_cells(&buffer, 0, 0, 5);
        assert_eq!(header.first(), Some(&'┌'));
        assert_eq!(header.last(), Some(&'┐'));
    }

    #[test]
    fn centered_title_and_metadata_preserve_both_corners() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(16, 4, &style);
        let mut dialog = Dialog::new(
            Some("very long title".to_string()),
            0,
            0,
            10,
            1,
            &style,
            BorderStyle::Single,
            &theme,
        );
        dialog.set_header_status(Some("12".to_string()));

        dialog.draw(&mut buffer).unwrap();

        let header = rendered_cells(&buffer, 0, 0, 12);
        assert_eq!(header.first(), Some(&'┌'));
        assert_eq!(header.last(), Some(&'┐'));
        assert!(header.into_iter().collect::<String>().contains(" 12 "));
    }

    #[test]
    fn rounded_border_preserves_all_four_corners() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(8, 4, &style);
        let dialog = Dialog::new(None, 1, 0, 4, 1, &style, BorderStyle::Rounded, &theme);

        dialog.draw(&mut buffer).unwrap();

        assert_eq!(buffer.cells[1].c, '╭');
        assert_eq!(buffer.cells[6].c, '╮');
        assert_eq!(buffer.cells[2 * buffer.width + 1].c, '╰');
        assert_eq!(buffer.cells[2 * buffer.width + 6].c, '╯');
    }

    #[test]
    fn themed_surface_refreshes_every_chrome_style() {
        use crate::color::Color;

        let style = Style::default();
        let initial_theme = Theme::default();
        let mut dialog = Dialog::new(
            None,
            0,
            0,
            4,
            1,
            &style,
            BorderStyle::Single,
            &initial_theme,
        )
        .with_surface_theme(&initial_theme, SurfaceRole::Popup);
        let mut updated = Theme::default();
        updated.ui_style.popup.fg = Some(Color::Rgb { r: 1, g: 2, b: 3 });
        updated.ui_style.popup_border.fg = Some(Color::Rgb { r: 4, g: 5, b: 6 });
        updated.ui_style.popup_title.fg = Some(Color::Rgb { r: 7, g: 8, b: 9 });
        updated.ui_style.muted.fg = Some(Color::Rgb {
            r: 10,
            g: 11,
            b: 12,
        });

        dialog.set_theme(&updated);

        assert_eq!(dialog.style, updated.ui_style.popup);
        assert_eq!(dialog.border_draw_style, updated.ui_style.popup_border);
        assert_eq!(dialog.title_style, updated.ui_style.popup_title);
        assert_eq!(dialog.footer_style, updated.ui_style.muted);
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
            rendered_cells(&buffer, 1, 6, 6),
            vec![' ', 'E', 's', 'c', ' ', '│']
        );
        assert_eq!(buffer.cells[2 * buffer.width].c, '└');
        assert_eq!(buffer.cells[2 * buffer.width + 11].c, '┘');
    }

    #[test]
    fn responsive_footer_preserves_complete_actions_inside_dialog_border() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(32, 5, &style);
        let dialog = Dialog::new(None, 0, 0, 28, 2, &style, BorderStyle::Single, &theme)
            .with_actions(vec![
                UiAction::new("select", "Enter", "Select"),
                UiAction::new("close", "Esc", "Close"),
            ]);

        dialog.draw(&mut buffer).unwrap();

        let footer = rendered_cells(&buffer, 2, 1, 28)
            .into_iter()
            .collect::<String>();
        assert!(footer.contains("Enter Select"));
        assert!(footer.contains("Esc Close"));
        assert_eq!(buffer.cells[3 * buffer.width].c, '└');
        assert_eq!(buffer.cells[3 * buffer.width + 29].c, '┘');
    }

    #[test]
    fn header_metadata_preserves_both_corners_and_the_left_aligned_title() {
        let style = Style::default();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(40, 6, &style);
        let mut dialog = Dialog::new(
            Some("Themes".to_string()),
            2,
            1,
            30,
            3,
            &style,
            BorderStyle::Single,
            &theme,
        )
        .with_left_aligned_title();
        dialog.set_header_status(Some("133".to_string()));

        dialog.draw(&mut buffer).unwrap();

        let header = rendered_cells(&buffer, 1, 2, 32)
            .into_iter()
            .collect::<String>();
        assert!(header.starts_with("┌ Themes "), "{header:?}");
        assert!(header.ends_with(" 133 ┐"), "{header:?}");
    }

    #[test]
    fn footer_actions_inherit_the_dialog_background() {
        use crate::color::Color;

        let surface_background = Color::Rgb { r: 9, g: 8, b: 7 };
        let footer_background = Color::Rgb { r: 1, g: 2, b: 3 };
        let style = Style {
            bg: Some(surface_background),
            ..Style::default()
        };
        let footer = Style {
            bg: Some(footer_background),
            ..Style::default()
        };
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(30, 5, &Style::default());
        let dialog = Dialog::new(None, 0, 0, 24, 2, &style, BorderStyle::Single, &theme)
            .with_footer_style(&footer)
            .with_actions(vec![UiAction::new("close", "Esc", "Close")]);

        dialog.draw(&mut buffer).unwrap();

        let row = &buffer.cells[2 * buffer.width + 1..2 * buffer.width + 25];
        assert!(row
            .iter()
            .all(|cell| cell.style.bg == Some(surface_background)));
        assert_eq!(buffer.cells[3 * buffer.width].c, '└');
        assert_eq!(buffer.cells[3 * buffer.width + 25].c, '┘');
    }
}
