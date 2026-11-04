//! Paint the regex prompt and its search-history candidates in document windows.
use super::*;
use crate::unicode_utils::truncate_display_width;

/// The highlighted regex occupies the first row of a word-wrapped area.
fn first_prompt_row(line: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut visible = String::new();
    let mut cells = 0;
    let mut last_word_end = 0;
    let mut was_space = false;
    for grapheme in line.graphemes(true) {
        if grapheme.contains(['\n', '\r']) {
            break;
        }
        let size = display_width(grapheme);
        if size > width {
            continue;
        }
        let space =
            grapheme.chars().all(char::is_whitespace) && !matches!(grapheme, "\u{a0}" | "\u{202f}");
        if space && !was_space {
            last_word_end = visible.len();
        }
        if cells + size > width {
            if last_word_end > 0 {
                visible.truncate(last_word_end);
            }
            break;
        }
        visible.push_str(grapheme);
        cells += size;
        was_space = space;
    }
    visible
}

impl Editor {
    pub(in crate::editor) fn felix_prompt(&self) -> Option<(&'static str, &str, usize)> {
        let prompt = self.felix.session.prompt().or_else(|| {
            self.felix
                .session
                .register_prompt()
                .map(|title| (title, "", 0))
        })?;
        (!self.has_term()
            && self.current_dialog.is_none()
            && !self.panel_manager.has_focused_panel())
        .then_some(prompt)
    }

    pub(in crate::editor) fn felix_prompt_caret(&self) -> Option<usize> {
        let (label, line, cursor) = self.felix_prompt()?;
        Some(
            (label.len() + 1 + display_width(&line[..cursor]))
                .min(usize::from(self.size.0).saturating_sub(1)),
        )
    }

    pub(in crate::editor) fn draw_felix_prompt(&self, buffer: &mut RenderBuffer) -> bool {
        let Some((label, line, _)) = self.felix_prompt() else {
            return false;
        };
        let width = usize::from(self.size.0);
        let row = usize::from(self.size.1).saturating_sub(1);
        let prefix = format!("{label}:");
        let start = prefix.len();
        let style = &self.theme.style;
        buffer.set_text(0, row, &prefix, style);
        if self.felix.session.register_prompt().is_some() {
            let mut col = start + 1;
            for (name, first) in self.felix.registers.previews() {
                if col >= width {
                    break;
                }
                let item = truncate_display_width(
                    &format!("{name} {}", first.lines().next().unwrap_or("")),
                    (width - col).min(20),
                );
                buffer.set_text(col, row, &item, style);
                col += display_width(&item) + 2;
            }
            return true;
        }
        if let Some(suggestion) = self
            .felix
            .session
            .prompt_history_hint(self.felix.registers.search_history())
            .filter(|_| line.is_empty())
        {
            let muted = self
                .theme
                .get_style("comment")
                .unwrap_or_else(|| style.clone());
            buffer.set_text(
                start,
                row,
                &truncate_display_width(suggestion, width.saturating_sub(start)),
                &muted,
            );
        } else {
            let visible = first_prompt_row(line, width.saturating_sub(start + 2));
            buffer.set_text(start, row, &visible, style);
        }
        if let Some((items, selected)) = self.felix.session.prompt_completions() {
            self.draw_felix_prompt_candidates(buffer, width, row, items, selected);
        }
        true
    }

    fn draw_felix_prompt_candidates(
        &self,
        buffer: &mut RenderBuffer,
        width: usize,
        bottom: usize,
        items: &[String],
        selected: Option<usize>,
    ) {
        // Helix sizes columns by the longest candidate's UTF-8 length, then
        // clips each item by display width. Items fill columns top to bottom.
        let longest = items.iter().map(String::len).max().unwrap_or(0).max(30);
        let columns = (width / longest).max(1);
        let height = items.len().div_ceil(columns).min(10).min(bottom);
        if height == 0 {
            return;
        }
        let pitch = width / columns;
        let top = bottom - height;
        let normal = self.theme.ui_style.popup.fallback_bg(&self.theme.style);
        let highlight = self.theme.list_selection_style().fallback_bg(&normal);
        for y in top..bottom {
            buffer.fill_ascii_spaces(0, y, width, &normal);
        }
        let page = height * columns;
        let start = selected.unwrap_or(0) / page * page;
        for (index, item) in items.iter().skip(start).take(page).enumerate() {
            let text = truncate_display_width(item, pitch.saturating_sub(2));
            let style = if selected == Some(start + index) {
                &highlight
            } else {
                &normal
            };
            buffer.set_text(index / height * pitch, top + index % height, &text, style);
        }
    }
}
