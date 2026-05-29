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
    unicode_utils::{char_display_width, display_width, fit_display_width, truncate_display_width},
};

use super::{
    adjust_color_brightness, determine_style_for_position, render_buffer::Change, Editor, Mode,
    Point, Rect, RenderBuffer,
};

/// Join key hints with a thin separator, dropping the lowest-priority hints
/// (those last in the list) when they don't fit rather than hard-truncating the
/// line mid-word. A trailing "…" signals that hints were omitted.
fn render_hint_line(hints: &[String], width: usize) -> String {
    const SEP: &str = " · ";
    const ELLIPSIS: &str = " …";

    if hints.is_empty() || width == 0 {
        return String::new();
    }

    // Greedily include leading hints while the running line still fits.
    let mut included = 0;
    let mut line = String::new();
    for hint in hints {
        let candidate = if line.is_empty() {
            hint.clone()
        } else {
            format!("{line}{SEP}{hint}")
        };
        if display_width(&candidate) <= width {
            line = candidate;
            included += 1;
        } else {
            break;
        }
    }

    // Nothing fit whole — fall back to a truncated first hint.
    if included == 0 {
        return fit_display_width(&hints[0], width);
    }

    // Append an ellipsis when hints were dropped and there's room for it.
    if included < hints.len() && display_width(&line) + display_width(ELLIPSIS) <= width {
        line.push_str(ELLIPSIS);
    }
    line
}

/// Vertical layout of the chat body below the heading: the transcript, an
/// optional separator, and the composer. The composer is padded with one blank
/// row above and below its input (Codex-style) whenever the body is tall enough
/// to still show a separator and at least one transcript row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChatBodyLayout {
    transcript_height: usize,
    separator_height: usize,
    pad_top: usize,
    input_height: usize,
    pad_bottom: usize,
}

/// Split `body_height` rows between the transcript and a padded composer that
/// must hold `input_rows` wrapped lines of typed text. Falls back to no padding
/// on short windows so the composer never crowds out the whole transcript.
fn chat_body_layout(body_height: usize, input_rows: usize) -> ChatBodyLayout {
    if body_height == 0 {
        return ChatBodyLayout {
            transcript_height: 0,
            separator_height: 0,
            pad_top: 0,
            input_height: 0,
            pad_bottom: 0,
        };
    }

    // One blank padding row above and below the input, but only when the body can
    // still afford a transcript row (1), separator (1), and padded composer
    // (input + 2). Requiring >= 5 guarantees every region has at least one row.
    let pad = usize::from(body_height >= 5);
    let reserved_transcript = usize::from(body_height >= 2);
    let reserved_separator = usize::from(body_height > reserved_transcript + 2 * pad + 1);
    let max_input = body_height
        .saturating_sub(reserved_transcript + reserved_separator + 2 * pad)
        .max(1);
    let input_height = input_rows.max(1).min(max_input);
    let composer_block = input_height + 2 * pad;
    let separator_height = usize::from(body_height > composer_block + reserved_transcript);
    let transcript_height = body_height.saturating_sub(composer_block + separator_height);
    ChatBodyLayout {
        transcript_height,
        separator_height,
        pad_top: pad,
        input_height,
        pad_bottom: pad,
    }
}

fn wrap_preserving_whitespace(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in text.chars() {
        let ch_width = char_display_width(ch).max(1);
        if current_width > 0 && current_width + ch_width > width {
            lines.push(current);
            current = String::new();
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }
    lines.push(current);
    lines
}

fn plugin_composer_display_width(text: &str) -> usize {
    text.chars().map(|ch| char_display_width(ch).max(1)).sum()
}

fn plugin_composer_wrapped_line_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let display_width = plugin_composer_display_width(text);
    display_width.div_ceil(width).max(1)
}

fn plugin_composer_cursor_wrap_position(prefix_width: usize, wrap_width: usize) -> (usize, usize) {
    let wrap_width = wrap_width.max(1);
    if prefix_width <= wrap_width {
        return (0, prefix_width);
    }

    let wrapped_offset = prefix_width.saturating_sub(1) / wrap_width;
    let display_col = if prefix_width.is_multiple_of(wrap_width) {
        wrap_width
    } else {
        prefix_width % wrap_width
    };
    (wrapped_offset, display_col)
}

fn plugin_transcript_subsequent_indent(text: &str) -> &str {
    if text.starts_with("› ") || text.starts_with("• ") {
        "  "
    } else if text.starts_with("  ") {
        "  "
    } else {
        ""
    }
}

fn rgb_components(color: Color, background: Color) -> (u8, u8, u8) {
    match blend_color(color, background) {
        Color::Rgb { r, g, b } | Color::Rgba { r, g, b, .. } => (r, g, b),
    }
}

fn relative_luminance(color: Color, background: Color) -> f32 {
    let (r, g, b) = rgb_components(color, background);
    let channel = |value: u8| {
        let value = value as f32 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

fn contrast_ratio(foreground: Color, background: Color) -> f32 {
    let foreground = relative_luminance(foreground, background);
    let background = relative_luminance(background, background);
    let lighter = foreground.max(background);
    let darker = foreground.min(background);
    (lighter + 0.05) / (darker + 0.05)
}

fn contrast_color_for(background: Color) -> Color {
    let black = Color::Rgb { r: 0, g: 0, b: 0 };
    let white = Color::Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    if contrast_ratio(black, background) >= contrast_ratio(white, background) {
        black
    } else {
        white
    }
}

fn tint_theme_foreground(
    foreground: Color,
    background: Color,
    channel_shift: (i16, i16, i16),
) -> Color {
    let (r, g, b) = rgb_components(foreground, background);
    let apply = |value: u8, shift: i16| -> u8 {
        let away_from_bg = if relative_luminance(background, background) >= 0.5 {
            -shift
        } else {
            shift
        };
        (i16::from(value) + away_from_bg).clamp(0, 255) as u8
    };
    Color::Rgb {
        r: apply(r, channel_shift.0),
        g: apply(g, channel_shift.1),
        b: apply(b, channel_shift.2),
    }
}

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

fn ordered_plugin_selection(
    selection: crate::window::PluginWindowSelection,
) -> ((usize, usize), (usize, usize)) {
    let start = (selection.start_line, selection.start_column);
    let end = (selection.end_line, selection.end_column);
    if start <= end {
        (start, end)
    } else {
        (end, start)
    }
}

impl Editor {
    /// Renders the entire editor state to the terminal
    /// This is the main entry point for all rendering operations
    pub fn render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.update_gutter_width();
        self.apply_panel_layout();
        let current_buffer = buffer.clone();

        // Render all editor-backed windows
        let window_count = self.window_manager.windows().len();
        log!("Starting render of {} windows", window_count);
        for window_id in 0..window_count {
            self.render_window(buffer, window_id)?;
        }

        self.render_plugin_windows(buffer)?;

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

            windows
                .get(window_id)
                .map(|window| ((*window).clone(), window_count))
        };

        if let Some((window, _window_count)) = window_data {
            log!(
                "Rendering window {} at position ({}, {}) size {}x{}",
                window_id,
                window.position.x,
                window.position.y,
                window.size.0,
                window.size.1
            );

            // Render the gutter for this window
            self.render_gutter_in_window(buffer, &window, window_id)?;

            // Render the window content with proper boundaries
            self.render_main_content_in_window(buffer, &window)?;

            // Render overlays within window bounds
            self.render_overlays_in_window(buffer, &window)?;
        }

        Ok(())
    }

    fn render_plugin_windows(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let windows: Vec<_> = self
            .window_manager
            .plugin_windows()
            .into_iter()
            .cloned()
            .collect();

        let base_style = self.theme.style.clone();
        let title_style = Style {
            fg: Some(Color::Rgb {
                r: 180,
                g: 180,
                b: 180,
            }),
            bg: None,
            bold: true,
            italic: false,
        };

        for window in windows {
            for y in 0..window.size.1 {
                let term_y = window.position.y + y;
                for x in 0..window.size.0 {
                    let term_x = window.position.x + x;
                    buffer.set_char(term_x, term_y, ' ', &base_style, &self.theme);
                }
            }

            if window.size.0 == 0 || window.size.1 == 0 {
                continue;
            }

            if let Some(render_state) = &window.render_state {
                self.render_plugin_chat_window(buffer, &window, render_state)?;
                continue;
            }

            let title = window.title.as_deref().unwrap_or(window.id.window.as_str());
            let marker = if window.active { "> " } else { "  " };
            let heading = fit_display_width(&format!("{marker}{title}"), window.size.0);
            for (offset, ch) in heading.chars().enumerate() {
                let x = window.position.x + offset;
                if x >= window.position.x + window.size.0 {
                    break;
                }
                buffer.set_char(x, window.position.y, ch, &title_style, &self.theme);
            }
        }

        Ok(())
    }

    fn render_plugin_chat_window(
        &self,
        buffer: &mut RenderBuffer,
        window: &crate::window::PluginWindow,
        state: &crate::window::PluginWindowRenderState,
    ) -> anyhow::Result<()> {
        let width = window.size.0;
        let height = window.size.1;
        if width == 0 || height == 0 {
            return Ok(());
        }

        let title_style = Style {
            bold: true,
            ..self.theme.style.clone()
        };
        let muted_style = self.plugin_window_role_style(
            Some(crate::window::PluginWindowLineRole::Muted),
            self.theme.style.clone(),
        );
        let composer_style = Style {
            fg: self.theme.style.fg,
            bg: self
                .theme
                .statusline_style
                .inner_style
                .bg
                .or(self.theme.style.bg),
            bold: false,
            italic: false,
        };

        let title = state
            .title
            .as_deref()
            .or(window.title.as_deref())
            .unwrap_or(window.id.window.as_str());
        let marker = if window.active { "> " } else { "  " };
        let heading = format!("{marker}{title}");
        buffer.set_text(
            window.position.x,
            window.position.y,
            &fit_display_width(&heading, width),
            &title_style,
        );

        if height == 1 {
            return Ok(());
        }

        let hint_height = usize::from(!state.key_hints.is_empty());
        let body_height = height.saturating_sub(1 + hint_height);
        if body_height == 0 {
            return Ok(());
        }

        let composer_width = width.saturating_sub(2).max(1);
        let composer_lines = self.wrap_plugin_composer_lines_preserving_whitespace(
            &state.composer,
            composer_width,
            composer_style.clone(),
        );
        let layout = chat_body_layout(body_height, composer_lines.len());
        let composer_height = layout.input_height;
        let separator_height = layout.separator_height;
        let transcript_height = layout.transcript_height;
        let visible_transcript_lines = self.visible_plugin_transcript_lines(
            &state.transcript,
            width,
            self.theme.style.clone(),
            transcript_height,
            state.scroll,
        );
        // Bottom-anchor: when the conversation is shorter than the viewport, pad
        // the top so messages sit just above the composer instead of clinging to
        // the top edge with a void beneath them.
        let top_pad = transcript_height.saturating_sub(visible_transcript_lines.len());
        for (row, (text, style)) in visible_transcript_lines.iter().enumerate() {
            buffer.set_text(
                window.position.x,
                window.position.y + 1 + top_pad + row,
                &fit_display_width(text, width),
                style,
            );
        }

        let composer_separator_y = window.position.y + 1 + transcript_height;
        if separator_height == 1 {
            buffer.set_text(
                window.position.x,
                composer_separator_y,
                &fit_display_width(&"─".repeat(width), width),
                &muted_style,
            );
        }

        // The composer is a padded block: pad_top blank rows, the input rows, then
        // pad_bottom blank rows. The blank rows fill the composer background so the
        // input reads as a roomy box rather than a single cramped line.
        let composer_block_top = composer_separator_y + separator_height;
        let blank_composer_row = fit_display_width("", width);
        for pad_row in 0..layout.pad_top {
            buffer.set_text(
                window.position.x,
                composer_block_top + pad_row,
                &blank_composer_row,
                &composer_style,
            );
        }
        let composer_top = composer_block_top + layout.pad_top;
        let visible_composer_start = composer_lines.len().saturating_sub(composer_height);
        for row in 0..composer_height {
            let y = composer_top + row;
            let prefix = if row == 0 { "› " } else { "  " };
            let (text, style) = composer_lines
                .get(visible_composer_start + row)
                .cloned()
                .unwrap_or_else(|| (String::new(), composer_style.clone()));
            buffer.set_text(
                window.position.x,
                y,
                &fit_display_width(&format!("{prefix}{text}"), width),
                &style,
            );
        }
        for pad_row in 0..layout.pad_bottom {
            buffer.set_text(
                window.position.x,
                composer_top + composer_height + pad_row,
                &blank_composer_row,
                &composer_style,
            );
        }
        self.render_plugin_composer_selection(
            buffer,
            window,
            state,
            composer_width,
            composer_top,
            composer_height,
        );
        if hint_height == 1 {
            let hints = render_hint_line(&state.key_hints, width);
            buffer.set_text(
                window.position.x,
                window.position.y + height - 1,
                &fit_display_width(&hints, width),
                &muted_style,
            );
        }

        Ok(())
    }

    fn render_plugin_composer_selection(
        &self,
        buffer: &mut RenderBuffer,
        window: &crate::window::PluginWindow,
        state: &crate::window::PluginWindowRenderState,
        composer_width: usize,
        composer_top: usize,
        composer_height: usize,
    ) {
        let Some(selection) = state.composer_selection else {
            return;
        };
        let composer_lines = if state.composer.is_empty() {
            vec![String::new()]
        } else {
            state
                .composer
                .iter()
                .map(|line| line.text.clone())
                .collect::<Vec<_>>()
        };
        if composer_lines.is_empty() || composer_width == 0 || composer_height == 0 {
            return;
        }
        let width = window.size.0;

        let ((start_line, start_column), (end_line, end_column)) =
            ordered_plugin_selection(selection);
        if start_line == end_line && start_column == end_column {
            return;
        }

        let wrapped_line_count = composer_lines
            .iter()
            .map(|line| plugin_composer_wrapped_line_count(line, composer_width))
            .sum::<usize>()
            .max(1);
        let visible_start = wrapped_line_count.saturating_sub(composer_height);
        let selection_bg = self.theme.get_selection_bg();
        let mut wrapped_before_line = 0usize;

        for (line_index, line) in composer_lines.iter().enumerate() {
            let chars = line.chars().collect::<Vec<_>>();
            let line_start = if line_index == start_line {
                start_column.min(chars.len())
            } else if line_index > start_line {
                0
            } else {
                chars.len()
            };
            let line_end = if line_index == end_line {
                end_column.min(chars.len())
            } else if line_index < end_line {
                chars.len()
            } else {
                0
            };

            if line_start < line_end {
                let mut display_col = 0usize;
                for (column, ch) in chars.iter().enumerate() {
                    let ch_width = char_display_width(*ch).max(1);
                    if column >= line_start && column < line_end {
                        let wrapped_row = wrapped_before_line + (display_col / composer_width);
                        if wrapped_row >= visible_start {
                            let visible_row = wrapped_row - visible_start;
                            if visible_row < composer_height {
                                let x = window.position.x + 2 + (display_col % composer_width);
                                let y = composer_top + visible_row;
                                for offset in 0..ch_width.min(composer_width) {
                                    if x + offset < window.position.x + width {
                                        buffer.set_bg(x + offset, y, &selection_bg, &self.theme);
                                    }
                                }
                            }
                        }
                    }
                    display_col += ch_width;
                }
            }

            wrapped_before_line += plugin_composer_wrapped_line_count(line, composer_width);
        }
    }

    fn wrap_plugin_composer_lines_preserving_whitespace(
        &self,
        lines: &[crate::window::PluginWindowLine],
        width: usize,
        fallback_style: Style,
    ) -> Vec<(String, Style)> {
        let width = width.max(1);
        let mut wrapped_lines = Vec::new();
        for line in lines {
            let style = self.plugin_window_line_style(line, fallback_style.clone());
            let text_lines = wrap_preserving_whitespace(&line.text, width);
            for text in text_lines {
                wrapped_lines.push((text, style.clone()));
            }
        }
        wrapped_lines
    }

    fn visible_plugin_transcript_lines(
        &self,
        lines: &[crate::window::PluginWindowLine],
        width: usize,
        fallback_style: Style,
        visible_height: usize,
        scroll: usize,
    ) -> Vec<(String, Style)> {
        if visible_height == 0 {
            return Vec::new();
        }

        let mut skipped = 0;
        let mut visible_reversed = Vec::with_capacity(visible_height);

        'lines: for line in lines.iter().rev() {
            let wrapped = self.wrap_plugin_line(line, width, fallback_style.clone());
            for row in wrapped.into_iter().rev() {
                if skipped < scroll {
                    skipped += 1;
                    continue;
                }
                visible_reversed.push(row);
                if visible_reversed.len() == visible_height {
                    break 'lines;
                }
            }
        }

        if visible_reversed.is_empty() && skipped < scroll {
            let mut visible = Vec::with_capacity(visible_height);
            'lines: for line in lines {
                for row in self.wrap_plugin_line(line, width, fallback_style.clone()) {
                    visible.push(row);
                    if visible.len() == visible_height {
                        break 'lines;
                    }
                }
            }
            return visible;
        }

        visible_reversed.reverse();
        visible_reversed
    }

    fn wrap_plugin_line(
        &self,
        line: &crate::window::PluginWindowLine,
        width: usize,
        fallback_style: Style,
    ) -> Vec<(String, Style)> {
        let width = width.max(1);
        let style = self.plugin_window_line_style(line, fallback_style);
        if line.text.is_empty() {
            return vec![(String::new(), style)];
        }

        let options = textwrap::Options::new(width)
            .subsequent_indent(plugin_transcript_subsequent_indent(&line.text));
        textwrap::wrap(&line.text, options)
            .into_iter()
            .map(|wrapped| (wrapped.into_owned(), style.clone()))
            .collect()
    }

    fn plugin_window_line_style(
        &self,
        line: &crate::window::PluginWindowLine,
        fallback_style: Style,
    ) -> Style {
        line.style
            .clone()
            .unwrap_or_else(|| self.plugin_window_role_style(line.role, fallback_style))
    }

    fn plugin_window_role_style(
        &self,
        role: Option<crate::window::PluginWindowLineRole>,
        fallback_style: Style,
    ) -> Style {
        use crate::window::PluginWindowLineRole;

        let mut style = fallback_style;
        match role.unwrap_or(PluginWindowLineRole::Default) {
            PluginWindowLineRole::Default => style,
            PluginWindowLineRole::Muted | PluginWindowLineRole::System => {
                style.fg = adjust_color_brightness(style.fg, -38);
                style.bold = false;
                style.italic = false;
                style
            }
            PluginWindowLineRole::User => {
                style.fg = self
                    .theme
                    .get_style("string")
                    .and_then(|style| style.fg)
                    .or_else(|| self.theme.get_style("function").and_then(|style| style.fg))
                    .or_else(|| self.role_fallback_fg(&style, (0, 18, 18)));
                style.fg = self.contrast_checked_plugin_fg(style.fg, &style);
                style.bold = false;
                style.italic = false;
                style
            }
            PluginWindowLineRole::Assistant => {
                style.fg = self
                    .theme
                    .get_style("keyword")
                    .and_then(|style| style.fg)
                    .or_else(|| self.theme.get_style("type").and_then(|style| style.fg))
                    .or_else(|| self.theme.get_style("constant").and_then(|style| style.fg))
                    .or_else(|| self.role_fallback_fg(&style, (18, 0, 18)));
                style.fg = self.contrast_checked_plugin_fg(style.fg, &style);
                style.bold = false;
                style.italic = false;
                style
            }
            PluginWindowLineRole::Success => {
                style.fg = self
                    .theme
                    .get_style("string")
                    .and_then(|style| style.fg)
                    .or_else(|| self.theme.get_style("function").and_then(|style| style.fg))
                    .or_else(|| self.role_fallback_fg(&style, (0, 18, 0)));
                style.fg = self.contrast_checked_plugin_fg(style.fg, &style);
                style.bold = false;
                style.italic = false;
                style
            }
            PluginWindowLineRole::Error => {
                style.fg = self
                    .theme
                    .error_style
                    .as_ref()
                    .and_then(|error_style| error_style.fg)
                    .or_else(|| self.theme.get_style("error").and_then(|style| style.fg))
                    .or_else(|| self.theme.get_style("invalid").and_then(|style| style.fg))
                    .or_else(|| self.role_fallback_fg(&style, (18, 0, 0)));
                style.fg = self.contrast_checked_plugin_fg(style.fg, &style);
                style.bold = false;
                style.italic = false;
                style
            }
        }
    }

    fn role_fallback_fg(&self, style: &Style, channel_shift: (i16, i16, i16)) -> Option<Color> {
        let background =
            style
                .bg
                .or(self.theme.style.bg)
                .unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });
        let foreground = style
            .fg
            .or(self.theme.style.fg)
            .unwrap_or_else(|| contrast_color_for(background));
        Some(tint_theme_foreground(foreground, background, channel_shift))
    }

    fn contrast_checked_plugin_fg(&self, preferred: Option<Color>, style: &Style) -> Option<Color> {
        let background =
            style
                .bg
                .or(self.theme.style.bg)
                .unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });
        let foreground = preferred
            .or(style.fg)
            .or(self.theme.style.fg)
            .unwrap_or_else(|| contrast_color_for(background));
        if contrast_ratio(foreground, background) >= 4.5 {
            Some(foreground)
        } else {
            Some(contrast_color_for(background))
        }
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

        // Get all leaves to find separators, including plugin-backed windows.
        let leaves = self.window_manager.leaves();
        if leaves.len() <= 1 {
            return Ok(());
        }

        // Use ASCII or Unicode characters based on configuration
        let use_ascii = self.config.window_borders_ascii;

        log!("render_all_window_separators: {} leaves", leaves.len());
        log!("  Terminal size: {}x{}", term_width, term_height);
        log!("  ASCII mode: {}", use_ascii);
        for (i, w) in leaves.iter().enumerate() {
            log!(
                "  Leaf {} ({:?}): pos=({},{}), size=({},{})",
                i,
                w.kind,
                w.position.x,
                w.position.y,
                w.size.0,
                w.size.1
            );
        }

        // First, collect all unique vertical and horizontal separator lines
        let mut vertical_lines: Vec<(usize, usize, usize)> = Vec::new(); // (x, y_start, y_end)
        let mut horizontal_lines: Vec<(usize, usize, usize)> = Vec::new(); // (y, x_start, x_end)

        // Find all vertical separators by looking for adjacent windows
        // We need to find continuous vertical lines, not segments
        let mut vertical_x_positions: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        for i in 0..leaves.len() {
            for j in 0..leaves.len() {
                if i == j {
                    continue;
                }
                let w1 = leaves[i];
                let w2 = leaves[j];

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
            for window in &leaves {
                if window.position.x + window.size.0 == x {
                    min_y = min_y.min(window.position.y);
                    max_y = max_y.max(window.position.y + window.size.1);
                }
            }

            if min_y < max_y {
                log!(
                    "  Adding vertical separator at x={}, from y={} to y={}",
                    x,
                    min_y,
                    max_y
                );
                vertical_lines.push((x, min_y, max_y));
            }
        }

        // Find all horizontal separators by looking for adjacent windows
        // Similar approach for horizontal lines
        let mut horizontal_y_positions: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        for i in 0..leaves.len() {
            for j in 0..leaves.len() {
                if i == j {
                    continue;
                }
                let w1 = leaves[i];
                let w2 = leaves[j];

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
            for window in &leaves {
                if window.position.y + window.size.1 == y {
                    min_x = min_x.min(window.position.x);
                    max_x = max_x.max(window.position.x + window.size.0);
                }
            }

            if min_x < max_x {
                log!(
                    "  Adding horizontal separator at y={}, from x={} to x={}",
                    y,
                    min_x,
                    max_x
                );
                horizontal_lines.push((y, min_x, max_x));
            }
        }

        log!(
            "Found {} vertical lines and {} horizontal lines",
            vertical_lines.len(),
            horizontal_lines.len()
        );

        // Log detailed line information
        for (x, y1, y2) in &vertical_lines {
            log!("  Vertical line: x={}, y={}..{}", x, y1, y2);
        }
        for (y, x1, x2) in &horizontal_lines {
            log!("  Horizontal line: y={}, x={}..{}", y, x1, x2);
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

        log!("Temp grid has {} positions", temp_grid.len());

        // Log some key positions from temp_grid for debugging
        let mut intersections = Vec::new();
        for ((x, y), ch) in &temp_grid {
            if *ch == '┼' || *ch == '+' {
                intersections.push((*x, *y, *ch));
            }
        }
        if !intersections.is_empty() {
            log!("Found {} intersections in Pass 1:", intersections.len());
            for (x, y, ch) in &intersections {
                log!("  Intersection at ({}, {}): '{}'", x, y, ch);
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

        for ((x, y), current_char) in &temp_grid {
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

            // Log detailed connection info for all positions
            log!(
                "Pass 2 - Position ({}, {}): current='{}', up={}, down={}, left={}, right={}",
                x,
                y,
                current_char,
                connects_up,
                connects_down,
                connects_left,
                connects_right
            );

            // Also log what's in the adjacent cells
            if connects_up || connects_down || connects_left || connects_right {
                if let Some(up_char) = temp_grid.get(&(*x, y.saturating_sub(1))) {
                    log!(
                        "    Up neighbor at ({}, {}): '{}'",
                        x,
                        y.saturating_sub(1),
                        up_char
                    );
                }
                if let Some(down_char) = temp_grid.get(&(*x, y + 1)) {
                    log!("    Down neighbor at ({}, {}): '{}'", x, y + 1, down_char);
                }
                if let Some(left_char) = temp_grid.get(&(x.saturating_sub(1), *y)) {
                    log!(
                        "    Left neighbor at ({}, {}): '{}'",
                        x.saturating_sub(1),
                        y,
                        left_char
                    );
                }
                if let Some(right_char) = temp_grid.get(&(x + 1, *y)) {
                    log!("    Right neighbor at ({}, {}): '{}'", x + 1, y, right_char);
                }
            }

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

            log!(
                "    Selected character for ({}, {}): '{}' (pattern: {:?})",
                x,
                y,
                junction_char,
                (connects_up, connects_down, connects_left, connects_right)
            );

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

        let file = window_buffer.file.clone();
        let style_info = self.highlight(file.as_deref(), &viewport_content)?;
        let theme_style = self.theme.style.clone();

        // Start at window position, accounting for gutter
        let gutter_width = self.gutter_width_for_window(window);
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
                        window.inner_width().saturating_sub(x),
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

        if !viewport_content.is_empty()
            && !viewport_content.ends_with('\n')
            && y < window.inner_height()
        {
            let term_y = self.window_to_terminal_y(window, y);
            if x < window.inner_width() {
                let term_x = self.window_to_terminal_x(window, x);
                self.fill_line_in_window(
                    buffer,
                    term_x,
                    term_y,
                    window.inner_width().saturating_sub(x),
                    &theme_style,
                );
            }
            y += 1;
        }

        // Fill any remaining lines within the window
        while y < window.inner_height() {
            let term_y = self.window_to_terminal_y(window, y);
            let term_x = self.window_to_terminal_x(window, gutter_width + 1);
            self.fill_line_in_window(
                buffer,
                term_x,
                term_y,
                window.inner_width().saturating_sub(gutter_width + 1),
                &theme_style,
            );
            y += 1;
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
                // Calculate window-relative cursor position
                let window_cy = window.cy;
                let term_y = self.window_to_terminal_y(window, window_cy);

                // Only highlight if the line is within the window
                if window_cy < window.inner_height() {
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
            let gutter_width = self.gutter_width_for_window(window);
            let content_end = gutter_width + display_width(line.trim_end_matches('\n'));
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

            // Convert to terminal coordinates
            for x in start_x..=end_x {
                // Skip if x is outside window bounds
                let gutter_width = self.gutter_width_for_window(window);
                if x + gutter_width + 1 >= window.inner_width() {
                    continue;
                }

                let term_x = self.window_to_terminal_x(window, x + gutter_width + 1);
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
        if !self.terminal_output_enabled {
            return Ok(());
        }

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
            self.stdout.queue(style::Print(cell.text.as_str()))?;
        }

        self.stdout.queue(cursor::Show)?;

        self.set_cursor_style()?;
        self.draw_cursor()?;
        self.stdout.flush()?;

        Ok(())
    }

    pub fn draw_statusline(&mut self, buffer: &mut RenderBuffer) {
        if self.size.0 == 0 || self.size.1 < 2 {
            return;
        }

        let active_plugin_window = self.window_manager.active_plugin_window();
        let mode = if !self.has_term() {
            active_plugin_window
                .and_then(|window| window.render_state.as_ref())
                .map(|state| format_plugin_input_mode_name(state.input_mode))
                .unwrap_or_else(|| format_mode_name(&self.mode))
        } else {
            format_mode_name(&self.mode)
        };
        let mode = format!(" {mode} ");

        // Get information from the active leaf
        let (file, pos, window_indicator) = if let Some(window) = active_plugin_window {
            let title = window
                .render_state
                .as_ref()
                .and_then(|state| state.title.clone())
                .or_else(|| window.title.clone())
                .unwrap_or_else(|| window.id.window.clone());
            let status = window
                .render_state
                .as_ref()
                .and_then(|state| state.status.as_deref())
                .filter(|status| !status.is_empty())
                .unwrap_or("plugin");
            let file = format!(" {}: {}", title, status);
            let pos = format!(" {} ", window.id.window);
            let window_count = self.window_manager.leaf_count();
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
        } else if let Some(window) = self.window_manager.active_window() {
            let window_buffer = &self.buffers[window.buffer_index];
            let dirty = if window_buffer.is_dirty() {
                " [+] "
            } else {
                ""
            };
            let file = format!(" {}{}", window_buffer.name(), dirty);
            let pos = format!(" {}:{} ", window.vtop + window.cy + 1, window.cx + 1);

            // Add window indicator if there are multiple windows
            let window_count = self.window_manager.leaf_count();
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
            let wc = format!("{:<width$}", wc, width = 10);

            if let Some(ref last_error) = self.last_error {
                buffer.set_text(0, y, last_error, style);
            }

            buffer.set_text(width.saturating_sub(10), y, &wc, style);

            return;
        }

        let text = if self.is_command() {
            &self.command
        } else {
            &self.search_term
        };
        let prefix = if self.is_command() { ":" } else { "/" };
        let cmdline = format!("{}{}", prefix, text);
        buffer.set_text(0, y, &cmdline, style);
    }

    /// Renders the gutter with line numbers for a specific window
    fn render_gutter_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        _window_id: usize,
    ) -> anyhow::Result<()> {
        use crate::log;
        let width = self.gutter_width_for_window(window);
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
            let mut line_count = window_buffer.navigable_line_count();
            if window.active && self.is_insert() {
                line_count = line_count.max(window.vtop + window.cy + 1);
            }
            let text = if line_number <= line_count {
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

    pub fn draw_cursor(&mut self) -> anyhow::Result<()> {
        self.fix_cursor_pos();
        self.check_bounds();

        if !self.terminal_output_enabled {
            return Ok(());
        }

        self.set_cursor_style()?;

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
            current_dialog.cursor_position()
        } else if self.has_term() {
            Some((
                display_width(self.term()) + 1,
                (self.size.1 as usize).saturating_sub(1),
            ))
        } else if let Some(window) = self.window_manager.active_plugin_window() {
            self.plugin_window_cursor_position(window)
        } else {
            // Get the active window to calculate cursor position
            if let Some(window) = self.window_manager.active_window() {
                // Use window's cursor position
                let window_cy = window.cy;
                let window_cx = window.cx;
                let buffer_y = window.vtop + window_cy;

                // Calculate the actual display column for the cursor
                let display_col =
                    if let Some(line) = self.buffers[window.buffer_index].get(buffer_y) {
                        let line = line.trim_end_matches('\n');
                        crate::unicode_utils::grapheme_to_column(line, window_cx)
                    } else {
                        window_cx
                    };

                // Convert to terminal coordinates based on active window
                let gutter_width = self.gutter_width_for_window(window);
                let term_x = window.position.x + gutter_width + 1 + display_col;
                let term_y =
                    window.position.y + window_cy.min(window.inner_height().saturating_sub(1));
                Some((term_x, term_y))
            } else {
                // Fallback to old behavior if no active window
                let display_col = if let Some(line) = self.viewport_line(self.cy) {
                    let line = line.trim_end_matches('\n');
                    crate::unicode_utils::grapheme_to_column(line, self.cx)
                } else {
                    self.cx
                };
                Some(((self.vx + display_col), self.cy))
            }
        }
    }

    fn plugin_window_cursor_position(
        &self,
        window: &crate::window::PluginWindow,
    ) -> Option<(usize, usize)> {
        let width = window.size.0;
        let height = window.size.1;
        if width == 0 || height == 0 {
            return None;
        }

        let Some(state) = &window.render_state else {
            return Some((window.position.x, window.position.y));
        };

        if height == 1 {
            return Some((window.position.x, window.position.y));
        }

        let hint_height = usize::from(!state.key_hints.is_empty());
        let body_height = height.saturating_sub(1 + hint_height);
        if body_height == 0 {
            return Some((window.position.x, window.position.y));
        }

        let composer_width = width.saturating_sub(2).max(1);
        let composer_lines = if state.composer.is_empty() {
            vec![String::new()]
        } else {
            state
                .composer
                .iter()
                .map(|line| line.text.clone())
                .collect()
        };
        let wrapped_line_count = composer_lines
            .iter()
            .map(|line| plugin_composer_wrapped_line_count(line, composer_width))
            .sum::<usize>()
            .max(1);
        let layout = chat_body_layout(body_height, wrapped_line_count);
        let composer_height = layout.input_height;
        // The cursor sits in the input region, below the composer's top padding row.
        let composer_top = window.position.y
            + 1
            + layout.transcript_height
            + layout.separator_height
            + layout.pad_top;

        let cursor = state.composer_cursor.unwrap_or_else(|| {
            let line = composer_lines.len().saturating_sub(1);
            crate::window::PluginWindowCursor {
                line,
                column: composer_lines
                    .get(line)
                    .map(|line| line.chars().count())
                    .unwrap_or_default(),
            }
        });
        let cursor_line = cursor.line.min(composer_lines.len().saturating_sub(1));
        let cursor_column = cursor
            .column
            .min(composer_lines[cursor_line].chars().count());

        let wrapped_before_cursor = composer_lines[..cursor_line]
            .iter()
            .map(|line| plugin_composer_wrapped_line_count(line, composer_width))
            .sum::<usize>();
        let cursor_prefix = composer_lines[cursor_line]
            .chars()
            .take(cursor_column)
            .collect::<String>();
        let cursor_prefix_width = plugin_composer_display_width(&cursor_prefix);
        let (cursor_wrapped_offset, cursor_display_col) =
            plugin_composer_cursor_wrap_position(cursor_prefix_width, composer_width);

        let cursor_wrapped_line = wrapped_before_cursor + cursor_wrapped_offset;
        let visible_start = wrapped_line_count.saturating_sub(composer_height);
        let visible_row = cursor_wrapped_line
            .saturating_sub(visible_start)
            .min(composer_height.saturating_sub(1));
        let cursor_x = window.position.x + 2 + cursor_display_col;
        let cursor_x = cursor_x.min(window.position.x + width.saturating_sub(1));
        let cursor_y = composer_top + visible_row;
        let cursor_y = cursor_y.min(window.position.y + height.saturating_sub(1));

        Some((cursor_x, cursor_y))
    }

    fn set_cursor_style(&mut self) -> anyhow::Result<()> {
        if !self.terminal_output_enabled {
            return Ok(());
        }

        self.stdout.queue(self.cursor_style())?;

        Ok(())
    }

    fn cursor_style(&self) -> cursor::SetCursorStyle {
        match self.waiting_key_action {
            Some(_) => cursor::SetCursorStyle::SteadyUnderScore,
            _ if self
                .window_manager
                .active_plugin_window()
                .is_some_and(|window| {
                    window.render_state.as_ref().is_some_and(|state| {
                        state.input_mode == crate::window::PluginWindowInputMode::Insert
                    })
                }) =>
            {
                cursor::SetCursorStyle::SteadyBar
            }
            _ if self.window_manager.active_plugin_window().is_some() => {
                cursor::SetCursorStyle::SteadyBlock
            }
            _ => match self.mode {
                Mode::Normal => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Command => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Insert => cursor::SetCursorStyle::SteadyBar,
                Mode::Search => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                    cursor::SetCursorStyle::DefaultUserShape
                }
            },
        }
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

fn format_plugin_input_mode_name(mode: crate::window::PluginWindowInputMode) -> String {
    match mode {
        crate::window::PluginWindowInputMode::Normal => "NORMAL".to_string(),
        crate::window::PluginWindowInputMode::Insert => "INSERT".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        lsp::{LspManager, Position, Range},
        theme::Theme,
        window::{
            PluginWindowId, PluginWindowInputMode, PluginWindowLine, PluginWindowRenderState,
        },
    };

    #[test]
    fn composer_layout_pads_above_and_below_on_a_roomy_body() {
        let layout = chat_body_layout(20, 1);
        assert_eq!(layout.pad_top, 1);
        assert_eq!(layout.pad_bottom, 1);
        assert_eq!(layout.input_height, 1);
        assert_eq!(layout.separator_height, 1);
        // header is outside body_height; everything below sums back to body_height.
        assert_eq!(
            layout.transcript_height
                + layout.separator_height
                + layout.pad_top
                + layout.input_height
                + layout.pad_bottom,
            20
        );
    }

    #[test]
    fn composer_layout_grows_input_with_typed_lines_but_keeps_a_transcript_row() {
        let layout = chat_body_layout(20, 5);
        assert_eq!(layout.input_height, 5);
        assert!(layout.transcript_height >= 1);
        assert_eq!(
            layout.transcript_height
                + layout.separator_height
                + layout.pad_top
                + layout.input_height
                + layout.pad_bottom,
            20
        );
    }

    #[test]
    fn composer_layout_clamps_tall_input_to_keep_transcript_visible() {
        let layout = chat_body_layout(20, 17);
        assert_eq!(layout.transcript_height, 1);
        assert_eq!(layout.separator_height, 1);
        assert_eq!(layout.input_height, 16);
        assert_eq!(layout.pad_top, 1);
        assert_eq!(layout.pad_bottom, 1);
    }

    #[test]
    fn composer_layout_drops_padding_on_a_cramped_body() {
        // Too short to afford padding: degrade to the unpadded single-row composer.
        let layout = chat_body_layout(2, 1);
        assert_eq!(layout.pad_top, 0);
        assert_eq!(layout.pad_bottom, 0);
        assert_eq!(layout.input_height, 1);
        assert_eq!(layout.transcript_height, 1);
        assert_eq!(layout.separator_height, 0);
    }

    #[test]
    fn contrast_color_picks_readable_foreground_for_light_and_dark_backgrounds() {
        let light = Color::Rgb {
            r: 245,
            g: 245,
            b: 245,
        };
        let dark = Color::Rgb {
            r: 15,
            g: 15,
            b: 15,
        };
        assert!(contrast_ratio(contrast_color_for(light), light) >= 4.5);
        assert!(contrast_ratio(contrast_color_for(dark), dark) >= 4.5);
    }

    #[test]
    fn active_plugin_window_insert_mode_uses_bar_cursor() {
        let editor = editor_with_plugin_input_mode(PluginWindowInputMode::Insert);

        assert!(matches!(
            editor.cursor_style(),
            cursor::SetCursorStyle::SteadyBar
        ));
    }

    #[test]
    fn active_plugin_window_normal_mode_uses_block_cursor() {
        let editor = editor_with_plugin_input_mode(PluginWindowInputMode::Normal);

        assert!(matches!(
            editor.cursor_style(),
            cursor::SetCursorStyle::SteadyBlock
        ));
    }

    fn editor_with_plugin_input_mode(input_mode: PluginWindowInputMode) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let theme = Theme::default();
        let buffer = Buffer::new(None, String::new());
        let mut editor = Editor::with_size(lsp, 80, 24, config, theme, vec![buffer]).unwrap();
        let id = PluginWindowId::new("codex", "chat");
        editor
            .window_manager
            .split_vertical_plugin(id.clone(), Some("Codex".to_string()))
            .unwrap();
        editor.window_manager.update_plugin_window(
            &id,
            PluginWindowRenderState {
                input_mode,
                composer: vec![PluginWindowLine {
                    text: "draft".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        editor
    }

    #[test]
    fn hint_line_joins_with_separator_when_everything_fits() {
        let hints = vec!["Enter send".to_string(), "Ctrl-j newline".to_string()];
        assert_eq!(render_hint_line(&hints, 80), "Enter send · Ctrl-j newline");
    }

    #[test]
    fn hint_line_drops_overflow_hints_and_marks_with_ellipsis() {
        let hints = vec![
            "Enter send".to_string(),
            "Ctrl-j newline".to_string(),
            "context commands".to_string(),
        ];
        // Wide enough for the first two plus the ellipsis, but not the third.
        let rendered = render_hint_line(&hints, 30);
        assert_eq!(rendered, "Enter send · Ctrl-j newline …");
        assert!(display_width(&rendered) <= 30);
    }

    #[test]
    fn hint_line_truncates_first_hint_when_nothing_fits() {
        let hints = vec!["Enter send".to_string()];
        let rendered = render_hint_line(&hints, 4);
        assert!(display_width(&rendered) <= 4);
    }

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
}
