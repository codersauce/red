//! Language-aware line commenting with Neovim-compatible range semantics.
//!
//! A comment template contains exactly one %s placeholder. Text before the
//! placeholder is inserted before a line and text after it is inserted after a
//! line, allowing the same implementation to handle line and wrapping comments.

use crate::editing::ReflowLine;

/// Validated left and right halves of a language-specific comment template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentSyntax {
    left: String,
    right: String,
}

impl CommentSyntax {
    /// Parses a template containing exactly one placeholder and a real marker.
    pub(crate) fn parse(template: &str) -> Option<Self> {
        let (left, right) = template.split_once("%s")?;
        if right.contains("%s") || (left.trim().is_empty() && right.trim().is_empty()) {
            return None;
        }

        Some(Self {
            left: left.to_string(),
            right: right.to_string(),
        })
    }

    /// Reports whether a nonblank line already has both comment markers.
    pub(crate) fn is_commented(&self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return false;
        }

        line.strip_prefix(self.left.trim())
            .is_some_and(|content| content.trim_end().ends_with(self.right.trim()))
    }

    /// Classifies one complete comment line for paragraph reflow.
    pub(crate) fn reflow_line(&self, line: &str) -> Option<ReflowLine> {
        let content = line.trim_start_matches(char::is_whitespace);
        let indent = &line[..line.len() - content.len()];
        let left_marker = self.left.trim_end();
        let after_left = content.strip_prefix(left_marker)?;

        if !self.right.trim().is_empty() {
            let right_marker = self.right.trim();
            let body = after_left.trim_end().strip_suffix(right_marker)?.trim();
            if body.is_empty() {
                return Some(ReflowLine::Literal(line.to_string()));
            }
            return Some(ReflowLine::paragraph(
                format!("{indent}{}", self.left),
                body,
                self.right.clone(),
            ));
        }

        let (extra_marker, body) = self.line_comment_suffix(after_left);
        let body = body.trim();
        if body.is_empty() {
            return Some(ReflowLine::Literal(line.to_string()));
        }
        let padding = self.left.strip_prefix(left_marker).unwrap_or_default();
        Some(ReflowLine::paragraph(
            format!("{indent}{left_marker}{extra_marker}{padding}"),
            body,
            "",
        ))
    }

    /// Returns the leader to insert when continuing a line comment.
    pub(crate) fn continuation_prefix(&self, line: &str) -> Option<String> {
        if !self.right.trim().is_empty() {
            return None;
        }

        let content = line.trim_start_matches(char::is_whitespace);
        let indent = &line[..line.len() - content.len()];
        let left_marker = self.left.trim_end();
        let after_left = content.strip_prefix(left_marker)?;
        let (extra_marker, _) = self.line_comment_suffix(after_left);
        let padding = self.left.strip_prefix(left_marker).unwrap_or_default();
        Some(format!("{indent}{left_marker}{extra_marker}{padding}"))
    }

    fn line_comment_suffix<'a>(&self, after_left: &'a str) -> (&'a str, &'a str) {
        let marker_character = self.left.trim_end().chars().last();
        let extra_marker_len = after_left
            .char_indices()
            .take_while(|(_, character)| Some(*character) == marker_character || *character == '!')
            .map(|(offset, character)| offset + character.len_utf8())
            .last()
            .unwrap_or_default();
        after_left.split_at(extra_marker_len)
    }

    /// Reports a wrapping-comment opener that cannot be represented line-by-line.
    pub(crate) fn is_unclosed_wrapping_start(&self, line: &str) -> bool {
        let left_marker = self.left.trim();
        let right_marker = self.right.trim();
        !right_marker.is_empty()
            && line
                .trim_start_matches(char::is_whitespace)
                .starts_with(left_marker)
            && !line.trim_end().ends_with(right_marker)
    }

    /// Toggles the supplied lines as one range, preserving relative indentation.
    pub(crate) fn toggle_lines(&self, lines: &[String]) -> Vec<String> {
        let all_commented = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .all(|line| self.is_commented(line));

        if all_commented {
            return lines.iter().map(|line| self.uncomment_line(line)).collect();
        }

        let common_indent = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| leading_whitespace(line))
            .min_by_key(|indent| indent.len())
            .unwrap_or_default();

        lines
            .iter()
            .map(|line| {
                if line.trim().is_empty() {
                    return format!("{common_indent}{}{}", self.left.trim(), self.right.trim());
                }

                let content = line
                    .get(common_indent.len()..)
                    .unwrap_or_else(|| line.trim_start_matches(char::is_whitespace));
                format!("{common_indent}{}{content}{}", self.left, self.right)
            })
            .collect()
    }

    fn uncomment_line(&self, line: &str) -> String {
        let content = line.trim_start_matches(char::is_whitespace);
        let indent = &line[..line.len() - content.len()];
        let Some(content) = content
            .strip_prefix(&self.left)
            .or_else(|| content.strip_prefix(self.left.trim()))
        else {
            return line.to_string();
        };

        let (content, trailing) = if self.right.is_empty() {
            (content, "")
        } else {
            let without_trailing = content.trim_end_matches(char::is_whitespace);
            let trailing = &content[without_trailing.len()..];
            let Some(content) = without_trailing
                .strip_suffix(&self.right)
                .or_else(|| without_trailing.strip_suffix(self.right.trim()))
            else {
                return line.to_string();
            };
            (content, trailing)
        };

        if content.trim().is_empty() {
            return String::new();
        }

        format!("{indent}{content}{trailing}")
    }
}

fn leading_whitespace(line: &str) -> &str {
    let end = line
        .find(|character: char| !character.is_whitespace())
        .unwrap_or(line.len());
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::CommentSyntax;
    use crate::editing::ReflowLine;

    fn toggle(template: &str, lines: &[&str]) -> Vec<String> {
        let syntax = CommentSyntax::parse(template).expect("test template should be valid");
        let lines = lines
            .iter()
            .map(|line| (*line).to_string())
            .collect::<Vec<_>>();
        syntax.toggle_lines(&lines)
    }

    #[test]
    fn parses_line_and_wrapping_comment_templates() {
        assert!(CommentSyntax::parse("// %s").is_some());
        assert!(CommentSyntax::parse("# %s").is_some());
        assert!(CommentSyntax::parse("<!-- %s -->").is_some());
        assert!(CommentSyntax::parse("/* %s */").is_some());
    }

    #[test]
    fn rejects_missing_duplicate_and_markerless_placeholders() {
        assert!(CommentSyntax::parse("//").is_none());
        assert!(CommentSyntax::parse("%s %s").is_none());
        assert!(CommentSyntax::parse("%s").is_none());
        assert!(CommentSyntax::parse("  %s  ").is_none());
    }

    #[test]
    fn comments_at_the_least_indented_nonblank_line() {
        assert_eq!(
            toggle("// %s", &["    alpha", "      beta", "", "    gamma"]),
            ["    // alpha", "    //   beta", "    //", "    // gamma"]
        );
    }

    #[test]
    fn uncomments_the_whole_range_when_all_nonblank_lines_are_commented() {
        assert_eq!(
            toggle("// %s", &["    // alpha", "    // beta", "    //"]),
            ["    alpha", "    beta", ""]
        );
    }

    #[test]
    fn comments_the_whole_range_when_comment_state_is_mixed() {
        assert_eq!(
            toggle("// %s", &["    // alpha", "    beta"]),
            ["    // // alpha", "    // beta"]
        );
    }

    #[test]
    fn leaves_a_blank_only_range_unchanged() {
        assert_eq!(toggle("// %s", &["", "    ", "\t"]), ["", "    ", "\t"]);
    }

    #[test]
    fn toggles_wrapping_comment_markers() {
        assert_eq!(
            toggle("<!-- %s -->", &["    <div>hello</div>"]),
            ["    <!-- <div>hello</div> -->"]
        );
        assert_eq!(
            toggle("<!-- %s -->", &["    <!-- <div>hello</div> -->"]),
            ["    <div>hello</div>"]
        );
    }

    #[test]
    fn uncomments_markers_without_the_configured_padding() {
        assert_eq!(toggle("// %s", &["    //alpha"]), ["    alpha"]);
        assert_eq!(toggle("<!-- %s -->", &["    <!--hello-->"]), ["    hello"]);
    }

    #[test]
    fn preserves_tabs_when_aligning_comment_markers() {
        assert_eq!(
            toggle("# %s", &["\talpha", "\t\tbeta"]),
            ["\t# alpha", "\t# \tbeta"]
        );
    }

    #[test]
    fn extracts_reflow_parts_and_preserves_documentation_leaders() {
        let syntax = CommentSyntax::parse("// %s").unwrap();

        assert_eq!(
            syntax.reflow_line("    /// documented item"),
            Some(ReflowLine::paragraph("    /// ", "documented item", ""))
        );
        assert_eq!(
            syntax.reflow_line("    //! module docs"),
            Some(ReflowLine::paragraph("    //! ", "module docs", ""))
        );
        assert_eq!(
            syntax.reflow_line("    // - list item"),
            Some(ReflowLine::paragraph("    // ", "- list item", ""))
        );
        assert_eq!(
            syntax.reflow_line("    //"),
            Some(ReflowLine::Literal("    //".to_string()))
        );
    }

    #[test]
    fn extracts_complete_wrapping_comments_but_rejects_open_blocks() {
        let syntax = CommentSyntax::parse("/* %s */").unwrap();

        assert_eq!(
            syntax.reflow_line("  /* comment body */"),
            Some(ReflowLine::paragraph("  /* ", "comment body", " */"))
        );
        assert!(syntax.reflow_line("  /* comment body").is_none());
        assert!(syntax.is_unclosed_wrapping_start("  /* comment body"));
    }

    #[test]
    fn continues_line_and_documentation_leaders_but_not_wrapping_comments() {
        let syntax = CommentSyntax::parse("// %s").unwrap();

        assert_eq!(
            syntax.continuation_prefix("    // prose"),
            Some("    // ".to_string())
        );
        assert_eq!(
            syntax.continuation_prefix("    /// docs"),
            Some("    /// ".to_string())
        );
        assert_eq!(
            syntax.continuation_prefix("    //! module docs"),
            Some("    //! ".to_string())
        );
        assert_eq!(
            syntax.continuation_prefix("    // - list item"),
            Some("    // ".to_string())
        );
        assert_eq!(
            CommentSyntax::parse("/* %s */")
                .unwrap()
                .continuation_prefix("/* prose */"),
            None
        );
    }
}
