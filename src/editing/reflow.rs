//! Source text reflow shared by file-backed and embedded editors.

use textwrap::{Options, WordSplitter, WrapAlgorithm};

use crate::unicode_utils::{display_width, display_width_with_tabs};

/// One logical source line classified for paragraph reflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReflowLine {
    /// Reflowable prose with source text that must surround every output line.
    Paragraph {
        prefix: String,
        body: String,
        suffix: String,
    },
    /// A paragraph boundary that must remain byte-for-byte unchanged.
    Literal(String),
}

impl ReflowLine {
    pub(crate) fn paragraph(
        prefix: impl Into<String>,
        body: impl Into<String>,
        suffix: impl Into<String>,
    ) -> Self {
        Self::Paragraph {
            prefix: prefix.into(),
            body: body.into(),
            suffix: suffix.into(),
        }
    }
}

/// Classifies an ordinary source line, preserving its leading indentation.
pub(crate) fn plain_line(line: &str) -> ReflowLine {
    let body = line.trim_start_matches(char::is_whitespace);
    if body.is_empty() {
        return ReflowLine::Literal(line.to_string());
    }

    let prefix = &line[..line.len() - body.len()];
    ReflowLine::paragraph(prefix, body.trim(), "")
}

/// Reflows text while preserving its existing line-ending convention.
pub(crate) fn reflow_text(
    text: &str,
    width: usize,
    tab_width: usize,
    mut classify: impl FnMut(&str) -> ReflowLine,
) -> String {
    let trailing_newline = text.ends_with('\n');
    let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines = text
        .split_terminator('\n')
        .map(|line| classify(line.strip_suffix('\r').unwrap_or(line)))
        .collect::<Vec<_>>();
    let mut formatted = reflow_lines(&lines, width, tab_width).join(line_ending);
    if trailing_newline {
        formatted.push_str(line_ending);
    }
    formatted
}

fn reflow_lines(lines: &[ReflowLine], width: usize, tab_width: usize) -> Vec<String> {
    let mut formatted = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let ReflowLine::Paragraph {
            prefix,
            body,
            suffix,
        } = &lines[index]
        else {
            if let ReflowLine::Literal(line) = &lines[index] {
                formatted.push(line.clone());
            }
            index += 1;
            continue;
        };

        let mut paragraph = body.trim().to_string();
        let mut end = index + 1;
        while let Some(ReflowLine::Paragraph {
            prefix: next_prefix,
            body: next_body,
            suffix: next_suffix,
        }) = lines.get(end)
        {
            if next_prefix != prefix || next_suffix != suffix {
                break;
            }
            if !paragraph.is_empty() && !next_body.trim().is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(next_body.trim());
            end += 1;
        }

        let fixed_width =
            display_width_with_tabs(prefix, tab_width).saturating_add(display_width(suffix));
        let body_width = width.saturating_sub(fixed_width);
        if body_width == 0 || paragraph.is_empty() {
            for line in &lines[index..end] {
                if let ReflowLine::Paragraph {
                    prefix,
                    body,
                    suffix,
                } = line
                {
                    formatted.push(format!("{prefix}{body}{suffix}"));
                }
            }
        } else {
            let options = Options::new(body_width)
                .break_words(false)
                .wrap_algorithm(WrapAlgorithm::FirstFit)
                .word_splitter(WordSplitter::NoHyphenation);
            formatted.extend(
                textwrap::wrap(&paragraph, options)
                    .into_iter()
                    .map(|line| format!("{prefix}{line}{suffix}")),
            );
        }
        index = end;
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::{plain_line, reflow_text, ReflowLine};

    #[test]
    fn reflows_plain_paragraphs_and_preserves_boundaries() {
        let text = "  alpha beta\n  gamma delta\n\n  epsilon";

        assert_eq!(
            reflow_text(text, 14, 4, plain_line),
            "  alpha beta\n  gamma delta\n\n  epsilon"
        );
        assert_eq!(
            reflow_text(text, 20, 4, plain_line),
            "  alpha beta gamma\n  delta\n\n  epsilon"
        );
    }

    #[test]
    fn counts_prefix_suffix_unicode_and_tabs_in_the_width() {
        let lines = "ignored\nignored";
        let mut bodies = ["wide 界界 words", "stay together"].into_iter();
        let formatted = reflow_text(lines, 20, 4, |_| {
            ReflowLine::paragraph("\t// ", bodies.next().unwrap(), " !")
        });

        assert_eq!(
            formatted,
            "\t// wide 界界 !\n\t// words stay !\n\t// together !"
        );
    }

    #[test]
    fn preserves_crlf_and_long_unbreakable_words() {
        assert_eq!(
            reflow_text("supercalifragilistic\r\nword\r\n", 8, 4, plain_line),
            "supercalifragilistic\r\nword\r\n"
        );
    }
}
