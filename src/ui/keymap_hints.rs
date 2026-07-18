use crate::{
    command_palette::KeymapHint,
    editor::RenderBuffer,
    theme::Theme,
    unicode_utils::{display_width, truncate_display_width},
};

use super::{dialog::BorderStyle, Component, Dialog};

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

    let key_width = hints
        .iter()
        .map(|hint| display_width(&hint.key))
        .max()
        .unwrap_or(1);
    let entry_width = hints
        .iter()
        .map(|hint| key_width + 2 + display_width(&hint.label) + usize::from(hint.is_group) * 2)
        .max()
        .unwrap_or(1)
        .min(available_width.saturating_sub(2));
    let column_width = entry_width.saturating_add(3);
    let columns = (available_width.saturating_sub(2) / column_width)
        .clamp(1, 3)
        .min(hints.len());
    let max_rows = available_height.saturating_sub(2).max(1);
    let rows = hints.len().div_ceil(columns).min(max_rows);
    let visible = rows * columns;
    let title = if visible < hints.len() {
        format!("{prefix} · keymaps ({}/{})", visible, hints.len())
    } else {
        format!("{prefix} · keymaps")
    };
    let inner_width = (columns * entry_width + columns.saturating_sub(1) * 3)
        .max(display_width(&title).saturating_add(2))
        .min(available_width.saturating_sub(2));
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

    for (index, hint) in hints.iter().take(visible).enumerate() {
        let column = index / rows;
        let row = index % rows;
        let marker = if hint.is_group { "› " } else { "" };
        let text = format!(
            "{:width$}  {marker}{}",
            hint.key,
            hint.label,
            width = key_width
        );
        let text = truncate_display_width(&text, entry_width);
        let style = if hint.is_group {
            &theme.ui_style.popup_title
        } else {
            &theme.ui_style.picker_item
        };
        buffer.set_text(
            x + 1 + column * (entry_width + 3),
            y + 1 + row,
            &text,
            style,
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Style;

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
