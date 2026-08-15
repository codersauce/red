//! Editor-owned structural selection, navigation, and safe sibling swaps.
//!
//! Tree-sitter ranges use UTF-8 bytes. This service converts them to the editor's
//! Unicode-scalar coordinates before exposing a result, and never mutates a buffer.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context as _};
use tree_sitter::{
    Node, ParseOptions, ParseState, Parser, Query, QueryCursor, QueryCursorOptions,
    QueryCursorState, StreamingIterator, Tree,
};

use crate::{
    buffer::{Buffer, BufferId},
    editing::TextObjectScope,
    highlighter::LanguageRegistry,
    undo::{TextPosition, TextRange},
};

const MAX_STRUCTURAL_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_STRUCTURAL_CACHE_ENTRIES: usize = 16;
const MAX_STRUCTURAL_CAPTURES: usize = 50_000;
const MAX_FULL_STRUCTURAL_QUERY_BYTES: usize = 128 * 1024;
const INITIAL_STRUCTURAL_QUERY_WINDOW_BYTES: usize = 32 * 1024;
const STRUCTURAL_PARSE_BUDGET: Duration = Duration::from_millis(1_500);
const STRUCTURAL_QUERY_BUDGET: Duration = Duration::from_millis(150);

/// Structural captures recognized by Red's editor-level Vim integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SyntaxObjectKind {
    Call,
    Function,
    Class,
    Comment,
    Parameter,
}

impl SyntaxObjectKind {
    pub(crate) const fn for_text_object_key(key: char) -> Option<Self> {
        match key {
            'm' => Some(Self::Call),
            'f' => Some(Self::Function),
            'c' => Some(Self::Class),
            'k' => Some(Self::Comment),
            _ => None,
        }
    }

    fn from_capture(name: &str) -> Option<(Self, TextObjectScope)> {
        match name {
            "call.outer" => Some((Self::Call, TextObjectScope::Around)),
            "call.inner" => Some((Self::Call, TextObjectScope::Inner)),
            "function.outer" => Some((Self::Function, TextObjectScope::Around)),
            "function.inner" => Some((Self::Function, TextObjectScope::Inner)),
            "class.outer" => Some((Self::Class, TextObjectScope::Around)),
            "class.inner" => Some((Self::Class, TextObjectScope::Inner)),
            "comment.outer" => Some((Self::Comment, TextObjectScope::Around)),
            "comment.inner" => Some((Self::Comment, TextObjectScope::Inner)),
            "parameter.outer" => Some((Self::Parameter, TextObjectScope::Around)),
            "parameter.inner" => Some((Self::Parameter, TextObjectScope::Inner)),
            _ => None,
        }
    }

    const fn capture_prefix(self) -> &'static str {
        match self {
            Self::Call => "@call.",
            Self::Function => "@function.",
            Self::Class => "@class.",
            Self::Comment => "@comment.",
            Self::Parameter => "@parameter.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedTextObject {
    pub(crate) range: TextRange,
    pub(crate) linewise: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Container {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
struct Capture {
    kind: SyntaxObjectKind,
    scope: TextObjectScope,
    range: TextRange,
    start: usize,
    end: usize,
    container: Option<Container>,
}

struct DocumentCaptures {
    revision: u64,
    language_id: String,
    contents: Arc<str>,
    tree: Tree,
    captures: HashMap<SyntaxObjectKind, Vec<Capture>>,
    searched: HashMap<SyntaxObjectKind, Vec<Range<usize>>>,
}

#[derive(Clone, Copy)]
enum SearchGoal {
    Selection,
    Motion { backward: bool, count: u16 },
    Swap { backward: bool },
}

/// Caches document parses and indexes only the structural object requested by an edit.
pub(crate) struct SyntaxTextObjectService {
    registry: Arc<LanguageRegistry>,
    queries: HashMap<(String, SyntaxObjectKind), Arc<Query>>,
    documents: HashMap<BufferId, DocumentCaptures>,
}

impl SyntaxTextObjectService {
    pub(crate) fn new(registry: Arc<LanguageRegistry>) -> Self {
        Self {
            registry,
            queries: HashMap::new(),
            documents: HashMap::new(),
        }
    }

    pub(crate) fn reset(&mut self, registry: Arc<LanguageRegistry>) {
        self.registry = registry;
        self.queries.clear();
        self.documents.clear();
    }

    pub(crate) fn invalidate(&mut self, id: BufferId) {
        self.documents.remove(&id);
    }

    pub(crate) fn select(
        &mut self,
        buffer: &Buffer,
        language_id: &str,
        cursor: TextPosition,
        kind: SyntaxObjectKind,
        scope: TextObjectScope,
    ) -> anyhow::Result<Option<ResolvedTextObject>> {
        let Some(document) =
            self.document(buffer, language_id, kind, cursor, SearchGoal::Selection)?
        else {
            return Ok(None);
        };

        let candidates = |scope| {
            document
                .captures
                .get(&kind)
                .into_iter()
                .flatten()
                .copied()
                .filter(move |capture| capture.scope == scope)
        };

        let selected = if scope == TextObjectScope::Inner {
            let outer = candidates(TextObjectScope::Around)
                .filter(|capture| contains_position(capture.range, cursor))
                .min_by_key(|capture| capture.end.saturating_sub(capture.start));
            if let Some(outer) = outer {
                candidates(scope)
                    .filter(|capture| contains_range(outer.range, capture.range))
                    .filter(|capture| contains_position(capture.range, cursor))
                    .min_by_key(|capture| capture.end.saturating_sub(capture.start))
                    .or_else(|| {
                        candidates(scope)
                            .filter(|capture| contains_range(outer.range, capture.range))
                            .filter(|capture| {
                                position_key(capture.range.start) >= position_key(cursor)
                            })
                            .min_by_key(|capture| capture.start)
                    })
            } else {
                best_capture(candidates(scope), cursor, true)
            }
        } else {
            best_capture(candidates(scope), cursor, true)
        };

        Ok(selected.map(|capture| ResolvedTextObject {
            range: capture.range,
            linewise: scope == TextObjectScope::Around
                && matches!(kind, SyntaxObjectKind::Function | SyntaxObjectKind::Class),
        }))
    }

    pub(crate) fn motion_target(
        &mut self,
        buffer: &Buffer,
        language_id: &str,
        cursor: TextPosition,
        kind: SyntaxObjectKind,
        backward: bool,
        count: u16,
    ) -> anyhow::Result<Option<TextPosition>> {
        let Some(document) = self.document(
            buffer,
            language_id,
            kind,
            cursor,
            SearchGoal::Motion { backward, count },
        )?
        else {
            return Ok(None);
        };
        let mut positions = document
            .captures
            .get(&kind)
            .into_iter()
            .flatten()
            .filter(|capture| capture.scope == TextObjectScope::Around)
            .map(|capture| capture.range.start)
            .filter(|position| {
                if backward {
                    position_key(*position) < position_key(cursor)
                } else {
                    position_key(*position) > position_key(cursor)
                }
            })
            .collect::<Vec<_>>();
        positions.sort_unstable_by_key(|position| position_key(*position));
        positions.dedup_by_key(|position| position_key(*position));
        if backward {
            positions.reverse();
        }
        Ok(positions
            .into_iter()
            .nth(usize::from(count.saturating_sub(1))))
    }

    pub(crate) fn swap_ranges(
        &mut self,
        buffer: &Buffer,
        language_id: &str,
        cursor: TextPosition,
        kind: SyntaxObjectKind,
        backward: bool,
    ) -> anyhow::Result<Option<(TextRange, TextRange)>> {
        let Some(document) = self.document(
            buffer,
            language_id,
            kind,
            cursor,
            SearchGoal::Swap { backward },
        )?
        else {
            return Ok(None);
        };
        let scope = if kind == SyntaxObjectKind::Parameter {
            TextObjectScope::Inner
        } else {
            TextObjectScope::Around
        };
        let candidates = document
            .captures
            .get(&kind)
            .into_iter()
            .flatten()
            .copied()
            .filter(|capture| capture.scope == scope)
            .collect::<Vec<_>>();
        let Some(current) = best_capture(candidates.iter().copied(), cursor, false) else {
            return Ok(None);
        };
        let adjacent = candidates
            .into_iter()
            .filter(|candidate| {
                candidate.container == current.container
                    && if backward {
                        candidate.end <= current.start
                    } else {
                        candidate.start >= current.end
                    }
            })
            .min_by_key(|candidate| {
                if backward {
                    current.start.saturating_sub(candidate.end)
                } else {
                    candidate.start.saturating_sub(current.end)
                }
            });

        Ok(adjacent.map(|capture| (current.range, capture.range)))
    }

    fn document(
        &mut self,
        buffer: &Buffer,
        language_id: &str,
        kind: SyntaxObjectKind,
        position: TextPosition,
        goal: SearchGoal,
    ) -> anyhow::Result<Option<&DocumentCaptures>> {
        let id = buffer.id();
        let Some((language, source)) = self.registry.textobject_language(language_id) else {
            self.documents.remove(&id);
            return Ok(None);
        };
        let current = self.documents.get(&id).is_some_and(|document| {
            document.revision == buffer.revision() && document.language_id == language_id
        });
        if !current {
            let contents: Arc<str> = Arc::from(buffer.contents());
            if contents.len() > MAX_STRUCTURAL_DOCUMENT_BYTES {
                return Err(anyhow!(
                    "structural navigation is unavailable for documents larger than {} bytes",
                    MAX_STRUCTURAL_DOCUMENT_BYTES
                ));
            }
            let mut parser = Parser::new();
            parser
                .set_language(&language)
                .with_context(|| format!("incompatible structural grammar for {language_id}"))?;
            let started = Instant::now();
            let bytes = contents.as_bytes();
            let mut progress = |_: &ParseState| started.elapsed() > STRUCTURAL_PARSE_BUDGET;
            let tree = parser
                .parse_with_options(
                    &mut |offset, _| bytes.get(offset..).unwrap_or_default(),
                    None,
                    Some(ParseOptions::new().progress_callback(&mut progress)),
                )
                .ok_or_else(|| anyhow!("structural document parsing exceeded its time budget"))?;

            if self.documents.len() >= MAX_STRUCTURAL_CACHE_ENTRIES {
                self.documents.clear();
            }
            self.documents.insert(
                id,
                DocumentCaptures {
                    revision: buffer.revision(),
                    language_id: language_id.to_string(),
                    contents,
                    tree,
                    captures: HashMap::new(),
                    searched: HashMap::new(),
                },
            );
        }

        let query_key = (language_id.to_string(), kind);
        let query = if let Some(query) = self.queries.get(&query_key) {
            Arc::clone(query)
        } else {
            let scoped_source = structural_query_patterns(&source)
                .into_iter()
                .filter(|pattern| pattern.contains(kind.capture_prefix()))
                .collect::<Vec<_>>()
                .join("\n\n");
            if scoped_source.is_empty() {
                return Ok(self.documents.get(&id));
            }
            let query = Arc::new(
                Query::new(&language, &scoped_source)
                    .with_context(|| format!("invalid text-object query for {language_id}"))?,
            );
            self.queries.insert(query_key, Arc::clone(&query));
            query
        };

        let Some(document) = self.documents.get(&id) else {
            return Ok(None);
        };
        let length = document.contents.len();
        let cursor_byte = buffer.position_to_byte_idx(position).min(length);
        let started = Instant::now();
        let mut window = if length <= MAX_FULL_STRUCTURAL_QUERY_BYTES {
            length
        } else {
            INITIAL_STRUCTURAL_QUERY_WINDOW_BYTES
        };
        loop {
            let search = goal.range(cursor_byte, length, window);
            let covered = self.documents.get(&id).is_some_and(|document| {
                document.searched.get(&kind).is_some_and(|ranges| {
                    ranges
                        .iter()
                        .any(|range| range.start <= search.start && search.end <= range.end)
                })
            });
            if !covered {
                self.collect_captures(id, buffer, &query, kind, search.clone(), started)?;
            }
            let Some(document) = self.documents.get(&id) else {
                return Ok(None);
            };
            if goal.satisfied(document, kind, position)
                || (search.start == 0 && search.end == length)
                || matches!(
                    goal,
                    SearchGoal::Motion {
                        backward: false,
                        ..
                    }
                ) && search.end == length
                || matches!(goal, SearchGoal::Motion { backward: true, .. }) && search.start == 0
            {
                break;
            }
            if started.elapsed() > STRUCTURAL_QUERY_BUDGET {
                return Err(anyhow!("structural query exceeded its time budget"));
            }
            window = window.saturating_mul(2).min(length.max(1));
        }
        Ok(self.documents.get(&id))
    }

    fn collect_captures(
        &mut self,
        id: BufferId,
        buffer: &Buffer,
        query: &Query,
        kind: SyntaxObjectKind,
        search: Range<usize>,
        started: Instant,
    ) -> anyhow::Result<()> {
        let Some(document) = self.documents.get(&id) else {
            return Ok(());
        };
        let contents = Arc::clone(&document.contents);
        let tree = document.tree.clone();
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(8_192);
        cursor.set_byte_range(search.clone());
        let mut progress = |_: &QueryCursorState| started.elapsed() > STRUCTURAL_QUERY_BUDGET;
        let mut matches = cursor.matches_with_options(
            query,
            tree.root_node(),
            contents.as_bytes(),
            QueryCursorOptions::new().progress_callback(&mut progress),
        );
        let mut captures = Vec::new();
        while let Some(matched) = matches.next() {
            let mut grouped = HashMap::<u32, (usize, usize, Node<'_>)>::new();
            for capture in matched.captures {
                grouped
                    .entry(capture.index)
                    .and_modify(|range| {
                        range.0 = range.0.min(capture.node.start_byte());
                        range.1 = range.1.max(capture.node.end_byte());
                    })
                    .or_insert((
                        capture.node.start_byte(),
                        capture.node.end_byte(),
                        capture.node,
                    ));
            }
            for (index, (start, end, node)) in grouped {
                let Some((capture_kind, scope)) = query
                    .capture_names()
                    .get(index as usize)
                    .and_then(|name| SyntaxObjectKind::from_capture(name))
                else {
                    continue;
                };
                if capture_kind != kind {
                    continue;
                }
                let Some(range) = byte_range(buffer, start, end) else {
                    continue;
                };
                captures.push(Capture {
                    kind,
                    scope,
                    range,
                    start,
                    end,
                    container: capture_container(kind, node),
                });
                if captures.len() > MAX_STRUCTURAL_CAPTURES {
                    return Err(anyhow!("structural query produced too many captures"));
                }
            }
        }
        if started.elapsed() > STRUCTURAL_QUERY_BUDGET {
            return Err(anyhow!("structural query exceeded its time budget"));
        }
        if kind == SyntaxObjectKind::Comment {
            synthesize_comment_interiors(buffer, &contents, &mut captures);
        }
        if kind == SyntaxObjectKind::Function && document.language_id == "fish" {
            synthesize_fish_function_interiors(buffer, &contents, &mut captures);
        }
        let Some(document) = self.documents.get_mut(&id) else {
            return Ok(());
        };
        let existing = document.captures.entry(kind).or_default();
        existing.extend(captures);
        let mut seen = HashSet::new();
        existing.retain(|capture| {
            seen.insert((
                capture.scope == TextObjectScope::Around,
                capture.start,
                capture.end,
            ))
        });
        if existing.len() > MAX_STRUCTURAL_CAPTURES {
            return Err(anyhow!("structural query produced too many captures"));
        }
        existing.sort_unstable_by_key(|capture| (capture.start, capture.end));
        document.searched.entry(kind).or_default().push(search);
        Ok(())
    }
}

impl SearchGoal {
    fn range(self, position: usize, length: usize, window: usize) -> Range<usize> {
        if length <= MAX_FULL_STRUCTURAL_QUERY_BYTES {
            return 0..length;
        }
        match self {
            Self::Motion {
                backward: false, ..
            } => position..position.saturating_add(window).min(length),
            Self::Motion { backward: true, .. } => {
                position.saturating_sub(window)..position.saturating_add(1).min(length)
            }
            Self::Selection | Self::Swap { .. } => {
                position.saturating_sub(window)..position.saturating_add(window).min(length)
            }
        }
    }

    fn satisfied(
        self,
        document: &DocumentCaptures,
        kind: SyntaxObjectKind,
        position: TextPosition,
    ) -> bool {
        let Some(captures) = document.captures.get(&kind) else {
            return false;
        };
        match self {
            Self::Selection => captures.iter().any(|capture| {
                contains_position(capture.range, position)
                    || position_key(capture.range.start) > position_key(position)
            }),
            Self::Motion { backward, count } => {
                let mut positions = captures
                    .iter()
                    .filter(|capture| capture.scope == TextObjectScope::Around)
                    .map(|capture| position_key(capture.range.start))
                    .filter(|start| {
                        if backward {
                            *start < position_key(position)
                        } else {
                            *start > position_key(position)
                        }
                    })
                    .collect::<Vec<_>>();
                positions.sort_unstable();
                positions.dedup();
                positions.len() >= usize::from(count.max(1))
            }
            Self::Swap { backward } => {
                let scope = if kind == SyntaxObjectKind::Parameter {
                    TextObjectScope::Inner
                } else {
                    TextObjectScope::Around
                };
                let Some(current) = best_capture(
                    captures
                        .iter()
                        .copied()
                        .filter(|capture| capture.scope == scope),
                    position,
                    false,
                ) else {
                    return false;
                };
                captures.iter().any(|capture| {
                    capture.scope == scope
                        && capture.container == current.container
                        && if backward {
                            capture.end <= current.start
                        } else {
                            capture.start >= current.end
                        }
                })
            }
        }
    }
}

fn structural_query_patterns(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut patterns = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut comment = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if comment {
            if byte == b'\n' {
                comment = false;
            }
            continue;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b';' => comment = true,
            b'"' => quoted = true,
            b'(' | b'[' => {
                if depth == 0 {
                    if let Some(previous) = start.replace(index) {
                        let pattern = source[previous..index].trim();
                        if !pattern.is_empty() {
                            patterns.push(pattern);
                        }
                    } else {
                        start = Some(index);
                    }
                }
                depth += 1;
            }
            b')' | b']' => depth = depth.saturating_sub(1),
            b'\n' if depth == 0 => {
                if let Some(previous) = start.take() {
                    let pattern = source[previous..index].trim();
                    if !pattern.is_empty() {
                        patterns.push(pattern);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(start) = start {
        let pattern = source[start..].trim();
        if !pattern.is_empty() {
            patterns.push(pattern);
        }
    }
    patterns
}

fn byte_range(buffer: &Buffer, start: usize, end: usize) -> Option<TextRange> {
    Some(TextRange::new(
        buffer.byte_idx_to_position(start)?,
        buffer.byte_idx_to_position(end)?,
    ))
}

fn position_key(position: TextPosition) -> (usize, usize) {
    (position.line, position.character)
}

fn contains_position(range: TextRange, position: TextPosition) -> bool {
    position_key(range.start) <= position_key(position)
        && position_key(position) < position_key(range.end)
}

fn contains_range(outer: TextRange, inner: TextRange) -> bool {
    position_key(outer.start) <= position_key(inner.start)
        && position_key(inner.end) <= position_key(outer.end)
}

fn best_capture(
    captures: impl Iterator<Item = Capture>,
    cursor: TextPosition,
    lookahead: bool,
) -> Option<Capture> {
    let captures = captures.collect::<Vec<_>>();
    captures
        .iter()
        .copied()
        .filter(|capture| contains_position(capture.range, cursor))
        .min_by_key(|capture| capture.end.saturating_sub(capture.start))
        .or_else(|| {
            lookahead.then(|| {
                captures
                    .into_iter()
                    .filter(|capture| position_key(capture.range.start) > position_key(cursor))
                    .min_by_key(|capture| capture.start)
            })?
        })
}

fn capture_container(kind: SyntaxObjectKind, node: Node<'_>) -> Option<Container> {
    let mut parent = node.parent();
    if kind == SyntaxObjectKind::Parameter {
        while let Some(candidate) = parent {
            if matches!(
                candidate.kind(),
                "arguments"
                    | "parameters"
                    | "formal_parameters"
                    | "argument_list"
                    | "parameter_list"
                    | "type_parameters"
                    | "type_arguments"
                    | "class_method_parameter_list"
                    | "object_type"
                    | "interface_body"
                    | "array"
                    | "inline_table"
                    | "table_constructor"
            ) {
                return Some(Container {
                    start: candidate.start_byte(),
                    end: candidate.end_byte(),
                });
            }
            parent = candidate.parent();
        }
        return None;
    }
    parent.map(|candidate| Container {
        start: candidate.start_byte(),
        end: candidate.end_byte(),
    })
}

fn synthesize_comment_interiors(buffer: &Buffer, source: &str, captures: &mut Vec<Capture>) {
    let additions = captures
        .iter()
        .filter(|capture| {
            capture.kind == SyntaxObjectKind::Comment && capture.scope == TextObjectScope::Around
        })
        .filter_map(|capture| {
            if captures.iter().any(|inner| {
                inner.kind == SyntaxObjectKind::Comment
                    && inner.scope == TextObjectScope::Inner
                    && contains_range(capture.range, inner.range)
            }) {
                return None;
            }
            let comment = source.get(capture.start..capture.end)?;
            let (prefix, suffix) = if comment.starts_with("--[[") {
                (4, 2)
            } else if comment.starts_with("/*") || comment.starts_with("<#") {
                (2, 2)
            } else if comment.starts_with("//") || comment.starts_with("--") {
                (2, 0)
            } else if comment.starts_with('#') {
                (1, 0)
            } else {
                return None;
            };
            let mut start = capture.start.saturating_add(prefix);
            let end = capture.end.saturating_sub(suffix);
            if source.get(start..end)?.starts_with(' ') {
                start += 1;
            }
            if start >= end {
                return None;
            }
            Some(Capture {
                kind: SyntaxObjectKind::Comment,
                scope: TextObjectScope::Inner,
                range: byte_range(buffer, start, end)?,
                start,
                end,
                container: capture.container,
            })
        })
        .collect::<Vec<_>>();
    captures.extend(additions);
}

fn synthesize_fish_function_interiors(buffer: &Buffer, source: &str, captures: &mut Vec<Capture>) {
    let additions = captures
        .iter()
        .filter(|capture| {
            capture.kind == SyntaxObjectKind::Function && capture.scope == TextObjectScope::Around
        })
        .filter_map(|capture| {
            let function = source.get(capture.start..capture.end)?;
            let start = capture.start + function.find('\n')? + 1;
            let end = capture.start + function.rfind('\n')?;
            if start >= end {
                return None;
            }
            Some(Capture {
                kind: SyntaxObjectKind::Function,
                scope: TextObjectScope::Inner,
                range: byte_range(buffer, start, end)?,
                start,
                end,
                container: capture.container,
            })
        })
        .collect::<Vec<_>>();
    captures.extend(additions);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tree_sitter::Query;

    use super::{structural_query_patterns, SyntaxObjectKind, SyntaxTextObjectService};
    use crate::{
        buffer::Buffer,
        editing::TextObjectScope,
        highlighter::LanguageRegistry,
        undo::{TextPosition, TextRange},
    };

    #[test]
    fn every_bundled_structural_query_compiles_against_its_grammar() {
        let registry = LanguageRegistry::bundled();
        for language_id in [
            "rust",
            "markdown",
            "javascript",
            "jsx",
            "typescript",
            "tsx",
            "json",
            "toml",
            "yaml",
            "bash",
            "fish",
            "powershell",
            "lua",
        ] {
            let (language, source) = registry
                .textobject_language(language_id)
                .unwrap_or_else(|| panic!("{language_id} should have structural queries"));
            Query::new(&language, &source).unwrap_or_else(|error| {
                panic!("{language_id} structural query is invalid: {error}")
            });
            for kind in [
                SyntaxObjectKind::Call,
                SyntaxObjectKind::Function,
                SyntaxObjectKind::Class,
                SyntaxObjectKind::Comment,
                SyntaxObjectKind::Parameter,
            ] {
                let scoped = structural_query_patterns(&source)
                    .into_iter()
                    .filter(|pattern| pattern.contains(kind.capture_prefix()))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !scoped.is_empty() {
                    Query::new(&language, &scoped).unwrap_or_else(|error| {
                        panic!("{language_id} {kind:?} structural query is invalid: {error}")
                    });
                }
            }
        }
    }

    #[test]
    fn real_rust_source_navigation_scans_only_function_patterns() {
        let buffer = Buffer::new(
            Some("main.rs".to_string()),
            include_str!("main.rs").to_string(),
        );
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));
        let cursor = TextPosition::new(89, 0);

        let target = service
            .motion_target(
                &buffer,
                "rust",
                cursor,
                SyntaxObjectKind::Function,
                false,
                1,
            )
            .unwrap()
            .expect("Red's real main.rs must contain another function");

        assert!(target.line > cursor.line);
        assert_eq!(service.queries.len(), 1);
        assert!(service
            .queries
            .contains_key(&("rust".to_string(), SyntaxObjectKind::Function)));
    }

    #[test]
    fn large_documents_expand_directional_windows_without_reparsing() {
        let source = format!("fn first() {{}}\n{}\nfn next() {{}}\n", " ".repeat(150_000));
        let buffer = Buffer::new(Some("large.rs".to_string()), source);
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));

        let target = service
            .motion_target(
                &buffer,
                "rust",
                TextPosition::new(0, 0),
                SyntaxObjectKind::Function,
                false,
                1,
            )
            .unwrap()
            .expect("the directional query should expand until the next function");
        assert_eq!(target, TextPosition::new(2, 0));

        let parsed = std::ptr::from_ref(&service.documents[&buffer.id()].tree);
        let previous = service
            .motion_target(&buffer, "rust", target, SyntaxObjectKind::Function, true, 1)
            .unwrap();
        assert_eq!(previous, Some(TextPosition::new(0, 0)));
        assert!(std::ptr::eq(parsed, &service.documents[&buffer.id()].tree));
    }

    #[test]
    fn rust_function_body_combines_repeated_captures() {
        let buffer = Buffer::new(
            Some("sample.rs".to_string()),
            "fn one() {\n    first();\n    second();\n}\n".to_string(),
        );
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));
        let object = service
            .select(
                &buffer,
                "rust",
                TextPosition::new(0, 3),
                SyntaxObjectKind::Function,
                TextObjectScope::Inner,
            )
            .unwrap()
            .unwrap();

        assert_eq!(
            buffer.text_in_range(object.range),
            "first();\n    second();"
        );
        assert!(!object.linewise);
    }

    #[test]
    fn synthesized_comment_interiors_preserve_unicode_scalar_positions() {
        let buffer = Buffer::new(
            Some("sample.rs".to_string()),
            "let café = 1; // comentário\n".to_string(),
        );
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));
        let object = service
            .select(
                &buffer,
                "rust",
                TextPosition::new(0, 18),
                SyntaxObjectKind::Comment,
                TextObjectScope::Inner,
            )
            .unwrap()
            .unwrap();

        assert_eq!(buffer.text_in_range(object.range), "comentário");
    }

    #[test]
    fn selection_prefers_the_smallest_nested_capture_and_looks_ahead() {
        let source = "// before\nfn outer() {\n    fn nested() { target(); }\n}\n";
        let buffer = Buffer::new(Some("sample.rs".to_string()), source.to_string());
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));

        let nested = service
            .select(
                &buffer,
                "rust",
                TextPosition::new(2, 23),
                SyntaxObjectKind::Function,
                TextObjectScope::Around,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            buffer.text_in_range(nested.range),
            "fn nested() { target(); }"
        );

        let next = service
            .select(
                &buffer,
                "rust",
                TextPosition::new(0, 0),
                SyntaxObjectKind::Function,
                TextObjectScope::Around,
            )
            .unwrap()
            .unwrap();
        assert_eq!(next.range.start, TextPosition::new(1, 0));
    }

    #[test]
    fn cache_refreshes_after_edit_and_registry_replacement() {
        let mut buffer = Buffer::new(
            Some("sample.rs".to_string()),
            "fn first() {}\nfn second() {}\n".to_string(),
        );
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));

        service
            .select(
                &buffer,
                "rust",
                TextPosition::new(0, 0),
                SyntaxObjectKind::Function,
                TextObjectScope::Around,
            )
            .unwrap();
        let previous_revision = service.documents[&buffer.id()].revision;
        assert_eq!(service.queries.len(), 1);

        buffer.replace_range_raw(
            TextRange::new(TextPosition::new(0, 0), TextPosition::new(0, 13)),
            "let value = 1;",
        );
        let next = service
            .select(
                &buffer,
                "rust",
                TextPosition::new(0, 0),
                SyntaxObjectKind::Function,
                TextObjectScope::Around,
            )
            .unwrap()
            .unwrap();
        assert_eq!(next.range.start, TextPosition::new(1, 0));
        assert!(service.documents[&buffer.id()].revision > previous_revision);

        service.reset(Arc::new(LanguageRegistry::bundled()));
        assert!(service.documents.is_empty());
        assert!(service.queries.is_empty());
    }

    #[test]
    fn typescript_inherits_ecmascript_function_and_class_queries() {
        let buffer = Buffer::new(
            Some("sample.ts".to_string()),
            "class Example {\n  method(value: string) { return value; }\n}\n".to_string(),
        );
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));

        let class = service
            .select(
                &buffer,
                "typescript",
                TextPosition::new(0, 6),
                SyntaxObjectKind::Class,
                TextObjectScope::Around,
            )
            .unwrap()
            .unwrap();
        assert!(class.linewise);
        assert!(buffer
            .text_in_range(class.range)
            .starts_with("class Example"));

        let function = service
            .select(
                &buffer,
                "typescript",
                TextPosition::new(1, 4),
                SyntaxObjectKind::Function,
                TextObjectScope::Inner,
            )
            .unwrap()
            .unwrap();
        assert_eq!(buffer.text_in_range(function.range), "return value;");
    }

    #[test]
    fn fish_function_body_is_synthesized_without_neovim_directives() {
        let buffer = Buffer::new(
            Some("sample.fish".to_string()),
            "function greeting\n    echo olá\nend\n".to_string(),
        );
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));

        let body = service
            .select(
                &buffer,
                "fish",
                TextPosition::new(0, 3),
                SyntaxObjectKind::Function,
                TextObjectScope::Inner,
            )
            .unwrap()
            .unwrap();

        assert_eq!(buffer.text_in_range(body.range), "    echo olá");
    }

    #[test]
    fn unsupported_languages_and_oversized_documents_fail_safely() {
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));
        let unsupported = Buffer::new(Some("sample.hk".to_string()), "fn work() {}".to_string());
        assert!(service
            .select(
                &unsupported,
                "husk",
                TextPosition::new(0, 0),
                SyntaxObjectKind::Function,
                TextObjectScope::Around,
            )
            .unwrap()
            .is_none());

        let oversized = Buffer::new(
            Some("sample.rs".to_string()),
            " ".repeat(super::MAX_STRUCTURAL_DOCUMENT_BYTES + 1),
        );
        let error = service
            .select(
                &oversized,
                "rust",
                TextPosition::new(0, 0),
                SyntaxObjectKind::Function,
                TextObjectScope::Around,
            )
            .unwrap_err();
        assert!(error.to_string().contains("larger than"));
    }
}
