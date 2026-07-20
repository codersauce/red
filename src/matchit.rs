//! Configurable matching motions for brackets and language-specific token groups.
//!
//! Match discovery combines literal pairs with syntax-aware token groups configured per
//! language. Returned positions use editor grapheme coordinates; tree-sitter byte spans
//! are converted before crossing the module boundary.

use regex::Regex;

use crate::{
    config::{MatchitConfig, MatchitLanguageConfig},
    undo::{TextPosition, TextRange},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Charwise,
    Linewise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchMotion {
    pub target: TextPosition,
    pub target_end: TextPosition,
    pub kind: MatchKind,
}

impl MatchMotion {
    pub fn range_from(self, start: TextPosition) -> TextRange {
        if self.kind == MatchKind::Linewise {
            let (start_line, end_line) = if start.line <= self.target.line {
                (start.line, self.target.line + 1)
            } else {
                (self.target.line, start.line + 1)
            };
            return TextRange::new(
                TextPosition::new(start_line, 0),
                TextPosition::new(end_line, 0),
            );
        }

        if position_le(start, self.target) {
            TextRange::new(start, self.target_end)
        } else {
            TextRange::new(self.target, advance_position(start))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenRole {
    Open,
    Middle,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Pair,
    Comment,
    Preprocessor,
    Language,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    start: usize,
    end: usize,
    group: usize,
    item: usize,
    role: TokenRole,
    kind: TokenKind,
    linewise: bool,
}

#[derive(Debug, Clone)]
struct Group {
    kind: TokenKind,
    patterns: Vec<Pattern>,
    linewise: bool,
}

#[derive(Debug, Clone)]
enum Pattern {
    Literal(String),
    Regex(Regex),
}

impl Pattern {
    fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    fn regex(value: &str) -> Option<Self> {
        Regex::new(value).ok().map(Self::Regex)
    }
}

#[derive(Debug)]
struct Document {
    text: String,
    line_starts: Vec<usize>,
}

impl Document {
    fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (idx, c) in text.chars().enumerate() {
            if c == '\n' {
                line_starts.push(idx + 1);
            }
        }
        Self {
            text: text.to_string(),
            line_starts,
        }
    }

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn offset_for_position(&self, position: TextPosition) -> usize {
        let line_start = self
            .line_starts
            .get(position.line)
            .copied()
            .unwrap_or_else(|| self.char_len());
        let line_end = self
            .line_starts
            .get(position.line + 1)
            .copied()
            .map(|end| end.saturating_sub(1))
            .unwrap_or_else(|| self.char_len());
        line_start + position.character.min(line_end.saturating_sub(line_start))
    }

    fn position_for_offset(&self, offset: usize) -> TextPosition {
        let offset = offset.min(self.char_len());
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        };
        TextPosition::new(line, offset.saturating_sub(self.line_starts[line]))
    }

    fn line_bounds(&self, line: usize) -> (usize, usize) {
        let start = self.line_starts.get(line).copied().unwrap_or(0);
        let end = self
            .line_starts
            .get(line + 1)
            .copied()
            .map(|end| end.saturating_sub(1))
            .unwrap_or_else(|| self.char_len());
        (start, end)
    }
}

pub fn find_motion(
    text: &str,
    cursor: TextPosition,
    language_id: Option<&str>,
    config: &MatchitConfig,
    direction: MatchDirection,
) -> Option<MatchMotion> {
    let doc = Document::new(text);
    let tokens = tokens_for_document(&doc, language_id, config);
    let cursor_offset = doc.offset_for_position(cursor);
    let token = choose_token_on_line(&doc, &tokens, cursor.line, cursor_offset)?;
    let target = match direction {
        MatchDirection::Forward => matching_token(&tokens, token, MatchDirection::Forward),
        MatchDirection::Backward => matching_token(&tokens, token, MatchDirection::Backward),
    }?;
    Some(motion_for_token(&doc, target))
}

pub fn find_unmatched_group(
    text: &str,
    cursor: TextPosition,
    language_id: Option<&str>,
    config: &MatchitConfig,
    direction: MatchDirection,
) -> Option<MatchMotion> {
    let doc = Document::new(text);
    let tokens = tokens_for_document(&doc, language_id, config);
    let cursor_offset = doc.offset_for_position(cursor);
    let target = match direction {
        MatchDirection::Backward => unmatched_before(&tokens, cursor_offset),
        MatchDirection::Forward => unmatched_after(&tokens, cursor_offset),
    }?;
    Some(motion_for_token(&doc, target))
}

pub fn select_around(
    text: &str,
    cursor: TextPosition,
    language_id: Option<&str>,
    config: &MatchitConfig,
) -> Option<TextRange> {
    let doc = Document::new(text);
    let tokens = tokens_for_document(&doc, language_id, config);
    let cursor_offset = doc.offset_for_position(cursor);
    let token = containing_pair(&tokens, cursor_offset)?;
    let target = matching_token(&tokens, token, MatchDirection::Forward)
        .or_else(|| matching_token(&tokens, token, MatchDirection::Backward))?;
    let start = token.start.min(target.start);
    let end = token.end.max(target.end);
    Some(TextRange::new(
        doc.position_for_offset(start),
        doc.position_for_offset(end),
    ))
}

fn tokens_for_document(
    doc: &Document,
    language_id: Option<&str>,
    config: &MatchitConfig,
) -> Vec<Token> {
    let groups = groups_for_config(language_id, config);
    let skip_ranges = skip_ranges(doc);
    let mut tokens = Vec::new();
    for (group_idx, group) in groups.iter().enumerate() {
        collect_group_tokens(doc, group_idx, group, &skip_ranges, &mut tokens);
    }
    collect_tag_tokens(doc, groups.len(), &mut tokens);
    tokens.sort_by_key(|token| (token.start, token.end));
    tokens
}

fn groups_for_config(language_id: Option<&str>, config: &MatchitConfig) -> Vec<Group> {
    let mut groups = Vec::new();
    for pair in &config.pairs {
        groups.push(Group {
            kind: TokenKind::Pair,
            patterns: vec![Pattern::literal(&pair[0]), Pattern::literal(&pair[1])],
            linewise: false,
        });
    }

    groups.push(Group {
        kind: TokenKind::Comment,
        patterns: vec![Pattern::literal("/*"), Pattern::literal("*/")],
        linewise: false,
    });
    groups.push(Group {
        kind: TokenKind::Preprocessor,
        patterns: vec![
            Pattern::regex(r"(?m)^\s*#\s*(?:if|ifdef|ifndef)\b").unwrap(),
            Pattern::regex(r"(?m)^\s*#\s*(?:elif|else)\b").unwrap(),
            Pattern::regex(r"(?m)^\s*#\s*endif\b").unwrap(),
        ],
        linewise: true,
    });

    if config.enabled {
        if let Some(language_id) = language_id {
            groups.extend(builtin_language_groups(language_id));
            if let Some(language) = config.languages.get(language_id) {
                groups.extend(config_language_groups(language));
            }
        }
    }

    groups
}

fn builtin_language_groups(language_id: &str) -> Vec<Group> {
    match language_id {
        "bash" => vec![Group {
            kind: TokenKind::Language,
            patterns: vec![
                Pattern::regex(r"\bif\b").unwrap(),
                Pattern::regex(r"\b(?:elif|else)\b").unwrap(),
                Pattern::regex(r"\bfi\b").unwrap(),
            ],
            linewise: false,
        }],
        _ => Vec::new(),
    }
}

fn config_language_groups(language: &MatchitLanguageConfig) -> Vec<Group> {
    language
        .groups
        .iter()
        .filter(|group| group.len() >= 2)
        .filter_map(|group| {
            let patterns = group
                .iter()
                .map(|pattern| Pattern::regex(pattern))
                .collect::<Option<Vec<_>>>()?;
            Some(Group {
                kind: TokenKind::Language,
                patterns,
                linewise: false,
            })
        })
        .collect()
}

fn collect_group_tokens(
    doc: &Document,
    group_idx: usize,
    group: &Group,
    skip_ranges: &[(usize, usize)],
    tokens: &mut Vec<Token>,
) {
    for (item_idx, pattern) in group.patterns.iter().enumerate() {
        match pattern {
            Pattern::Literal(value) => {
                collect_literal_tokens(doc, group_idx, group, item_idx, value, skip_ranges, tokens)
            }
            Pattern::Regex(regex) => {
                collect_regex_tokens(doc, group_idx, group, item_idx, regex, skip_ranges, tokens)
            }
        }
    }
}

fn collect_literal_tokens(
    doc: &Document,
    group_idx: usize,
    group: &Group,
    item_idx: usize,
    value: &str,
    skip_ranges: &[(usize, usize)],
    tokens: &mut Vec<Token>,
) {
    let chars = doc.chars();
    let pattern = value.chars().collect::<Vec<_>>();
    if pattern.is_empty() || pattern.len() > chars.len() {
        return;
    }

    for start in 0..=chars.len() - pattern.len() {
        let end = start + pattern.len();
        if chars[start..end] == pattern && should_keep_token(group.kind, start, end, skip_ranges) {
            tokens.push(token(group_idx, item_idx, start, end, group));
        }
    }
}

fn collect_regex_tokens(
    doc: &Document,
    group_idx: usize,
    group: &Group,
    item_idx: usize,
    regex: &Regex,
    skip_ranges: &[(usize, usize)],
    tokens: &mut Vec<Token>,
) {
    for match_ in regex.find_iter(&doc.text) {
        let start = doc.text[..match_.start()].chars().count();
        let end = start + match_.as_str().chars().count();
        if should_keep_token(group.kind, start, end, skip_ranges) {
            tokens.push(token(group_idx, item_idx, start, end, group));
        }
    }
}

fn token(group: usize, item: usize, start: usize, end: usize, meta: &Group) -> Token {
    let role = if item == 0 {
        TokenRole::Open
    } else if item + 1 == meta.patterns.len() {
        TokenRole::Close
    } else {
        TokenRole::Middle
    };
    Token {
        start,
        end,
        group,
        item,
        role,
        kind: meta.kind,
        linewise: meta.linewise,
    }
}

fn should_keep_token(
    kind: TokenKind,
    start: usize,
    end: usize,
    skip_ranges: &[(usize, usize)],
) -> bool {
    if matches!(kind, TokenKind::Comment | TokenKind::Preprocessor) {
        return true;
    }
    !skip_ranges
        .iter()
        .any(|(range_start, range_end)| start >= *range_start && end <= *range_end)
}

fn skip_ranges(doc: &Document) -> Vec<(usize, usize)> {
    let chars = doc.chars();
    let mut ranges = Vec::new();
    let mut idx = 0;
    while idx < chars.len() {
        match chars[idx] {
            '"' | '\'' => {
                let quote = chars[idx];
                let start = idx;
                idx += 1;
                while idx < chars.len() {
                    if chars[idx] == quote && !is_escaped(&chars, idx) {
                        idx += 1;
                        break;
                    }
                    idx += 1;
                }
                ranges.push((start, idx));
            }
            '/' if chars.get(idx + 1) == Some(&'/') => {
                let start = idx;
                idx += 2;
                while idx < chars.len() && chars[idx] != '\n' {
                    idx += 1;
                }
                ranges.push((start, idx));
            }
            _ => idx += 1,
        }
    }
    ranges
}

fn is_escaped(chars: &[char], idx: usize) -> bool {
    let mut slash_count = 0;
    let mut cursor = idx;
    while cursor > 0 {
        cursor -= 1;
        if chars[cursor] != '\\' {
            break;
        }
        slash_count += 1;
    }
    slash_count % 2 == 1
}

fn collect_tag_tokens(doc: &Document, group_base: usize, tokens: &mut Vec<Token>) {
    let Ok(regex) = Regex::new(r"</?([A-Za-z][A-Za-z0-9:_-]*)(?:\s[^<>]*)?>") else {
        return;
    };
    let mut tag_groups = Vec::<String>::new();
    for capture in regex.captures_iter(&doc.text) {
        let Some(full) = capture.get(0) else {
            continue;
        };
        let text = full.as_str();
        if text.ends_with("/>") || text.starts_with("<!") || text.starts_with("<?") {
            continue;
        }
        let Some(name) = capture.get(1).map(|name| name.as_str().to_string()) else {
            continue;
        };
        let group_offset = tag_groups
            .iter()
            .position(|existing| existing == &name)
            .unwrap_or_else(|| {
                tag_groups.push(name);
                tag_groups.len() - 1
            });
        let start = doc.text[..full.start()].chars().count();
        let end = start + text.chars().count();
        let is_close = text.starts_with("</");
        tokens.push(Token {
            start,
            end,
            group: group_base + group_offset,
            item: usize::from(is_close),
            role: if is_close {
                TokenRole::Close
            } else {
                TokenRole::Open
            },
            kind: TokenKind::Tag,
            linewise: false,
        });
    }
}

fn choose_token_on_line<'a>(
    doc: &Document,
    tokens: &'a [Token],
    line: usize,
    cursor_offset: usize,
) -> Option<&'a Token> {
    let (line_start, line_end) = doc.line_bounds(line);
    tokens
        .iter()
        .filter(|token| {
            token.start >= line_start && token.start <= line_end && token.end > cursor_offset
        })
        .min_by_key(|token| {
            (
                !(token.start <= cursor_offset && cursor_offset < token.end),
                token.start.saturating_sub(cursor_offset),
                token.group,
                token.item,
            )
        })
}

fn containing_pair(tokens: &[Token], cursor_offset: usize) -> Option<&Token> {
    tokens
        .iter()
        .filter(|token| token.role == TokenRole::Open)
        .filter_map(|token| {
            let target = matching_token(tokens, token, MatchDirection::Forward)?;
            (token.start <= cursor_offset && cursor_offset < target.end)
                .then_some((token, target.end.saturating_sub(token.start)))
        })
        .min_by_key(|(_, width)| *width)
        .map(|(token, _)| token)
}

fn matching_token<'a>(
    tokens: &'a [Token],
    token: &Token,
    direction: MatchDirection,
) -> Option<&'a Token> {
    let effective_direction = match direction {
        MatchDirection::Forward if token.role == TokenRole::Close => MatchDirection::Backward,
        MatchDirection::Backward if token.role == TokenRole::Open => MatchDirection::Forward,
        direction => direction,
    };

    match effective_direction {
        MatchDirection::Forward => matching_forward(tokens, token),
        MatchDirection::Backward => matching_backward(tokens, token),
    }
}

fn matching_forward<'a>(tokens: &'a [Token], token: &Token) -> Option<&'a Token> {
    let mut depth = 0usize;
    for candidate in tokens
        .iter()
        .filter(|candidate| candidate.group == token.group && candidate.start > token.start)
    {
        match candidate.role {
            TokenRole::Open => depth += 1,
            TokenRole::Middle if depth == 0 => return Some(candidate),
            TokenRole::Middle => {}
            TokenRole::Close if depth == 0 => return Some(candidate),
            TokenRole::Close => depth = depth.saturating_sub(1),
        }
    }
    None
}

fn matching_backward<'a>(tokens: &'a [Token], token: &Token) -> Option<&'a Token> {
    let mut depth = 0usize;
    for candidate in tokens
        .iter()
        .rev()
        .filter(|candidate| candidate.group == token.group && candidate.start < token.start)
    {
        match candidate.role {
            TokenRole::Close => depth += 1,
            TokenRole::Middle if depth == 0 => return Some(candidate),
            TokenRole::Middle => {}
            TokenRole::Open if depth == 0 => return Some(candidate),
            TokenRole::Open => depth = depth.saturating_sub(1),
        }
    }
    None
}

fn unmatched_before(tokens: &[Token], cursor_offset: usize) -> Option<&Token> {
    let mut stack = Vec::<&Token>::new();
    for token in tokens.iter().filter(|token| token.start < cursor_offset) {
        match token.role {
            TokenRole::Open => stack.push(token),
            TokenRole::Close => {
                if let Some(pos) = stack.iter().rposition(|open| open.group == token.group) {
                    stack.remove(pos);
                }
            }
            TokenRole::Middle => {}
        }
    }
    stack.pop()
}

fn unmatched_after(tokens: &[Token], cursor_offset: usize) -> Option<&Token> {
    let mut stack = Vec::<&Token>::new();
    for token in tokens
        .iter()
        .rev()
        .filter(|token| token.start > cursor_offset)
    {
        match token.role {
            TokenRole::Close => stack.push(token),
            TokenRole::Open => {
                if let Some(pos) = stack.iter().rposition(|close| close.group == token.group) {
                    stack.remove(pos);
                }
            }
            TokenRole::Middle => {}
        }
    }
    stack.pop()
}

fn motion_for_token(doc: &Document, token: &Token) -> MatchMotion {
    MatchMotion {
        target: doc.position_for_offset(token.start),
        target_end: doc.position_for_offset(token.end),
        kind: if token.linewise {
            MatchKind::Linewise
        } else {
            MatchKind::Charwise
        },
    }
}

fn advance_position(position: TextPosition) -> TextPosition {
    TextPosition::new(position.line, position.character + 1)
}

fn position_le(left: TextPosition, right: TextPosition) -> bool {
    (left.line, left.character) <= (right.line, right.character)
}
