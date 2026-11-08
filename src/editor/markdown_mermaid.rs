//! Mermaid fences as text blocks.
//!
//! Pie charts and the vertical class-diagram layout are adapted from leaf
//! (MIT). See `licenses/leaf-MIT.txt`. Red crops a diagram that is wider than
//! the window instead of replacing it with a "too wide" label.

use mmdflux::{render_diagram, OutputFormat, RenderConfig};

use super::{BlockRole, BlockRow, BlockSpan, BlockSpec};
use crate::unicode_utils::{display_width, truncate_display_width};

/// One fenced Mermaid region. `fence` is every source line, including the
/// opening and closing markers, used when the diagram cannot be drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Region {
    pub fence: Vec<String>,
    pub body: String,
}

pub(super) fn find_blocks(text: &str) -> Vec<BlockSpec> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let Some((marker, count)) = open_mermaid(lines[index]) else {
            index += 1;
            continue;
        };
        let mut end = index + 1;
        while end < lines.len() && !closes_fence(lines[end], marker, count) {
            end += 1;
        }
        let closed = end < lines.len();
        let last = if closed { end + 1 } else { lines.len() };
        if last <= index + 1 {
            index += 1;
            continue;
        }
        let fence = lines[index..last]
            .iter()
            .map(|line| (*line).to_string())
            .collect();
        let body_end = if closed { end } else { lines.len() };
        let body = lines[index + 1..body_end].join("\n");
        blocks.push(BlockSpec::mermaid(index..last, Region { fence, body }));
        index = last;
    }
    blocks
}

pub(super) fn render(region: &Region, width: usize, ascii: bool) -> Vec<BlockRow> {
    if width == 0 {
        return Vec::new();
    }
    match render_text(&region.body, width, ascii) {
        Ok(text) => paint_diagram(&text, width, ascii),
        Err(message) => failure_rows(region, &message, width),
    }
}

fn render_text(body: &str, width: usize, ascii: bool) -> Result<String, String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("empty diagram".to_string());
    }
    if width < 4 {
        return Err("window is too narrow".to_string());
    }
    if first_word(trimmed) == "pie" {
        return render_pie(trimmed, width, ascii)
            .ok_or_else(|| "could not read pie entries".to_string());
    }
    if first_word(trimmed) == "classDiagram" && horizontal_class_cannot_fit(trimmed, width) {
        return render_vertical_class(trimmed, width, ascii);
    }

    let format = if ascii {
        OutputFormat::Ascii
    } else {
        OutputFormat::Text
    };
    let config = RenderConfig {
        padding: Some(1),
        text_color_mode: mmdflux::TextColorMode::Plain,
        ..RenderConfig::default()
    };
    match render_diagram(trimmed, format, &config) {
        Ok(text) if text.lines().any(|line| !line.trim().is_empty()) => {
            if widest(&text) > width {
                if let Some(vertical) = vertical_flowchart(trimmed) {
                    if let Ok(narrower) = render_diagram(&vertical, format, &config) {
                        if widest(&narrower) < widest(&text) {
                            return Ok(narrower);
                        }
                    }
                }
            }
            Ok(text)
        }
        Ok(_) => fallback_or_error(trimmed, width, ascii, "empty diagram"),
        Err(error) => fallback_or_error(trimmed, width, ascii, &error.to_string()),
    }
}

fn fallback_or_error(
    body: &str,
    width: usize,
    ascii: bool,
    message: &str,
) -> Result<String, String> {
    if first_word(body) == "classDiagram" {
        render_vertical_class(body, width, ascii)
    } else {
        Err(message.to_string())
    }
}

fn paint_diagram(text: &str, width: usize, ascii: bool) -> Vec<BlockRow> {
    let mut wider = false;
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if display_width(line) > width {
            wider = true;
            rows.push(colorize(&truncate_display_width(line, width)));
        } else {
            rows.push(colorize(line));
        }
    }
    while rows.last().is_some_and(|row| row.text().is_empty()) {
        rows.pop();
    }
    if wider {
        let marker = if ascii {
            "-> wider than window"
        } else {
            "→ wider than window"
        };
        rows.push(BlockRow::spans(vec![BlockSpan::muted(crop(marker, width))]));
    }
    rows
}

fn failure_rows(region: &Region, message: &str, width: usize) -> Vec<BlockRow> {
    let mut rows = region
        .fence
        .iter()
        .map(|line| BlockRow::plain(crop(line, width)))
        .collect::<Vec<_>>();
    let detail = message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("unsupported diagram")
        .trim();
    rows.push(BlockRow::spans(vec![BlockSpan::muted(crop(
        &format!("mermaid: could not render: {detail}"),
        width,
    ))]));
    rows
}

fn crop(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        text.to_string()
    } else {
        truncate_display_width(text, width)
    }
}

fn widest(text: &str) -> usize {
    text.lines().map(display_width).max().unwrap_or(0)
}

fn first_word(text: &str) -> &str {
    text.lines()
        .find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then(|| trimmed.split_whitespace().next().unwrap_or(""))
        })
        .unwrap_or("")
}

fn open_mermaid(line: &str) -> Option<(char, usize)> {
    let (marker, count, info) = fence(line)?;
    info.split_whitespace()
        .next()
        .is_some_and(|word| word == "mermaid")
        .then_some((marker, count))
}

fn closes_fence(line: &str, marker: char, open: usize) -> bool {
    fence(line)
        .is_some_and(|(char, count, info)| char == marker && count >= open && info.is_empty())
}

fn fence(line: &str) -> Option<(char, usize, &str)> {
    let line = line.trim_end_matches('\r');
    let rest = line.trim_start();
    let indent = line.len() - rest.len();
    if indent > 3 || rest.is_empty() {
        return None;
    }
    let marker = rest.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let count = rest.chars().take_while(|ch| *ch == marker).count();
    if count < 3 {
        return None;
    }
    let info = rest[count..].trim();
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some((marker, count, info))
}

fn vertical_flowchart(content: &str) -> Option<String> {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    for line in &mut lines {
        let trimmed = line.trim_start();
        let keyword_end = trimmed.find(char::is_whitespace)?;
        if !matches!(&trimmed[..keyword_end], "flowchart" | "graph") {
            continue;
        }
        let after = trimmed[keyword_end..].trim_start();
        let direction = after.split_whitespace().next()?.trim_end_matches(';');
        if !matches!(direction, "LR" | "RL") {
            return None;
        }
        let start = line.find(direction)?;
        line.replace_range(start..start + direction.len(), "TD");
        return Some(lines.join("\n"));
    }
    None
}

fn horizontal_class_cannot_fit(content: &str, max_width: usize) -> bool {
    let mut total = 0usize;
    let mut card = 0usize;
    let mut in_class = false;
    for line in content.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(declaration) = trimmed.strip_prefix("class ") {
            let name = declaration.trim_end_matches('{').trim();
            in_class = declaration.ends_with('{');
            card = display_width(name);
            if !in_class {
                total = total.saturating_add(card + 4);
                if total > max_width {
                    return true;
                }
                card = 0;
            }
            continue;
        }
        if in_class {
            if trimmed == "}" {
                total = total.saturating_add(card + 4);
                if total > max_width {
                    return true;
                }
                card = 0;
                in_class = false;
                continue;
            }
            card = card.max(display_width(trimmed));
        }
    }
    if in_class {
        total = total.saturating_add(card + 4);
    }
    total > max_width
}

struct ClassBlock {
    name: String,
    members: Vec<String>,
}

/// Stacked class cards. Adapted from leaf's vertical class-diagram fallback.
fn render_vertical_class(content: &str, max_width: usize, ascii: bool) -> Result<String, String> {
    let mut classes = Vec::new();
    let mut relationships = Vec::new();
    let mut current: Option<ClassBlock> = None;
    for source in content.lines().skip(1) {
        let line = source.trim();
        if line.is_empty() || line.starts_with("direction ") {
            continue;
        }
        if let Some(class) = current.as_mut() {
            if line == "}" {
                classes.push(current.take().expect("open class"));
            } else {
                class.members.push(line.to_string());
            }
            continue;
        }
        if let Some(declaration) = line.strip_prefix("class ") {
            if let Some(name) = declaration.strip_suffix('{') {
                current = Some(ClassBlock {
                    name: name.trim().to_string(),
                    members: Vec::new(),
                });
            } else {
                classes.push(ClassBlock {
                    name: declaration.trim().to_string(),
                    members: Vec::new(),
                });
            }
            continue;
        }
        if looks_like_relationship(line) {
            relationships.push(line.to_string());
        }
    }
    if let Some(class) = current {
        classes.push(class);
    }
    if classes.is_empty() {
        return Err("could not read class diagram".to_string());
    }

    let (top_left, top_right, bottom_left, bottom_right, tee_left, tee_right, horizontal, vertical) =
        if ascii {
            ('+', '+', '+', '+', '+', '+', '-', '|')
        } else {
            ('┌', '┐', '└', '┘', '├', '┤', '─', '│')
        };
    let widest_text = classes
        .iter()
        .flat_map(|class| {
            std::iter::once(class.name.as_str()).chain(class.members.iter().map(String::as_str))
        })
        .map(display_width)
        .max()
        .unwrap_or(0);
    let card_width = (widest_text + 4).clamp(8, max_width.max(8));
    let inner = card_width.saturating_sub(2).max(1);
    let text_width = inner.saturating_sub(2).max(1);
    let rule = horizontal.to_string().repeat(inner);
    let mut out = String::new();
    for (index, class) in classes.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push(top_left);
        out.push_str(&rule);
        out.push(top_right);
        out.push('\n');
        push_card_line(&mut out, &class.name, text_width, inner, vertical);
        if !class.members.is_empty() {
            out.push(tee_left);
            out.push_str(&rule);
            out.push(tee_right);
            out.push('\n');
            for member in &class.members {
                push_card_line(&mut out, member, text_width, inner, vertical);
            }
        }
        out.push(bottom_left);
        out.push_str(&rule);
        out.push(bottom_right);
        out.push('\n');
    }
    if !relationships.is_empty() {
        let heading = crop("Relationships", max_width);
        out.push_str(&heading);
        out.push('\n');
        out.push_str(
            &horizontal
                .to_string()
                .repeat(display_width(&heading).max(1)),
        );
        out.push('\n');
        for relationship in &relationships {
            for (index, line) in wrap_words(relationship, max_width.saturating_sub(2).max(1))
                .into_iter()
                .enumerate()
            {
                if index == 0 {
                    out.push_str("• ");
                } else {
                    out.push_str("  ");
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
    }
    Ok(out.trim_end().to_string())
}

fn looks_like_relationship(line: &str) -> bool {
    line.split_whitespace().any(|part| {
        part.contains("--") || part.contains("..") || part.contains("<|") || part.contains("|>")
    })
}

fn push_card_line(out: &mut String, text: &str, text_width: usize, inner: usize, vertical: char) {
    for line in wrap_words(text, text_width) {
        let padding = inner.saturating_sub(display_width(&line).saturating_add(1));
        out.push(vertical);
        out.push(' ');
        out.push_str(&line);
        out.push_str(&" ".repeat(padding));
        out.push(vertical);
        out.push('\n');
    }
}

fn wrap_words(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    if text.trim().is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let extra = usize::from(!current.is_empty());
        if display_width(&current) + extra + display_width(word) <= max_width {
            if extra > 0 {
                current.push(' ');
            }
            current.push_str(word);
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if display_width(word) <= max_width {
            current = word.to_string();
            continue;
        }
        let mut rest = word;
        while display_width(rest) > max_width {
            let chunk = truncate_display_width(rest, max_width);
            if chunk.is_empty() || chunk.len() >= rest.len() {
                break;
            }
            lines.push(chunk.clone());
            rest = &rest[chunk.len()..];
        }
        current = rest.to_string();
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Bar rows for a pie chart. Adapted from leaf's pie fallback.
fn render_pie(content: &str, width: usize, ascii: bool) -> Option<String> {
    let mut title = String::new();
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("pie") {
            let rest = rest.trim();
            if let Some(text) = rest.strip_prefix("title") {
                title = text.trim().trim_matches('"').to_string();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("title") {
            title = rest.trim().trim_matches('"').to_string();
            continue;
        }
        let Some((label, value)) = line.rsplit_once(':') else {
            continue;
        };
        let label = label.trim().trim_matches('"').to_string();
        let Ok(value) = value.trim().parse::<f64>() else {
            continue;
        };
        if value.is_finite() {
            entries.push((label, value));
        }
    }
    if entries.is_empty() {
        return None;
    }
    let total: f64 = entries.iter().map(|(_, value)| *value).sum();
    if total <= 0.0 {
        return None;
    }
    let label_width = entries
        .iter()
        .map(|(label, _)| display_width(label))
        .max()
        .unwrap_or(0);
    let bar_max = width
        .saturating_sub(label_width.saturating_add(8))
        .clamp(1, 24);
    let fill = if ascii { '#' } else { '█' };
    let mut out = String::new();
    if !title.is_empty() {
        out.push_str(&crop(&title, width));
        out.push('\n');
    }
    for (label, value) in &entries {
        let pct = value / total * 100.0;
        let filled = ((pct / 100.0) * bar_max as f64).round() as usize;
        let bar = fill.to_string().repeat(filled.min(bar_max));
        let row = format!("{bar:<bar_max$} {label:<label_width$} {pct:5.1}%");
        out.push_str(&crop(&row, width));
        out.push('\n');
    }
    Some(out.trim_end().to_string())
}

fn colorize(line: &str) -> BlockRow {
    if line.is_empty() {
        return BlockRow::plain("");
    }
    let frame = line.chars().any(|ch| is_box(ch) || ch == '+' || ch == '|');
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut role = BlockRole::Plain;
    let mut started = false;
    let mut rest = line;
    while !rest.is_empty() {
        let (text, next_role) = if !frame {
            if let Some(arrow) = arrow_at(rest) {
                (arrow, BlockRole::Edge)
            } else {
                let ch = rest.chars().next().unwrap();
                let text = &rest[..ch.len_utf8()];
                let role = if is_box(ch) {
                    BlockRole::Border
                } else {
                    BlockRole::Plain
                };
                (text, role)
            }
        } else {
            let ch = rest.chars().next().unwrap();
            let text = &rest[..ch.len_utf8()];
            let role = if is_box(ch) || matches!(ch, '+' | '|' | '-' | '_') {
                BlockRole::Border
            } else if is_arrow_char(ch) {
                BlockRole::Edge
            } else {
                BlockRole::Plain
            };
            (text, role)
        };
        if !started {
            role = next_role;
            started = true;
        } else if next_role != role {
            spans.push(BlockSpan::role(role, std::mem::take(&mut current)));
            role = next_role;
        }
        current.push_str(text);
        rest = &rest[text.len()..];
    }
    if !current.is_empty() {
        spans.push(BlockSpan::role(role, current));
    }
    BlockRow::spans(spans)
}

fn arrow_at(text: &str) -> Option<&str> {
    for pattern in [
        "<|--", "--|>", "<|..", "..|>", "-.->", "==>", "-->", "<--", "---", "->", "<-", "=>", "--",
    ] {
        if text.starts_with(pattern) {
            return Some(&text[..pattern.len()]);
        }
    }
    let ch = text.chars().next()?;
    is_arrow_char(ch).then(|| &text[..ch.len_utf8()])
}

fn is_arrow_char(ch: char) -> bool {
    matches!(
        ch,
        '←' | '→' | '↑' | '↓' | '↔' | '⇒' | '⇐' | '➜' | '►' | '▶' | '◀' | '▲' | '▼'
    )
}

fn is_box(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{257F}' | '\u{2580}'..='\u{259F}')
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

    fn region(body: &str) -> Region {
        let mut fence = vec!["```mermaid".to_string()];
        fence.extend(body.lines().map(str::to_string));
        fence.push("```".to_string());
        Region {
            fence,
            body: body.to_string(),
        }
    }

    #[test]
    fn finds_mermaid_fences_and_ignores_other_code() {
        let text = "intro\n```rust\nfn main() {}\n```\n```mermaid\ngraph TD\n  A-->B\n```\n    ```mermaid\nhidden\n```\n";
        let blocks = find_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines, 4..8);
        let body = &blocks[0].mermaid.as_ref().unwrap().body;
        assert!(body.contains("A-->B"));
    }

    #[test]
    fn pie_rows_show_shares_and_ascii_bars() {
        let body = "pie\n    title Pets\n    \"Dogs\" : 40\n    \"Cats\" : 60\n";
        let rows = render(&region(body), 40, false);
        let text = joined(&rows);
        assert!(text.contains("Pets"), "{text}");
        assert!(text.contains("Dogs"), "{text}");
        assert!(text.contains("40.0%"), "{text}");
        assert!(text.contains('█'), "{text}");
        assert!(!text.contains("could not render"), "{text}");
        let ascii = joined(&render(&region(body), 40, true));
        assert!(ascii.contains('#'), "{ascii}");
        assert!(!ascii.chars().any(|ch| ch == '█'), "{ascii}");
    }

    #[test]
    fn wide_class_diagram_stacks_cards() {
        let body = "classDiagram\n    class Aaaaaaaaaaaaaaaaaaaa\n    class Bbbbbbbbbbbbbbbbbbbb\n";
        let rows = render(&region(body), 30, false);
        let text = joined(&rows);
        assert!(text.contains("Aaaaaaaaaaaaaaaaaaaa"), "{text}");
        assert!(text.contains("Bbbbbbbbbbbbbbbbbbbb"), "{text}");
        assert!(text.contains('┌'), "{text}");
        assert!(
            text.contains("→ wider than window")
                || text.lines().all(|line| display_width(line) <= 30),
            "{text}"
        );
        for line in text.lines() {
            assert!(display_width(line) <= 30, "{line}");
        }
        let ascii = joined(&render(&region(body), 30, true));
        assert!(ascii.contains('+'), "{ascii}");
        assert!(!ascii.contains('┌'), "{ascii}");
    }

    #[test]
    fn parse_error_keeps_the_fence_and_a_muted_line() {
        let body = "not a diagram at all\n";
        let rows = render(&region(body), 60, false);
        let text = joined(&rows);
        assert!(text.contains("```mermaid"), "{text}");
        assert!(text.contains("not a diagram at all"), "{text}");
        assert!(text.contains("mermaid: could not render:"), "{text}");
        let muted = rows.last().unwrap();
        assert_eq!(muted.spans[0].role, BlockRole::Muted);
    }

    #[test]
    fn flowchart_sequence_and_state_render_at_two_widths() {
        let flowchart = "flowchart TD\n    A[Start] --> B[End]\n";
        let flowchart_rows = "\
┌───────┐
│ Start │
└───────┘
    │
    │
    ▼
 ┌─────┐
 │ End │
 └─────┘";
        assert_eq!(
            joined(&render(&region(flowchart), 80, false)),
            flowchart_rows
        );
        assert_eq!(
            joined(&render(&region(flowchart), 16, false)),
            flowchart_rows
        );

        let sequence = "sequenceDiagram\n    Alice->>Bob: Hello\n";
        let sequence_rows = "\
┌───────┐            ┌─────┐
│ Alice │            │ Bob │
└───┬───┘            └──┬──┘
    │                   │
    │─Hello────────────>│
    │                   │";
        let sequence_ascii = "\
+-------+            +-----+
| Alice |            | Bob |
+---+---+            +--+--+
    |                   |
    |-Hello------------>|
    |                   |";
        assert_eq!(joined(&render(&region(sequence), 80, false)), sequence_rows);
        assert_eq!(joined(&render(&region(sequence), 80, true)), sequence_ascii);
        let sequence_narrow = joined(&render(&region(sequence), 16, false));
        assert!(
            sequence_narrow.contains("→ wider than win"),
            "{sequence_narrow}"
        );
        for line in sequence_narrow.lines() {
            assert!(display_width(line) <= 16, "{line}");
        }

        let state = "stateDiagram-v2\n    [*] --> Still\n    Still --> [*]\n";
        // A leading `\` newline would eat the indent on the first row.
        let state_rows = "    ●\n\n    │\n    │\n    ▼\n╭───────╮\n│ Still │\n╰───────╯\n    │\n    │\n    ▼\n\n    ◉";
        let state_text = joined(&render(&region(state), 80, false));
        assert_eq!(state_text, state_rows);
        assert_eq!(joined(&render(&region(state), 16, false)), state_text);
        let state_ascii = joined(&render(&region(state), 80, true));
        assert!(state_ascii.contains("Still"), "{state_ascii}");
        assert!(!state_ascii.chars().any(is_box), "{state_ascii}");

        let wide_flow = "flowchart LR\n    A[AlphaStart] --> B[BetaMiddle] --> C[GammaEnd]\n";
        let wide = joined(&render(&region(wide_flow), 80, false));
        let narrow = joined(&render(&region(wide_flow), 16, false));
        assert!(!wide.contains("could not render"), "{wide}");
        assert!(wide.contains("AlphaStart"), "{wide}");
        assert_ne!(wide, narrow);
        for line in narrow.lines() {
            assert!(display_width(line) <= 16, "{line}\n{narrow}");
        }
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
    async fn motion_skips_a_diagram_and_reveal_shows_the_fence() {
        let source = "before\n```mermaid\nflowchart TD\n    A[Start] --> B[End]\n```\nafter\n";
        let mut editor = markdown_editor(source);
        let before = editor.test_buffer_contents();
        let drawn = screen(&mut editor);
        assert!(drawn.contains("Start"), "{drawn}");
        assert!(!drawn.contains("flowchart TD"), "{drawn}");
        editor.test_execute_action(Action::MoveDown).await.unwrap();
        assert_eq!(editor.test_buffer_line(), 5);
        assert_eq!(editor.test_buffer_contents(), before);
        assert!(editor.test_undo_tree().is_empty());

        editor.cy = 1;
        editor.vtop = 0;
        let revealed = screen(&mut editor);
        assert!(revealed.contains("flowchart TD"), "{revealed}");
        assert!(editor.test_undo_tree().is_empty());

        editor
            .test_execute_action(Action::ToggleMarkdownMermaid)
            .await
            .unwrap();
        editor.cy = 0;
        editor.vtop = 0;
        let off = screen(&mut editor);
        assert!(off.contains("flowchart TD"), "{off}");
    }
}
