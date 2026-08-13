//! Language-aware indentation decisions for newly created and reindented lines.
//!
//! Providers are deliberately pure: they inspect a document snapshot and return a
//! display-column target. The editor remains responsible for applying whitespace
//! through its undoable text-mutation boundary.

use crate::unicode_utils::display_width_with_tabs;

const PYTHON_BRACKET_LOOKBACK_LINES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndentDecision {
    Keep,
    Columns(usize),
}

#[derive(Debug, Clone, Copy)]
struct OpenBracket {
    character: char,
    line: usize,
    char_index: usize,
    column: usize,
}

#[derive(Debug, Clone, Copy)]
struct QuoteState {
    delimiter: char,
    triple: bool,
    escaped: bool,
}

#[derive(Debug, Default)]
struct PythonLineAnalysis {
    code: String,
    starts_in_multiline_string: bool,
    stack_before: Vec<OpenBracket>,
    last_closed_opener: Option<OpenBracket>,
}

/// Returns the language-specific indentation for `target_line`.
///
/// `Keep` means the editor should retain its ordinary auto-indent fallback.
pub(crate) fn indent_for_line(
    language_id: Option<&str>,
    document: &str,
    target_line: usize,
    shift_width: usize,
    tab_width: usize,
) -> IndentDecision {
    match language_id {
        Some("python") => python_indent_for_line(document, target_line, shift_width, tab_width),
        _ => IndentDecision::Keep,
    }
}

/// Mirrors the useful parts of Vim's `indentkeys` for the bundled providers.
pub(crate) fn should_reindent_after(
    language_id: Option<&str>,
    inserted: char,
    current_line: &str,
) -> bool {
    if language_id != Some("python") {
        return false;
    }

    if matches!(inserted, ':' | ')' | ']' | '}') {
        return true;
    }

    let line = current_line.trim();
    matches!(line, "elif" | "except")
}

fn python_indent_for_line(
    document: &str,
    target_line: usize,
    shift_width: usize,
    tab_width: usize,
) -> IndentDecision {
    let shift_width = shift_width.max(1);
    let tab_width = tab_width.max(1);
    // `split` preserves the implicit final empty line after a trailing newline,
    // which is exactly the line `o` or Enter asks us to indent at EOF.
    let lines = document
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    if target_line >= lines.len() {
        return IndentDecision::Keep;
    }

    let analysis = analyze_python(&lines, tab_width);
    let target = &analysis[target_line];
    if target.starts_in_multiline_string {
        return IndentDecision::Keep;
    }

    let target_indent = indentation_columns(lines[target_line], tab_width);
    let Some(previous_line) = (0..target_line)
        .rev()
        .find(|line| !lines[*line].trim().is_empty())
    else {
        return IndentDecision::Columns(0);
    };
    let previous_indent = indentation_columns(lines[previous_line], tab_width);
    let previous_code = analysis[previous_line].code.trim_end();

    // Explicit backslash continuations use two shift widths for their first line,
    // then retain that continuation indentation.
    if previous_code.ends_with('\\') {
        let previous_previous = (0..previous_line)
            .rev()
            .find(|line| !lines[*line].trim().is_empty());
        if previous_previous.is_some_and(|line| analysis[line].code.trim_end().ends_with('\\')) {
            return IndentDecision::Columns(previous_indent);
        }
        return IndentDecision::Columns(previous_indent + shift_width * 2);
    }

    // Work from the unmatched opener visible immediately before the target line.
    // The bounded lookup follows Vim's Python indent script and prevents malformed
    // documents from turning Enter into an unbounded scan.
    let opener =
        target.stack_before.iter().rev().find(|opener| {
            target_line.saturating_sub(opener.line) <= PYTHON_BRACKET_LOOKBACK_LINES
        });
    if let Some(opener) = opener {
        if target
            .code
            .trim_start()
            .chars()
            .next()
            .is_some_and(|character| matching_brackets(opener.character, character))
        {
            return preserve_manual_dedent(
                target_indent,
                indentation_columns(lines[opener.line], tab_width),
                shift_width,
            );
        }

        let opener_line = &analysis[opener.line].code;
        let content_after_opener = opener_line
            .chars()
            .skip(opener.char_index + 1)
            .any(|character| !character.is_whitespace());
        if content_after_opener {
            return IndentDecision::Columns(opener.column + 1);
        }

        if previous_line > opener.line {
            return IndentDecision::Columns(previous_indent);
        }

        let opener_indent = indentation_columns(lines[opener.line], tab_width);
        let continuation = if target.stack_before.len() > 1 {
            shift_width
        } else {
            shift_width * 2
        };
        return IndentDecision::Columns(opener_indent + continuation);
    }

    // A colon in code opens a Python suite. Comments and strings were blanked by
    // the lexical pass, so `# note:` and `"value:"` do not trigger this rule.
    if previous_code.ends_with(':') {
        return IndentDecision::Columns(previous_indent + shift_width);
    }

    let target_code = target.code.trim_start();
    if starts_with_any_keyword(target_code, &["except", "finally"]) {
        for line in (0..target_line).rev() {
            let code = analysis[line].code.trim_start();
            if starts_with_any_keyword(code, &["try", "except"]) {
                let expected = indentation_columns(lines[line], tab_width);
                return preserve_manual_dedent(target_indent, expected, shift_width);
            }
        }
        return IndentDecision::Keep;
    }

    if starts_with_any_keyword(target_code, &["elif", "else"]) {
        if starts_with_any_keyword(previous_code.trim_start(), &["for", "if", "elif", "try"]) {
            return IndentDecision::Columns(previous_indent);
        }
        return preserve_manual_dedent(
            target_indent,
            previous_indent.saturating_sub(shift_width),
            shift_width,
        );
    }

    if starts_with_any_keyword(
        previous_code.trim_start(),
        &["break", "continue", "raise", "return", "pass"],
    ) {
        return preserve_manual_dedent(
            target_indent,
            previous_indent.saturating_sub(shift_width),
            shift_width,
        );
    }

    if let Some(opener) = analysis[previous_line].last_closed_opener {
        if previous_code
            .chars()
            .last()
            .is_some_and(|character| matching_brackets(opener.character, character))
        {
            return preserve_manual_dedent(
                target_indent,
                indentation_columns(lines[opener.line], tab_width),
                shift_width,
            );
        }
    }

    IndentDecision::Keep
}

fn preserve_manual_dedent(current: usize, expected: usize, shift_width: usize) -> IndentDecision {
    if current == expected || current + shift_width <= expected {
        IndentDecision::Keep
    } else {
        IndentDecision::Columns(expected)
    }
}

fn indentation_columns(line: &str, tab_width: usize) -> usize {
    let whitespace = line
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(line, |(index, _)| &line[..index]);
    display_width_with_tabs(whitespace, tab_width)
}

fn starts_with_any_keyword(line: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| {
        line.strip_prefix(keyword).is_some_and(|suffix| {
            suffix
                .chars()
                .next()
                .is_none_or(|character| !is_identifier(character))
        })
    })
}

fn is_identifier(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn matching_brackets(open: char, close: char) -> bool {
    matches!((open, close), ('(', ')') | ('[', ']') | ('{', '}'))
}

fn analyze_python(lines: &[&str], tab_width: usize) -> Vec<PythonLineAnalysis> {
    let mut analyses = Vec::with_capacity(lines.len());
    let mut stack = Vec::<OpenBracket>::new();
    let mut quote = None::<QuoteState>;

    for (line_index, line) in lines.iter().enumerate() {
        let starts_in_multiline_string = quote.is_some_and(|state| state.triple);
        let stack_before = stack.clone();
        let characters = line.chars().collect::<Vec<_>>();
        let mut code = String::with_capacity(line.len());
        let mut last_closed_opener = None;
        let mut index = 0;

        while index < characters.len() {
            let character = characters[index];
            if let Some(mut state) = quote {
                if state.escaped {
                    state.escaped = false;
                    quote = Some(state);
                    code.push(' ');
                    index += 1;
                    continue;
                }
                if character == '\\' {
                    state.escaped = true;
                    quote = Some(state);
                    code.push(' ');
                    index += 1;
                    continue;
                }
                if state.triple
                    && character == state.delimiter
                    && characters.get(index + 1) == Some(&state.delimiter)
                    && characters.get(index + 2) == Some(&state.delimiter)
                {
                    code.push_str("   ");
                    quote = None;
                    index += 3;
                    continue;
                }
                if !state.triple && character == state.delimiter {
                    quote = None;
                } else {
                    quote = Some(state);
                }
                code.push(' ');
                index += 1;
                continue;
            }

            if character == '#' {
                code.extend(std::iter::repeat_n(' ', characters.len() - index));
                break;
            }

            if matches!(character, '\'' | '"') {
                let triple = characters.get(index + 1) == Some(&character)
                    && characters.get(index + 2) == Some(&character);
                quote = Some(QuoteState {
                    delimiter: character,
                    triple,
                    escaped: false,
                });
                if triple {
                    code.push_str("   ");
                    index += 3;
                } else {
                    code.push(' ');
                    index += 1;
                }
                continue;
            }

            if matches!(character, '(' | '[' | '{') {
                let prefix = characters[..index].iter().collect::<String>();
                stack.push(OpenBracket {
                    character,
                    line: line_index,
                    char_index: index,
                    column: display_width_with_tabs(&prefix, tab_width),
                });
            } else if matches!(character, ')' | ']' | '}')
                && stack
                    .last()
                    .is_some_and(|open| matching_brackets(open.character, character))
            {
                last_closed_opener = stack.pop();
            }

            code.push(character);
            index += 1;
        }

        if quote.is_some_and(|state| !state.triple) {
            quote = None;
        }
        analyses.push(PythonLineAnalysis {
            code,
            starts_in_multiline_string,
            stack_before,
            last_closed_opener,
        });
    }

    analyses
}

#[cfg(test)]
mod tests {
    use super::{indent_for_line, should_reindent_after, IndentDecision};

    fn python(document: &str, line: usize) -> IndentDecision {
        indent_for_line(Some("python"), document, line, 4, 4)
    }

    #[test]
    fn python_suite_colons_indent_but_comment_and_string_colons_do_not() {
        assert_eq!(
            python("def something(x):\n    ", 1),
            IndentDecision::Columns(4)
        );
        assert_eq!(python("value = 1  # note:\n", 1), IndentDecision::Keep);
        assert_eq!(python("value = \"note:\"\n", 1), IndentDecision::Keep);
    }

    #[test]
    fn python_continuations_align_or_use_hanging_indent() {
        assert_eq!(python("call(first,\n     ", 1), IndentDecision::Columns(5));
        assert_eq!(
            python("values = [\n        ", 1),
            IndentDecision::Columns(8)
        );
        assert_eq!(
            python("value = first + \\\n        ", 1),
            IndentDecision::Columns(8)
        );
    }

    #[test]
    fn python_stop_statements_and_branch_headers_dedent() {
        assert_eq!(
            python("if ready:\n    return value\n    ", 2),
            IndentDecision::Columns(0)
        );
        assert_eq!(
            python("if ready:\n    work()\n    else:", 2),
            IndentDecision::Columns(0)
        );
        assert_eq!(
            python("try:\n    work()\n    except:", 2),
            IndentDecision::Columns(0)
        );
    }

    #[test]
    fn python_preserves_an_existing_manual_dedent() {
        assert_eq!(
            python("if ready:\n    return value\n", 2),
            IndentDecision::Keep
        );
    }

    #[test]
    fn python_indent_triggers_match_vim_style_keys() {
        assert!(should_reindent_after(Some("python"), ':', "    else:"));
        assert!(should_reindent_after(Some("python"), 'f', "    elif"));
        assert!(should_reindent_after(Some("python"), 't', "    except"));
        assert!(!should_reindent_after(Some("python"), 'x', "value = x"));
        assert!(!should_reindent_after(Some("rust"), ':', "label:"));
    }
}
