use std::{collections::HashMap, io::Write as _};

use crossterm::{
    cursor::{self, MoveTo},
    style, QueueableCommand as _,
};

use crate::{
    color::{blend_color, Color},
    editor::RenderCommand,
    lsp::Diagnostic,
    theme::Style,
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
        
        // Check conditions that would require a full redraw
        if self.full_redraw_needed || 
           self.has_scrolled() || 
           self.has_gutter_changed() || 
           self.current_dialog.is_some() ||
           !self.render_commands.is_empty() {
            // Force a full redraw when necessary
            self.mark_full_redraw();
        }
        
        // Create a clone for diffing
        let current_buffer = buffer.clone();

        // Determine if we need incremental or full rendering
        if self.full_redraw_needed {
            // Full rendering
            self.render_main_content(buffer)?;
            self.render_overlays(buffer)?;
            self.render_ui_chrome(buffer)?;
            self.render_dialog(buffer)?;
        } else {
            // Incremental rendering
            self.render_dirty_lines(buffer)?;
            self.render_dirty_overlays(buffer)?;
            
            // Always update UI chrome as it's relatively cheap and visible changes happen often
            self.render_ui_chrome(buffer)?;
            
            // Handle dialog if present
            if let Some(_) = &self.current_dialog {
                self.render_dialog(buffer)?;
            }
        }

        // Render plugin content regardless of render mode
        self.render_from_plugins(buffer)?;

        // Flush changes to terminal
        let diff = buffer.diff(&current_buffer);
        self.render_diff(diff)?;
        
        // Reset dirty tracking
        self.clear_dirty_state();

        Ok(())
    }

    fn render_from_plugins(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // If there are no commands, don't bother processing
        if self.render_commands.is_empty() {
            return Ok(());
        }

        // Create a map to track the last command for each line position
        // This ensures we only render the final state, eliminating flickering
        let mut position_commands: std::collections::HashMap<(usize, usize), RenderCommand> = std::collections::HashMap::new();

        // Process all commands and keep only the last one for each position
        while let Some(cmd) = self.render_commands.pop_front() {
            match &cmd {
                RenderCommand::BufferText { x, y, .. } => {
                    position_commands.insert((*x, *y), cmd);
                }
            }
        }

        // Apply only the final commands for each position
        for cmd in position_commands.values() {
            match cmd {
                RenderCommand::BufferText { x, y, text, style } => {
                    buffer.set_text(*x, *y, text, style);
                }
            }
        }

        Ok(())
    }

    /// Renders only the dirty lines (for incremental rendering)
    fn render_dirty_lines(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let theme_style = self.theme.style.clone();
        
        // Create a copy of dirty lines to avoid borrowing issues
        let dirty_lines: Vec<usize> = self.dirty_lines.iter().copied().collect();
        
        // Process each dirty line that's within the viewport
        for line_num in dirty_lines {
            let viewport_y = line_num.saturating_sub(self.vtop);
            
            // Skip if line is not in viewport
            if viewport_y >= self.vheight() {
                continue;
            }
            
            // Get line content
            let line_content = self.current_buffer().get(line_num).unwrap_or_default();
            
            // Get highlighting for this specific line
            let style_info = self.get_line_highlighting(line_num)?;
            
            // Start at gutter width
            let mut x = self.gutter_width() + 1;
            
            // Clear the line with default styling
            self.fill_line(buffer, x, viewport_y, &theme_style);
            
            // Render each character with proper styling
            for (pos, c) in line_content.chars().enumerate() {
                if c == '\n' || x >= self.vwidth() {
                    break;
                }
                
                let style = determine_style_for_position(&style_info, pos)
                    .unwrap_or_else(|| self.theme.style.clone());
                
                buffer.set_char(x, viewport_y, c, &style, &self.theme);
                x += 1;
            }
        }
        
        Ok(())
    }
    
    /// Renders dirty diagnostics and other overlays
    fn render_dirty_overlays(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Handle current line highlight if it moved
        if let Some(prev_y) = self.prev_highlight_y {
            if prev_y != self.cy && self.is_within_viewport(prev_y + self.vtop) {
                // Clear previous highlight by re-rendering the line
                let prev_line = prev_y + self.vtop;
                if self.dirty_lines.insert(prev_line) {
                    self.render_dirty_lines(buffer)?;
                }
            }
        }
        
        // Add current line highlight if we're not in visual mode
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
        
        // Update selection if in visual mode
        if self.is_visual() {
            self.update_selection();
            
            if let Some(selection) = self.selection {
                let points = self.selected_cells(&Some(selection));
                buffer.set_bg_for_points(points, &self.theme.get_selection_bg(), &self.theme);
            }
        }
        
        // Render diagnostics for dirty diagnostic lines
        self.render_dirty_diagnostics(buffer)?;
        
        // Store current line for next render
        self.prev_highlight_y = Some(self.cy);
        
        Ok(())
    }
    
    /// Renders diagnostics only for lines that have changed
    fn render_dirty_diagnostics(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
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
            fg: adjust_color_brightness(self.theme.style.fg, -20),
            bg: adjust_color_brightness(self.theme.style.bg, 10),
            italic: true,
            ..Default::default()
        });
        
        // Build a map of diagnostics by line
        let mut diagnostics_by_line: HashMap<usize, Vec<&Diagnostic>> = HashMap::new();
        for diagnostic in diagnostics {
            diagnostics_by_line
                .entry(diagnostic.range.start.line)
                .or_default()
                .push(diagnostic);
        }
        
        // Create a copy of dirty diagnostic lines to avoid borrowing issues
        let dirty_lines: Vec<usize> = self.dirty_diagnostic_lines.iter().copied().collect();
        
        // Process only lines that need to be updated
        for line_num in dirty_lines {
            // Skip if line is not in viewport
            if !self.is_within_viewport(line_num) {
                continue;
            }
            
            // Get the viewport line number
            let viewport_y = line_num - self.vtop;
            
            // Get any diagnostics for this line
            let Some(line_diagnostics) = diagnostics_by_line.get(&line_num) else {
                continue;
            };
            
            // Get the line content to determine where to place the diagnostic
            let Some(line) = self.current_buffer().get(line_num) else {
                continue;
            };
            
            // Calculate diagnostic indicator position - place it after line content with padding
            let gutter_width = self.gutter_width();
            let content_end = gutter_width + line.len();
            let indicator_x = content_end + 5;
            
            // Skip if diagnostic would be outside visible area
            if indicator_x >= self.vwidth() {
                continue;
            }
            
            // Available width for diagnostic message
            let available_width = self.vwidth() - indicator_x;
            if available_width < 3 {
                continue;
            }
            
            // Render this specific diagnostic
            self.render_line_diagnostics(
                buffer,
                line_diagnostics.as_slice(), 
                viewport_y,
                indicator_x,
                available_width,
                &diagnostic_style,
            )?;
        }
        
        Ok(())
    }
    
    /// Renders the main editor content (text buffer)
    fn render_main_content(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let viewport_content = self.current_buffer().viewport(self.vtop, self.vheight());
        let style_info = self.highlight(&viewport_content)?;
        let theme_style = self.theme.style.clone();

        let mut x = self.gutter_width() + 1; // Account for gutter
        let mut y = 0;

        // Render each character with appropriate styling
        for (pos, c) in viewport_content.chars().enumerate() {
            if c == '\n' {
                // || x >= self.vwidth() {
                self.fill_line(buffer, x, y, &theme_style);
                x = self.gutter_width() + 1;
                y += 1;
                if y >= self.vheight() {
                    break;
                }
                if c == '\n' {
                    continue;
                }
            }

            if x > self.vwidth() {
                continue;
            }

            let style = determine_style_for_position(&style_info, pos)
                .unwrap_or_else(|| self.theme.style.clone());

            buffer.set_char(x, y, c, &style, &self.theme);
            x += 1;
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
        if change_set.is_empty() {
            // No changes, just update cursor
            self.set_cursor_style()?;
            self.draw_cursor()?;
            return Ok(());
        }
        
        self.stdout.queue(cursor::Hide)?;
        
        // Organize changes by line for more efficient rendering
        let mut changes_by_line: HashMap<usize, Vec<(usize, &Change)>> = HashMap::new();
        for (i, change) in change_set.iter().enumerate() {
            changes_by_line.entry(change.y).or_default().push((i, change));
        }
        
        // Sort each line's changes by x position for efficient rendering
        for changes in changes_by_line.values_mut() {
            changes.sort_by_key(|(_, change)| change.x);
        }
        
        // Sort lines by y position
        let mut sorted_lines: Vec<usize> = changes_by_line.keys().cloned().collect();
        sorted_lines.sort();
        
        // Current style state
        let mut current_fg = None;
        let mut current_bg = None;
        let mut current_italic = false;
        
        // Process each line
        for line_y in sorted_lines {
            let line_changes = &changes_by_line[&line_y];
            let mut last_x = 0;
            
            // Process each change on this line
            for &(_, change) in line_changes {
                let x = change.x;
                let y = change.y;
                let cell = change.cell;
                
                // Move cursor only when needed
                if y != line_y || x != last_x {
                    self.stdout.queue(MoveTo(x as u16, y as u16))?;
                }
                
                // Update background color only when it changes
                let bg_color = cell.style.bg.map(|bg| {
                    blend_color(
                        bg,
                        self.theme.style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 }),
                    )
                }).unwrap_or_else(|| self.theme.style.bg.unwrap());
                
                if current_bg != Some(bg_color) {
                    self.stdout.queue(style::SetBackgroundColor(bg_color.into()))?;
                    current_bg = Some(bg_color);
                }
                
                // Update foreground color only when it changes
                let fg_color = cell.style.fg.map(|fg| {
                    blend_color(
                        fg,
                        self.theme.style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 }),
                    )
                }).unwrap_or_else(|| self.theme.style.fg.unwrap());
                
                if current_fg != Some(fg_color) {
                    self.stdout.queue(style::SetForegroundColor(fg_color.into()))?;
                    current_fg = Some(fg_color);
                }
                
                // Update italic attribute only when it changes
                if current_italic != cell.style.italic {
                    if cell.style.italic {
                        self.stdout.queue(style::SetAttribute(style::Attribute::Italic))?;
                    } else {
                        self.stdout.queue(style::SetAttribute(style::Attribute::NoItalic))?;
                    }
                    current_italic = cell.style.italic;
                }
                
                // Print the character
                self.stdout.queue(style::Print(cell.c))?;
                
                // Update last position
                last_x = x + 1;
            }
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
            Some(((self.vx + self.cx), self.cy))
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
