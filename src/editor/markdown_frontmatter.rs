//! Leading YAML frontmatter as a key/value table.
//!
//! The line parser is adapted from leaf (MIT). See `licenses/leaf-MIT.txt`.
//! A block that is not a list of flat pairs stays in source form.

use super::{BlockRole, BlockRow, BlockSpan, BlockSpec};
use crate::unicode_utils::{display_width, truncate_display_width};

const VERTICAL_PAIR_THRESHOLD: usize = 5;

pub(super) fn find_block(text: &str) -> Option<BlockSpec> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().copied() != Some("---") {
        return None;
    }
    let mut end = 1usize;
    while end < lines.len() && lines[end] != "---" && lines[end] != "..." {
        end += 1;
    }
    if end >= lines.len() {
        return None;
    }
    let pairs = parse_pairs(&lines[1..end])?;
    Some(BlockSpec::frontmatter(0..end + 1, pairs))
}

pub(super) fn render(pairs: &[(String, String)], width: usize, ascii: bool) -> Vec<BlockRow> {
    if pairs.is_empty() || width == 0 {
        return Vec::new();
    }
    if pairs.len() < VERTICAL_PAIR_THRESHOLD {
        let preferred: Vec<usize> = pairs
            .iter()
            .map(|(key, value)| display_width(key).max(display_width(value)).max(1))
            .collect();
        if let Some(columns) = fit_columns(&preferred, width) {
            return draw_horizontal(pairs, &columns, ascii);
        }
    }
    let key_width = pairs
        .iter()
        .map(|(key, _)| display_width(key))
        .max()
        .unwrap_or(1)
        .max(1);
    let value_width = pairs
        .iter()
        .map(|(_, value)| display_width(value))
        .max()
        .unwrap_or(1)
        .max(1);
    let Some(columns) = fit_columns(&[key_width, value_width], width) else {
        return Vec::new();
    };
    draw_vertical(pairs, &columns, ascii)
}

fn parse_pairs(lines: &[&str]) -> Option<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            index += 1;
            continue;
        }
        if is_indented(line) {
            return None;
        }
        let (key, raw) = split_key(trimmed)?;
        if key.is_empty() {
            return None;
        }
        if matches!(raw, ">" | ">-" | "|" | "|-") {
            index += 1;
            let mut parts = Vec::new();
            while index < lines.len() && is_indented(lines[index]) {
                let part = strip_quotes(lines[index].trim());
                if !part.is_empty() {
                    parts.push(part.to_string());
                }
                index += 1;
            }
            pairs.push((key, parts.join(" ")));
        } else if raw.is_empty() {
            index += 1;
            let mut items = Vec::new();
            while index < lines.len() && is_indented(lines[index]) {
                let part = lines[index].trim();
                if !part.is_empty() {
                    let item = part.strip_prefix("- ").unwrap_or(part);
                    let item = strip_quotes(item.trim());
                    if !item.is_empty() {
                        items.push(item.to_string());
                    }
                }
                index += 1;
            }
            pairs.push((key, items.join(", ")));
        } else {
            pairs.push((key, strip_quotes(raw).to_string()));
            index += 1;
        }
    }
    (!pairs.is_empty()).then_some(pairs)
}

fn split_key(line: &str) -> Option<(String, &str)> {
    let colon = line.find(':')?;
    let key = strip_quotes(line[..colon].trim()).to_string();
    Some((key, line[colon + 1..].trim()))
}

fn is_indented(line: &str) -> bool {
    line.starts_with([' ', '\t'])
}

fn strip_quotes(text: &str) -> &str {
    let bytes = text.as_bytes();
    if text.len() >= 2
        && ((bytes[0] == b'"' && bytes[text.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[text.len() - 1] == b'\''))
    {
        return &text[1..text.len() - 1];
    }
    text
}

struct Frame {
    top_left: char,
    top_right: char,
    bottom_left: char,
    bottom_right: char,
    tee_left: char,
    tee_right: char,
    tee_top: char,
    tee_bottom: char,
    cross: char,
    horizontal: char,
    vertical: char,
}

fn frame(ascii: bool) -> Frame {
    if ascii {
        Frame {
            top_left: '+',
            top_right: '+',
            bottom_left: '+',
            bottom_right: '+',
            tee_left: '+',
            tee_right: '+',
            tee_top: '+',
            tee_bottom: '+',
            cross: '+',
            horizontal: '-',
            vertical: '|',
        }
    } else {
        Frame {
            top_left: '┌',
            top_right: '┐',
            bottom_left: '└',
            bottom_right: '┘',
            tee_left: '├',
            tee_right: '┤',
            tee_top: '┬',
            tee_bottom: '┴',
            cross: '┼',
            horizontal: '─',
            vertical: '│',
        }
    }
}

/// `n` columns need `n + 1` borders and two spaces of padding in each cell.
fn fit_columns(preferred: &[usize], max_width: usize) -> Option<Vec<usize>> {
    if preferred.is_empty() {
        return None;
    }
    let overhead = preferred.len() * 3 + 1;
    if max_width <= overhead {
        return None;
    }
    let mut columns: Vec<usize> = preferred.iter().map(|width| (*width).max(1)).collect();
    let mut used = overhead + columns.iter().sum::<usize>();
    while used > max_width {
        let widest = columns
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 1)
            .max_by_key(|(_, width)| *width)
            .map(|(index, _)| index)?;
        columns[widest] -= 1;
        used -= 1;
    }
    Some(columns)
}

fn draw_horizontal(pairs: &[(String, String)], columns: &[usize], ascii: bool) -> Vec<BlockRow> {
    let frame = frame(ascii);
    let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    let values: Vec<&str> = pairs.iter().map(|(_, value)| value.as_str()).collect();
    let key_roles = vec![BlockRole::Key; pairs.len()];
    let value_roles = vec![BlockRole::Value; pairs.len()];
    vec![
        rule_row(
            frame.top_left,
            frame.tee_top,
            frame.top_right,
            frame.horizontal,
            columns,
        ),
        cell_row(frame.vertical, columns, &keys, &key_roles),
        rule_row(
            frame.tee_left,
            frame.cross,
            frame.tee_right,
            frame.horizontal,
            columns,
        ),
        cell_row(frame.vertical, columns, &values, &value_roles),
        rule_row(
            frame.bottom_left,
            frame.tee_bottom,
            frame.bottom_right,
            frame.horizontal,
            columns,
        ),
    ]
}

fn draw_vertical(pairs: &[(String, String)], columns: &[usize], ascii: bool) -> Vec<BlockRow> {
    let frame = frame(ascii);
    let mut rows = vec![rule_row(
        frame.top_left,
        frame.tee_top,
        frame.top_right,
        frame.horizontal,
        columns,
    )];
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            rows.push(rule_row(
                frame.tee_left,
                frame.cross,
                frame.tee_right,
                frame.horizontal,
                columns,
            ));
        }
        rows.push(cell_row(
            frame.vertical,
            columns,
            &[key.as_str(), value.as_str()],
            &[BlockRole::Key, BlockRole::Value],
        ));
    }
    rows.push(rule_row(
        frame.bottom_left,
        frame.tee_bottom,
        frame.bottom_right,
        frame.horizontal,
        columns,
    ));
    rows
}

fn rule_row(left: char, mid: char, right: char, horizontal: char, columns: &[usize]) -> BlockRow {
    let mut line = String::new();
    line.push(left);
    for (index, width) in columns.iter().enumerate() {
        if index > 0 {
            line.push(mid);
        }
        line.push_str(&horizontal.to_string().repeat(width + 2));
    }
    line.push(right);
    BlockRow::spans(vec![BlockSpan::role(BlockRole::Guide, line)])
}

fn cell_row(vertical: char, columns: &[usize], texts: &[&str], roles: &[BlockRole]) -> BlockRow {
    let mut spans = Vec::new();
    spans.push(BlockSpan::role(BlockRole::Guide, vertical.to_string()));
    for (index, width) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(BlockSpan::role(BlockRole::Guide, vertical.to_string()));
        }
        let text = texts.get(index).copied().unwrap_or("");
        let role = roles.get(index).copied().unwrap_or(BlockRole::Plain);
        spans.push(padded_cell(text, *width, role));
    }
    spans.push(BlockSpan::role(BlockRole::Guide, vertical.to_string()));
    BlockRow::spans(spans)
}

fn padded_cell(text: &str, width: usize, role: BlockRole) -> BlockSpan {
    let shown = truncate_display_width(text, width);
    let pad = width.saturating_sub(display_width(&shown));
    let mut cell = String::from(" ");
    cell.push_str(&shown);
    cell.push_str(&" ".repeat(pad));
    cell.push(' ');
    BlockSpan::role(role, cell)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        editor::{render_buffer::RenderBuffer, Action, Editor},
        test_utils::EditorTestExt,
        theme::Theme,
    };

    fn joined(rows: &[BlockRow]) -> String {
        rows.iter()
            .map(BlockRow::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rendered(source: &str, width: usize, ascii: bool) -> Option<Vec<BlockRow>> {
        find_block(source).map(|spec| render(spec.frontmatter.as_ref().unwrap(), width, ascii))
    }

    #[test]
    fn flat_pairs_are_a_horizontal_table() {
        let source = "---\ntitle: \"Hello\"\nauthor: Ada\n---\n# Note\n";
        let rows = rendered(source, 40, false).unwrap();
        assert_eq!(
            joined(&rows),
            "\
┌───────┬────────┐
│ title │ author │
├───────┼────────┤
│ Hello │ Ada    │
└───────┴────────┘"
        );
        assert!(rows[1].spans.iter().any(|span| span.role == BlockRole::Key));
        assert!(rows[3]
            .spans
            .iter()
            .any(|span| span.role == BlockRole::Value));
        assert!(rows[0]
            .spans
            .iter()
            .all(|span| span.role == BlockRole::Guide));
        let ascii = joined(&rendered(source, 40, true).unwrap());
        assert!(ascii.contains('+'));
        assert!(ascii.contains("Hello"));
        assert!(!ascii.contains('┌'));
    }

    #[test]
    fn lists_and_nested_maps_are_inline() {
        let source = "\
---
tags:
  - rust
  - editor
author:
  name: Ada
  city: London
description: >
  hello
  world
---
";
        let rows = rendered(source, 80, false).unwrap();
        let text = joined(&rows);
        assert!(text.contains("rust, editor"), "{text}");
        assert!(text.contains("name: Ada, city: London"), "{text}");
        assert!(text.contains("hello world"), "{text}");
        assert!(rows.iter().all(|row| display_width(&row.text()) <= 80));
    }

    #[test]
    fn five_pairs_stack_and_a_narrow_window_truncates() {
        let source = "\
---
title: Notes
author: Ada
date: 2026-11-08
tags: rust, editor
draft: false
---
";
        let rows = rendered(source, 60, false).unwrap();
        for row in &rows {
            let hits = ["title", "author", "date", "tags", "draft"]
                .into_iter()
                .filter(|key| row.text().contains(key))
                .count();
            assert!(hits <= 1, "{}", row.text());
        }
        assert!(rows.iter().any(|row| row.text().contains("Ada")));

        let long = "---\ntitle: Hello\n---\n";
        let narrow = rendered(long, 12, false).unwrap();
        assert!(!narrow.is_empty());
        for row in &narrow {
            assert!(display_width(&row.text()) <= 12, "{}", row.text());
        }
        assert!(rendered(long, 4, false).unwrap().is_empty());
    }

    #[test]
    fn invalid_or_missing_frontmatter_stays_source() {
        assert!(find_block("# Title\n").is_none());
        assert!(find_block("note\n---\ntitle: Hello\n---\n").is_none());
        assert!(find_block("---\ntitle: Hello\n").is_none());
        assert!(find_block("---\n- just a list\n---\n").is_none());
        assert!(find_block("---\ntitle: Hello\nnot a pair\n---\n").is_none());
        assert!(find_block("---\n# only a comment\n---\n").is_none());
    }

    fn markdown_editor(source: &str) -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        config.scrolloff = Some(0);
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            72,
            24,
            config,
            Theme::default(),
            vec![Buffer::new(Some("notes.md".into()), source.into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn screen(editor: &mut Editor) -> String {
        let mut frame = RenderBuffer::new(72, 24, &Theme::default().style);
        editor.render(&mut frame).unwrap();
        frame
            .cells
            .chunks(frame.width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn reveal_and_toggle_keep_the_buffer() {
        let source = "---\ntitle: Hello\n---\nafter\n";
        let mut editor = markdown_editor(source);
        let before = editor.test_buffer_contents();
        editor.cy = 3;
        editor.vtop = 0;
        let drawn = screen(&mut editor);
        assert!(drawn.contains("Hello"), "{drawn}");
        assert!(!drawn.contains("---"), "{drawn}");
        assert_eq!(editor.test_buffer_contents(), before);
        assert!(editor.test_undo_tree().is_empty());

        editor.test_execute_action(Action::MoveUp).await.unwrap();
        assert_eq!(editor.test_buffer_line(), 0);
        let revealed = screen(&mut editor);
        assert!(revealed.contains("---"), "{revealed}");
        assert!(revealed.contains("title: Hello"), "{revealed}");
        assert!(editor.test_undo_tree().is_empty());

        editor.cy = 3;
        editor.vtop = 0;
        editor
            .test_execute_action(Action::ToggleMarkdownFrontmatter)
            .await
            .unwrap();
        let off = screen(&mut editor);
        assert!(off.contains("title: Hello"), "{off}");
        assert!(off.contains("---"), "{off}");
        assert_eq!(editor.test_buffer_contents(), before);
    }
}
