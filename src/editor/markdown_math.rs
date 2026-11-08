//! LaTeX delimiters drawn as Unicode.
//!
//! The mapping is original. Inline `$...$` becomes a concealment swap.
//! A `$$` block becomes rendered rows. Unknown commands stay as source.

use std::ops::Range;

use super::{BlockRow, BlockSpec};
use crate::unicode_utils::truncate_display_width;

pub(super) fn find_blocks(text: &str) -> Vec<BlockSpec> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut index = 0usize;
    let mut in_fence = false;
    let mut marker = '`';
    let mut count = 0usize;
    while index < lines.len() {
        if take_fence(lines[index], &mut in_fence, &mut marker, &mut count) {
            index += 1;
            continue;
        }
        if in_fence || is_indented_code(lines[index]) {
            index += 1;
            continue;
        }
        let trimmed = lines[index].trim();
        if trimmed == "$$" {
            if let Some(end) = find_display_close(&lines, index + 1) {
                if let Some(rendered) = convert_lines(&lines[index + 1..end]) {
                    blocks.push(BlockSpec::math(index..end + 1, rendered));
                }
                index = end + 1;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(inner) = single_line_math(trimmed) {
            if let Some(rendered) = to_unicode(inner) {
                blocks.push(BlockSpec::math(index..index + 1, vec![rendered]));
            }
        }
        index += 1;
    }
    blocks
}

pub(super) fn render(lines: &[String], width: usize) -> Vec<BlockRow> {
    if width == 0 {
        return Vec::new();
    }
    lines
        .iter()
        .map(|line| BlockRow::plain(truncate_display_width(line, width)))
        .collect()
}

/// One row of inline swaps per source line, including the empty line after a
/// trailing newline. Ranges are byte offsets on the line without its ending.
pub(in crate::editor) fn swaps_by_line(source: &str) -> Vec<Vec<(Range<usize>, String)>> {
    let mut offsets = vec![0];
    offsets.extend(source.match_indices('\n').map(|(index, _)| index + 1));
    let mut in_fence = false;
    let mut marker = '`';
    let mut count = 0usize;
    let mut rows = Vec::with_capacity(offsets.len());
    for index in 0..offsets.len() {
        let line = line_text(source, &offsets, index);
        if take_fence(line, &mut in_fence, &mut marker, &mut count)
            || in_fence
            || is_indented_code(line)
        {
            rows.push(Vec::new());
            continue;
        }
        rows.push(inline_swaps(line));
    }
    rows
}

pub(super) fn to_unicode(input: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = input;
    while !rest.is_empty() {
        if rest.starts_with('\\') {
            eat_command(&mut rest, &mut out);
            continue;
        }
        if rest.starts_with('^') || rest.starts_with('_') {
            eat_script(&mut rest, &mut out);
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    (!out.contains('\\')).then_some(out)
}

fn convert_lines(lines: &[&str]) -> Option<Vec<String>> {
    if lines.is_empty() {
        return None;
    }
    let mut rendered = Vec::with_capacity(lines.len());
    for line in lines {
        rendered.push(to_unicode(line.trim())?);
    }
    Some(rendered)
}

fn single_line_math(trimmed: &str) -> Option<&str> {
    if !trimmed.starts_with("$$") || !trimmed.ends_with("$$") || trimmed.len() < 5 {
        return None;
    }
    let inner = trimmed[2..trimmed.len() - 2].trim();
    if inner.is_empty() || inner.contains("$$") {
        return None;
    }
    Some(inner)
}

fn find_display_close(lines: &[&str], mut index: usize) -> Option<usize> {
    let mut in_fence = false;
    let mut marker = '`';
    let mut count = 0usize;
    while index < lines.len() {
        if take_fence(lines[index], &mut in_fence, &mut marker, &mut count) {
            index += 1;
            continue;
        }
        if !in_fence && !is_indented_code(lines[index]) && lines[index].trim() == "$$" {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn line_text<'a>(source: &'a str, offsets: &[usize], index: usize) -> &'a str {
    let start = offsets.get(index).copied().unwrap_or(source.len());
    let mut end = offsets.get(index + 1).copied().unwrap_or(source.len());
    if start >= source.len() {
        return "";
    }
    end = end.min(source.len());
    if end > start && source.as_bytes()[end - 1] == b'\n' {
        end -= 1;
    }
    if end > start && source.as_bytes()[end - 1] == b'\r' {
        end -= 1;
    }
    &source[start..end]
}

fn is_indented_code(line: &str) -> bool {
    line.starts_with('\t') || line.starts_with("    ")
}

fn fence_info(line: &str) -> Option<(char, usize, &str)> {
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

fn take_fence(line: &str, in_fence: &mut bool, marker: &mut char, count: &mut usize) -> bool {
    let Some((ch, len, info)) = fence_info(line) else {
        return false;
    };
    if *in_fence {
        if ch == *marker && len >= *count && info.is_empty() {
            *in_fence = false;
        }
        return true;
    }
    *in_fence = true;
    *marker = ch;
    *count = len;
    true
}

fn inline_swaps(line: &str) -> Vec<(Range<usize>, String)> {
    if line.trim() == "$$" {
        return Vec::new();
    }
    let code = code_span_ranges(line);
    let bytes = line.as_bytes();
    let mut swaps = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$'
            || (index > 0 && bytes[index - 1] == b'\\')
            || code.iter().any(|range| range.contains(&index))
        {
            index += 1;
            continue;
        }
        let delim = if index + 1 < bytes.len() && bytes[index + 1] == b'$' {
            2
        } else {
            1
        };
        if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let body_start = index + delim;
        let mut cursor = body_start;
        let mut close = None;
        while cursor < bytes.len() {
            if bytes[cursor] == b'$' && bytes[cursor - 1] != b'\\' {
                let len = if delim == 2 && cursor + 1 < bytes.len() && bytes[cursor + 1] == b'$' {
                    2
                } else {
                    1
                };
                if len == delim {
                    close = Some(cursor);
                    break;
                }
            }
            cursor += 1;
        }
        let Some(close_at) = close else {
            index += 1;
            continue;
        };
        let end = close_at + delim;
        if end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        if code
            .iter()
            .any(|range| range.start < end && index < range.end)
        {
            index += 1;
            continue;
        }
        let body = &line[body_start..close_at];
        if !unambiguous_body(body) {
            index += 1;
            continue;
        }
        if let Some(unicode) = to_unicode(body).filter(|unicode| !unicode.is_empty()) {
            swaps.push((index..end, unicode));
            index = end;
            continue;
        }
        index += 1;
    }
    swaps
}

fn unambiguous_body(body: &str) -> bool {
    if body.is_empty()
        || body.chars().next().is_some_and(char::is_whitespace)
        || body.chars().next_back().is_some_and(char::is_whitespace)
    {
        return false;
    }
    !body
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.' || byte == b',')
}

fn code_span_ranges(line: &str) -> Vec<Range<usize>> {
    let bytes = line.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let open = index;
        while index < bytes.len() && bytes[index] == b'`' {
            index += 1;
        }
        let width = index - open;
        let mut cursor = index;
        while cursor < bytes.len() {
            if bytes[cursor] != b'`' {
                cursor += 1;
                continue;
            }
            let close = cursor;
            while cursor < bytes.len() && bytes[cursor] == b'`' {
                cursor += 1;
            }
            if cursor - close == width {
                ranges.push(open..cursor);
                index = cursor;
                break;
            }
        }
    }
    ranges
}

fn eat_command(rest: &mut &str, out: &mut String) {
    let after = &rest[1..];
    if after.is_empty() {
        out.push('\\');
        *rest = "";
        return;
    }
    let ch = after.chars().next().unwrap();
    if !ch.is_ascii_alphabetic() {
        let replacement = match ch {
            ',' | ';' | ':' => Some(" "),
            '!' => Some(""),
            '{' => Some("{"),
            '}' => Some("}"),
            '|' => Some("|"),
            _ => None,
        };
        if let Some(replacement) = replacement {
            out.push_str(replacement);
        } else {
            out.push('\\');
            out.push(ch);
        }
        *rest = &after[ch.len_utf8()..];
        return;
    }
    let name_len = after
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .map(char::len_utf8)
        .sum::<usize>();
    let name = &after[..name_len];
    let mut next = &after[name_len..];
    if let Some(symbol) = symbol(name) {
        out.push_str(symbol);
        *rest = next;
        return;
    }
    match name {
        "frac" => {
            if let (Some(numerator), Some(denominator)) =
                (parse_braced(&mut next), parse_braced(&mut next))
            {
                let numerator = to_unicode(&numerator).unwrap_or(numerator);
                let denominator = to_unicode(&denominator).unwrap_or(denominator);
                out.push_str(&wrap_part(&numerator));
                out.push('/');
                out.push_str(&wrap_part(&denominator));
            } else {
                out.push_str("\\frac");
            }
            *rest = next;
        }
        "binom" => {
            if let (Some(n), Some(k)) = (parse_braced(&mut next), parse_braced(&mut next)) {
                let n = to_unicode(&n).unwrap_or(n);
                let k = to_unicode(&k).unwrap_or(k);
                out.push_str("C(");
                out.push_str(&n);
                out.push(',');
                out.push_str(&k);
                out.push(')');
            } else {
                out.push_str("\\binom");
            }
            *rest = next;
        }
        "sqrt" => {
            if let Some(inner) = parse_braced(&mut next) {
                let inner = to_unicode(&inner).unwrap_or(inner);
                out.push('√');
                out.push('(');
                out.push_str(&inner);
                out.push(')');
            } else {
                out.push_str("\\sqrt");
            }
            *rest = next;
        }
        "text" | "mathrm" | "mathbf" => {
            if let Some(inner) = parse_braced(&mut next) {
                let inner = to_unicode(&inner).unwrap_or(inner);
                out.push_str(&inner);
            } else {
                out.push('\\');
                out.push_str(name);
            }
            *rest = next;
        }
        "left" | "right" => {
            if !consume_delimiter(&mut next) {
                out.push('\\');
                out.push_str(name);
            }
            *rest = next;
        }
        _ => {
            out.push('\\');
            out.push_str(name);
            *rest = next;
        }
    }
}

fn eat_script(rest: &mut &str, out: &mut String) {
    let superscript = rest.starts_with('^');
    *rest = &rest[1..];
    let Some(group) = take_group(rest) else {
        out.push(if superscript { '^' } else { '_' });
        return;
    };
    let converted = to_unicode(&group).unwrap_or(group);
    if let Some(mapped) = map_script(&converted, superscript) {
        out.push_str(&mapped);
        return;
    }
    out.push(if superscript { '^' } else { '_' });
    out.push('(');
    out.push_str(&converted);
    out.push(')');
}

fn take_group(rest: &mut &str) -> Option<String> {
    if rest.is_empty() {
        return None;
    }
    if rest.starts_with('{') {
        return parse_braced(rest);
    }
    if rest.starts_with('\\') {
        let mut command = String::new();
        eat_command(rest, &mut command);
        return Some(command);
    }
    let ch = rest.chars().next()?;
    *rest = &rest[ch.len_utf8()..];
    Some(ch.to_string())
}

fn parse_braced(rest: &mut &str) -> Option<String> {
    if !rest.starts_with('{') {
        return None;
    }
    let mut depth = 0i32;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let inner = rest[1..index].to_string();
                    *rest = &rest[index + 1..];
                    return Some(inner);
                }
            }
            _ => {}
        }
    }
    None
}

fn consume_delimiter(rest: &mut &str) -> bool {
    if rest.starts_with("\\{")
        || rest.starts_with("\\}")
        || rest.starts_with("\\|")
        || rest.starts_with("\\.")
    {
        *rest = &rest[2..];
        return true;
    }
    let Some(ch) = rest.chars().next() else {
        return false;
    };
    if matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | '|' | '.') {
        *rest = &rest[ch.len_utf8()..];
        return true;
    }
    false
}

fn wrap_part(text: &str) -> String {
    if text
        .chars()
        .any(|ch| matches!(ch, '+' | '-' | '−' | '=' | ' ' | '<' | '>' | '/'))
    {
        format!("({text})")
    } else {
        text.to_string()
    }
}

fn map_script(text: &str, superscript: bool) -> Option<String> {
    let mut out = String::new();
    for ch in text.chars() {
        out.push(script_char(ch, superscript)?);
    }
    Some(out)
}

fn script_char(ch: char, superscript: bool) -> Option<char> {
    if superscript {
        return Some(match ch {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            '+' => '⁺',
            '-' => '⁻',
            '=' => '⁼',
            '(' => '⁽',
            ')' => '⁾',
            'n' => 'ⁿ',
            'i' => 'ⁱ',
            _ => return None,
        });
    }
    Some(match ch {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'n' => 'ₙ',
        'i' => 'ᵢ',
        _ => return None,
    })
}

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "Delta" => "Δ",
        "epsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "phi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "Psi" => "Ψ",
        "omega" => "ω",
        "Omega" => "Ω",
        "infty" => "∞",
        "times" => "×",
        "cdot" => "·",
        "pm" => "±",
        "mp" => "∓",
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "equiv" => "≡",
        "sim" => "∼",
        "rightarrow" | "to" => "→",
        "leftarrow" => "←",
        "Rightarrow" => "⇒",
        "sum" => "∑",
        "int" => "∫",
        "prod" => "∏",
        "partial" => "∂",
        "nabla" => "∇",
        "in" => "∈",
        "forall" => "∀",
        "exists" => "∃",
        "ldots" | "dots" => "…",
        "cdots" => "⋯",
        "quad" => " ",
        "qquad" => "  ",
        _ => return None,
    })
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
        unicode_utils::display_width,
    };

    #[test]
    fn unicode_maps_scripts_symbols_and_fractions() {
        assert_eq!(to_unicode("E = mc^2").as_deref(), Some("E = mc²"));
        assert_eq!(to_unicode("mc^{2}").as_deref(), Some("mc²"));
        assert_eq!(to_unicode(r"\alpha").as_deref(), Some("α"));
        assert_eq!(to_unicode(r"\frac{1}{2}").as_deref(), Some("1/2"));
        assert_eq!(to_unicode(r"\frac{a+b}{c}").as_deref(), Some("(a+b)/c"));
        assert_eq!(to_unicode(r"\sqrt{x}").as_deref(), Some("√(x)"));
        assert_eq!(to_unicode(r"\binom{n}{k}").as_deref(), Some("C(n,k)"));
        assert!(to_unicode(r"\notacommand").is_none());
    }

    #[test]
    fn unambiguous_delimiters_and_code_spans() {
        assert!(swaps("$5 and $6").is_empty());
        assert!(swaps("$20$").is_empty());
        assert!(swaps("price$x$more").is_empty());
        assert!(swaps("$$").is_empty());
        assert_eq!(swaps("($x$)"), vec![(1..4, "x".to_string())]);
        assert!(swaps("`$x$`").is_empty());
        assert_eq!(
            swaps(r"energy $E = mc^2$ rest"),
            vec![(7..17, "E = mc²".to_string())]
        );
        let fenced = swaps_by_line("```\n$\\alpha$\n```\n");
        assert!(fenced.iter().all(|row| row.is_empty()));
    }

    #[test]
    fn display_math_is_a_block_and_unknown_commands_stay_source() {
        let blocks = find_blocks("$$\nE = mc^2\n$$\nafter\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines, 0..3);
        assert_eq!(
            blocks[0].math.as_deref(),
            Some(&[String::from("E = mc²")][..])
        );
        let single = find_blocks("$$E = mc^2$$\n");
        assert_eq!(single[0].lines, 0..1);
        assert!(find_blocks("see $$E = mc^2$$ now\n").is_empty());
        assert!(find_blocks("$$\n\\notacommand\n$$\n").is_empty());
        assert!(find_blocks("$$\nE = mc^2\n").is_empty());
        assert!(find_blocks("```\n$$\nE = mc^2\n$$\n```\n").is_empty());
        let rows = render(&["E = mc²".into()], 4);
        assert!(display_width(&rows[0].text()) <= 4);
        assert!(render(&["E = mc²".into()], 0).is_empty());
    }

    fn swaps(source: &str) -> Vec<(Range<usize>, String)> {
        swaps_by_line(source).into_iter().next().unwrap_or_default()
    }

    fn markdown_editor(source: &str, math: bool) -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        config.scrolloff = Some(0);
        if math {
            config.markdown_math = Some(true);
        }
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
    async fn inline_math_renders_reveals_and_toggles() {
        let source = "energy $E = mc^2$ rest\n";
        let mut editor = markdown_editor(source, true);
        let before = editor.test_buffer_contents();
        let drawn = screen(&mut editor);
        assert!(drawn.contains("E = mc²"), "{drawn}");
        assert!(!drawn.contains("$E"), "{drawn}");
        assert_eq!(editor.test_buffer_contents(), before);
        assert!(editor.test_undo_tree().is_empty());

        editor.cx = 7;
        let revealed = screen(&mut editor);
        assert!(revealed.contains("$E = mc^2$"), "{revealed}");

        editor
            .test_execute_action(Action::ToggleMarkdownMath)
            .await
            .unwrap();
        let off = screen(&mut editor);
        assert!(off.contains("$E"), "{off}");
        assert!(!off.contains('²'), "{off}");
        assert_eq!(editor.test_buffer_contents(), before);
        assert!(editor.test_undo_tree().is_empty());
    }

    #[test]
    fn math_stays_source_when_the_setting_is_off() {
        let mut editor = markdown_editor("energy $E = mc^2$ rest\n", false);
        let drawn = screen(&mut editor);
        assert!(drawn.contains("$E"), "{drawn}");
        assert!(drawn.contains('^'), "{drawn}");
        assert!(!drawn.contains('²'), "{drawn}");
    }

    #[tokio::test]
    async fn display_math_is_a_block_until_the_cursor_or_toggle() {
        let source = "$$\nE = mc^2\n$$\nafter\n";
        let mut editor = markdown_editor(source, true);
        let before = editor.test_buffer_contents();
        editor.cy = 3;
        editor.vtop = 0;
        let drawn = screen(&mut editor);
        assert!(drawn.contains('²'), "{drawn}");
        assert!(!drawn.contains("$$"), "{drawn}");
        assert_eq!(editor.test_buffer_contents(), before);
        assert!(editor.test_undo_tree().is_empty());

        editor.test_execute_action(Action::MoveUp).await.unwrap();
        assert_eq!(editor.test_buffer_line(), 0);
        let revealed = screen(&mut editor);
        assert!(revealed.contains("$$"), "{revealed}");
        assert!(revealed.contains("E = mc^2"), "{revealed}");

        editor.cy = 3;
        editor.vtop = 0;
        editor
            .test_execute_action(Action::ToggleMarkdownMath)
            .await
            .unwrap();
        let off = screen(&mut editor);
        assert!(off.contains("$$"), "{off}");
        assert!(off.contains("mc^2"), "{off}");
        assert_eq!(editor.test_buffer_contents(), before);
        assert!(editor.test_undo_tree().is_empty());
    }
}
