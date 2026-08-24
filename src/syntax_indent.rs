//! Bounded, editor-owned indentation driven by language-pack Tree-sitter queries.
//!
//! Queries describe structure, never edits. Display-column decisions are applied by
//! the editor's existing transaction boundary. Unknown query features are rejected.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context as _};
use tree_sitter::{
    InputEdit, ParseOptions, Parser, Point, Query, QueryCursor, QueryCursorOptions,
    StreamingIterator, Tree,
};

use crate::{
    buffer::BufferId, highlighter::LanguageRegistry, indent::IndentDecision,
    unicode_utils::display_width_with_tabs,
};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_EVENTS: usize = 100_000;
const PARSE_BUDGET: Duration = Duration::from_millis(25);
const QUERY_BUDGET: Duration = Duration::from_millis(15);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndentReason {
    NewLine,
    Typed,
}

pub(crate) struct IndentRequest<'a> {
    pub id: BufferId,
    pub revision: u64,
    pub language: &'a str,
    pub source: &'a str,
    pub line: usize,
    pub shift_width: usize,
    pub tab_width: usize,
    pub reason: IndentReason,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Begin,
    End,
    Branch,
    Ignore,
    Zero,
    Continuation,
}

#[derive(Clone)]
struct Event {
    kind: Kind,
    bytes: Range<usize>,
    line: usize,
    key: String,
}

struct Document {
    revision: u64,
    language: String,
    source: String,
    parser: Parser,
    tree: Tree,
}

/// Keeps native grammar owners alive for every cached tree and compiled query.
pub(crate) struct SyntaxIndentation {
    queries: HashMap<String, Arc<Query>>,
    documents: HashMap<BufferId, Document>,
    // Fields drop in declaration order: native grammar libraries must outlive
    // the queries, parsers, and trees that refer to their static tables.
    registry: Arc<LanguageRegistry>,
}

pub(crate) fn validate_query(query: &Query) -> anyhow::Result<()> {
    for name in query.capture_names() {
        anyhow::ensure!(
            matches!(
                *name,
                "indent.begin"
                    | "indent.end"
                    | "indent.branch"
                    | "indent.ignore"
                    | "indent.zero"
                    | "indent.match"
                    | "indent.continuation"
            ),
            "unsupported indentation capture @{name}"
        );
    }
    for pattern in 0..query.pattern_count() {
        anyhow::ensure!(
            query.general_predicates(pattern).is_empty()
                && query.property_predicates(pattern).is_empty(),
            "unsupported indentation predicate in pattern {pattern}"
        );
        for property in query.property_settings(pattern) {
            anyhow::ensure!(
                property.capture_id.is_none()
                    && property.key.as_ref() == "indent.match"
                    && property
                        .value
                        .as_deref()
                        .is_some_and(|value| !value.is_empty()),
                "unsupported indentation property {}",
                property.key
            );
        }
    }
    Ok(())
}

/// Avoid reparsing ordinary text while allowing keyword closers such as `end`.
pub(crate) fn is_reindent_candidate(inserted: char, line: &str) -> bool {
    let text = line.trim();
    matches!(inserted, '}' | ')' | ']' | '>')
        || (inserted.is_ascii_alphabetic()
            && !text.is_empty()
            && text.len() <= 32
            && text.chars().all(|c| c.is_ascii_alphabetic()))
}

impl SyntaxIndentation {
    pub(crate) fn new(registry: Arc<LanguageRegistry>) -> Self {
        Self {
            registry,
            queries: HashMap::new(),
            documents: HashMap::new(),
        }
    }

    pub(crate) fn reset(&mut self, registry: Arc<LanguageRegistry>) {
        self.documents.clear();
        self.queries.clear();
        self.registry = registry;
    }

    pub(crate) fn indent(&mut self, request: IndentRequest<'_>) -> IndentDecision {
        self.try_indent(request).unwrap_or(IndentDecision::Keep)
    }

    /// Reports whether a line's first non-whitespace byte is inside an ignored syntax node.
    pub(crate) fn line_is_ignored(
        &mut self,
        id: BufferId,
        revision: u64,
        language: &str,
        source: &str,
        line: usize,
    ) -> bool {
        if source.len() > MAX_BYTES {
            return false;
        }

        let mut start = 0;
        let Some(target) = source.split('\n').enumerate().find_map(|(index, text)| {
            if index == line {
                Some(text)
            } else {
                start += text.len() + 1;
                None
            }
        }) else {
            return false;
        };
        let first = start + target.len() - target.trim_start().len();

        self.events(id, revision, language, source, start + target.len())
            .ok()
            .flatten()
            .is_some_and(|events| {
                events.iter().any(|event| {
                    event.kind == Kind::Ignore
                        && event.bytes.start < first
                        && first < event.bytes.end
                })
            })
    }

    fn try_indent(&mut self, request: IndentRequest<'_>) -> anyhow::Result<IndentDecision> {
        let IndentRequest {
            id,
            revision,
            language,
            source,
            line,
            shift_width,
            tab_width,
            reason,
        } = request;
        let widths = (shift_width, tab_width);
        if source.len() > MAX_BYTES {
            return Ok(IndentDecision::Keep);
        }
        let lines = source.split('\n').collect::<Vec<_>>();
        let Some(target) = lines.get(line) else {
            return Ok(IndentDecision::Keep);
        };
        let start = lines[..line].iter().map(|s| s.len() + 1).sum::<usize>();
        let first = start + target.len() - target.trim_start().len();
        let Some(events) = self.events(id, revision, language, source, start + target.len())?
        else {
            return Ok(IndentDecision::Keep);
        };
        if events
            .iter()
            .any(|e| e.kind == Kind::Ignore && e.bytes.start < first && first < e.bytes.end)
        {
            return Ok(IndentDecision::Keep);
        }
        let events = significant_events(events);
        if events.is_empty() {
            return Ok(IndentDecision::Keep);
        }
        let target_event = events.iter().find(|e| {
            e.bytes.start == first && matches!(e.kind, Kind::End | Kind::Branch | Kind::Zero)
        });
        if reason == IndentReason::Typed && target_event.is_none() {
            return Ok(IndentDecision::Keep);
        }
        if target_event.is_some_and(|e| e.kind == Kind::Zero) {
            return Ok(IndentDecision::Columns(0));
        }

        let mut stack: Vec<&Event> = Vec::new();
        for event in events.iter().take_while(|e| e.bytes.start < first) {
            apply_event(&mut stack, event);
        }
        if let Some(closer) = target_event {
            if let Some(opener) = stack.iter().rev().find(|e| e.key == closer.key) {
                return Ok(IndentDecision::Columns(columns(
                    lines[opener.line],
                    widths.1,
                )));
            }
            return Ok(IndentDecision::Keep);
        }
        let target_depth = depth(&stack);
        let Some(previous) = (0..line).rev().find(|i| !lines[*i].trim().is_empty()) else {
            return Ok(IndentDecision::Columns(0));
        };
        let continuation_lines = events
            .iter()
            .filter(|e| e.kind == Kind::Continuation)
            .map(|e| e.line)
            .collect::<HashSet<_>>();
        let continued = |line| continuation_lines.contains(&line);
        if continued(previous) || previous.checked_sub(1).is_some_and(continued) {
            let mut anchor = previous;
            while anchor > 0 && continued(anchor - 1) {
                anchor -= 1;
            }
            let extra = if continued(previous) {
                widths.0.max(1)
            } else {
                0
            };
            return Ok(IndentDecision::Columns(
                columns(lines[anchor], widths.1) + extra,
            ));
        }
        let previous_start = lines[..previous].iter().map(|s| s.len() + 1).sum::<usize>();
        let previous_first =
            previous_start + lines[previous].len() - lines[previous].trim_start().len();
        stack.clear();
        let first_event = events.partition_point(|event| event.bytes.start < previous_first);
        for event in &events[..first_event] {
            apply_event(&mut stack, event);
        }
        if let Some(event) = events
            .get(first_event)
            .filter(|event| event.bytes.start == previous_first)
        {
            if event.kind == Kind::Branch {
                if let Some(index) = stack.iter().rposition(|open| open.key == event.key) {
                    stack.truncate(index);
                }
            } else if event.kind == Kind::End {
                apply_leading_closer_group(&mut stack, &events[first_event..], source);
            }
        }
        let delta = target_depth as isize - depth(&stack) as isize;
        let expected = columns(lines[previous], widths.1)
            .saturating_add_signed(delta.saturating_mul(widths.0.max(1) as isize));
        Ok(IndentDecision::Columns(expected))
    }

    /// Only split an empty, syntactically recognized pair. Never split string text.
    pub(crate) fn split_pair(
        &mut self,
        id: BufferId,
        revision: u64,
        language: &str,
        source: &str,
        cursor: usize,
    ) -> bool {
        let Ok(Some(events)) = self.events(id, revision, language, source, source.len()) else {
            return false;
        };
        let events = significant_events(events);
        let before = events.iter().rev().find(|e| e.bytes.end <= cursor);
        let after = events.iter().find(|e| e.bytes.start >= cursor);
        match (before, after) {
            (Some(before), Some(after)) => {
                before.kind == Kind::Begin
                    && after.kind == Kind::End
                    && before.key == after.key
                    && source[before.bytes.end..after.bytes.start]
                        .chars()
                        .all(|c| c == ' ' || c == '\t')
            }
            _ => false,
        }
    }

    fn events(
        &mut self,
        id: BufferId,
        revision: u64,
        language_id: &str,
        source: &str,
        end: usize,
    ) -> anyhow::Result<Option<Vec<Event>>> {
        if source.len() > MAX_BYTES {
            return Ok(None);
        }
        let query = if let Some(query) = self.queries.get(language_id) {
            Arc::clone(query)
        } else {
            let Some((language, text)) = self.registry.indentation_language(language_id) else {
                return Ok(None);
            };
            let query = Query::new(&language, &text)?;
            validate_query(&query)?;
            let query = Arc::new(query);
            self.queries
                .insert(language_id.to_owned(), Arc::clone(&query));
            query
        };
        let current = self.documents.get(&id).is_some_and(|d| {
            d.revision == revision && d.language == language_id && d.source == source
        });
        if !current {
            let previous = self
                .documents
                .remove(&id)
                .filter(|d| d.language == language_id);
            let (mut parser, old_tree) = if let Some(mut previous) = previous {
                previous
                    .tree
                    .edit(&replacement_edit(&previous.source, source));
                (previous.parser, Some(previous.tree))
            } else {
                let (language, _) = self
                    .registry
                    .indentation_language(language_id)
                    .ok_or_else(|| anyhow!("missing indentation grammar"))?;
                let mut parser = Parser::new();
                parser.set_language(&language)?;
                (parser, None)
            };
            let started = Instant::now();
            let mut cancel = |_: &tree_sitter::ParseState| started.elapsed() > PARSE_BUDGET;
            let tree = parser
                .parse_with_options(
                    &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
                    old_tree.as_ref(),
                    Some(ParseOptions::new().progress_callback(&mut cancel)),
                )
                .ok_or_else(|| anyhow!("indentation parse budget exceeded"))?;
            if self.documents.len() >= 16 {
                self.documents.clear();
            }
            self.documents.insert(
                id,
                Document {
                    revision,
                    language: language_id.to_owned(),
                    source: source.to_owned(),
                    parser,
                    tree,
                },
            );
        }
        let document = &self.documents[&id];
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(0..end.saturating_add(1));
        let started = Instant::now();
        let mut cancelled = false;
        let mut cancel = |_: &tree_sitter::QueryCursorState| {
            cancelled = started.elapsed() > QUERY_BUDGET;
            cancelled
        };
        let mut matches = cursor.matches_with_options(
            &query,
            document.tree.root_node(),
            source.as_bytes(),
            QueryCursorOptions::new().progress_callback(&mut cancel),
        );
        let mut events = Vec::new();
        while let Some(matched) = matches.next() {
            let captured_group = matched
                .captures
                .iter()
                .find(|c| query.capture_names()[c.index as usize] == "indent.match")
                .and_then(|c| c.node.utf8_text(source.as_bytes()).ok());
            let group = captured_group.or_else(|| {
                query
                    .property_settings(matched.pattern_index)
                    .iter()
                    .find(|p| p.key.as_ref() == "indent.match")
                    .and_then(|p| p.value.as_deref())
            });
            for capture in matched.captures {
                let kind = match query.capture_names()[capture.index as usize] {
                    "indent.begin" => Kind::Begin,
                    "indent.end" => Kind::End,
                    "indent.branch" => Kind::Branch,
                    "indent.ignore" => Kind::Ignore,
                    "indent.zero" => Kind::Zero,
                    "indent.continuation" => Kind::Continuation,
                    _ => continue,
                };
                let node = capture.node;
                if node.is_missing() {
                    continue;
                }
                let text = node.utf8_text(source.as_bytes()).unwrap_or_default();
                let key = if matches!(kind, Kind::Ignore | Kind::Zero | Kind::Continuation) {
                    ""
                } else {
                    group.unwrap_or_else(|| {
                        if kind == Kind::Begin {
                            match text {
                                "{" => "}",
                                "(" => ")",
                                "[" => "]",
                                _ => text,
                            }
                        } else {
                            text
                        }
                    })
                };
                events.push(Event {
                    kind,
                    bytes: node.byte_range(),
                    line: node.start_position().row,
                    key: key.to_owned(),
                });
                anyhow::ensure!(events.len() <= MAX_EVENTS, "too many indentation captures");
            }
        }
        drop(matches);
        anyhow::ensure!(
            !cancelled && !cursor.did_exceed_match_limit(),
            "indentation query budget exceeded"
        );
        events.sort_by_key(|e| (e.bytes.start, e.bytes.end));
        Ok(Some(events))
    }
}

fn significant_events(events: Vec<Event>) -> Vec<Event> {
    let mut ignored: Vec<Range<usize>> = Vec::new();
    for event in events.iter().filter(|e| e.kind == Kind::Ignore) {
        if let Some(last) = ignored
            .last_mut()
            .filter(|last| event.bytes.start <= last.end)
        {
            last.end = last.end.max(event.bytes.end);
        } else {
            ignored.push(event.bytes.clone());
        }
    }
    let mut result = events
        .into_iter()
        .filter(|e| {
            if e.kind == Kind::Ignore {
                return false;
            }
            let index = ignored.partition_point(|range| range.start <= e.bytes.start);
            index == 0 || e.bytes.end > ignored[index - 1].end
        })
        .collect::<Vec<_>>();
    result.dedup_by(|a, b| a.kind == b.kind && a.bytes == b.bytes && a.key == b.key);
    result
}

fn apply_event<'a>(stack: &mut Vec<&'a Event>, event: &'a Event) {
    match event.kind {
        Kind::Begin => stack.push(event),
        Kind::End => {
            if let Some(index) = stack.iter().rposition(|open| open.key == event.key) {
                stack.truncate(index);
            }
        }
        _ => {}
    }
}

/// Applies adjacent leading closers that unwind one visual indentation level.
///
/// Multiple openers on one source line contribute one level, so their matching
/// closers must leave that level together. A closer for an opener on an earlier
/// line starts a separate group and remains available to the ordinary depth delta.
fn apply_leading_closer_group<'a>(stack: &mut Vec<&'a Event>, events: &'a [Event], source: &str) {
    let Some(first) = events.first().filter(|event| event.kind == Kind::End) else {
        return;
    };
    let Some(opener_line) = stack
        .iter()
        .rfind(|open| open.key == first.key)
        .map(|open| open.line)
    else {
        apply_event(stack, first);
        return;
    };

    let closer_line = first.line;
    let mut previous_end = first.bytes.end;
    apply_event(stack, first);
    for event in &events[1..] {
        if event.kind != Kind::End
            || event.line != closer_line
            || event.bytes.start < previous_end
            || !source.as_bytes()[previous_end..event.bytes.start]
                .iter()
                .all(|byte| matches!(byte, b' ' | b'\t'))
            || stack
                .iter()
                .rfind(|open| open.key == event.key)
                .is_none_or(|open| open.line != opener_line)
        {
            break;
        }
        previous_end = event.bytes.end;
        apply_event(stack, event);
    }
}

fn depth(stack: &[&Event]) -> usize {
    stack.iter().map(|e| e.line).collect::<HashSet<_>>().len()
}

fn columns(line: &str, tab_width: usize) -> usize {
    let whitespace = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
    display_width_with_tabs(whitespace, tab_width.max(1))
}

fn point(source: &str, byte: usize) -> Point {
    let prefix = &source[..byte];
    Point::new(
        prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix.rfind('\n').map_or(byte, |i| byte - i - 1),
    )
}

pub(crate) fn replacement_edit(old: &str, new: &str) -> InputEdit {
    let mut start = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    let mut suffix = old[start..]
        .bytes()
        .rev()
        .zip(new[start..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(old, start),
        old_end_position: point(old, old_end),
        new_end_position: point(new, new_end),
    }
}

/// Runs portable indentation fixtures against the same provider used by the editor.
pub fn check_fixtures(
    path: &std::path::Path,
    registry: Arc<LanguageRegistry>,
) -> anyhow::Result<usize> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fixture {
        name: String,
        language: String,
        source: String,
        line: usize,
        expected: usize,
        #[serde(default = "four")]
        width: usize,
    }
    fn four() -> usize {
        4
    }
    let fixtures: Vec<Fixture> = serde_json::from_str(&std::fs::read_to_string(path)?)
        .context("invalid indentation fixtures")?;
    for fixture in &fixtures {
        anyhow::ensure!(
            registry.indentation_language(&fixture.language).is_some(),
            "{}: no indentation queries loaded for {}",
            fixture.name,
            fixture.language
        );
    }
    let mut service = SyntaxIndentation::new(registry);
    for fixture in &fixtures {
        let buffer = crate::buffer::Buffer::new(None, fixture.source.clone());
        let mut decision = service.indent(IndentRequest {
            id: buffer.id(),
            revision: buffer.revision(),
            language: &fixture.language,
            source: &fixture.source,
            line: fixture.line,
            shift_width: fixture.width,
            tab_width: fixture.width,
            reason: IndentReason::NewLine,
        });
        if decision == IndentDecision::Keep {
            decision = crate::indent::indent_for_line(
                Some(&fixture.language),
                &fixture.source,
                fixture.line,
                fixture.width,
                fixture.width,
            );
        }
        if decision == IndentDecision::Keep {
            let line = fixture
                .source
                .split('\n')
                .nth(fixture.line)
                .ok_or_else(|| anyhow!("{}: target line is out of range", fixture.name))?;
            decision = IndentDecision::Columns(columns(line, fixture.width));
        }
        anyhow::ensure!(
            decision == IndentDecision::Columns(fixture.expected),
            "{}: expected {} columns, got {:?}",
            fixture.name,
            fixture.expected,
            decision
        );
    }
    Ok(fixtures.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    #[test]
    fn bundled_language_behavior_fixtures() {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/indent/bundled.json"
        ));
        assert_eq!(
            check_fixtures(path, Arc::new(LanguageRegistry::bundled())).unwrap(),
            18
        );
    }

    #[test]
    fn every_bundled_indentation_query_compiles() {
        let registry = LanguageRegistry::bundled();
        for language in [
            "rust",
            "javascript",
            "jsx",
            "typescript",
            "tsx",
            "json",
            "toml",
            "powershell",
            "bash",
            "fish",
            "lua",
            "yaml",
        ] {
            let (grammar, source) = registry.indentation_language(language).unwrap();
            let query =
                Query::new(&grammar, &source).unwrap_or_else(|error| panic!("{language}: {error}"));
            validate_query(&query).unwrap();
        }
    }

    fn decision(source: &str, line: usize) -> IndentDecision {
        let buffer = Buffer::new(Some("sample.rs".into()), source.into());
        SyntaxIndentation::new(Arc::new(LanguageRegistry::bundled())).indent(IndentRequest {
            id: buffer.id(),
            revision: buffer.revision(),
            language: "rust",
            source,
            line,
            shift_width: 4,
            tab_width: 4,
            reason: IndentReason::NewLine,
        })
    }

    #[test]
    fn rust_nested_incomplete_blocks_and_closers() {
        for (source, line, expected) in [
            ("fn wrap() {\n    if pos.x > 0 {\n\n}", 2, 8),
            ("fn wrap() {\n    if ready {\n        work();\n\n}", 3, 8),
            (
                "fn wrap() {\n    if ready {\n        work();\n        }\n}",
                3,
                4,
            ),
            (
                "fn wrap() {\n    if ready {\n        work();\n    }\n\n}",
                4,
                4,
            ),
            ("fn wrap() {\n    call(\n\n    );\n}", 2, 8),
            ("fn wrap() {\n\n\n}", 2, 4),
            ("fn wrap() {\n    let s = r###\"{[(\"###;\n\n}", 2, 4),
            ("fn wrap() {\n    // {[(\n\n}", 2, 4),
            ("fn wrap() {\n    if ready {}\n\n}", 2, 4),
        ] {
            assert_eq!(
                decision(source, line),
                IndentDecision::Columns(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn rust_compound_closers_preserve_grouped_opener_depth() {
        for (source, line, expected) in [
            (
                "        let isolated_client = Self {\n            state: Arc::new(ModelClientState {\n            }),\n            \n        };",
                3,
                12,
            ),
            (
                "fn f() {\n    let value = wrap(Inner {\n    });\n    \n}",
                3,
                4,
            ),
            (
                "fn f() {\n    let value = outer(inner(Inner {\n    }));\n    \n}",
                3,
                4,
            ),
            (
                "fn f() {\n    let value = wrap(Inner {\n    } );\n    \n}",
                3,
                4,
            ),
            (
                "fn f() {\n    outer(\n        inner(Inner {\n        }));\n    \n}",
                4,
                4,
            ),
            (
                "fn f() {\n    outer(\n        inner(\n            value,\n        ));\n    \n}",
                5,
                4,
            ),
        ] {
            assert_eq!(
                decision(source, line),
                IndentDecision::Columns(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn rust_multiline_literals_keep_manual_whitespace() {
        assert_eq!(
            decision("fn f() {\n    let s = r#\"hello\n  world\n\"#;\n}", 2),
            IndentDecision::Keep
        );
    }

    #[test]
    fn incremental_parse_handles_unicode_and_revisions() {
        let buffer = Buffer::new(None, String::new());
        let mut service = SyntaxIndentation::new(Arc::new(LanguageRegistry::bundled()));
        for (revision, source, expected) in [
            (1, "fn f() {\n    // café\n\n}", 4),
            (2, "fn f() {\n    if café {\n\n}", 8),
            (3, "fn f() {\n    // 🦀\n\n}", 4),
        ] {
            assert_eq!(
                service.indent(IndentRequest {
                    id: buffer.id(),
                    revision,
                    language: "rust",
                    source,
                    line: 2,
                    shift_width: 4,
                    tab_width: 4,
                    reason: IndentReason::NewLine
                }),
                IndentDecision::Columns(expected)
            );
        }
    }

    #[test]
    fn unsupported_query_features_fail_closed() {
        let language = tree_sitter_rust::LANGUAGE.into();
        for source in [
            "(block) @indent.unknown",
            "((block) @indent.begin (#set! unsupported \"yes\"))",
            "((block) @indent.begin (#unknown? @indent.begin))",
        ] {
            let query = Query::new(&language, source).unwrap();
            assert!(validate_query(&query).is_err());
        }
    }
}
