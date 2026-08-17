//! Searchable, non-destructive help above the currently focused surface.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    config::KeyAction,
    editor::RenderBuffer,
    theme::{SelectionForegroundPriority, Style, SurfacePalette, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    Component, ScreenRect, UiAction,
};

/// Alternate help chord. Shift is allowed for layouts that need it to type `/`.
pub(crate) fn is_keyboard_shortcuts_alias(event: &Event) -> bool {
    matches!(event, Event::Key(key)
        if key.code == KeyCode::Char('/')
            && key.modifiers.difference(KeyModifiers::SHIFT) == KeyModifiers::ALT)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ShortcutTarget {
    Editor(KeyAction),
    Surface(String),
    Workspace(String),
}

/// One complete binding, independent of whether it fits in an action bar.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShortcutEntry {
    pub group: String,
    pub key: String,
    pub label: String,
    pub description: String,
    pub target: Option<ShortcutTarget>,
}

impl ShortcutEntry {
    pub fn new(group: impl Into<String>, key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            group: group.into(),
            key: key.into(),
            label: label.into(),
            description: String::new(),
            target: None,
        }
    }

    pub fn from_actions(context: &str, actions: &[UiAction]) -> Vec<Self> {
        actions
            .iter()
            .filter(|action| action.enabled)
            .map(|action| Self {
                group: if action.group.is_empty() {
                    context.to_owned()
                } else {
                    action.group.clone()
                },
                key: action.key.clone(),
                label: if action.description.is_empty() {
                    action.label.clone()
                } else {
                    action.description.clone()
                },
                description: action.description.clone(),
                target: None,
            })
            .collect()
    }

    pub fn in_context(mut self, context: &str) -> Self {
        if self.group != context && !self.group.starts_with(&format!("{context} · ")) {
            self.group = format!("{context} · {}", self.group);
        }
        self.target = None;
        self
    }
}

/// Hit target emitted by the same layout that paints the help affordance.
#[derive(Debug, Clone)]
pub(crate) struct ShortcutHelpRegion {
    pub rect: ScreenRect,
    pub context: String,
    pub actions: Vec<UiAction>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum ShortcutEvent {
    None,
    Close,
    Activate(ShortcutTarget),
}

#[derive(Debug)]
pub(crate) struct KeyboardShortcuts {
    context: String,
    current: Vec<ShortcutEntry>,
    all: Vec<ShortcutEntry>,
    all_selected: bool,
    query: String,
    searching: bool,
    selected: usize,
}

#[derive(Clone)]
struct DisplayRow {
    entry: Option<usize>,
    key: String,
    text: String,
}

impl KeyboardShortcuts {
    pub fn new(
        context: String,
        mut current: Vec<ShortcutEntry>,
        mut all: Vec<ShortcutEntry>,
    ) -> Self {
        current.push(ShortcutEntry::new(
            "Help",
            "F1 or Alt+/",
            "Toggle keyboard shortcuts",
        ));
        current.sort_by(|a, b| (&a.group, &a.label, &a.key).cmp(&(&b.group, &b.label, &b.key)));
        current.dedup_by(|a, b| a.group == b.group && a.key == b.key && a.label == b.label);
        all.extend(
            current
                .iter()
                .cloned()
                .map(|entry| entry.in_context(&context)),
        );
        all.sort_by(|a, b| (&a.group, &a.label, &a.key).cmp(&(&b.group, &b.label, &b.key)));
        all.dedup_by(|a, b| a.group == b.group && a.key == b.key && a.label == b.label);
        Self {
            context,
            current,
            all,
            all_selected: false,
            query: String::new(),
            searching: false,
            selected: 0,
        }
    }

    fn matching(&self) -> Vec<&ShortcutEntry> {
        let words = self
            .query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let entries = if self.all_selected {
            &self.all
        } else {
            &self.current
        };
        entries
            .iter()
            .filter(|entry| {
                let text = format!(
                    "{} {} {} {}",
                    entry.group, entry.key, entry.label, entry.description
                )
                .to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .collect()
    }

    /// Whether a filtered result contains this complete effective binding.
    pub(crate) fn shows_filtered_binding(&self, key: &str) -> bool {
        !self.query.trim().is_empty()
            && self
                .matching()
                .iter()
                .any(|entry| entry.key.eq_ignore_ascii_case(key))
    }

    fn move_by(&mut self, amount: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(amount)
            .min(self.matching().len().saturating_sub(1));
    }

    fn toggle_scope(&mut self) {
        self.all_selected = !self.all_selected;
        self.selected = 0;
    }

    fn geometry(width: usize, height: usize) -> ScreenRect {
        let width = width.saturating_sub(usize::from(width > 4) * 4).min(96);
        let height = height.saturating_sub(usize::from(height > 4) * 2).min(29);
        ScreenRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn rect(width: usize, height: usize) -> ScreenRect {
        let mut rect = Self::geometry(width, height);
        rect.x = width.saturating_sub(rect.width) / 2;
        rect.y = height.saturating_sub(rect.height) / 2;
        rect
    }

    fn key_width(entries: &[&ShortcutEntry], width: usize) -> usize {
        entries
            .iter()
            .map(|entry| display_width(&entry.key))
            .max()
            .unwrap_or(0)
            .min(width.saturating_sub(12).max(width / 3))
            .min(width / 2)
    }

    fn rows(entries: &[&ShortcutEntry], width: usize) -> (usize, Vec<DisplayRow>) {
        let key_width = Self::key_width(entries, width);
        let label_width = width.saturating_sub(key_width + 2).max(1);
        let mut rows = Vec::new();
        let mut group = "";
        for (index, entry) in entries.iter().enumerate() {
            if group != entry.group {
                group = &entry.group;
                rows.push(DisplayRow {
                    entry: None,
                    key: String::new(),
                    text: group.to_owned(),
                });
            }
            let keys = wrap_cells(&entry.key, key_width.max(1));
            let labels = wrap_cells(&entry.label, label_width);
            for line in 0..keys.len().max(labels.len()) {
                rows.push(DisplayRow {
                    entry: Some(index),
                    key: keys.get(line).cloned().unwrap_or_default(),
                    text: labels.get(line).cloned().unwrap_or_default(),
                });
            }
        }
        (key_width, rows)
    }

    fn first_row(rows: &[DisplayRow], selected: usize, visible: usize) -> usize {
        let first = rows
            .iter()
            .position(|row| row.entry == Some(selected))
            .unwrap_or(0);
        let end = rows
            .iter()
            .rposition(|row| row.entry == Some(selected))
            .unwrap_or(first);
        end.saturating_add(1).saturating_sub(visible).min(first)
    }

    pub fn handle_event(&mut self, event: &Event, width: usize, height: usize) -> ShortcutEvent {
        let page = Self::rect(width, height).height.saturating_sub(8).max(1) as isize;
        match event {
            event if is_keyboard_shortcuts_alias(event) => return ShortcutEvent::Close,
            Event::Key(key) if key.code == KeyCode::F(1) => return ShortcutEvent::Close,
            Event::Key(key) if key.code == KeyCode::Esc => {
                if self.searching || !self.query.is_empty() {
                    self.searching = false;
                    self.query.clear();
                    self.selected = 0;
                } else {
                    return ShortcutEvent::Close;
                }
            }
            Event::Key(key) if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) => {
                self.toggle_scope()
            }
            Event::Key(key) if key.code == KeyCode::Up => self.move_by(-1),
            Event::Key(key) if key.code == KeyCode::Down => self.move_by(1),
            Event::Key(key) if key.code == KeyCode::PageUp => self.move_by(-page),
            Event::Key(key) if key.code == KeyCode::PageDown => self.move_by(page),
            Event::Key(key) if key.code == KeyCode::Enter => {
                if self.searching {
                    self.searching = false;
                } else if let Some(target) = self
                    .matching()
                    .get(self.selected)
                    .and_then(|entry| entry.target.clone())
                {
                    return ShortcutEvent::Activate(target);
                }
            }
            Event::Key(key) if key.code == KeyCode::Backspace && self.searching => {
                if let Some((index, _)) = self.query.grapheme_indices(true).next_back() {
                    self.query.truncate(index);
                }
                self.selected = 0;
            }
            Event::Key(key)
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('u') =>
            {
                self.query.clear();
                self.selected = 0;
            }
            Event::Key(key)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                match key.code {
                    KeyCode::Char('q') if !self.searching => return ShortcutEvent::Close,
                    KeyCode::Char('j') if !self.searching => self.move_by(1),
                    KeyCode::Char('k') if !self.searching => self.move_by(-1),
                    KeyCode::Char('/') if !self.searching => self.searching = true,
                    KeyCode::Char(c) => {
                        self.searching = true;
                        if self.query.len() < 4096 {
                            self.query.push(c);
                        }
                        self.selected = 0;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                self.searching = true;
                for c in text.chars().filter(|c| !c.is_control()) {
                    if self.query.len() >= 4096 {
                        break;
                    }
                    self.query.push(c);
                }
                self.selected = 0;
            }
            Event::Mouse(mouse) => {
                let rect = Self::rect(width, height);
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.move_by(-3),
                    MouseEventKind::ScrollDown => self.move_by(3),
                    MouseEventKind::Down(MouseButton::Left)
                        if rect.contains(usize::from(mouse.column), usize::from(mouse.row)) =>
                    {
                        let row = usize::from(mouse.row).saturating_sub(rect.y);
                        if row == 2 {
                            let all = usize::from(mouse.column).saturating_sub(rect.x + 2) >= 16;
                            if self.all_selected != all {
                                self.toggle_scope();
                            }
                        } else if row == 3 {
                            self.searching = true;
                        } else if row >= 5 && row < rect.height.saturating_sub(3) {
                            let entries = self.matching();
                            let (_, rows) = Self::rows(&entries, rect.width.saturating_sub(4));
                            let first = Self::first_row(
                                &rows,
                                self.selected,
                                rect.height.saturating_sub(8),
                            );
                            if let Some(index) = rows.get(first + row - 5).and_then(|row| row.entry)
                            {
                                self.selected = index;
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        ShortcutEvent::None
    }

    pub fn render(&self, buffer: &mut RenderBuffer, theme: &Theme) -> anyhow::Result<()> {
        let rect = Self::rect(buffer.width, buffer.height);
        if rect.width < 4 || rect.height < 3 {
            return Ok(());
        }
        let palette = SurfacePalette::new(theme, &theme.ui_style.dialog);
        let inner = rect.width - 2;
        let mut dialog = Dialog::new(
            Some("Keyboard shortcuts".to_owned()),
            rect.x,
            rect.y,
            inner,
            rect.height - 2,
            &palette.surface,
            BorderStyle::Rounded,
            theme,
        )
        .with_surface_theme(theme, SurfaceRole::Dialog)
        .with_left_aligned_title();
        if inner > display_width(" Keyboard shortcuts ") + display_width(" F1 or Alt+/ ") {
            dialog.set_header_status(Some("F1 or Alt+/".to_owned()));
        }
        dialog.draw(buffer)?;
        let paint = |buffer: &mut RenderBuffer, row: usize, text: &str, style: &Style| {
            if row < rect.height - 1 {
                buffer.set_text(
                    rect.x + 2,
                    rect.y + row,
                    &truncate_display_width(text, rect.width.saturating_sub(4)),
                    style,
                );
            }
        };
        if rect.width < 16 || rect.height < 10 {
            paint(buffer, 1, "F1 or Alt+/ · Esc back", &palette.secondary);
            return Ok(());
        }
        paint(buffer, 1, &self.context, &palette.muted);
        paint(
            buffer,
            2,
            if self.all_selected {
                " This context   [All Red keys]"
            } else {
                "[This context]   All Red keys"
            },
            &palette.accent,
        );
        let query = if self.query.is_empty() {
            "Find an action or key…"
        } else {
            &self.query
        };
        paint(
            buffer,
            3,
            &format!("/ {query}{}", if self.searching { "▏" } else { "" }),
            &palette.secondary,
        );
        paint(
            buffer,
            4,
            &"─".repeat(rect.width.saturating_sub(4)),
            &palette.divider,
        );
        let entries = self.matching();
        let selected = self.selected.min(entries.len().saturating_sub(1));
        let content_width = rect.width.saturating_sub(4);
        let (key_width, rows) = Self::rows(&entries, content_width);
        let visible = rect.height.saturating_sub(8);
        let first = Self::first_row(&rows, selected, visible);
        for (offset, row) in rows.iter().skip(first).take(visible).enumerate() {
            let y = rect.y + 5 + offset;
            if row.entry.is_none() {
                paint(buffer, 5 + offset, &row.text, &palette.muted);
                continue;
            }
            let style = if row.entry == Some(selected) {
                theme.selected_style(
                    &palette.surface,
                    &theme.list_selection_style(),
                    SelectionForegroundPriority::Selection,
                )
            } else {
                palette.surface.clone()
            };
            buffer.set_text(rect.x + 1, y, &" ".repeat(inner), &style);
            buffer.set_text(
                rect.x + 2,
                y,
                &row.key,
                &Style {
                    bold: true,
                    fg: palette.accent.fg,
                    ..style.clone()
                },
            );
            buffer.set_text(rect.x + 2 + key_width + 2, y, &row.text, &style);
        }
        if entries.is_empty() {
            paint(buffer, 5, "No matching shortcuts", &palette.muted);
        }
        let detail = entries
            .get(selected)
            .map(|entry| {
                if entry.description.is_empty() {
                    entry.label.as_str()
                } else {
                    entry.description.as_str()
                }
            })
            .unwrap_or("");
        paint(buffer, rect.height - 3, detail, &palette.muted);
        let executable = entries
            .get(selected)
            .is_some_and(|entry| entry.target.is_some());
        let footer = format!(
            "{}Tab scope · / find · Esc back  {}/{}",
            if executable { "↵ run · " } else { "" },
            if entries.is_empty() { 0 } else { selected + 1 },
            entries.len()
        );
        paint(buffer, rect.height - 2, &footer, &palette.secondary);
        Ok(())
    }
}

fn wrap_cells(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = display_width(grapheme);
        if used > 0 && used + cells > width {
            lines.push(String::new());
            used = 0;
        }
        if cells <= width {
            lines
                .last_mut()
                .expect("one line exists")
                .push_str(grapheme);
            used += cells;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn search_scope_and_escape_do_not_activate_actions() {
        let mut current = ShortcutEntry::new("Pane", "q", "Close pane");
        current.target = Some(ShortcutTarget::Surface("close".into()));
        let mut help = KeyboardShortcuts::new(
            "Agent".into(),
            vec![current],
            vec![ShortcutEntry::new("Editor", "Space f", "Format document")],
        );
        assert_eq!(
            help.handle_event(&key(KeyCode::Char('/')), 80, 24),
            ShortcutEvent::None
        );
        help.handle_event(&Event::Paste("format".into()), 80, 24);
        assert!(help.matching().is_empty());
        help.handle_event(&key(KeyCode::Tab), 80, 24);
        assert_eq!(help.matching()[0].label, "Format document");
        assert_eq!(
            help.handle_event(&key(KeyCode::Esc), 80, 24),
            ShortcutEvent::None
        );
        assert_eq!(
            help.handle_event(&key(KeyCode::Esc), 80, 24),
            ShortcutEvent::Close
        );
    }

    #[test]
    fn alternate_help_chord_is_exact_and_closes_during_search() {
        let alternate = |modifiers| Event::Key(KeyEvent::new(KeyCode::Char('/'), modifiers));
        assert!(is_keyboard_shortcuts_alias(&alternate(KeyModifiers::ALT)));
        assert!(is_keyboard_shortcuts_alias(&alternate(
            KeyModifiers::ALT | KeyModifiers::SHIFT
        )));
        assert!(!is_keyboard_shortcuts_alias(&alternate(KeyModifiers::NONE)));
        assert!(!is_keyboard_shortcuts_alias(&alternate(
            KeyModifiers::ALT | KeyModifiers::CONTROL
        )));
        assert!(!is_keyboard_shortcuts_alias(&Event::Key(KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::ALT
        ))));

        let mut help = KeyboardShortcuts::new("Editor".into(), Vec::new(), Vec::new());
        help.handle_event(&key(KeyCode::Char('/')), 80, 24);
        help.handle_event(&Event::Paste("Alt+/".into()), 80, 24);
        assert_eq!(help.matching()[0].key, "F1 or Alt+/");
        assert_eq!(
            help.handle_event(&alternate(KeyModifiers::ALT), 80, 24),
            ShortcutEvent::Close
        );
    }

    #[test]
    fn dialog_displays_both_help_chords() {
        let theme = Theme::default();
        let help = KeyboardShortcuts::new("Editor".into(), Vec::new(), Vec::new());
        let mut buffer = RenderBuffer::new(80, 24, &theme.style);
        help.render(&mut buffer, &theme).unwrap();
        let frame = buffer.cells.iter().map(|cell| cell.c).collect::<String>();
        assert!(frame.matches("F1 or Alt+/").count() >= 2, "{frame}");
    }

    #[test]
    fn rendering_and_wrapping_fit_tiny_and_unicode_viewports() {
        let theme = Theme::default();
        let help = KeyboardShortcuts::new(
            "Agent · Navigation".into(),
            vec![ShortcutEntry::new(
                "Navigation",
                "Ctrl+w 漢",
                "A long action 👩‍💻 with a complete label",
            )],
            Vec::new(),
        );
        for width in 0..100 {
            for height in [0, 2, 8, 12, 24] {
                let mut buffer = RenderBuffer::new(width, height, &theme.style);
                help.render(&mut buffer, &theme).unwrap();
                assert_eq!(buffer.cells.len(), width * height);
            }
        }
        assert_eq!(wrap_cells("ab漢cd", 3).concat(), "ab漢cd");
    }
}
