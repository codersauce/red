//! Fenced Markdown code blocks addressed by source line.
//!
//! The scan follows CommonMark opening and closing fences. Indented code and
//! shorter marker runs stay as text inside the block that contains them.

/// Returns the text inside the fence that covers `line`, without the fences.
///
/// The result ends with a newline when the block has any content lines. An
/// empty fence returns an empty string. `None` means the line is not inside
/// a fence, including its opening and closing lines.
pub fn fenced_block_content(source: &str, line: usize) -> Option<String> {
    let lines = source_lines(source);
    let block = scan_fences(&lines)
        .into_iter()
        .find(|block| block.start <= line && line <= block.end)?;
    Some(block.content)
}

struct FenceBlock {
    start: usize,
    end: usize,
    content: String,
}

struct OpenFence {
    marker: char,
    len: usize,
}

fn source_lines(source: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if source.ends_with('\n') || source.ends_with("\r\n") {
        lines.pop();
    }
    lines
}

fn scan_fences(lines: &[&str]) -> Vec<FenceBlock> {
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(open) = opening_fence(lines[index]) else {
            index += 1;
            continue;
        };
        let start = index;
        index += 1;
        let mut close = None;
        while index < lines.len() {
            if closing_fence(lines[index], open.marker, open.len) {
                close = Some(index);
                break;
            }
            index += 1;
        }
        let end = close.unwrap_or(lines.len().saturating_sub(1));
        let inner_end = close.unwrap_or(lines.len());
        let content = fence_content(&lines[start + 1..inner_end]);
        blocks.push(FenceBlock {
            start,
            end,
            content,
        });
        index = end + 1;
    }
    blocks
}

fn fence_content(lines: &[&str]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut content = lines.join("\n");
    content.push('\n');
    content
}

fn opening_fence(line: &str) -> Option<OpenFence> {
    let (indent, rest) = fence_indent(line)?;
    if indent > 3 {
        return None;
    }
    let marker = rest.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = rest
        .chars()
        .take_while(|character| *character == marker)
        .count();
    if len < 3 {
        return None;
    }
    let info = &rest[len..];
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some(OpenFence { marker, len })
}

fn closing_fence(line: &str, marker: char, min_len: usize) -> bool {
    let Some((indent, rest)) = fence_indent(line) else {
        return false;
    };
    if indent > 3 {
        return false;
    }
    let len = rest
        .chars()
        .take_while(|character| *character == marker)
        .count();
    len >= min_len && rest[len..].chars().all(|character| character == ' ')
}

fn fence_indent(line: &str) -> Option<(usize, &str)> {
    let indent = line
        .chars()
        .take_while(|character| *character == ' ')
        .count();
    Some((indent, line.get(indent..)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_excludes_fences_and_keeps_a_trailing_newline() {
        let source = "before\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\nafter\n";
        assert_eq!(
            fenced_block_content(source, 1).as_deref(),
            Some("fn main() {\n    println!(\"hi\");\n}\n")
        );
        assert_eq!(
            fenced_block_content(source, 3).as_deref(),
            Some("fn main() {\n    println!(\"hi\");\n}\n")
        );
        assert_eq!(
            fenced_block_content(source, 5).as_deref(),
            Some("fn main() {\n    println!(\"hi\");\n}\n")
        );
        assert_eq!(fenced_block_content(source, 0), None);
        assert_eq!(fenced_block_content(source, 6), None);
    }

    #[test]
    fn tilde_fences_and_empty_blocks_are_recognized() {
        let source = "~~~\nplain code\n~~~\n```\n```\n";
        assert_eq!(
            fenced_block_content(source, 1).as_deref(),
            Some("plain code\n")
        );
        assert_eq!(fenced_block_content(source, 3).as_deref(), Some(""));
    }

    #[test]
    fn indented_code_and_shorter_markers_stay_inside_the_block() {
        let source = "    ```\n    hidden\n    ```\n````\n```\ninner\n````\n";
        assert_eq!(fenced_block_content(source, 0), None);
        assert_eq!(fenced_block_content(source, 1), None);
        assert_eq!(
            fenced_block_content(source, 4).as_deref(),
            Some("```\ninner\n")
        );
    }

    #[test]
    fn an_unclosed_fence_runs_through_the_last_line() {
        let source = "```\nalpha\nbeta";
        assert_eq!(
            fenced_block_content(source, 2).as_deref(),
            Some("alpha\nbeta\n")
        );
    }
}
