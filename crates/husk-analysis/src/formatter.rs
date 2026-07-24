//! Conservative, lossless-line Husk formatting.

use husk_lexer::{Lexer, TokenKind};

/// Reindent brace-delimited source and remove trailing horizontal whitespace.
///
/// Token text and comments are otherwise preserved. This intentionally small
/// formatter is safe for recovered syntax and is idempotent.
#[must_use]
pub fn format_source(source: &str, indent_width: usize) -> String {
    let indent_width = indent_width.clamp(1, 16);
    let tokens = Lexer::new(source)
        .filter(|token| !matches!(token.kind, TokenKind::Eof))
        .collect::<Vec<_>>();
    let mut output = String::with_capacity(source.len().saturating_add(1));
    let mut depth = 0usize;
    let mut line_start = 0usize;

    for line_with_ending in source.split_inclusive('\n') {
        let has_newline = line_with_ending.ends_with('\n');
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending)
            .strip_suffix('\r')
            .unwrap_or_else(|| {
                line_with_ending
                    .strip_suffix('\n')
                    .unwrap_or(line_with_ending)
            });
        let line_end = line_start + line.len();
        let line_tokens = tokens
            .iter()
            .filter(|token| {
                token.span.range.start >= line_start && token.span.range.start <= line_end
            })
            .collect::<Vec<_>>();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if has_newline {
                output.push('\n');
            }
            line_start += line_with_ending.len();
            continue;
        }
        let closes_first = line_tokens
            .first()
            .is_some_and(|token| matches!(token.kind, TokenKind::RBrace));
        let line_depth = depth.saturating_sub(usize::from(closes_first));
        if !trimmed.starts_with("#!") {
            output.extend(std::iter::repeat_n(' ', line_depth * indent_width));
        }
        output.push_str(trimmed.trim_end());
        if has_newline {
            output.push('\n');
        }

        for token in line_tokens {
            match token.kind {
                TokenKind::LBrace => depth = depth.saturating_add(1),
                TokenKind::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        line_start += line_with_ending.len();
    }
    if !source.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_is_idempotent_and_preserves_comments_and_strings() {
        let source = "fn main() {\nlet text = \"}\";   \n// keep me\nif true {\ntext;\n}\n}\n";
        let expected = "fn main() {\n    let text = \"}\";\n    // keep me\n    if true {\n        text;\n    }\n}\n";
        let formatted = format_source(source, 4);

        assert_eq!(formatted, expected);
        assert_eq!(format_source(&formatted, 4), expected);
    }
}
