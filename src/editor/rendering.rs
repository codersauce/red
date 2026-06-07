use std::{
    collections::HashMap,
    io::{self, Write as _},
};

use crossterm::{
    cursor::{self, MoveTo},
    style, terminal, QueueableCommand as _,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::{
    color::{blend_color, Color},
    config::CursorShape,
    editor::RenderCommand,
    lsp::Diagnostic,
    theme::Style,
    unicode_utils::{
        char_prefix, display_width, fit_display_width, grapheme_to_column, truncate_display_width,
    },
};

use super::{
    adjust_color_brightness, render_buffer::Change, Editor, HighlightCacheKey, Mode, Point, Rect,
    RenderBuffer, StyleCursor,
};

fn diagnostic_row(diagnostics: &[&Diagnostic], available_width: usize) -> Option<String> {
    let diagnostic = diagnostics.first()?;
    if available_width == 0 {
        return None;
    }

    let indicator = "■".repeat(diagnostics.len());
    let message = diagnostic.message.replace('\n', " ");
    let message = message.trim();
    let row = if message.is_empty() {
        indicator
    } else {
        format!("{indicator} {message}")
    };

    if display_width(&row) <= available_width {
        return Some(fit_display_width(&row, available_width));
    }

    if available_width == 1 {
        return Some(truncate_display_width(&row, available_width));
    }

    let mut row = truncate_display_width(&row, available_width - 1);
    row.push('…');
    Some(fit_display_width(&row, available_width))
}

fn queue_cell_attributes(output: &mut impl io::Write, cell_style: &Style) -> anyhow::Result<()> {
    if cell_style.bold {
        output.queue(style::SetAttribute(style::Attribute::Bold))?;
    } else {
        output.queue(style::SetAttribute(style::Attribute::NormalIntensity))?;
    }

    if cell_style.italic {
        output.queue(style::SetAttribute(style::Attribute::Italic))?;
    } else {
        output.queue(style::SetAttribute(style::Attribute::NoItalic))?;
    }

    Ok(())
}

fn cursor_style_for_shape(shape: CursorShape) -> cursor::SetCursorStyle {
    match shape {
        CursorShape::Default => cursor::SetCursorStyle::DefaultUserShape,
        CursorShape::BlinkingBlock => cursor::SetCursorStyle::BlinkingBlock,
        CursorShape::SteadyBlock => cursor::SetCursorStyle::SteadyBlock,
        CursorShape::BlinkingUnderscore => cursor::SetCursorStyle::BlinkingUnderScore,
        CursorShape::SteadyUnderscore => cursor::SetCursorStyle::SteadyUnderScore,
        CursorShape::BlinkingBar => cursor::SetCursorStyle::BlinkingBar,
        CursorShape::SteadyBar => cursor::SetCursorStyle::SteadyBar,
    }
}

impl Editor {
    fn queue_theme_cursor_color(&mut self) -> anyhow::Result<()> {
        if let Some(cursor_color) = self.theme.cursor_style.as_ref().and_then(|style| style.fg) {
            write!(self.stdout, "\x1b]12;{}\x1b\\", cursor_color)?;
        }

        Ok(())
    }

    /// Renders the entire editor state to the terminal
    /// This is the main entry point for all rendering operations
    pub fn render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.update_gutter_width();
        self.apply_panel_layout();
        self.fix_cursor_pos();
        self.check_bounds();
        self.sync_to_window();
        let current_buffer = buffer.clone();

        // Render all windows
        let window_count = self.window_manager.windows().len();
        for window_id in 0..window_count {
            self.render_window(buffer, window_id)?;
        }

        // Render window separators
        self.render_all_window_separators(buffer)?;

        self.panel_manager.render(buffer, &self.theme.style);

        // Render global UI elements
        self.render_ui_chrome(buffer)?;
        self.render_dialog(buffer)?;

        // Render all plugins
        self.render_from_plugins(buffer)?;

        // Update overlay positions and render them
        self.update_and_render_overlays(buffer)?;

        self.render_cursor_cell(buffer);

        // Flush changes to terminal
        let diff = buffer.diff(&current_buffer);
        self.render_diff(diff)?;
        self.render_generation = self.render_generation.wrapping_add(1);

        Ok(())
    }

    pub(crate) fn uses_synthetic_block_cursor(&self) -> bool {
        let dialog_allows_editor_cursor = self
            .current_dialog
            .as_ref()
            .map(|dialog| dialog.allows_event_passthrough())
            .unwrap_or(true);

        self.is_focused
            && dialog_allows_editor_cursor
            && !self.has_term()
            && !self.panel_manager.has_focused_panel()
            && !self.is_waiting_for_key_sequence()
            && matches!(
                self.mode,
                Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
    }

    fn render_cursor_cell(&self, buffer: &mut RenderBuffer) {
        if !self.uses_synthetic_block_cursor() {
            return;
        }

        let Some((x, y)) = self.render_cursor_position() else {
            return;
        };
        if x >= buffer.width || y >= buffer.height {
            return;
        }

        let pos = y * buffer.width + x;
        let Some(cell) = buffer.cells.get_mut(pos) else {
            return;
        };

        let cursor_style = self.theme.cursor_style.as_ref();
        cell.style.fg = cursor_style
            .and_then(|style| style.bg)
            .or(self.theme.style.bg);
        cell.style.bg = cursor_style
            .and_then(|style| style.fg)
            .or(self.theme.style.fg);
        cell.style.bold = false;
        cell.style.italic = false;
    }

    pub(crate) fn render_motion_frame(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.update_gutter_width();
        self.fix_cursor_pos();
        self.sync_to_window();
        let current_buffer = buffer.clone();

        let active_window_id = self.window_manager.active_window_id();
        self.render_window(buffer, active_window_id)?;
        self.render_ui_chrome(buffer)?;
        self.render_dialog(buffer)?;
        self.update_and_render_overlays(buffer)?;
        self.render_cursor_cell(buffer);

        let diff = buffer.diff(&current_buffer);
        self.render_diff(diff)?;
        self.render_generation = self.render_generation.wrapping_add(1);

        Ok(())
    }

    /// Renders a single window
    fn render_window(&mut self, buffer: &mut RenderBuffer, window_id: usize) -> anyhow::Result<()> {
        // Clone the window data to avoid borrowing issues
        let window_data = {
            let windows = self.window_manager.windows();
            let window_count = windows.len();

            windows
                .get(window_id)
                .map(|window| ((*window).clone(), window_count))
        };

        if let Some((window, window_count)) = window_data {
            // Render the gutter for this window
            self.render_gutter_in_window(buffer, &window, window_id)?;

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

        // Get all windows to find separators
        let windows = self.window_manager.windows();
        if windows.len() <= 1 {
            return Ok(());
        }

        // Use ASCII or Unicode characters based on configuration
        let use_ascii = self.config.window_borders_ascii;

        // First, collect all unique vertical and horizontal separator lines
        let mut vertical_lines: Vec<(usize, usize, usize)> = Vec::new(); // (x, y_start, y_end)
        let mut horizontal_lines: Vec<(usize, usize, usize)> = Vec::new(); // (y, x_start, x_end)

        // Find all vertical separators by looking for adjacent windows
        // We need to find continuous vertical lines, not segments
        let mut vertical_x_positions: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        for i in 0..windows.len() {
            for j in 0..windows.len() {
                if i == j {
                    continue;
                }
                let w1 = windows[i];
                let w2 = windows[j];

                // Check if w1 is directly to the left of w2
                if w1.position.x + w1.size.0 + 1 == w2.position.x {
                    let x = w1.position.x + w1.size.0;
                    vertical_x_positions.insert(x);
                }
            }
        }

        // Now for each vertical separator position, find the full extent
        for x in vertical_x_positions {
            let mut min_y = term_height;
            let mut max_y = 0;

            // Find all windows that have this separator on their right edge
            for window in &windows {
                if window.position.x + window.size.0 == x {
                    min_y = min_y.min(window.position.y);
                    max_y = max_y.max(window.position.y + window.size.1);
                }
            }

            if min_y < max_y {
                vertical_lines.push((x, min_y, max_y));
            }
        }

        // Find all horizontal separators by looking for adjacent windows
        // Similar approach for horizontal lines
        let mut horizontal_y_positions: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        for i in 0..windows.len() {
            for j in 0..windows.len() {
                if i == j {
                    continue;
                }
                let w1 = windows[i];
                let w2 = windows[j];

                // Check if w1 is directly above w2
                if w1.position.y + w1.size.1 + 1 == w2.position.y {
                    let y = w1.position.y + w1.size.1;
                    horizontal_y_positions.insert(y);
                }
            }
        }

        // Now for each horizontal separator position, find the full extent
        for y in horizontal_y_positions {
            let mut min_x = term_width;
            let mut max_x = 0;

            // Find all windows that have this separator on their bottom edge
            for window in &windows {
                if window.position.y + window.size.1 == y {
                    min_x = min_x.min(window.position.x);
                    max_x = max_x.max(window.position.x + window.size.0);
                }
            }

            if min_x < max_x {
                horizontal_lines.push((y, min_x, max_x));
            }
        }

        // Pass 1: Draw basic segments into a temporary grid
        let mut temp_grid: HashMap<(usize, usize), char> = HashMap::new();

        // Draw vertical lines
        for (x, y_start, y_end) in &vertical_lines {
            for y in *y_start..*y_end {
                temp_grid.insert((*x, y), if use_ascii { '|' } else { '│' });
            }
        }

        // Draw horizontal lines, marking overlaps as cross
        for (y, x_start, x_end) in &horizontal_lines {
            for x in *x_start..*x_end {
                if let Some(existing) = temp_grid.get(&(x, *y)) {
                    if *existing == '|' || *existing == '│' {
                        // Overlap - mark as cross
                        temp_grid.insert((x, *y), if use_ascii { '+' } else { '┼' });
                    }
                } else {
                    temp_grid.insert((x, *y), if use_ascii { '-' } else { '─' });
                }
            }
        }

        // Helper functions to check if a character has vertical/horizontal components
        let has_vertical_component = |c: char| -> bool {
            matches!(
                c,
                '│' | '|' | '┼' | '+' | '├' | '┤' | '┬' | '┴' | '┌' | '┐' | '└' | '┘'
            )
        };

        let has_horizontal_component = |c: char| -> bool {
            matches!(
                c,
                '─' | '-' | '┼' | '+' | '┬' | '┴' | '├' | '┤' | '┌' | '┐' | '└' | '┘'
            )
        };

        // Pass 2: Refine intersections based on adjacent cells
        let mut final_grid: HashMap<(usize, usize), char> = HashMap::new();

        for (x, y) in temp_grid.keys() {
            // Check adjacent cells
            let connects_up = if *y > 0 {
                temp_grid
                    .get(&(*x, y.saturating_sub(1)))
                    .map(|&c| has_vertical_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            let connects_down = if *y < term_height - 1 {
                temp_grid
                    .get(&(*x, y + 1))
                    .map(|&c| has_vertical_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            let connects_left = if *x > 0 {
                temp_grid
                    .get(&(x.saturating_sub(1), *y))
                    .map(|&c| has_horizontal_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            let connects_right = if *x < term_width - 1 {
                temp_grid
                    .get(&(x + 1, *y))
                    .map(|&c| has_horizontal_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            // Select the appropriate character based on connections
            let junction_char = if use_ascii {
                // ASCII mode
                if connects_up || connects_down || connects_left || connects_right {
                    if (connects_up || connects_down) && (connects_left || connects_right) {
                        '+' // Any junction or cross
                    } else if connects_up || connects_down {
                        '|' // Vertical line
                    } else {
                        '-' // Horizontal line
                    }
                } else {
                    '+' // Isolated point (shouldn't happen)
                }
            } else {
                // Unicode mode
                match (connects_up, connects_down, connects_left, connects_right) {
                    // Four-way cross
                    (true, true, true, true) => '┼',
                    // T-junctions
                    (true, true, true, false) => '┤', // T-junction right
                    (true, true, false, true) => '├', // T-junction left
                    (true, false, true, true) => '┴', // T-junction bottom
                    (false, true, true, true) => '┬', // T-junction top
                    // Corners
                    (true, false, false, true) => '└', // Corner bottom-left
                    (true, false, true, false) => '┘', // Corner bottom-right
                    (false, true, false, true) => '┌', // Corner top-left
                    (false, true, true, false) => '┐', // Corner top-right
                    // Straight lines
                    (true, true, false, false) => '│', // Vertical only
                    (false, false, true, true) => '─', // Horizontal only
                    // Single connections (line ends)
                    (true, false, false, false) => '│', // Vertical from top
                    (false, true, false, false) => '│', // Vertical to bottom
                    (false, false, true, false) => '─', // Horizontal from left
                    (false, false, false, true) => '─', // Horizontal to right
                    // No connections (shouldn't happen in practice)
                    (false, false, false, false) => '·', // Isolated point
                }
            };

            final_grid.insert((*x, *y), junction_char);
        }

        // Draw all separator characters from the final grid
        for ((x, y), char) in final_grid {
            buffer.set_char(x, y, char, &separator_style, &self.theme);
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
        let cursor_pos = self.render_cursor_position().map(|(x, y)| Point::new(x, y));

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
        let layout = self.layout_for_window(window);
        let (file, revision) = {
            let window_buffer = &self.buffers[window.buffer_index];
            (window_buffer.file.clone(), window_buffer.revision())
        };
        let cache_key = HighlightCacheKey {
            buffer_index: window.buffer_index,
            revision,
            file: file.clone(),
            vtop: window.vtop,
            height: window.inner_height(),
        };
        let style_info =
            self.cached_viewport_highlight_spans(cache_key, file.as_deref(), &layout.text)?;
        let theme_style = self.theme.style.clone();
        let mut style_cursor = StyleCursor::new(&style_info);

        let gutter_width = self.gutter_width_for_window(window);
        let content_start = gutter_width + 1;
        let content_width = self.window_content_width(window);

        for segment in &layout.rows {
            let term_y = self.window_to_terminal_y(window, segment.row);
            let term_x = self.window_to_terminal_x(window, gutter_width + 1);
            self.fill_line_in_window(buffer, term_x, term_y, content_width, &theme_style);

            let Some(line) = self.buffers[window.buffer_index].get(segment.line) else {
                continue;
            };
            let line = line.trim_end_matches('\n');
            for (grapheme_index, (byte_offset, grapheme)) in line.grapheme_indices(true).enumerate()
            {
                if grapheme_index < segment.start_grapheme {
                    continue;
                }
                if grapheme_index >= segment.end_grapheme {
                    break;
                }

                let grapheme_col = grapheme_to_column(line, grapheme_index);
                if grapheme_col < segment.start_col {
                    continue;
                }
                let local_x = grapheme_col.saturating_sub(segment.start_col);
                if local_x >= content_width {
                    break;
                }

                let style = style_cursor
                    .style_at(segment.source_offset + byte_offset)
                    .unwrap_or(&theme_style);
                let term_x = self.window_to_terminal_x(window, content_start + local_x);
                let term_y = self.window_to_terminal_y(window, segment.row);
                buffer.set_text(term_x, term_y, grapheme, style);
            }
        }

        for y in layout.rows.len()..window.inner_height() {
            let term_y = self.window_to_terminal_y(window, y);
            let term_x = self.window_to_terminal_x(window, gutter_width + 1);
            self.fill_line_in_window(buffer, term_x, term_y, content_width, &theme_style);
        }

        Ok(())
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
                let layout = self.layout_for_window(window);
                let buffer_y = window.vtop + window.cy;
                if let Some(segment) = layout
                    .rows
                    .iter()
                    .find(|segment| segment.line == buffer_y && segment.row < window.inner_height())
                {
                    let term_y = self.window_to_terminal_y(window, segment.row);
                    let gutter_width = self.gutter_width_for_window(window);
                    let start_x = window.position.x + gutter_width + 1;
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

        self.render_search_highlights_in_window(buffer, window)?;

        Ok(())
    }

    fn render_search_highlights_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        if !window.active {
            return Ok(());
        }

        let active_search = self.active_search.clone();
        let pattern = active_search
            .as_ref()
            .map(|search| search.draft.as_str())
            .filter(|draft| !draft.is_empty())
            .or_else(|| {
                (self.config.search.hlsearch
                    && !self.search_highlights_suppressed
                    && !self.search_term.is_empty())
                .then_some(self.search_term.as_str())
            });
        let Some(pattern) = pattern else {
            return Ok(());
        };

        let matches = match self.search_matches(pattern) {
            Ok(matches) => matches,
            Err(_) => return Ok(()),
        };
        let current_match = active_search.as_ref().and_then(|search| search.preview);
        let current_start = current_match.map(|match_| (match_.start_x, match_.start_y));
        let cursor_start = (!self.is_search()).then(|| {
            (
                self.grapheme_to_char_on_line(self.cx, self.buffer_line()),
                self.buffer_line(),
            )
        });
        let match_bg = self
            .theme
            .find_match_style
            .as_ref()
            .and_then(|style| style.bg)
            .unwrap_or_else(|| self.theme.get_selection_bg());
        let highlight_bg = self
            .theme
            .find_match_highlight_style
            .as_ref()
            .and_then(|style| style.bg)
            .unwrap_or(match_bg);
        for match_ in matches {
            let layout = self.layout_for_window(window);
            let visible_start = layout.rows.first().map(|segment| segment.line);
            let visible_end = layout.rows.last().map(|segment| segment.line);
            if visible_start.is_none()
                || match_.end_y < visible_start.unwrap()
                || match_.start_y > visible_end.unwrap()
            {
                continue;
            }

            let is_current = current_start
                .or(cursor_start)
                .is_some_and(|start| start == (match_.start_x, match_.start_y));
            let bg = if is_current { &match_bg } else { &highlight_bg };
            let start_y = match_.start_y.max(visible_start.unwrap());
            let end_y = match_.end_y.min(visible_end.unwrap());

            for line_index in start_y..=end_y {
                let line = self
                    .buffers
                    .get(window.buffer_index)
                    .and_then(|buffer| buffer.get(line_index))
                    .unwrap_or_default();
                let line = line.trim_end_matches('\n');
                let line_len = line.chars().count();
                let start_x = if line_index == match_.start_y {
                    match_.start_x
                } else {
                    0
                };
                let end_x = if line_index == match_.end_y {
                    match_.end_x
                } else {
                    line_len
                };
                if end_x <= start_x {
                    continue;
                }

                let start_col = display_width(char_prefix(&line, start_x));
                let end_col = display_width(char_prefix(&line, end_x));
                let points =
                    self.display_col_range_points_in_window(window, line_index, start_col, end_col);
                buffer.set_bg_for_points(points, bg, &self.theme);
            }
        }

        Ok(())
    }

    fn display_col_range_points_in_window(
        &self,
        window: &crate::window::Window,
        line_index: usize,
        start_col: usize,
        end_col: usize,
    ) -> Vec<Point> {
        if end_col <= start_col {
            return Vec::new();
        }

        let layout = self.layout_for_window(window);
        let gutter_width = self.gutter_width_for_window(window);
        let content_start = gutter_width + 1;
        let content_width = self.window_content_width(window);
        let mut points = Vec::new();

        for segment in layout
            .rows
            .iter()
            .filter(|segment| segment.line == line_index)
        {
            let start = start_col.max(segment.start_col);
            let end = end_col.min(segment.end_col);
            if end <= start {
                continue;
            }

            for col in start..end {
                let local_x = col.saturating_sub(segment.start_col);
                if local_x >= content_width {
                    continue;
                }
                points.push(Point::new(
                    self.window_to_terminal_x(window, content_start + local_x),
                    self.window_to_terminal_y(window, segment.row),
                ));
            }
        }

        points
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
        if let Some(row) = diagnostic_row(diagnostics, available_width) {
            buffer.set_text(x, y, &row, style);
        }

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

        let layout = self.layout_for_window(window);

        // Render diagnostics for visible lines in this window
        for (line_num, diagnostics) in diagnostics_by_line {
            let Some(segment) = layout
                .rows
                .iter()
                .rev()
                .find(|segment| segment.line == line_num)
            else {
                continue;
            };

            // Get the line content to determine where to place the diagnostic
            let Some(line) = window_buffer.get(line_num) else {
                continue;
            };

            // Calculate diagnostic indicator position within window
            let gutter_width = self.gutter_width_for_window(window);
            let line_width = display_width(line.trim_end_matches('\n'));
            if line_width > segment.end_col {
                continue;
            }
            let content_end = gutter_width + 1 + line_width.saturating_sub(segment.start_col);
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
            let term_y = self.window_to_terminal_y(window, segment.row);

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
            let (start_x, end_x) = match self.mode {
                Mode::Visual => {
                    if y == selection.y0 && y == selection.y1 {
                        (selection.x0, selection.x1)
                    } else if y == selection.y0 {
                        (selection.x0, self.last_cell_for_line(y))
                    } else if y == selection.y1 {
                        (0, selection.x1)
                    } else {
                        (0, self.last_cell_for_line(y))
                    }
                }
                Mode::VisualLine => (0, self.last_cell_for_line(y)),
                Mode::VisualBlock => (selection.x0, selection.x1),
                _ => unreachable!(),
            };

            let Some(line) = self.buffers[window.buffer_index].get(y) else {
                continue;
            };
            let line = line.trim_end_matches('\n');
            let start_col = grapheme_to_column(line, start_x);
            let end_col = grapheme_to_column(line, end_x.saturating_add(1));
            cells.extend(self.display_col_range_points_in_window(window, y, start_col, end_col));
            if line.is_empty() && start_x == 0 && end_x == 0 {
                cells.extend(self.display_col_range_points_in_window(window, y, 0, 1));
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
        if !self.terminal_output_enabled {
            self.draw_cursor_preserving_cursor_goal()?;
            return Ok(());
        }

        if change_set.is_empty() {
            self.set_cursor_style()?;
            self.draw_cursor_preserving_cursor_goal()?;
            self.stdout.flush()?;
            return Ok(());
        }

        self.stdout.queue(cursor::Hide)?;
        self.stdout.queue(terminal::DisableLineWrap)?;

        let mut i = 0;
        let mut text = String::new();
        while i < change_set.len() {
            let change = &change_set[i];
            let x = change.x;
            let y = change.y;
            let style = change.cell.style.clone();

            self.stdout.queue(MoveTo(x as u16, y as u16))?;
            self.queue_cell_style(&style)?;

            let mut next_x = x;
            text.clear();

            while i < change_set.len() {
                let change = &change_set[i];
                if change.y != y || change.x != next_x || change.cell.style != style {
                    break;
                }

                let cell_width = display_width(change.cell.text.as_str()).max(1);
                text.push_str(change.cell.text.as_str());
                next_x += cell_width;
                i += 1;

                while cell_width > 1 && i < change_set.len() {
                    let padding = &change_set[i];
                    if padding.y != y || padding.x >= next_x || padding.cell.text != " " {
                        break;
                    }
                    i += 1;
                }
            }

            self.stdout.queue(style::Print(text.as_str()))?;
        }

        self.stdout.queue(terminal::EnableLineWrap)?;
        self.stdout.queue(cursor::Show)?;

        self.set_cursor_style()?;
        self.draw_cursor_preserving_cursor_goal()?;
        self.stdout.flush()?;

        Ok(())
    }

    fn queue_cell_style(&mut self, cell_style: &Style) -> anyhow::Result<()> {
        if let Some(bg) = cell_style.bg {
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
        if let Some(fg) = cell_style.fg {
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
        queue_cell_attributes(&mut self.stdout, cell_style)?;

        Ok(())
    }

    pub fn draw_statusline(&mut self, buffer: &mut RenderBuffer) {
        if self.size.0 == 0 || self.size.1 < 2 {
            return;
        }

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

        let term_width = self.size.0 as usize;
        let y = self.size.1 as usize - 2;

        let transition_style = Style {
            fg: self.theme.statusline_style.outer_style.bg,
            bg: self.theme.statusline_style.inner_style.bg,
            ..Default::default()
        };

        let clear_line = " ".repeat(term_width);
        buffer.set_text(0, y, &clear_line, &self.theme.statusline_style.inner_style);

        let left_transition = self.theme.statusline_style.outer_chars[1].to_string();
        let right_transition = self.theme.statusline_style.outer_chars[2].to_string();
        let position = format!("{}{}", pos, window_indicator);

        let mode_width = display_width(&mode);
        let left_transition_width = display_width(&left_transition);
        let right_transition_width = display_width(&right_transition);
        let position_width = display_width(&position);
        let position_start = term_width.saturating_sub(position_width);
        let right_transition_start = position_start.saturating_sub(right_transition_width);
        let file_start = mode_width + left_transition_width;
        let file_width = right_transition_start.saturating_sub(file_start);

        buffer.set_text(0, y, &mode, &self.theme.statusline_style.outer_style);

        buffer.set_text(mode_width, y, &left_transition, &transition_style);

        if file_width > 0 {
            buffer.set_text(
                file_start,
                y,
                &format!("{:<width$}", file, width = file_width),
                &self.theme.statusline_style.inner_style,
            );
        }

        if right_transition_start < term_width {
            buffer.set_text(
                right_transition_start,
                y,
                &right_transition,
                &transition_style,
            );
        }

        if position_start < term_width {
            buffer.set_text(
                position_start,
                y,
                &position,
                &self.theme.statusline_style.outer_style,
            );
        }
    }

    pub fn draw_commandline(&mut self, buffer: &mut RenderBuffer) {
        let style = &self.theme.style;
        let width = self.size.0 as usize;
        if width == 0 || self.size.1 == 0 {
            return;
        }

        let y = self.size.1 as usize - 1;
        let clear_line = " ".repeat(width);
        buffer.set_text(0, y, &clear_line, style);

        if !self.has_term() {
            let wc = if let Some(ref waiting_command) = self.waiting_command {
                waiting_command.clone()
            } else if let Some(ref repeater) = self.repeater {
                format!("{}", repeater)
            } else {
                String::new()
            };
            let wc_width = if wc.is_empty() { 0 } else { 10.min(width) };

            if let Some(ref last_error) = self.last_error {
                let width = width.saturating_sub(wc_width);
                let last_error = last_error.replace(['\r', '\n'], " ");
                let last_error = fit_display_width(&last_error, width);
                buffer.set_text(0, y, &last_error, style);
            }

            if wc_width > 0 {
                let wc = fit_display_width(&wc, wc_width);
                buffer.set_text(width.saturating_sub(wc_width), y, &wc, style);
            }

            return;
        }

        let text = if self.is_command() {
            &self.command
        } else {
            self.active_search_text().unwrap_or(&self.search_term)
        };
        let prefix = if self.is_command() {
            ":"
        } else {
            self.search_commandline_prefix()
        };
        let cmdline = format!("{}{}", prefix, text);
        buffer.set_text(0, y, &cmdline, style);
    }

    /// Renders the gutter with line numbers for a specific window
    fn render_gutter_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        window_id: usize,
    ) -> anyhow::Result<()> {
        let width = self.gutter_width_for_window(window);
        let gutter_style = self.theme.gutter_style.fallback_bg(&self.theme.style);

        let layout = self.layout_for_window(window);
        let window_buffer = &self.buffers[window.buffer_index];

        for y in 0..window.inner_height() {
            let mut line_count = window_buffer.navigable_line_count();
            if self.window_manager.active_window_id() == window_id && self.is_insert() {
                line_count = line_count.max(window.vtop + window.cy + 1);
            }
            let text = layout
                .row(y)
                .filter(|segment| segment.first_segment)
                .and_then(|segment| {
                    let line_number = segment.line + 1;
                    (line_number <= line_count).then(|| format!("{line_number:>width$} "))
                })
                .unwrap_or_else(|| " ".repeat(width + 1));

            let term_x = window.position.x;
            let term_y = window.position.y + y;
            buffer.set_text(term_x, term_y, &text, &gutter_style);
        }

        Ok(())
    }

    pub fn draw_cursor(&mut self) -> anyhow::Result<()> {
        self.draw_cursor_with_goal_refresh(true)
    }

    pub(crate) fn draw_cursor_preserving_cursor_goal(&mut self) -> anyhow::Result<()> {
        self.draw_cursor_with_goal_refresh(false)
    }

    fn draw_cursor_with_goal_refresh(&mut self, refresh_goal: bool) -> anyhow::Result<()> {
        if refresh_goal {
            self.refresh_cursor_goal();
        }
        self.fix_cursor_pos();
        self.sync_to_window();

        if !self.terminal_output_enabled {
            return Ok(());
        }

        if !self.is_focused {
            self.stdout.queue(cursor::Hide)?;
            return Ok(());
        }

        self.set_cursor_style()?;

        if self.uses_synthetic_block_cursor() {
            self.stdout.queue(cursor::Hide)?;
            return Ok(());
        }

        let cursor_pos = self.render_cursor_position();

        if let Some((x, y)) = cursor_pos {
            self.stdout.queue(cursor::MoveTo(x as u16, y as u16))?;
        } else {
            self.stdout.queue(cursor::Hide)?;
        }

        Ok(())
    }

    pub(crate) fn render_cursor_position(&self) -> Option<(usize, usize)> {
        if let Some(current_dialog) = &self.current_dialog {
            if let Some(cursor_position) = current_dialog.cursor_position() {
                return Some(cursor_position);
            }
            if !current_dialog.allows_event_passthrough() {
                return None;
            }
        }

        if self.panel_manager.has_focused_panel() && !self.has_term() {
            return None;
        }

        if self.has_term() {
            Some((
                display_width(self.term()) + 1,
                (self.size.1 as usize).saturating_sub(1),
            ))
        } else {
            // Get the active window to calculate cursor position
            if let Some(window) = self.window_manager.active_window() {
                let buffer_y = window.vtop + window.cy;

                // Calculate the actual display column for the cursor
                let display_col =
                    if let Some(line) = self.buffers[window.buffer_index].get(buffer_y) {
                        let line = line.trim_end_matches('\n');
                        self.display_col_for_cursor_goal(line, window.cursor_goal)
                    } else {
                        window.cx
                    };

                let layout = self.layout_for_window(window);
                let segment = layout.segment_for_cursor(buffer_y, display_col)?;
                let gutter_width = self.gutter_width_for_window(window);
                let term_x = window.position.x
                    + gutter_width
                    + 1
                    + segment
                        .screen_col_for_display_col(display_col, self.window_content_width(window));
                let term_y = window.position.y + segment.row;
                Some((term_x, term_y))
            } else {
                // Fallback to old behavior if no active window
                let display_col = if let Some(line) = self.viewport_line(self.cy) {
                    let line = line.trim_end_matches('\n');
                    self.display_col_for_cursor_goal(line, self.cursor_goal)
                } else {
                    self.cx
                };
                Some(((self.vx + display_col), self.cy))
            }
        }
    }

    fn set_cursor_style(&mut self) -> anyhow::Result<()> {
        if !self.terminal_output_enabled {
            return Ok(());
        }

        self.queue_theme_cursor_color()?;
        let shape = if self.is_waiting_for_key_sequence() {
            self.config.cursor.waiting
        } else {
            match self.mode {
                Mode::Normal => self.config.cursor.normal,
                Mode::Command => self.config.cursor.command,
                Mode::Insert => self.config.cursor.insert,
                Mode::Search => self.config.cursor.search,
                Mode::Visual => self.config.cursor.visual,
                Mode::VisualLine => self.config.cursor.visual_line,
                Mode::VisualBlock => self.config.cursor.visual_block,
            }
        };
        self.stdout.queue(cursor_style_for_shape(shape))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{Position, Range};

    fn diagnostic(message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            severity: None,
            code: None,
            message: message.to_string(),
            related_information: None,
            data: None,
            tags: None,
        }
    }

    #[test]
    fn diagnostic_row_fits_available_display_width() {
        let diagnostic = diagnostic("wide 👋 diagnostic 世界 message");
        let diagnostics = vec![&diagnostic];
        let row = diagnostic_row(&diagnostics, 12).unwrap();

        assert_eq!(display_width(&row), 12);
        assert!(row.ends_with('…'));
    }

    #[test]
    fn diagnostic_row_handles_cramped_width() {
        let diagnostic = diagnostic("message");
        let diagnostics = vec![&diagnostic, &diagnostic, &diagnostic];
        let row = diagnostic_row(&diagnostics, 2).unwrap();

        assert_eq!(display_width(&row), 2);
    }

    #[test]
    fn queue_cell_attributes_sets_and_clears_tracked_attributes() {
        let mut output = Vec::new();

        queue_cell_attributes(
            &mut output,
            &Style {
                bold: true,
                italic: true,
                ..Style::default()
            },
        )
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("\x1b[1m"),
            "bold style should emit bold attribute"
        );
        assert!(
            output.contains("\x1b[3m"),
            "italic style should emit italic attribute"
        );

        let mut output = Vec::new();
        queue_cell_attributes(&mut output, &Style::default()).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("\x1b[22m"),
            "plain style should clear bold/dim intensity"
        );
        assert!(
            output.contains("\x1b[23m"),
            "plain style should clear italic attribute"
        );
    }
}
