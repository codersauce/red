//! Three-bucket terminal panel for arranging status-line sections with live preview.

use crossterm::event::{Event, KeyCode, KeyModifiers};

use crate::{
    config::{KeyAction, PickerIconStyle, StatuslineConfig, StatuslineSection},
    editor::{
        rendering::{statusline_section_icon, statusline_slot_style},
        Action, Editor, RenderBuffer,
    },
    theme::{SelectionForegroundPriority, Style, Theme},
    unicode_utils::truncate_display_width,
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    Component, IconCatalog,
};

const WIDE_LAYOUT_MIN_WIDTH: usize = 72;
const DESIRED_WIDTH: usize = 96;
const BODY_ROWS: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bucket {
    Available,
    Left,
    Right,
}

impl Bucket {
    const ALL: [Self; 3] = [Self::Available, Self::Left, Self::Right];

    const fn index(self) -> usize {
        match self {
            Self::Available => 0,
            Self::Left => 1,
            Self::Right => 2,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Available => "Available",
            Self::Left => "Left · edge → center",
            Self::Right => "Right · edge → center",
        }
    }
}

/// Editor-owned modal that edits a draft while the real status line previews it.
pub struct StatuslineLayoutPanel {
    dialog: Dialog,
    original: StatuslineConfig,
    draft: StatuslineConfig,
    focus: Bucket,
    selected: [usize; 3],
    scroll: [usize; 3],
    theme: Theme,
}

impl StatuslineLayoutPanel {
    pub fn new(editor: &Editor) -> Self {
        let (width, height) = panel_size(editor.vwidth(), editor.vheight());
        let x = editor.vwidth().saturating_sub(width + 2) / 2;
        let y = editor.vheight().saturating_sub(height + 2) / 2;
        let style = editor.theme.ui_style.dialog.clone();
        let mut dialog = Dialog::new(
            Some("Statusline Layout · live preview below".to_string()),
            x,
            y,
            width,
            height,
            &style,
            BorderStyle::Rounded,
            &editor.theme,
        )
        .with_surface_theme(&editor.theme, SurfaceRole::Dialog);
        dialog.set_footer(Some(
            "h/l focus · j/k select · H left · L right · K/J reorder · x remove · s save · esc cancel"
                .to_string(),
        ));

        let original = editor.statusline_config().clone();
        let has_available = StatuslineSection::ALL
            .into_iter()
            .any(|section| !original.left.contains(&section) && !original.right.contains(&section));
        Self {
            dialog,
            draft: original.clone(),
            original,
            focus: if has_available {
                Bucket::Available
            } else {
                Bucket::Left
            },
            selected: [0; 3],
            scroll: [0; 3],
            theme: editor.theme.clone(),
        }
    }

    fn sections(&self, bucket: Bucket) -> Vec<StatuslineSection> {
        match bucket {
            Bucket::Available => StatuslineSection::ALL
                .into_iter()
                .filter(|section| {
                    !self.draft.left.contains(section) && !self.draft.right.contains(section)
                })
                .collect(),
            Bucket::Left => self.draft.left.clone(),
            Bucket::Right => self.draft.right.clone(),
        }
    }

    fn selected_section(&self) -> Option<StatuslineSection> {
        self.sections(self.focus)
            .get(self.selected[self.focus.index()])
            .copied()
    }

    fn clamp_selection(&mut self, bucket: Bucket) {
        let len = self.sections(bucket).len();
        let selected = &mut self.selected[bucket.index()];
        *selected = (*selected).min(len.saturating_sub(1));
        self.ensure_selection_visible(bucket);
    }

    fn visible_rows(&self) -> usize {
        self.dialog.height.saturating_sub(1).max(1)
    }

    fn ensure_selection_visible(&mut self, bucket: Bucket) {
        let rows = self.visible_rows();
        let index = bucket.index();
        let selected = self.selected[index];
        let len = self.sections(bucket).len();
        if selected < self.scroll[index] {
            self.scroll[index] = selected;
        } else if selected >= self.scroll[index] + rows {
            self.scroll[index] = selected + 1 - rows;
        }
        self.scroll[index] = self.scroll[index].min(len.saturating_sub(rows));
    }

    fn focus_by(&mut self, delta: isize) {
        let next = (self.focus.index() as isize + delta).clamp(0, 2) as usize;
        self.focus = Bucket::ALL[next];
        self.clamp_selection(self.focus);
    }

    fn select_by(&mut self, delta: isize) {
        let len = self.sections(self.focus).len();
        if len == 0 {
            self.selected[self.focus.index()] = 0;
            return;
        }
        let current = self.selected[self.focus.index()] as isize;
        self.selected[self.focus.index()] = (current + delta).clamp(0, len as isize - 1) as usize;
        self.ensure_selection_visible(self.focus);
    }

    fn move_to(&mut self, target: Bucket) -> bool {
        let Some(section) = self.selected_section() else {
            return false;
        };
        let previous = self.draft.clone();
        self.draft.left.retain(|candidate| *candidate != section);
        self.draft.right.retain(|candidate| *candidate != section);
        match target {
            Bucket::Available => {}
            Bucket::Left => self.draft.left.push(section),
            Bucket::Right => self.draft.right.push(section),
        }
        self.focus = target;
        let target_len = self.sections(target).len();
        self.selected[target.index()] = target_len.saturating_sub(1);
        for bucket in Bucket::ALL {
            self.clamp_selection(bucket);
        }
        self.draft != previous
    }

    fn reorder(&mut self, delta: isize) -> bool {
        let selected = self.selected[self.focus.index()];
        let list = match self.focus {
            Bucket::Available => return false,
            Bucket::Left => &mut self.draft.left,
            Bucket::Right => &mut self.draft.right,
        };
        if list.len() < 2 {
            return false;
        }
        let next = (selected as isize + delta).clamp(0, list.len() as isize - 1) as usize;
        if next == selected {
            return false;
        }
        list.swap(selected, next);
        self.selected[self.focus.index()] = next;
        self.ensure_selection_visible(self.focus);
        true
    }

    fn preview_action(&self) -> Option<KeyAction> {
        Some(KeyAction::Single(Action::PreviewStatuslineLayout(
            self.draft.clone(),
        )))
    }

    const fn refresh_action() -> Option<KeyAction> {
        Some(KeyAction::Single(Action::Refresh))
    }

    fn draw_wide(&self, buffer: &mut RenderBuffer, x: usize, y: usize, width: usize, rows: usize) {
        let available = width.saturating_sub(2);
        let first_width = available / 3;
        let second_width = available / 3;
        let third_width = available.saturating_sub(first_width + second_width);
        let panes = [
            (Bucket::Available, x, first_width),
            (Bucket::Left, x + first_width + 1, second_width),
            (
                Bucket::Right,
                x + first_width + second_width + 2,
                third_width,
            ),
        ];

        for separator_x in [x + first_width, x + first_width + second_width + 1] {
            buffer.fill_rect(
                separator_x,
                y,
                1,
                rows,
                '│',
                &self.theme.ui_style.dialog_border,
                &self.theme,
            );
        }
        for (bucket, pane_x, pane_width) in panes {
            self.draw_pane(buffer, bucket, pane_x, y, pane_width, rows);
        }
    }

    fn draw_narrow(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        rows: usize,
    ) {
        let tab_width = width / Bucket::ALL.len();
        for (index, bucket) in Bucket::ALL.into_iter().enumerate() {
            let tab_x = x + index * tab_width;
            let current_width = if index + 1 == Bucket::ALL.len() {
                width.saturating_sub(index * tab_width)
            } else {
                tab_width
            };
            let label = match bucket {
                Bucket::Available => "Available",
                Bucket::Left => "Left",
                Bucket::Right => "Right",
            };
            let style = if bucket == self.focus {
                self.theme.selected_style(
                    &self.theme.ui_style.dialog,
                    &self.theme.ui_style.picker_selected_item,
                    SelectionForegroundPriority::Selection,
                )
            } else {
                self.theme.ui_style.muted.clone()
            };
            buffer.fill_rect(tab_x, y, current_width, 1, ' ', &style, &self.theme);
            buffer.set_text(
                tab_x,
                y,
                &truncate_display_width(&format!(" {label}"), current_width),
                &style,
            );
        }
        self.draw_rows(buffer, self.focus, x, y + 1, width, rows.saturating_sub(1));
    }

    fn draw_pane(
        &self,
        buffer: &mut RenderBuffer,
        bucket: Bucket,
        x: usize,
        y: usize,
        width: usize,
        rows: usize,
    ) {
        let focused = bucket == self.focus;
        let style = if focused {
            Style {
                bold: true,
                ..self.theme.ui_style.dialog_title.clone()
            }
        } else {
            self.theme.ui_style.muted.clone()
        };
        buffer.set_text(
            x,
            y,
            &truncate_display_width(&format!(" {}", bucket.label()), width),
            &style,
        );
        self.draw_rows(buffer, bucket, x, y + 1, width, rows.saturating_sub(1));
    }

    fn draw_rows(
        &self,
        buffer: &mut RenderBuffer,
        bucket: Bucket,
        x: usize,
        y: usize,
        width: usize,
        rows: usize,
    ) {
        if rows == 0 {
            return;
        }
        let sections = self.sections(bucket);
        if sections.is_empty() {
            buffer.set_text(
                x + usize::from(width > 1),
                y,
                &truncate_display_width("(empty)", width.saturating_sub(1)),
                &self.theme.ui_style.muted,
            );
            return;
        }

        let scroll = self.scroll[bucket.index()];
        for (display_index, (index, section)) in sections
            .into_iter()
            .enumerate()
            .skip(scroll)
            .take(rows)
            .enumerate()
        {
            let row_y = y + display_index;
            let selected = bucket == self.focus && self.selected[bucket.index()] == index;
            let row_style = if selected {
                self.theme.selected_style(
                    &self.theme.ui_style.dialog,
                    &self.theme.ui_style.picker_selected_item,
                    SelectionForegroundPriority::Selection,
                )
            } else {
                self.theme.ui_style.dialog.clone()
            };
            buffer.fill_rect(x, row_y, width, 1, ' ', &row_style, &self.theme);

            let slot = if bucket == Bucket::Available {
                "+".to_string()
            } else {
                (index + 1).to_string()
            };
            let slot_style = if bucket == Bucket::Available {
                self.theme.ui_style.muted.clone().with_bg(row_style.bg)
            } else {
                statusline_slot_style(&self.theme, index)
            };
            let tag = format!(" {slot} ");
            buffer.set_text(x, row_y, &tag, &slot_style);

            let text_x = x + 3;
            let text = section_label(section, self.draft.icons.style);
            buffer.set_text(
                text_x,
                row_y,
                &truncate_display_width(&text, width.saturating_sub(3)),
                &row_style,
            );
        }
    }
}

impl Component for StatuslineLayoutPanel {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        let (width, height) = panel_size(viewport_width, viewport_height);
        self.dialog.width = width;
        self.dialog.height = height;
        self.dialog.x = viewport_width.saturating_sub(width + 2) / 2;
        self.dialog.y = viewport_height.saturating_sub(height + 2) / 2;
        for bucket in Bucket::ALL {
            self.ensure_selection_visible(bucket);
        }
        true
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let x = self.dialog.x + 1;
        let y = self.dialog.y + 1;
        if self.dialog.width >= WIDE_LAYOUT_MIN_WIDTH {
            self.draw_wide(buffer, x, y, self.dialog.width, self.dialog.height);
        } else {
            self.draw_narrow(buffer, x, y, self.dialog.width, self.dialog.height);
        }
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else {
            return None;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(
                KeyAction::Single(Action::CancelStatuslineLayout(self.original.clone())),
            ),
            (KeyCode::Char('s'), _) => Some(KeyAction::Single(Action::SaveStatuslineLayout(
                self.draft.clone(),
            ))),
            (KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab, _) => {
                self.focus_by(-1);
                Self::refresh_action()
            }
            (KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab, _) => {
                self.focus_by(1);
                Self::refresh_action()
            }
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.select_by(-1);
                Self::refresh_action()
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.select_by(1);
                Self::refresh_action()
            }
            (KeyCode::Char('H'), _) => {
                if self.move_to(Bucket::Left) {
                    self.preview_action()
                } else {
                    Self::refresh_action()
                }
            }
            (KeyCode::Char('L'), _) => {
                if self.move_to(Bucket::Right) {
                    self.preview_action()
                } else {
                    Self::refresh_action()
                }
            }
            (KeyCode::Char('x'), _) => {
                if self.move_to(Bucket::Available) {
                    self.preview_action()
                } else {
                    Self::refresh_action()
                }
            }
            (KeyCode::Char('K'), _) => {
                if self.reorder(-1) {
                    self.preview_action()
                } else {
                    Self::refresh_action()
                }
            }
            (KeyCode::Char('J'), _) => {
                if self.reorder(1) {
                    self.preview_action()
                } else {
                    Self::refresh_action()
                }
            }
            _ => None,
        }
    }
}

fn panel_size(viewport_width: usize, viewport_height: usize) -> (usize, usize) {
    (
        DESIRED_WIDTH.min(viewport_width.saturating_sub(2)).max(1),
        BODY_ROWS.min(viewport_height.saturating_sub(2)).max(1),
    )
}

fn section_label(section: StatuslineSection, icon_style: PickerIconStyle) -> String {
    let icon = match section {
        StatuslineSection::Filename => IconCatalog::file("src/editor.rs", icon_style).glyph,
        StatuslineSection::Syntax => IconCatalog::file("src/lib.rs", icon_style).glyph,
        _ => statusline_section_icon(section, icon_style),
    };
    if icon.is_empty() {
        section.label().to_string()
    } else {
        format!("{icon}  {}", section.label())
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};

    fn editor(width: usize, height: usize) -> Editor {
        editor_with_config(width, height, Config::default())
    }

    fn editor_with_config(width: usize, height: usize, config: Config) -> Editor {
        Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(Some("src/lib.rs".to_string()), String::new())],
        )
        .unwrap()
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn rendered_text(buffer: &RenderBuffer) -> String {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| row.iter().map(|cell| cell.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn moving_an_available_field_to_the_right_previews_edge_to_center_order() {
        let mut config = Config::default();
        config
            .statusline
            .left
            .retain(|section| *section != StatuslineSection::GitBranch);
        let editor = editor_with_config(100, 18, config);
        let mut panel = StatuslineLayoutPanel::new(&editor);

        let action = panel.handle_event(&key(KeyCode::Char('L'))).unwrap();

        let KeyAction::Single(Action::PreviewStatuslineLayout(config)) = action else {
            panic!("expected a live-preview action");
        };
        assert_eq!(config.right.last(), Some(&StatuslineSection::GitBranch));
        assert_eq!(panel.focus, Bucket::Right);
    }

    #[test]
    fn reorder_and_remove_update_the_draft_without_touching_the_original() {
        let editor = editor(100, 18);
        let mut panel = StatuslineLayoutPanel::new(&editor);
        let original = panel.original.clone();
        panel.focus = Bucket::Left;
        panel.selected[Bucket::Left.index()] = 1;

        assert!(matches!(
            panel.handle_event(&key(KeyCode::Char('K'))),
            Some(KeyAction::Single(Action::PreviewStatuslineLayout(_)))
        ));
        assert_eq!(panel.draft.left[0], StatuslineSection::GitBranch);
        assert!(matches!(
            panel.handle_event(&key(KeyCode::Char('x'))),
            Some(KeyAction::Single(Action::PreviewStatuslineLayout(_)))
        ));
        assert_eq!(panel.original, original);
        assert!(!panel.draft.left.contains(&StatuslineSection::GitBranch));
    }

    #[test]
    fn wide_and_narrow_layouts_keep_all_three_buckets_reachable() {
        let wide_editor = editor(100, 18);
        let wide = StatuslineLayoutPanel::new(&wide_editor);
        let mut wide_buffer = RenderBuffer::new(100, 18, &Style::default());
        wide.draw(&mut wide_buffer).unwrap();
        let wide_text = rendered_text(&wide_buffer);
        assert!(wide_text.contains("Available"));
        assert!(wide_text.contains("Left · edge → center"));
        assert!(wide_text.contains("Right · edge → center"));

        let narrow_editor = editor(42, 14);
        let mut narrow = StatuslineLayoutPanel::new(&narrow_editor);
        narrow.focus = Bucket::Right;
        let mut narrow_buffer = RenderBuffer::new(42, 14, &Style::default());
        narrow.draw(&mut narrow_buffer).unwrap();
        let narrow_text = rendered_text(&narrow_buffer);
        assert!(narrow_text.contains("Available"));
        assert!(narrow_text.contains("Left"));
        assert!(narrow_text.contains("Right"));
        assert!(narrow_text.contains("Cursor position"));
    }

    #[test]
    fn repository_scrolls_to_keep_late_fields_visible() {
        let editor = editor(100, 18);
        let mut panel = StatuslineLayoutPanel::new(&editor);
        assert_eq!(panel.focus, Bucket::Available);

        for _ in 0..StatuslineSection::ALL.len() {
            panel.handle_event(&key(KeyCode::Down));
        }

        let mut buffer = RenderBuffer::new(100, 18, &Style::default());
        panel.draw(&mut buffer).unwrap();
        let text = rendered_text(&buffer);

        assert!(panel.scroll[Bucket::Available.index()] > 0);
        assert!(text.contains("Clock"));
    }
}
