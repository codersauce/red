use std::{collections::HashMap, io::Write as _};

use crossterm::{
    cursor::{self, MoveTo},
    style, QueueableCommand as _,
};

use crate::{
    color::{blend_color, Color},
    editor::RenderCommand,
    log,
    lsp::Diagnostic,
    theme::Style,
    unicode_utils::char_display_width,
};

use super::{
    adjust_color_brightness, determine_style_for_position, render_buffer::Change, Editor, Mode,
    Point, RenderBuffer,
};

impl Editor {
    /// Renders the entire editor state to the terminal
    /// This is the main entry point for all rendering operations
    pub fn render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.update_gutter_width();
        let current_buffer = buffer.clone();

        // If we have multiple windows, render each one
        let window_count = self.window_manager.windows().len();
        if window_count > 1 {
            // Save current state
            self.sync_to_window();

            // Render each window
            for window_id in 0..window_count {
                self.window_manager.set_active(window_id);
                self.sync_with_window();
                self.render_window(buffer, window_id)?;
            }

            // Restore active window
            let active_id = self.window_manager.active_window_id();
            self.window_manager.set_active(active_id);
            self.sync_with_window();
        } else {
            // Single window - render normally
            self.render_main_content(buffer)?;
            self.render_overlays(buffer)?;
        }

        // Render global UI elements
        self.render_ui_chrome(buffer)?;
        self.render_dialog(buffer)?;

        // Render all plugins
        self.render_from_plugins(buffer)?;

        // Update overlay positions and render them
        self.update_and_render_overlays(buffer)?;

        // Flush changes to terminal
        let diff = buffer.diff(&current_buffer);
        self.render_diff(diff)?;
        // self.flush_to_terminal(buffer)?;

        Ok(())
    }

    /// Renders a single window
    fn render_window(&mut self, buffer: &mut RenderBuffer, window_id: usize) -> anyhow::Result<()> {
        let window_count = self.window_manager.windows().len();
        let is_last_window = window_id >= window_count - 1;

        // Render the window content
        self.render_main_content(buffer)?;
        self.render_overlays(buffer)?;

        // Draw window separator if not the last window
        if !is_last_window {
            // TODO: Draw separator
        }

        Ok(())
    }

    fn render_from_plugins(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        while let Some(cmd) = self.render_commands.pop_front() {
            match cmd {
                RenderCommand::BufferText { x, y, text, style } => {
                    buffer.set_text(x, y, &text, &style);
                }
            }
        }

        Ok(())
    }

    fn update_and_render_overlays(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Get current cursor position for avoid_cursor alignment
        let cursor_pos = if self.current_dialog.is_none() {
            Some(Point::new(self.cx + self.gutter_width() + 1, self.cy))
        } else {
            None
        };

        // Update positions for all overlays
        self.overlay_manager.update_positions(
            self.size.0 as usize,
            self.size.1 as usize,
            cursor_pos,
        );

        // Render all dirty overlays
        self.overlay_manager.render_all(buffer);

        Ok(())
    }

    /// Renders the main editor content (text buffer)
    fn render_main_content(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let viewport_content = self.current_buffer().viewport(self.vtop, self.vheight());

        // Debug: Check if viewport contains emoji
        if viewport_content
            .chars()
            .any(|c| c as u32 >= 0x1F300 && c as u32 <= 0x1F9FF)
        {
            log!("render_main_content: Viewport contains emoji");
            // Log each character to see what's happening
            for (i, c) in viewport_content.chars().enumerate().take(50) {
                if c as u32 >= 0x1F300 && c as u32 <= 0x1F9FF {
                    log!("  Char {}: '{}' (U+{:04X})", i, c, c as u32);
                }
            }
        }

        let style_info = self.highlight(&viewport_content)?;
        let theme_style = self.theme.style.clone();

        let mut x = self.gutter_width() + 1; // Account for gutter
        let mut y = 0;

        // Render each character with appropriate styling
        for (pos, c) in viewport_content.chars().enumerate() {
            if c == '\n' {
                self.fill_line(buffer, x, y, &theme_style);
                x = self.gutter_width() + 1;
                y += 1;
                if y >= self.vheight() {
                    break;
                }
                continue;
            }

            let char_width = char_display_width(c);

            // Skip if character would overflow the viewport width
            if x + char_width > self.vwidth() {
                continue;
            }

            let style = determine_style_for_position(&style_info, pos)
                .unwrap_or_else(|| self.theme.style.clone());

            // For wide characters, we need to handle them specially
            if char_width > 1 {
                // Debug: Log emoji to verify it's being processed
                if c as u32 >= 0x1F300 && c as u32 <= 0x1F9FF {
                    log!(
                        "Setting emoji '{}' (U+{:04X}) at ({}, {})",
                        c,
                        c as u32,
                        x,
                        y
                    );
                }
                // Set the main character
                buffer.set_char(x, y, c, &style, &self.theme);
                // Fill the remaining columns with spaces to maintain alignment
                for i in 1..char_width {
                    if x + i < self.vwidth() {
                        buffer.set_char(x + i, y, ' ', &style, &self.theme);
                    }
                }
                x += char_width;
            } else if char_width == 0 {
                // Zero-width characters (like combining marks) - don't advance x
                // TODO: These should ideally be combined with the previous character
            } else {
                buffer.set_char(x, y, c, &style, &self.theme);
                x += 1;
            }
        }

        // Fill any remaining lines
        while y < self.vheight() {
            self.fill_line(buffer, self.gutter_width() + 1, y, &theme_style);
            y += 1;
        }

        Ok(())
    }

    /// Renders overlays like selections, search highlights, diagnostics
    fn render_overlays(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Render diagnostics
        self.render_diagnostics(buffer)?;

        // Render current line highlight
        if !self.is_visual() && self.current_dialog.is_none() {
            if let Some(ref style) = self.theme.line_highlight_style {
                buffer.set_bg_for_range(
                    Point::new(self.gutter_width() + 1, self.cy),
                    Point::new(buffer.width - 1, self.cy),
                    &style.bg.unwrap(),
                    &self.theme,
                );
            }
        }

        // Render selection if in visual mode
        if self.is_visual() {
            self.update_selection();

            if let Some(selection) = self.selection {
                let points = self.selected_cells(&Some(selection));
                buffer.set_bg_for_points(points, &self.theme.get_selection_bg(), &self.theme);
            }
        }

        Ok(())
    }

    /// Renders diagnostic information in the editor viewport
    fn render_diagnostics(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Get current buffer URI
        let Some(uri) = self.buffer_uri()? else {
            return Ok(());
        };

        // Get diagnostics for current buffer
        let Some(diagnostics) = self.diagnostics.get(&uri) else {
            return Ok(());
        };

        // Style for diagnostic messages
        let diagnostic_style = self.theme.error_style.clone().unwrap_or(Style {
            fg: adjust_color_brightness(self.theme.style.fg, -20), // Slightly dimmer than normal text
            bg: adjust_color_brightness(self.theme.style.bg, 10),  // Slightly brighter background
            italic: true,
            ..Default::default()
        });

        let diagnostics_by_line: HashMap<_, Vec<_>> =
            diagnostics.iter().fold(HashMap::new(), |mut acc, d| {
                acc.entry(d.range.start.line).or_default().push(d);
                acc
            });

        // Render diagnostics for visible lines
        for (line_num, diagnostics) in diagnostics_by_line {
            // Skip if line is not in viewport
            if !self.is_within_viewport(line_num) {
                continue;
            }

            // Get the viewport line number
            let viewport_y = line_num - self.vtop;

            // Get the line content to determine where to place the diagnostic
            let Some(line) = self.current_buffer().get(line_num) else {
                continue;
            };

            // Calculate diagnostic indicator position
            // Place it after the line content with some padding
            let gutter_width = self.gutter_width();
            let content_end = gutter_width + line.len();
            let indicator_x = content_end + 5; // Add some padding

            // Skip if diagnostic would be outside visible area
            if indicator_x >= self.vwidth() {
                continue;
            }

            // Available width for diagnostic message
            let available_width = self.vwidth() - indicator_x;
            if available_width < 3 {
                // Minimum space for indicator
                continue;
            }

            // Render diagnostic indicator and truncated message
            self.render_line_diagnostics(
                buffer,
                &diagnostics[..],
                viewport_y,
                indicator_x,
                available_width,
                &diagnostic_style,
            )?;
        }

        Ok(())
    }

    /// Renders a single diagnostic entry
    fn render_line_diagnostics(
        &self,
        buffer: &mut RenderBuffer,
        diagnostics: &[&Diagnostic],
        y: usize,
        x: usize,
        available_width: usize,
        style: &Style,
    ) -> anyhow::Result<()> {
        let indicator = "■".repeat(diagnostics.len());
        let diagnostic = diagnostics[0];

        // Write the indicator
        buffer.set_text(x, y, &format!("{indicator} "), style);

        // Process the message - remove newlines and truncate if needed
        let message = diagnostic.message.replace('\n', " ");
        let message = message.trim();

        // Calculate available space for message
        let max_msg_length = available_width.saturating_sub(indicator.chars().count() + 1);
        if max_msg_length < 3 {
            // Not enough space for message
            return Ok(());
        }

        // Truncate message if needed and add ellipsis
        let display_message = if message.chars().count() > max_msg_length {
            format!("{}…", &message[..max_msg_length - 1])
        } else {
            message.to_string()
        };

        // Write the message with a space after the indicator
        buffer.set_text(
            x + indicator.chars().count() + 1,
            y,
            &display_message,
            style,
        );
        // buffer.set_text(x + 1 + 1, y, &display_message, style);

        Ok(())
    }

    /// Renders UI chrome (gutter, statusline, command line)
    fn render_ui_chrome(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Render gutter
        self.render_gutter(buffer)?;

        // Render status line
        self.draw_statusline(buffer);

        // Render command line if needed
        self.draw_commandline(buffer);

        Ok(())
    }

    fn render_dialog(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if let Some(current_dialog) = &self.current_dialog {
            current_dialog.draw(buffer)?;
        }

        Ok(())
    }

    pub fn render_diff(&mut self, change_set: Vec<Change<'_>>) -> anyhow::Result<()> {
        self.stdout.queue(cursor::Hide)?;

        // Debug: Log number of changes and emoji changes
        let emoji_changes = change_set
            .iter()
            .filter(|c| c.cell.c as u32 >= 0x1F300 && c.cell.c as u32 <= 0x1F9FF)
            .count();
        if emoji_changes > 0 {
            log!(
                "render_diff: Processing {} changes, {} are emoji",
                change_set.len(),
                emoji_changes
            );
        }

        // Sort changes by position to ensure we render left-to-right, top-to-bottom
        let mut sorted_changes = change_set;
        sorted_changes.sort_by_key(|change| (change.y, change.x));

        let mut skip_next = false;
        for (i, change) in sorted_changes.iter().enumerate() {
            // Skip if this was a padding space after an emoji
            if skip_next {
                skip_next = false;
                continue;
            }

            let x = change.x;
            let y = change.y;
            let cell = change.cell;

            // Check if this is an emoji followed by a space (padding)
            let is_emoji = cell.c as u32 >= 0x1F300 && cell.c as u32 <= 0x1F9FF;
            if is_emoji {
                // Check if next change is a space at x+1
                if i + 1 < sorted_changes.len() {
                    let next = &sorted_changes[i + 1];
                    if next.y == y && next.x == x + 1 && next.cell.c == ' ' {
                        skip_next = true;
                    }
                }
            }

            self.stdout.queue(MoveTo(x as u16, y as u16))?;
            if let Some(bg) = cell.style.bg {
                let bg = blend_color(
                    bg,
                    self.theme
                        .style
                        .bg
                        .unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 }),
                );
                self.stdout.queue(style::SetBackgroundColor(bg.into()))?;
            } else {
                self.stdout.queue(style::SetBackgroundColor(
                    self.theme.style.bg.unwrap().into(),
                ))?;
            }
            if let Some(fg) = cell.style.fg {
                let fg = blend_color(
                    fg,
                    self.theme
                        .style
                        .bg
                        .unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 }),
                );
                self.stdout.queue(style::SetForegroundColor(fg.into()))?;
            } else {
                self.stdout.queue(style::SetForegroundColor(
                    self.theme.style.fg.unwrap().into(),
                ))?;
            }
            if cell.style.italic {
                self.stdout
                    .queue(style::SetAttribute(style::Attribute::Italic))?;
            } else {
                self.stdout
                    .queue(style::SetAttribute(style::Attribute::NoItalic))?;
            }
            // Debug: Log what we're about to print
            if cell.c as u32 >= 0x1F300 && cell.c as u32 <= 0x1F9FF {
                log!(
                    "render_diff: About to print emoji '{}' (U+{:04X}) at ({}, {})",
                    cell.c,
                    cell.c as u32,
                    x,
                    y
                );
            }
            self.stdout.queue(style::Print(cell.c))?;
        }

        self.stdout.queue(cursor::Show)?;

        self.set_cursor_style()?;
        self.draw_cursor()?;
        self.stdout.flush()?;

        Ok(())
    }

    pub fn draw_statusline(&mut self, buffer: &mut RenderBuffer) {
        let mode = format_mode_name(&self.mode);
        let mode = format!(" {mode} ");
        let dirty = if self.current_buffer().is_dirty() {
            " [+] "
        } else {
            ""
        };
        let file = format!(" {}{}", self.current_buffer().name(), dirty);
        let pos = format!(" {}:{} ", self.vtop + self.cy + 1, self.cx + 1);

        let file_width = self.size.0 - mode.len() as u16 - pos.len() as u16 - 2;
        let y = self.size.1 as usize - 2;

        let transition_style = Style {
            fg: self.theme.statusline_style.outer_style.bg,
            bg: self.theme.statusline_style.inner_style.bg,
            ..Default::default()
        };

        buffer.set_text(0, y, &mode, &self.theme.statusline_style.outer_style);

        buffer.set_text(
            mode.len(),
            y,
            &self.theme.statusline_style.outer_chars[1].to_string(),
            &transition_style,
        );

        buffer.set_text(
            mode.len() + 1,
            y,
            &format!("{:<width$}", file, width = file_width as usize),
            &self.theme.statusline_style.inner_style,
        );

        buffer.set_text(
            mode.len() + 1 + file_width as usize,
            y,
            &self.theme.statusline_style.outer_chars[2].to_string(),
            &transition_style,
        );

        buffer.set_text(
            mode.len() + 2 + file_width as usize,
            y,
            &pos,
            &self.theme.statusline_style.outer_style,
        );
    }

    pub fn draw_commandline(&mut self, buffer: &mut RenderBuffer) {
        let style = &self.theme.style;
        let y = self.size.1 as usize - 1;

        if !self.has_term() {
            let wc = if let Some(ref waiting_command) = self.waiting_command {
                waiting_command.clone()
            } else if let Some(ref repeater) = self.repeater {
                format!("{}", repeater)
            } else {
                String::new()
            };
            let wc = format!("{:<width$}", wc, width = 10);

            if let Some(ref last_error) = self.last_error {
                let error = format!("{:width$}", last_error, width = self.size.0 as usize);
                buffer.set_text(0, self.size.1 as usize - 1, &error, style);
            } else {
                let clear_line = " ".repeat(self.size.0 as usize - 10);
                buffer.set_text(0, y, &clear_line, style);
            }

            buffer.set_text(self.size.0 as usize - 10, y, &wc, style);

            return;
        }

        let text = if self.is_command() {
            &self.command
        } else {
            &self.search_term
        };
        let prefix = if self.is_command() { ":" } else { "/" };
        let cmdline = format!(
            "{}{:width$}",
            prefix,
            text,
            width = self.size.0 as usize - self.command.len() - 1
        );
        buffer.set_text(0, self.size.1 as usize - 1, &cmdline, style);
    }

    /// Renders the gutter with line numbers
    fn render_gutter(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let width = self.gutter_width();

        let gutter_style = self.theme.gutter_style.fallback_bg(&self.theme.style);

        for y in 0..self.vheight() {
            let line_number = y + 1 + self.vtop;
            let text = if line_number <= self.current_buffer().len() {
                format!("{:>width$} ", line_number)
            } else {
                " ".repeat(width + 1)
            };
            buffer.set_text(0, y, &text, &gutter_style);
        }

        Ok(())
    }

    pub fn draw_cursor(&mut self) -> anyhow::Result<()> {
        self.fix_cursor_pos();
        self.set_cursor_style()?;
        self.check_bounds();

        // TODO: refactor this out to allow for dynamic setting of the cursor "target",
        // so we could transition from the editor to dialogs, to searches, etc.
        let cursor_pos = if let Some(current_dialog) = &self.current_dialog {
            current_dialog.cursor_position()
        } else if self.has_term() {
            Some((self.term().len() + 1, (self.size.1 - 1) as usize))
        } else {
            // Calculate the actual display column for the cursor
            let display_col = if let Some(line) = self.viewport_line(self.cy) {
                let line = line.trim_end_matches('\n');
                crate::unicode_utils::char_to_column(line, self.cx)
            } else {
                self.cx
            };
            Some(((self.vx + display_col), self.cy))
        };

        if let Some((x, y)) = cursor_pos {
            self.stdout.queue(cursor::MoveTo(x as u16, y as u16))?;
        } else {
            self.stdout.queue(cursor::Hide)?;
        }
        // self.draw_statusline(buffer);

        Ok(())
    }

    fn set_cursor_style(&mut self) -> anyhow::Result<()> {
        self.stdout.queue(match self.waiting_key_action {
            Some(_) => cursor::SetCursorStyle::SteadyUnderScore,
            _ => match self.mode {
                Mode::Normal => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Command => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Insert => cursor::SetCursorStyle::SteadyBar,
                Mode::Search => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                    cursor::SetCursorStyle::DefaultUserShape
                }
            },
        })?;

        Ok(())
    }

    fn update_gutter_width(&mut self) {
        self.vx = self.gutter_width() + 1;
    }
}

fn format_mode_name(mode: &Mode) -> String {
    match mode {
        Mode::Normal => "NORMAL".to_string(),
        Mode::Insert => "INSERT".to_string(),
        Mode::Command => "COMMAND".to_string(),
        Mode::Search => "SEARCH".to_string(),
        Mode::Visual => "VISUAL".to_string(),
        Mode::VisualLine => "V-LINE".to_string(),
        Mode::VisualBlock => "V-BLOCK".to_string(),
    }
}
