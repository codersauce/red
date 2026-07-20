//! Delayed, width-aware rendering of available continuations for pending key prefixes.
//!
//! Keymap hints are derived from effective configured mappings and command metadata. The
//! renderer is purely presentational; prefix timing and key resolution remain editor
//! responsibilities.

use crate::{
    command_palette::KeymapHint,
    editor::RenderBuffer,
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{dialog::BorderStyle, Component, Dialog};

const COLUMN_GAP: usize = 3;
const KEY_GAP: usize = 1;
const CONTINUATION_WIDTH: usize = 2;
const MIN_LABEL_WIDTH: usize = 18;
const MAX_COLUMNS: usize = 3;
const MAX_KEY_WIDTH: usize = 20;

fn foreground_style(base: &Style, semantic: &Style) -> Style {
    Style {
        fg: semantic.fg.or(base.fg),
        bg: base.bg,
        bold: base.bold || semantic.bold,
        italic: base.italic || semantic.italic,
    }
}

pub(crate) fn draw_keymap_hints(
    buffer: &mut RenderBuffer,
    theme: &Theme,
    prefix: &str,
    hints: &[KeymapHint],
) -> anyhow::Result<()> {
    let available_width = buffer.width.saturating_sub(2);
    let available_height = buffer.height.saturating_sub(2);
    if hints.is_empty() || available_width < 12 || available_height < 4 {
        return Ok(());
    }

    let max_inner_width = available_width.saturating_sub(2);
    let max_key_width = hints
        .iter()
        .map(|hint| display_width(&hint.key))
        .max()
        .unwrap_or(1)
        .min(MAX_KEY_WIDTH)
        .min(max_inner_width.saturating_sub(KEY_GAP + CONTINUATION_WIDTH + 1));
    let min_entry_width = (max_key_width + KEY_GAP + CONTINUATION_WIDTH + MIN_LABEL_WIDTH)
        .min(max_inner_width)
        .max(1);
    let columns = ((max_inner_width + COLUMN_GAP) / (min_entry_width + COLUMN_GAP))
        .clamp(1, MAX_COLUMNS)
        .min(hints.len());
    let max_rows = available_height.saturating_sub(2).max(1);
    let rows = hints.len().div_ceil(columns).min(max_rows);
    let visible = rows * columns;
    let key_widths = (0..columns)
        .map(|column| {
            hints
                .iter()
                .skip(column * rows)
                .take(rows)
                .map(|hint| display_width(&hint.key))
                .max()
                .unwrap_or(1)
                .min(max_key_width)
        })
        .collect::<Vec<_>>();
    let preferred_entry_width = hints
        .iter()
        .take(visible)
        .enumerate()
        .map(|(index, hint)| {
            key_widths[index / rows] + KEY_GAP + CONTINUATION_WIDTH + display_width(&hint.label)
        })
        .max()
        .unwrap_or(1);
    let max_entry_width =
        max_inner_width.saturating_sub(columns.saturating_sub(1) * COLUMN_GAP) / columns;
    let entry_width = preferred_entry_width.min(max_entry_width);
    let title = if visible < hints.len() {
        format!("{prefix} · keymaps ({}/{})", visible, hints.len())
    } else {
        format!("{prefix} · keymaps")
    };
    let inner_width = (columns * entry_width + columns.saturating_sub(1) * COLUMN_GAP)
        .max(display_width(&title).saturating_add(2))
        .min(max_inner_width);
    let outer_width = inner_width + 2;
    let outer_height = rows + 2;
    let x = buffer.width.saturating_sub(outer_width + 1);
    let y = buffer.height.saturating_sub(2).saturating_sub(outer_height);
    let title = truncate_display_width(&title, inner_width.saturating_sub(2));
    let dialog = Dialog::new(
        Some(title),
        x,
        y,
        inner_width,
        rows,
        &theme.ui_style.popup,
        BorderStyle::Single,
        theme,
    )
    .with_border_draw_style(&theme.ui_style.popup_border)
    .with_title_style(&theme.ui_style.popup_title);
    dialog.draw(buffer)?;

    let item_style = &theme.ui_style.picker_item;
    let key_semantic = theme
        .colors
        .get("peekViewResult.lineForeground")
        .copied()
        .map(|fg| Style {
            fg: Some(fg),
            ..Style::default()
        })
        .unwrap_or_else(|| theme.ui_style.muted.clone());
    let label_semantic = theme
        .get_style("markup.underline.link")
        .or_else(|| {
            theme
                .colors
                .get("peekViewResult.fileForeground")
                .copied()
                .map(|fg| Style {
                    fg: Some(fg),
                    ..Style::default()
                })
        })
        .or_else(|| theme.get_style("string.other.link"))
        .unwrap_or_else(|| theme.ui_style.picker_prompt.clone());
    let key_style = foreground_style(item_style, &key_semantic);
    let label_style = foreground_style(item_style, &label_semantic);
    let mut group_style = label_style.clone();
    group_style.bold |= theme.ui_style.popup_title.bold;
    let blank_row = " ".repeat(inner_width);
    for row in 0..rows {
        buffer.set_text(x + 1, y + 1 + row, &blank_row, item_style);
    }

    for (index, hint) in hints.iter().take(visible).enumerate() {
        let column = index / rows;
        let row = index % rows;
        let entry_x = x + 1 + column * (entry_width + COLUMN_GAP);
        let entry_y = y + 1 + row;
        let key_width = key_widths[column];
        let key = truncate_display_width(&hint.key, key_width);
        buffer.set_text(entry_x, entry_y, &key, &key_style);

        let label_x = entry_x + key_width + KEY_GAP;
        let label_width = entry_width.saturating_sub(key_width + KEY_GAP + CONTINUATION_WIDTH);
        let label = truncate_display_width(&hint.label, label_width);
        let item_label_style = if hint.is_group {
            &group_style
        } else {
            &label_style
        };
        buffer.set_text(label_x, entry_y, &label, item_label_style);
        if hint.is_group {
            buffer.set_text(label_x + display_width(&label), entry_y, " …", &group_style);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{color::Color, theme::Style};

    fn row(buffer: &RenderBuffer, y: usize) -> String {
        let cells = &buffer.cells[y * buffer.width..(y + 1) * buffer.width];
        let mut text = String::new();
        let mut column = 0;
        while let Some(cell) = cells.get(column) {
            text.push_str(&cell.text);
            column += display_width(&cell.text).max(1);
        }
        text
    }

    fn column(row: &str, needle: &str) -> Option<usize> {
        row.find(needle).map(|index| display_width(&row[..index]))
    }

    #[test]
    fn keymap_hints_align_and_style_keys_groups_and_labels_independently() {
        let item_background = Color::Rgb {
            r: 20,
            g: 22,
            b: 24,
        };
        let label_color = Color::Rgb {
            r: 60,
            g: 210,
            b: 140,
        };
        let key_color = Color::Rgb {
            r: 110,
            g: 120,
            b: 150,
        };
        let mut theme = Theme::default();
        theme
            .colors
            .insert("peekViewResult.fileForeground".to_string(), label_color);
        theme
            .colors
            .insert("peekViewResult.lineForeground".to_string(), key_color);
        theme.ui_style.picker_item = Style {
            fg: Some(Color::Rgb { r: 0, g: 180, b: 0 }),
            bg: Some(item_background),
            ..Style::default()
        };
        theme.ui_style.picker_prompt = Style {
            fg: Some(Color::Rgb {
                r: 240,
                g: 220,
                b: 20,
            }),
            bg: Some(Color::Rgb { r: 0, g: 0, b: 200 }),
            bold: true,
            ..Style::default()
        };
        theme.ui_style.muted = Style {
            fg: Some(Color::Rgb {
                r: 100,
                g: 100,
                b: 100,
            }),
            bg: Some(Color::Rgb { r: 200, g: 0, b: 0 }),
            ..Style::default()
        };
        theme.ui_style.popup_title = Style {
            fg: Some(Color::Rgb {
                r: 20,
                g: 220,
                b: 220,
            }),
            bg: Some(Color::Rgb {
                r: 200,
                g: 0,
                b: 200,
            }),
            bold: true,
            ..Style::default()
        };
        let hints = vec![
            KeymapHint {
                key: "?".to_string(),
                label: "All commands".to_string(),
                is_group: false,
            },
            KeymapHint {
                key: "Ctrl-w".to_string(),
                label: "Windows".to_string(),
                is_group: true,
            },
            KeymapHint {
                key: "漢".to_string(),
                label: "Unicode action".to_string(),
                is_group: false,
            },
        ];
        let mut buffer = RenderBuffer::new(48, 10, &Style::default());

        draw_keymap_hints(&mut buffer, &theme, "Space", &hints).unwrap();

        let rows = (0..buffer.height)
            .map(|index| row(&buffer, index))
            .collect::<Vec<_>>();
        let leaf_y = rows
            .iter()
            .position(|row| row.contains("All commands"))
            .unwrap();
        let group_y = rows.iter().position(|row| row.contains("Windows")).unwrap();
        let unicode_y = rows
            .iter()
            .position(|row| row.contains("Unicode action"))
            .unwrap();
        let leaf = &rows[leaf_y];
        let group = &rows[group_y];
        let unicode = &rows[unicode_y];
        let label_x = column(leaf, "All commands").unwrap();

        assert_eq!(column(group, "Windows"), Some(label_x));
        assert_eq!(column(unicode, "Unicode action"), Some(label_x));
        assert!(group.contains("Windows …"));
        assert!(!group.contains("… Windows"));

        let key_x = column(leaf, "?").unwrap();
        let group_key_x = column(group, "Ctrl-w").unwrap();
        let marker_x = column(group, "…").unwrap();
        assert_eq!(key_x, group_key_x);
        assert_eq!(
            buffer.cells[leaf_y * buffer.width + key_x].style.fg,
            Some(key_color)
        );
        assert_eq!(
            buffer.cells[leaf_y * buffer.width + key_x].style.bg,
            Some(item_background)
        );
        assert_eq!(
            buffer.cells[leaf_y * buffer.width + label_x].style.fg,
            Some(label_color)
        );
        assert_eq!(
            buffer.cells[leaf_y * buffer.width + label_x].style.bg,
            Some(item_background)
        );
        assert_eq!(
            buffer.cells[group_y * buffer.width + marker_x].style.fg,
            Some(label_color)
        );
        assert_eq!(
            buffer.cells[group_y * buffer.width + marker_x].style.bg,
            Some(item_background)
        );
        assert_eq!(
            buffer.cells[group_y * buffer.width + label_x].style.fg,
            Some(label_color)
        );
        assert_eq!(
            buffer.cells[group_y * buffer.width + label_x].style.bg,
            Some(item_background)
        );
    }

    #[test]
    fn keymap_hints_size_keys_per_column_and_keep_action_spacing_compact() {
        let hints = [
            ("?", "All commands", false),
            ("a", "Select all", false),
            ("b", "Buffer picker", false),
            ("Space", "Next buffer", false),
            ("Ctrl-w", "Windows", true),
            ("g", "Project search", false),
        ]
        .into_iter()
        .map(|(key, label, is_group)| KeymapHint {
            key: key.to_string(),
            label: label.to_string(),
            is_group,
        })
        .collect::<Vec<_>>();
        let mut buffer = RenderBuffer::new(70, 10, &Style::default());

        draw_keymap_hints(&mut buffer, &Theme::default(), "Space", &hints).unwrap();

        let screen = (0..buffer.height)
            .map(|index| row(&buffer, index))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("? All commands"), "{screen}");
        assert!(screen.contains("a Select all"), "{screen}");
        assert!(screen.contains("Space  Next buffer"), "{screen}");
        assert!(screen.contains("Ctrl-w Windows …"), "{screen}");
        assert!(!screen.contains("?      All commands"), "{screen}");
    }

    #[test]
    fn keymap_hints_fall_back_to_picker_theme_roles() {
        let item_background = Color::Rgb {
            r: 18,
            g: 20,
            b: 24,
        };
        let key_color = Color::Rgb {
            r: 90,
            g: 100,
            b: 110,
        };
        let label_color = Color::Rgb {
            r: 100,
            g: 210,
            b: 160,
        };
        let mut theme = Theme::default();
        theme.ui_style.picker_item.bg = Some(item_background);
        theme.ui_style.muted.fg = Some(key_color);
        theme.ui_style.picker_prompt.fg = Some(label_color);
        theme.ui_style.popup_title.bold = true;
        let hints = [
            KeymapHint {
                key: "a".to_string(),
                label: "Select all".to_string(),
                is_group: false,
            },
            KeymapHint {
                key: "h".to_string(),
                label: "Git hunks".to_string(),
                is_group: true,
            },
        ];
        let mut buffer = RenderBuffer::new(40, 8, &Style::default());

        draw_keymap_hints(&mut buffer, &theme, "Space", &hints).unwrap();

        let rows = (0..buffer.height)
            .map(|index| row(&buffer, index))
            .collect::<Vec<_>>();
        let leaf_y = rows
            .iter()
            .position(|row| row.contains("Select all"))
            .unwrap();
        let group_y = rows
            .iter()
            .position(|row| row.contains("Git hunks …"))
            .unwrap();
        let key_x = column(&rows[leaf_y], "a").unwrap();
        let label_x = column(&rows[leaf_y], "Select all").unwrap();
        let marker_x = column(&rows[group_y], "…").unwrap();

        assert_eq!(
            buffer.cells[leaf_y * buffer.width + key_x].style.fg,
            Some(key_color)
        );
        assert_eq!(
            buffer.cells[leaf_y * buffer.width + label_x].style.fg,
            Some(label_color)
        );
        assert_eq!(
            buffer.cells[group_y * buffer.width + marker_x].style.fg,
            Some(label_color)
        );
        assert!(buffer.cells[group_y * buffer.width + marker_x].style.bold);
        assert_eq!(
            buffer.cells[group_y * buffer.width + marker_x].style.bg,
            Some(item_background)
        );
    }

    #[test]
    fn keymap_hints_keep_multiple_columns_when_one_label_is_long() {
        let hints = (0..9)
            .map(|index| KeymapHint {
                key: index.to_string(),
                label: if index == 0 {
                    "One unusually long command label that must be truncated".to_string()
                } else {
                    format!("Command {index}")
                },
                is_group: index % 3 == 0,
            })
            .collect::<Vec<_>>();
        let mut buffer = RenderBuffer::new(80, 10, &Style::default());

        draw_keymap_hints(&mut buffer, &Theme::default(), "Space", &hints).unwrap();

        let screen = (0..buffer.height)
            .map(|index| row(&buffer, index))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Command 8"), "{screen}");
        assert!(!screen.contains("(6/9)"), "{screen}");
        assert!(screen.contains('┌'));
        assert!(screen.contains('┘'));
    }

    #[test]
    fn keymap_hints_prefer_readable_labels_over_an_extra_column() {
        let labels = [
            "Show code actions",
            "All commands",
            "Agent",
            "Select all",
            "Buffer picker",
            "Format document",
            "Git dashboard",
            "Project search",
            "LSP references",
            "Next buffer",
            "Previous buffer",
            "Rename symbol",
            "Theme browser",
            "LSP workspace symbols",
            "Commit message",
            "Debug",
            "Git hunks",
            "Next buffer",
        ];
        let hints = labels
            .iter()
            .enumerate()
            .map(|(index, label)| KeymapHint {
                key: if index == 17 {
                    "Space".to_string()
                } else {
                    index.to_string()
                },
                label: (*label).to_string(),
                is_group: index >= 14,
            })
            .collect::<Vec<_>>();
        let mut buffer = RenderBuffer::new(80, 14, &Style::default());

        draw_keymap_hints(&mut buffer, &Theme::default(), "Space", &hints).unwrap();

        let screen = (0..buffer.height)
            .map(|index| row(&buffer, index))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Show code actions"), "{screen}");
        assert!(screen.contains("Previous buffer"), "{screen}");
        assert!(screen.contains("LSP workspace symbols"), "{screen}");
        assert!(!screen.contains("(16/18)"), "{screen}");
    }

    #[test]
    fn keymap_hints_fit_a_small_terminal_without_overflow() {
        let mut buffer = RenderBuffer::new(24, 8, &Style::default());
        let hints = (0..24)
            .map(|index| KeymapHint {
                key: index.to_string(),
                label: format!("Command {index}"),
                is_group: index % 2 == 0,
            })
            .collect::<Vec<_>>();

        draw_keymap_hints(&mut buffer, &Theme::default(), "Space", &hints).unwrap();

        assert_eq!(buffer.cells.len(), 24 * 8);
        assert!(buffer.cells.iter().any(|cell| cell.c == '┌'));
        assert!(buffer.cells.iter().any(|cell| cell.c == '└'));
    }
}
