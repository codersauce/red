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
    Point, Rect, RenderBuffer,
};

impl Editor {
    /// Renders the entire editor state to the terminal
    /// This is the main entry point for all rendering operations
    pub fn render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.update_gutter_width();
        let current_buffer = buffer.clone();

        // Render all windows
        let window_count = self.window_manager.windows().len();
        log!("Starting render of {} windows", window_count);
        for window_id in 0..window_count {
            self.render_window(buffer, window_id)?;
        }

        // Render window separators
        self.render_all_window_separators(buffer)?;

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

        Ok(())
    }

    /// Renders a single window
    fn render_window(&mut self, buffer: &mut RenderBuffer, window_id: usize) -> anyhow::Result<()> {
        use crate::log;

        // Clone the window data to avoid borrowing issues
        let window_data = {
            let windows = self.window_manager.windows();
            let window_count = windows.len();

            if let Some(window) = windows.get(window_id) {
                Some(((*window).clone(), window_count))
            } else {
                None
            }
        };

        if let Some((window, window_count)) = window_data {
            log!(
                "Rendering window {} at position ({}, {}) size {}x{}",
                window_id,
                window.position.x,
                window.position.y,
                window.size.0,
                window.size.1
            );

            // Render the gutter for this window
            self.render_gutter_in_window(buffer, &window)?;

            // Render the window content with proper boundaries
            self.render_main_content_in_window(buffer, &window)?;

            // Render overlays within window bounds
            self.render_overlays_in_window(buffer, &window)?;

            // Draw window separator if not the last window
            if window_id < window_count - 1 {
                // TODO: Draw separator
                self.render_window_separator(buffer, &window)?;
            }
        }

        Ok(())
    }

    /// Render window separator (placeholder for now)
    fn render_window_separator(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        // For now, just draw a simple vertical line on the right edge of the window
        let separator_style = Style {
            fg: Some(Color::Rgb {
                r: 100,
                g: 100,
                b: 100,
            }),
            bg: None,
            bold: false,
            italic: false,
        };

        let x = window.position.x + window.size.0;
        if x < self.size.0 as usize {
            for y in 0..window.size.1 {
                let term_y = window.position.y + y;
                buffer.set_char(x, term_y, '│', &separator_style, &self.theme);
            }
        }

        Ok(())
    }

    /// Render all window separators based on the split tree
    fn render_all_window_separators(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let separator_style = Style {
            fg: Some(Color::Rgb {
                r: 100,
                g: 100,
                b: 100,
            }),
            bg: None,
            bold: false,
            italic: false,
        };

        // Get terminal size for bounds checking
        let (term_width, term_height) = (self.size.0 as usize, self.size.1 as usize);

        // Collect all separator lines (vertical and horizontal)
        let mut vertical_lines: Vec<(usize, usize, usize)> = Vec::new(); // (x, y_start, y_end)
        let mut horizontal_lines: Vec<(usize, usize, usize)> = Vec::new(); // (y, x_start, x_end)

        // Get all windows to find separators
        let windows = self.window_manager.windows();
        if windows.len() <= 1 {
            return Ok(());
        }

        // Find all vertical separators
        for window in &windows {
            let right_edge = window.position.x + window.size.0;
            if right_edge < term_width - 1 {
                // Not at terminal edge
                // Check if any other window starts at this edge
                let has_neighbor = windows.iter().any(|w| w.position.x == right_edge + 1);
                if has_neighbor {
                    vertical_lines.push((
                        right_edge,
                        window.position.y,
                        window.position.y + window.size.1,
                    ));
                }
            }
        }

        // Find all horizontal separators
        for window in &windows {
            let bottom_edge = window.position.y + window.size.1;
            if bottom_edge < term_height - 2 {
                // Leave room for status/command line
                // Check if any other window starts at this edge
                let has_neighbor = windows.iter().any(|w| w.position.y == bottom_edge + 1);
                if has_neighbor {
                    horizontal_lines.push((
                        bottom_edge,
                        window.position.x,
                        window.position.x + window.size.0,
                    ));
                }
            }
        }

        // Draw vertical lines
        for (x, y_start, y_end) in &vertical_lines {
            for y in *y_start..*y_end {
                // Check if this position intersects with a horizontal line
                let is_intersection = horizontal_lines
                    .iter()
                    .any(|(hy, x_start, x_end)| *hy == y && *x >= *x_start && *x < *x_end);

                let char = if is_intersection {
                    // Determine the type of intersection
                    let has_top = vertical_lines
                        .iter()
                        .any(|(vx, vy_start, _)| *vx == *x && *vy_start < y);
                    let has_bottom = vertical_lines
                        .iter()
                        .any(|(vx, _, vy_end)| *vx == *x && *vy_end > y + 1);
                    let has_left = horizontal_lines
                        .iter()
                        .any(|(hy, hx_start, _)| *hy == y && *hx_start < *x);
                    let has_right = horizontal_lines
                        .iter()
                        .any(|(hy, _, hx_end)| *hy == y && *hx_end > *x + 1);

                    match (has_top, has_bottom, has_left, has_right) {
                        (true, true, true, true) => '┼',  // Four-way intersection
                        (true, true, true, false) => '┤', // T-junction right
                        (true, true, false, true) => '├', // T-junction left
                        (true, false, true, true) => '┴', // T-junction bottom
                        (false, true, true, true) => '┬', // T-junction top
                        _ => '│',                         // Default to vertical line
                    }
                } else {
                    '│'
                };

                buffer.set_char(*x, y, char, &separator_style, &self.theme);
            }
        }

        // Draw horizontal lines
        for (y, x_start, x_end) in &horizontal_lines {
            for x in *x_start..*x_end {
                // Skip if we already drew an intersection character here
                let is_intersection = vertical_lines
                    .iter()
                    .any(|(vx, y_start, y_end)| *vx == x && *y >= *y_start && *y < *y_end);

                if !is_intersection {
                    buffer.set_char(x, *y, '─', &separator_style, &self.theme);
                }
            }
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

    /// Renders the main editor content (text buffer) within a window
    fn render_main_content_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        // Get the buffer for this window
        let window_buffer = &self.buffers[window.buffer_index];
        // Use window's viewport instead of editor's global viewport
        let viewport_content = window_buffer.viewport(window.vtop, window.inner_height());

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

        // Start at window position, accounting for gutter
        let gutter_width = self.gutter_width();
        let mut x = gutter_width + 1; // Content starts after gutter within window
        let mut y = 0; // Window-local y coordinate

        // Render each character with appropriate styling
        for (pos, c) in viewport_content.chars().enumerate() {
            if c == '\n' {
                // Fill the rest of the line within the window
                let term_x = self.window_to_terminal_x(window, x);
                let term_y = self.window_to_terminal_y(window, y);

                // Only fill if within window bounds
                if x < window.inner_width() {
                    self.fill_line_in_window(
                        buffer,
                        term_x,
                        term_y,
                        window.inner_width() - x,
                        &theme_style,
                    );
                }

                x = gutter_width + 1;
                y += 1;
                if y >= window.inner_height() {
                    break;
                }
                continue;
            }

            let char_width = char_display_width(c);

            // Skip if character would overflow the window width
            if x + char_width > window.inner_width() {
                continue;
            }

            let style = determine_style_for_position(&style_info, pos)
                .unwrap_or_else(|| self.theme.style.clone());

            // Convert to terminal coordinates
            let term_x = self.window_to_terminal_x(window, x);
            let term_y = self.window_to_terminal_y(window, y);

            // For wide characters, we need to handle them specially
            if char_width > 1 {
                // Debug: Log emoji to verify it's being processed
                if c as u32 >= 0x1F300 && c as u32 <= 0x1F9FF {
                    log!(
                        "Setting emoji '{}' (U+{:04X}) at ({}, {})",
                        c,
                        c as u32,
                        term_x,
                        term_y
                    );
                }
                // Set the main character
                buffer.set_char(term_x, term_y, c, &style, &self.theme);
                // Fill the remaining columns with spaces to maintain alignment
                for i in 1..char_width {
                    if x + i < window.inner_width() {
                        buffer.set_char(term_x + i, term_y, ' ', &style, &self.theme);
                    }
                }
                x += char_width;
            } else if char_width == 0 {
                // Zero-width characters (like combining marks) - don't advance x
                // TODO: These should ideally be combined with the previous character
            } else {
                buffer.set_char(term_x, term_y, c, &style, &self.theme);
                x += 1;
            }
        }

        // Fill any remaining lines within the window
        while y < window.inner_height() {
            let term_y = self.window_to_terminal_y(window, y);
            let term_x = self.window_to_terminal_x(window, gutter_width + 1);
            self.fill_line_in_window(
                buffer,
                term_x,
                term_y,
                window.inner_width() - gutter_width - 1,
                &theme_style,
            );
            y += 1;
        }

        Ok(())
    }

    /// Renders the main editor content (text buffer) - legacy method for single window
    fn render_main_content(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Create a fake window that covers the entire editor area
        let window = crate::window::Window {
            buffer_index: self.current_buffer_index,
            position: Point::new(0, 0),
            size: (self.vwidth(), self.vheight()),
            vtop: self.vtop,
            vleft: self.vleft,
            cx: self.cx,
            cy: self.cy,
            active: true,
            vx: self.vx,
        };

        self.render_main_content_in_window(buffer, &window)
    }

    /// Fill a line with the given style within window bounds
    fn fill_line_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        style: &Style,
    ) {
        for i in 0..width {
            buffer.set_char(x + i, y, ' ', style, &self.theme);
        }
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

    /// Renders overlays like selections, search highlights, diagnostics within a window
    fn render_overlays_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        // Only render overlays if this window is active
        if !window.active {
            return Ok(());
        }

        // Render diagnostics within window bounds
        self.render_diagnostics_in_window(buffer, window)?;

        // Render current line highlight
        if !self.is_visual() && self.current_dialog.is_none() && window.active {
            if let Some(ref style) = self.theme.line_highlight_style {
                // Calculate window-relative cursor position
                let window_cy = window.cy;
                let term_y = self.window_to_terminal_y(window, window_cy);

                // Only highlight if the line is within the window
                if window_cy < window.inner_height() {
                    let start_x = window.position.x + self.gutter_width() + 1;
                    let end_x = window.position.x + window.inner_width() - 1;

                    buffer.set_bg_for_range(
                        Point::new(start_x, term_y),
                        Point::new(end_x, term_y),
                        &style.bg.unwrap(),
                        &self.theme,
                    );
                }
            }
        }

        // Render selection if in visual mode
        if self.is_visual() && window.active {
            self.update_selection();

            if let Some(selection) = self.selection {
                let points = self.selected_cells_in_window(&Some(selection), window);
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

    /// Renders diagnostic information within a specific window
    fn render_diagnostics_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        // Get the buffer for this window
        let window_buffer = &self.buffers[window.buffer_index];

        // Get current buffer URI
        let Some(uri) = window_buffer.uri()? else {
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

        // Render diagnostics for visible lines in this window
        for (line_num, diagnostics) in diagnostics_by_line {
            // Skip if line is not in window's viewport
            if line_num < window.vtop || line_num >= window.vtop + window.inner_height() {
                continue;
            }

            // Get the window-relative line number
            let window_y = line_num - window.vtop;

            // Get the line content to determine where to place the diagnostic
            let Some(line) = window_buffer.get(line_num) else {
                continue;
            };

            // Calculate diagnostic indicator position within window
            let gutter_width = self.gutter_width();
            let content_end = gutter_width + line.len();
            let indicator_x = content_end + 5; // Add some padding

            // Skip if diagnostic would be outside window
            if indicator_x >= window.inner_width() {
                continue;
            }

            // Available width for diagnostic message within window
            let available_width = window.inner_width() - indicator_x;
            if available_width < 3 {
                // Minimum space for indicator
                continue;
            }

            // Convert to terminal coordinates
            let term_x = self.window_to_terminal_x(window, indicator_x);
            let term_y = self.window_to_terminal_y(window, window_y);

            // Render diagnostic indicator and truncated message
            self.render_line_diagnostics(
                buffer,
                &diagnostics[..],
                term_y,
                term_x,
                available_width,
                &diagnostic_style,
            )?;
        }

        Ok(())
    }

    /// Convert selected cells to window-relative coordinates
    fn selected_cells_in_window(
        &self,
        selection: &Option<Rect>,
        window: &crate::window::Window,
    ) -> Vec<Point> {
        let Some(selection) = selection else {
            return vec![];
        };

        let mut cells = Vec::new();

        for y in selection.y0..=selection.y1 {
            // Skip lines outside window viewport
            if y < window.vtop || y >= window.vtop + window.inner_height() {
                continue;
            }

            let window_y = y - window.vtop;

            let (start_x, end_x) = match self.mode {
                Mode::Visual => {
                    if y == selection.y0 && y == selection.y1 {
                        (selection.x0, selection.x1)
                    } else if y == selection.y0 {
                        (selection.x0, self.length_for_line(y))
                    } else if y == selection.y1 {
                        (0, selection.x1)
                    } else {
                        (0, self.length_for_line(y))
                    }
                }
                Mode::VisualLine => (0, self.length_for_line(y).saturating_sub(2)),
                Mode::VisualBlock => (selection.x0, selection.x1),
                _ => unreachable!(),
            };

            // Convert to terminal coordinates
            for x in start_x..=end_x {
                // Skip if x is outside window bounds
                if x + self.gutter_width() + 1 >= window.inner_width() {
                    continue;
                }

                let term_x = self.window_to_terminal_x(window, x + self.gutter_width() + 1);
                let term_y = self.window_to_terminal_y(window, window_y);
                cells.push(Point::new(term_x, term_y));
            }
        }

        cells
    }

    /// Renders UI chrome (gutter, statusline, command line)
    fn render_ui_chrome(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Don't render global gutter - each window renders its own gutter
        // self.render_gutter(buffer)?;

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

        // Get information from the active window
        let active_window = self.window_manager.active_window();
        let (file, pos, window_indicator) = if let Some(window) = active_window {
            let window_buffer = &self.buffers[window.buffer_index];
            let dirty = if window_buffer.is_dirty() {
                " [+] "
            } else {
                ""
            };
            let file = format!(" {}{}", window_buffer.name(), dirty);
            let pos = format!(" {}:{} ", window.vtop + window.cy + 1, window.cx + 1);

            // Add window indicator if there are multiple windows
            let window_count = self.window_manager.windows().len();
            let window_indicator = if window_count > 1 {
                format!(
                    " [{}/{}]",
                    self.window_manager.active_window_id() + 1,
                    window_count
                )
            } else {
                String::new()
            };

            (file, pos, window_indicator)
        } else {
            // Fallback to global state if no active window
            let dirty = if self.current_buffer().is_dirty() {
                " [+] "
            } else {
                ""
            };
            let file = format!(" {}{}", self.current_buffer().name(), dirty);
            let pos = format!(" {}:{} ", self.vtop + self.cy + 1, self.cx + 1);
            (file, pos, String::new())
        };

        let file_width =
            self.size.0 - mode.len() as u16 - pos.len() as u16 - window_indicator.len() as u16 - 2;
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
            &format!("{}{}", pos, window_indicator),
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

    /// Renders the gutter with line numbers for a specific window
    fn render_gutter_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        use crate::log;
        let width = self.gutter_width();
        let gutter_style = self.theme.gutter_style.fallback_bg(&self.theme.style);

        log!(
            "render_gutter_in_window: window at ({}, {}) size {}x{}",
            window.position.x,
            window.position.y,
            window.size.0,
            window.size.1
        );

        // Get the buffer for this window
        let window_buffer = &self.buffers[window.buffer_index];

        for y in 0..window.inner_height() {
            let line_number = y + 1 + window.vtop;
            let text = if line_number <= window_buffer.len() {
                format!("{:>width$} ", line_number)
            } else {
                " ".repeat(width + 1)
            };

            let term_x = window.position.x;
            let term_y = window.position.y + y;
            log!(
                "  Drawing gutter at ({}, {}): '{}'",
                term_x,
                term_y,
                text.trim()
            );
            buffer.set_text(term_x, term_y, &text, &gutter_style);
        }

        Ok(())
    }

    /// Renders the gutter with line numbers (legacy for single window)
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
            // Get the active window to calculate cursor position
            if let Some(window) = self.window_manager.active_window() {
                // Use window's cursor position
                let window_cy = window.cy;
                let window_cx = window.cx;

                // Calculate the actual display column for the cursor
                let display_col = if let Some(line) = self.viewport_line(window.vtop + window_cy) {
                    let line = line.trim_end_matches('\n');
                    crate::unicode_utils::char_to_column(line, window_cx)
                } else {
                    window_cx
                };

                // Convert to terminal coordinates based on active window
                let term_x = window.position.x + self.gutter_width() + 1 + display_col;
                let term_y =
                    window.position.y + window_cy.min(window.inner_height().saturating_sub(1));
                Some((term_x, term_y))
            } else {
                // Fallback to old behavior if no active window
                let display_col = if let Some(line) = self.viewport_line(self.cy) {
                    let line = line.trim_end_matches('\n');
                    crate::unicode_utils::char_to_column(line, self.cx)
                } else {
                    self.cx
                };
                Some(((self.vx + display_col), self.cy))
            }
        };

        if let Some((x, y)) = cursor_pos {
            self.stdout.queue(cursor::MoveTo(x as u16, y as u16))?;
        } else {
            self.stdout.queue(cursor::Hide)?;
        }

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
