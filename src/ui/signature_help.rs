//! Compact, non-interactive signature help. All coordinates are terminal cells.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    editor::RenderBuffer,
    lsp::{Documentation, SignatureHelp, SignatureHelpParameterLabel, SignatureHelpSignature},
    theme::{SelectionForegroundPriority, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    geometry::anchored_popup_geometry_avoiding_rows,
    Component, OverlayLayout, ScreenRect,
};

fn utf16_boundary(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += ch.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

pub(crate) fn active_parameter(help: &SignatureHelp) -> Option<(&SignatureHelpSignature, usize)> {
    let signature = help
        .signatures
        .get(help.active_signature.unwrap_or(0))
        .or_else(|| help.signatures.first())?;
    let parameters = signature.parameters.as_deref().unwrap_or_default();
    let index = signature
        .active_parameter
        .or(help.active_parameter)
        .unwrap_or(0);
    Some((signature, if index < parameters.len() { index } else { 0 }))
}

fn parameter_range(signature: &SignatureHelpSignature, index: usize) -> Option<Range<usize>> {
    let parameter = signature.parameters.as_ref()?.get(index)?;
    match &parameter.label {
        SignatureHelpParameterLabel::Offsets([start, end]) => {
            let range =
                utf16_boundary(&signature.label, *start)?..utf16_boundary(&signature.label, *end)?;
            (range.start <= range.end).then_some(range)
        }
        SignatureHelpParameterLabel::Text(label) => {
            // String labels may repeat (for example two parameters both labelled `int`).
            let mut after = 0;
            for previous in signature.parameters.as_ref()?.iter().take(index) {
                if let SignatureHelpParameterLabel::Text(text) = &previous.label {
                    if let Some(start) = signature.label[after..].find(text) {
                        after += start + text.len();
                    }
                }
            }
            let start = signature.label[after..].find(label)? + after;
            Some(start..start + label.len())
        }
    }
}

fn documentation_text(documentation: &Documentation) -> &str {
    match documentation {
        Documentation::String(text) => text,
        Documentation::MarkupContent(markup) => &markup.value,
    }
}

#[derive(Default)]
struct Row {
    parts: Vec<(String, bool)>,
    width: usize,
    active: bool,
}

fn signature_rows(label: &str, active: Option<Range<usize>>, width: usize) -> Vec<Row> {
    let mut rows = vec![Row::default()];
    for (byte, grapheme) in label.grapheme_indices(true).take(4096) {
        let text = if grapheme.chars().any(char::is_control) {
            " "
        } else {
            grapheme
        };
        let cells = display_width(text);
        if cells > width {
            continue;
        }
        if rows.last().unwrap().width + cells > width {
            rows.push(Row::default());
        }
        let selected = active
            .as_ref()
            .is_some_and(|range| byte < range.end && byte + grapheme.len() > range.start);
        let row = rows.last_mut().unwrap();
        row.parts.push((text.to_owned(), selected));
        row.width += cells;
        row.active |= selected;
    }
    rows
}

fn intersects(a: ScreenRect, b: ScreenRect) -> bool {
    a.x < b.x.saturating_add(b.width)
        && b.x < a.x.saturating_add(a.width)
        && a.y < b.y.saturating_add(b.height)
        && b.y < a.y.saturating_add(a.height)
}

pub(crate) fn render(
    buffer: &mut RenderBuffer,
    theme: &Theme,
    help: &SignatureHelp,
    layout: OverlayLayout,
    completion: Option<ScreenRect>,
    show_documentation: bool,
) -> anyhow::Result<()> {
    let Some((signature, parameter)) = active_parameter(help) else {
        return Ok(());
    };
    let width = layout.viewport.width.saturating_sub(2).min(88);
    if width < 8 || layout.viewport.height < 3 {
        return Ok(());
    }
    let rows = signature_rows(
        &signature.label,
        parameter_range(signature, parameter),
        width,
    );
    let active_row = rows.iter().position(|row| row.active).unwrap_or(0);
    let docs = if show_documentation {
        signature
            .parameters
            .as_ref()
            .and_then(|p| p.get(parameter))
            .and_then(|p| p.documentation.as_ref())
            .or(signature.documentation.as_ref())
            .map(documentation_text)
            .unwrap_or("")
            .split_whitespace()
            .take(80)
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        String::new()
    };
    let width = rows
        .iter()
        .map(|row| row.width)
        .max()
        .unwrap_or(0)
        .max(display_width(&docs).min(width))
        .max(20)
        .min(width);
    let desired_height = rows.len().min(3) + usize::from(!docs.is_empty());
    let avoid_rows = completion
        .map(|rect| {
            (
                rect.y.min(layout.anchor.1),
                rect.y
                    .saturating_add(rect.height)
                    .saturating_sub(1)
                    .max(layout.anchor.1),
            )
        })
        .unwrap_or((layout.anchor.1, layout.anchor.1));
    let viewport = layout.viewport;
    let local_row = |row: usize| {
        row.saturating_sub(viewport.y)
            .min(viewport.height.saturating_sub(1))
    };
    let (x, y, height) = anchored_popup_geometry_avoiding_rows(
        (
            layout.anchor.0.saturating_sub(viewport.x),
            local_row(layout.anchor.1),
        ),
        (local_row(avoid_rows.0), local_row(avoid_rows.1)),
        viewport.width,
        viewport.height,
        width,
        desired_height,
    );
    let (x, y) = (x + viewport.x, y + viewport.y);
    let rect = ScreenRect {
        x,
        y,
        width: width + 2,
        height: height + 2,
    };
    if height == 0 || completion.is_some_and(|other| intersects(rect, other)) {
        return Ok(());
    }
    // On short panes the active parameter matters more than preceding rows or docs.
    let row_count = rows.len().min(3).min(height);
    let first = active_row
        .saturating_sub(row_count.saturating_sub(1) / 2)
        .min(rows.len().saturating_sub(row_count));
    let rows = &rows[first..first + row_count];
    let index = help
        .active_signature
        .unwrap_or(0)
        .min(help.signatures.len() - 1);
    let title = if help.signatures.len() > 1 {
        format!(
            "Signature {}/{} · Ctrl-k next",
            index + 1,
            help.signatures.len()
        )
    } else {
        "Signature".to_owned()
    };
    Dialog::new(
        Some(title),
        x,
        y,
        width,
        height,
        &theme.ui_style.popup,
        BorderStyle::Rounded,
        theme,
    )
    .with_surface_theme(theme, SurfaceRole::Popup)
    .draw(buffer)?;
    let base = &theme.ui_style.popup;
    let mut selected = theme.selected_style(
        base,
        &theme.ui_style.picker_selected_item,
        SelectionForegroundPriority::Selection,
    );
    selected.bold = true;
    for (row_index, row) in rows.iter().take(height).enumerate() {
        let mut column = x + 1;
        for (text, active) in &row.parts {
            buffer.set_text(
                column,
                y + 1 + row_index,
                text,
                if *active { &selected } else { base },
            );
            column += display_width(text);
        }
    }
    if !docs.is_empty() && height > rows.len() {
        buffer.set_text(
            x + 1,
            y + 1 + rows.len(),
            &truncate_display_width(&docs, width),
            &theme.ui_style.muted.clone().fallback_bg(base),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parameter_offsets_are_utf16_and_signature_override_wins() {
        let help: SignatureHelp = serde_json::from_value(json!({"activeParameter":0,"signatures":[{"label":"f(😀: T, y: U)","activeParameter":1,"parameters":[{"label":[2,7]},{"label":[9,13]}]}]})).unwrap();
        let (signature, index) = active_parameter(&help).unwrap();
        let range = parameter_range(signature, index).unwrap();
        assert_eq!(&signature.label[range], "y: U");
        assert_eq!(utf16_boundary("😀", 1), None);
    }

    #[test]
    fn repeated_string_labels_highlight_the_right_occurrence() {
        let help: SignatureHelp = serde_json::from_value(json!({"activeParameter":1,"signatures":[{"label":"f(int, int)","parameters":[{"label":"int"},{"label":"int"}]}]})).unwrap();
        let (signature, index) = active_parameter(&help).unwrap();
        assert_eq!(parameter_range(signature, index), Some(7..10));
    }

    #[test]
    fn narrow_rows_keep_wide_graphemes_and_active_parameter() {
        let rows = signature_rows("f(界界, argument)", Some(10..18), 8);
        assert!(rows.iter().all(|row| row.width <= 8));
        assert!(rows.iter().any(|row| row.active));
    }

    #[test]
    fn short_split_keeps_active_parameter_visible_and_avoids_completion() {
        let help: SignatureHelp = serde_json::from_value(json!({"activeParameter":2,"signatures":[{
            "label":"call(first: LongType, second: LongType, last: U)",
            "parameters":[{"label":"first: LongType"},{"label":"second: LongType"},{"label":"last: U"}]
        }]})).unwrap();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(60, 12, &theme.style);
        let viewport = ScreenRect {
            x: 25,
            y: 2,
            width: 30,
            height: 9,
        };
        let completion = ScreenRect {
            x: 27,
            y: 6,
            width: 24,
            height: 5,
        };
        let layout = OverlayLayout {
            viewport,
            anchor: (28, 5),
            avoid_rows: None,
            protected_rows: Some((5, 5)),
        };
        render(&mut buffer, &theme, &help, layout, Some(completion), false).unwrap();
        let text = buffer.cells.iter().map(|cell| cell.c).collect::<String>();
        assert!(text.contains("last: U"));
        for y in 0..12 {
            for x in 0..60 {
                if !viewport.contains(x, y) || completion.contains(x, y) || y == 5 {
                    assert_eq!(buffer.cells[y * 60 + x].c, ' ');
                }
            }
        }
        assert!(buffer
            .cells
            .iter()
            .any(|cell| cell.c == 'U' && cell.style.bold));
    }
}
