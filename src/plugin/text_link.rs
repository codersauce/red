//! Link targets recognized in source-backed text panels.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TextPanelLinkTarget {
    File {
        path: String,
        /// One-based source line.
        line: usize,
        /// One-based source column.
        column: usize,
    },
    ExternalUrl(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextPanelLink {
    pub(crate) id: u64,
    pub(crate) target: TextPanelLinkTarget,
}

pub(crate) fn markdown_link_target(destination: &str) -> Option<TextPanelLinkTarget> {
    let destination = destination.trim();
    let lowercase = destination.to_ascii_lowercase();
    if lowercase.starts_with("https://") || lowercase.starts_with("http://") {
        return Some(TextPanelLinkTarget::ExternalUrl(destination.to_string()));
    }
    if destination.is_empty()
        || destination.starts_with('#')
        || destination.contains("://")
        || destination.starts_with("mailto:")
    {
        return None;
    }

    if let Some((path, line, column)) = parse_source_location(destination) {
        return Some(TextPanelLinkTarget::File { path, line, column });
    }

    Some(TextPanelLinkTarget::File {
        path: destination.to_string(),
        line: 1,
        column: 1,
    })
}

pub(crate) fn linkify_source_locations(text: &str) -> Vec<(&str, Option<TextPanelLinkTarget>)> {
    let mut fragments = Vec::new();
    let mut cursor = 0;

    for (token_start, token) in whitespace_tokens(text) {
        let leading = token
            .char_indices()
            .take_while(|(_, character)| matches!(character, '(' | '[' | '{' | '\'' | '"'))
            .map(|(index, character)| index + character.len_utf8())
            .last()
            .unwrap_or(0);
        let candidate = &token[leading..];
        let candidate_len = candidate
            .trim_end_matches(|character: char| {
                matches!(character, '.' | ',' | ';' | '!' | '?' | ')' | ']' | '}')
            })
            .len();
        let candidate = &candidate[..candidate_len];
        let Some((path, line, column)) = parse_source_location(candidate) else {
            continue;
        };
        let start = token_start + leading;
        let end = start + candidate.len();
        if cursor < start {
            fragments.push((&text[cursor..start], None));
        }
        fragments.push((
            &text[start..end],
            Some(TextPanelLinkTarget::File { path, line, column }),
        ));
        cursor = end;
    }

    if cursor < text.len() {
        fragments.push((&text[cursor..], None));
    }
    if fragments.is_empty() && !text.is_empty() {
        fragments.push((text, None));
    }
    fragments
}

fn whitespace_tokens(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.char_indices()
        .filter(|(_, character)| !character.is_whitespace())
        .filter(|(index, _)| {
            *index == 0
                || text[..*index]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace)
        })
        .map(|(start, _)| {
            let end = text[start..]
                .find(char::is_whitespace)
                .map_or(text.len(), |offset| start + offset);
            (start, &text[start..end])
        })
}

fn parse_source_location(value: &str) -> Option<(String, usize, usize)> {
    if value.is_empty() || value.contains("://") {
        return None;
    }

    if let Some(fragment) = value.rfind("#L") {
        let path = &value[..fragment];
        let location = &value[fragment + 2..];
        let (line, column) = location
            .split_once('C')
            .map_or((location, "1"), |(line, column)| (line, column));
        return valid_location(path, line, column);
    }

    let (before_last, last) = value.rsplit_once(':')?;
    if !is_positive_integer(last) {
        return None;
    }
    if let Some((path, possible_line)) = before_last.rsplit_once(':') {
        if is_positive_integer(possible_line) {
            return valid_location(path, possible_line, last);
        }
    }
    valid_location(before_last, last, "1")
}

fn valid_location(path: &str, line: &str, column: &str) -> Option<(String, usize, usize)> {
    if path.is_empty()
        || path.chars().all(|character| character.is_ascii_digit())
        || path.ends_with(':')
    {
        return None;
    }
    Some((path.to_string(), line.parse().ok()?, column.parse().ok()?))
}

fn is_positive_integer(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) && value != "0"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linkifies_source_locations_without_swallowing_punctuation() {
        let fragments = linkify_source_locations(
            "See src/editor.rs:42:7, (README.md:8) and https://example.com:443.",
        );
        let links = fragments
            .iter()
            .filter_map(|(text, target)| target.as_ref().map(|target| (*text, target)))
            .collect::<Vec<_>>();

        assert_eq!(
            links,
            [
                (
                    "src/editor.rs:42:7",
                    &TextPanelLinkTarget::File {
                        path: "src/editor.rs".to_string(),
                        line: 42,
                        column: 7,
                    },
                ),
                (
                    "README.md:8",
                    &TextPanelLinkTarget::File {
                        path: "README.md".to_string(),
                        line: 8,
                        column: 1,
                    },
                ),
            ]
        );
    }

    #[test]
    fn classifies_markdown_destinations() {
        assert_eq!(
            markdown_link_target("https://example.com/docs"),
            Some(TextPanelLinkTarget::ExternalUrl(
                "https://example.com/docs".to_string()
            ))
        );
        assert_eq!(
            markdown_link_target("src/main.rs#L12C4"),
            Some(TextPanelLinkTarget::File {
                path: "src/main.rs".to_string(),
                line: 12,
                column: 4,
            })
        );
        assert_eq!(markdown_link_target("#section"), None);
    }
}
