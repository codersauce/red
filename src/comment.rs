//! Language-aware line commenting with Neovim-compatible range semantics.
//!
//! A comment template contains exactly one %s placeholder. Text before the
//! placeholder is inserted before a line and text after it is inserted after a
//! line, allowing the same implementation to handle line and wrapping comments.

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
}
