//! Ordered Markdown headings for the outline picker and the explore sidebar.
//!
//! Slugs match link targets: rendered text, with duplicate identifiers
//! suffixed `-1`, `-2`, and so on. Parsing does not change the document.

use std::collections::HashMap;
use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Bound synchronous parsing, as with Markdown link indexes.
pub const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownHeading {
    pub level: u8,
    pub title: String,
    pub slug: String,
    /// UTF-8 offset of the heading start, including Setext headings.
    pub offset: usize,
    /// Zero-based source line of `offset`.
    pub line: usize,
}

/// Headings in source order. Lists and quotes are included. Fenced and indented
/// code are not. Documents above [`MAX_DOCUMENT_BYTES`] yield an empty list.
pub fn headings(source: &str) -> Vec<MarkdownHeading> {
    if source.len() > MAX_DOCUMENT_BYTES {
        return Vec::new();
    }
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    );
    let mut headings = Vec::new();
    let mut slugs = HashMap::new();
    let mut suffixes = HashMap::new();
    let mut heading: Option<(usize, String, u8)> = None;
    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((range.start, String::new(), level as u8));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, title, _)) = &mut heading {
                    title.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, title, _)) = &mut heading {
                    title.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((offset, title, level)) = heading.take() {
                    let slug = unique_slug(&title, &slugs, &mut suffixes);
                    slugs.insert(slug.clone(), offset);
                    headings.push(MarkdownHeading {
                        level,
                        title,
                        slug,
                        offset,
                        line: line_at(source, offset),
                    });
                }
            }
            _ => {}
        }
    }
    headings
}

/// Last heading whose line is at or before `line`.
pub fn active_heading(headings: &[MarkdownHeading], line: usize) -> Option<usize> {
    headings.iter().rposition(|heading| heading.line <= line)
}

/// Window of heading indexes that keeps `active` on screen.
pub fn visible_range(len: usize, active: Option<usize>, height: usize) -> Range<usize> {
    if height == 0 || len == 0 {
        return 0..0;
    }
    if len <= height {
        return 0..len;
    }
    let active = active.unwrap_or(0).min(len - 1);
    let mut start = active.saturating_sub(height / 2).min(len - height);
    if active >= start + height {
        start = active + 1 - height;
    }
    start..start + height
}

/// Lowercase, omit punctuation, replace spaces with hyphens, and retain
/// Unicode letters and marks.
pub fn heading_slug(title: &str) -> String {
    title
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|character| {
            if character.is_whitespace() {
                Some('-')
            } else if character == '-'
                || character == '_'
                || character.is_alphanumeric()
                || unicode_ident::is_xid_continue(character)
            {
                Some(character)
            } else {
                None
            }
        })
        .collect()
}

fn unique_slug(
    title: &str,
    slugs: &HashMap<String, usize>,
    suffixes: &mut HashMap<String, usize>,
) -> String {
    let base = heading_slug(title);
    let suffix = suffixes.entry(base.clone()).or_insert(0);
    let mut slug = base.clone();
    while slugs.contains_key(&slug) {
        *suffix += 1;
        slug = format!("{base}-{suffix}");
    }
    slug
}

fn line_at(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_setext_quoted_and_listed_headings_keep_levels() {
        let source = "# One\n\n## Two\n\n### Three\n\nSetext\n======\n\n> # Quoted\n\n- # Listed\n";
        let headings = headings(source);
        let titles: Vec<_> = headings
            .iter()
            .map(|heading| (heading.level, heading.title.as_str(), heading.slug.as_str()))
            .collect();
        assert_eq!(
            titles,
            vec![
                (1, "One", "one"),
                (2, "Two", "two"),
                (3, "Three", "three"),
                (1, "Setext", "setext"),
                (1, "Quoted", "quoted"),
                (1, "Listed", "listed"),
            ]
        );
        assert_eq!(headings[0].line, 0);
        assert_eq!(
            headings[3].line,
            source.lines().position(|line| line == "Setext").unwrap()
        );
    }

    #[test]
    fn fences_and_indented_code_are_not_headings() {
        let source = "```\n# Hidden\n```\n\n~~~\n# Also hidden\n~~~\n\n    # Indented\n\n<h1>Html</h1>\n\n# Real\n";
        let headings = headings(source);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].title, "Real");
        assert_eq!(headings[0].slug, "real");
    }

    #[test]
    fn duplicate_slugs_follow_link_suffixes_and_strip_emphasis() {
        let source = "# **Hello** `code` &amp; World!\n\n\
                      # Hello code & World!\n\n\
                      # hello-code--world-1\n\n\
                      # Hello code & World!\n\n\
                      Café e\u{301} 中文\n---\n\n\
                      ```md\n# Hidden\n```\n";
        let headings = headings(source);
        let slugs: Vec<_> = headings
            .iter()
            .map(|heading| heading.slug.as_str())
            .collect();
        assert_eq!(
            slugs,
            vec![
                "hello-code--world",
                "hello-code--world-1",
                "hello-code--world-1-1",
                "hello-code--world-2",
                "café-e\u{301}-中文",
            ]
        );
        assert_eq!(headings[0].title, "Hello code & World!");
        assert_eq!(headings[0].offset, 0);
    }

    #[test]
    fn active_section_tracks_the_cursor_and_stays_in_the_window() {
        let source = (0..12)
            .map(|index| format!("# Section {index}\n\nbody\n"))
            .collect::<String>();
        let headings = headings(&source);
        assert_eq!(headings.len(), 12);
        assert_eq!(active_heading(&headings, 0), Some(0));
        assert_eq!(active_heading(&headings, headings[4].line), Some(4));
        assert_eq!(
            active_heading(&headings, headings[4].line + 1),
            Some(4),
            "lines under a heading stay in that section"
        );
        assert_eq!(active_heading(&headings, headings[11].line + 2), Some(11));

        let window = visible_range(headings.len(), Some(10), 4);
        assert!(window.contains(&10));
        assert!(!window.contains(&0));
        assert_eq!(window.end - window.start, 4);
        assert_eq!(visible_range(3, Some(2), 10), 0..3);
    }

    #[test]
    fn oversized_documents_yield_no_headings() {
        let source = format!("# Title\n{}", "x".repeat(MAX_DOCUMENT_BYTES));
        assert!(headings(&source).is_empty());
    }
}
