//! Lexical analysis: convert source text into a stream of tokens.

use std::ops::Range;

// ============================================================================
// Trivia (for formatter support)
// ============================================================================

/// Trivia represents non-semantic content: whitespace and comments.
/// Used by the formatter to preserve comments and intentional blank lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trivia {
    /// Horizontal whitespace (spaces and tabs)
    Whitespace(String),
    /// Line endings (\n or \r\n) - tracked separately for blank line detection
    Newline(String),
    /// Line comment including the `//` prefix
    LineComment(String),
}

impl Trivia {
    /// Returns true if this trivia is a newline
    pub fn is_newline(&self) -> bool {
        matches!(self, Trivia::Newline(_))
    }

    /// Returns true if this trivia is a line comment
    pub fn is_comment(&self) -> bool {
        matches!(self, Trivia::LineComment(_))
    }

    /// Returns true if this trivia is a documentation comment (starts with `/// `).
    pub fn is_doc_comment(&self) -> bool {
        matches!(self, Trivia::LineComment(s) if s.starts_with("/// "))
    }

    /// Extract doc content from a doc comment, removing the `/// ` prefix.
    /// Returns None if this is not a doc comment.
    pub fn doc_content(&self) -> Option<&str> {
        match self {
            Trivia::LineComment(s) if s.starts_with("/// ") => Some(&s[4..]),
            _ => None,
        }
    }
}

/// List of all Husk keywords.
pub const KEYWORDS: &[&str] = &[
    "as", "pub", "use", "fn", "let", "mod", "mut", "struct", "enum", "type", "extern", "if",
    "else", "while", "loop", "match", "return", "true", "false", "break", "continue", "trait",
    "impl", "for", "Self", "static", "const", "in", "global", "js",
];

/// Check if a string is a Husk reserved keyword.
pub fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}

/// Check if a string is a valid Husk identifier.
///
/// A valid identifier:
/// - Starts with an ASCII letter or underscore
/// - Contains only ASCII alphanumeric characters or underscores
/// - Is not a reserved keyword
pub fn is_valid_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    for ch in chars {
        if !ch.is_ascii_alphanumeric() && ch != '_' {
            return false;
        }
    }
    !is_keyword(name)
}

/// A span in the source file, represented as a byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { range: start..end }
    }
}

/// Language keywords (subset for the MVP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    As,
    Pub,
    Use,
    Fn,
    Let,
    Mut,
    Mod,
    Struct,
    Enum,
    Type,
    Extern,
    If,
    Else,
    While,
    Loop,
    Match,
    Return,
    True,
    False,
    Break,
    Continue,
    Trait,
    Impl,
    For,
    In,
    SelfType, // `Self` keyword (capital S)
    Static,
    Const, // `const` keyword for extern constants
    Global,
    Js, // `js` keyword for embedded JavaScript blocks
}

/// Token kinds produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    IntLiteral(String),
    FloatLiteral(String),
    StringLiteral(String),
    Keyword(Keyword),
    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Colon,
    ColonColon,
    Semicolon,
    Dot,
    DotDot,   // ..  (exclusive range)
    DotDotEq, // ..= (inclusive range)
    Arrow,    // ->
    FatArrow, // =>
    Eq,       // =
    EqEq,     // ==
    Bang,     // !
    BangEq,   // !=
    Question, // ? (try operator)
    Lt,       // <
    Gt,       // >
    Le,       // <=
    Ge,       // >=
    AndAnd,   // &&
    Amp,      // & (single ampersand for references/self receivers)
    OrOr,     // ||
    Pipe,     // | (single pipe for closures)
    Plus,
    PlusEq, // +=
    Minus,
    MinusEq, // -=
    Star,
    Slash,
    Percent,   // %
    PercentEq, // %=
    // Attribute-related tokens
    Hash,     // #
    LBracket, // [
    RBracket, // ]
    // End of input
    Eof,
}

/// A token with its kind, source span, and associated trivia.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// Trivia (whitespace, newlines, comments) that appears before this token
    pub leading_trivia: Vec<Trivia>,
    /// Trivia that appears after this token on the same line (typically trailing comments)
    pub trailing_trivia: Vec<Trivia>,
}

impl Token {
    /// Create a new token with no trivia (for backwards compatibility)
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self {
            kind,
            span,
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
        }
    }

    /// Create a new token with trivia
    pub fn with_trivia(
        kind: TokenKind,
        span: Span,
        leading_trivia: Vec<Trivia>,
        trailing_trivia: Vec<Trivia>,
    ) -> Self {
        Self {
            kind,
            span,
            leading_trivia,
            trailing_trivia,
        }
    }

    /// Returns true if this token has any leading comments
    pub fn has_leading_comments(&self) -> bool {
        self.leading_trivia.iter().any(|t| t.is_comment())
    }

    /// Returns true if this token has a trailing comment
    pub fn has_trailing_comment(&self) -> bool {
        self.trailing_trivia.iter().any(|t| t.is_comment())
    }

    /// Count consecutive newlines in leading trivia (for blank line detection)
    pub fn leading_blank_lines(&self) -> usize {
        let newline_count = self
            .leading_trivia
            .iter()
            .filter(|t| t.is_newline())
            .count();
        // 2 newlines = 1 blank line, 3 newlines = 2 blank lines, etc.
        newline_count.saturating_sub(1)
    }
}

/// Simple lexer over a UTF-8 string.
pub struct Lexer<'src> {
    src: &'src str,
    chars: std::str::CharIndices<'src>,
    peeked: Option<(usize, char)>,
    end: usize,
    finished: bool,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        let end = src.len();
        Self {
            src,
            chars: src.char_indices(),
            peeked: None,
            end,
            finished: false,
        }
    }

    fn bump(&mut self) -> Option<(usize, char)> {
        if let Some(p) = self.peeked.take() {
            Some(p)
        } else {
            self.chars.next()
        }
    }

    fn peek(&mut self) -> Option<(usize, char)> {
        if self.peeked.is_none() {
            self.peeked = self.chars.next();
        }
        self.peeked
    }

    fn make_span(&self, start: usize, end: usize) -> Span {
        Span::new(start, end)
    }

    fn consume_while<F>(&mut self, start: usize, mut pred: F) -> (Span, &'src str)
    where
        F: FnMut(char) -> bool,
    {
        let mut last = start;
        let mut saw_any = false;
        while let Some((idx, ch)) = self.peek() {
            if !pred(ch) {
                break;
            }
            saw_any = true;
            last = idx;
            self.bump();
        }
        let end = if saw_any { last + 1 } else { start + 1 };
        let span = self.make_span(start, end);
        let lexeme = &self.src[span.range.clone()];
        (span, lexeme)
    }

    /// Collect leading trivia: whitespace, newlines, and comments before a token.
    fn collect_leading_trivia(&mut self) -> Vec<Trivia> {
        let mut trivia = Vec::new();
        loop {
            match self.peek() {
                Some((_, ' ')) | Some((_, '\t')) => {
                    // Collect horizontal whitespace
                    let mut ws = String::new();
                    while let Some((_, ch)) = self.peek() {
                        if ch == ' ' || ch == '\t' {
                            ws.push(ch);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    if !ws.is_empty() {
                        trivia.push(Trivia::Whitespace(ws));
                    }
                }
                Some((_, '\n')) => {
                    self.bump();
                    trivia.push(Trivia::Newline("\n".to_string()));
                }
                Some((_, '\r')) => {
                    self.bump();
                    if let Some((_, '\n')) = self.peek() {
                        self.bump();
                        trivia.push(Trivia::Newline("\r\n".to_string()));
                    } else {
                        // Standalone \r - treat as newline
                        trivia.push(Trivia::Newline("\r".to_string()));
                    }
                }
                Some((start, '/')) => {
                    // Check if this is a line comment
                    let mut clone = self.chars.clone();
                    if let Some((_, '/')) = clone.next() {
                        // It's a line comment
                        let comment_start = start;
                        self.bump(); // consume first '/'
                        self.bump(); // consume second '/'

                        // Collect until end of line
                        while let Some((_, ch)) = self.peek() {
                            if ch == '\n' {
                                break;
                            }
                            self.bump();
                        }

                        // Extract the comment text from source
                        let comment_end = self.peek().map(|(i, _)| i).unwrap_or(self.end);
                        let comment = &self.src[comment_start..comment_end];
                        trivia.push(Trivia::LineComment(comment.to_string()));
                    } else {
                        // Not a comment, done collecting trivia
                        break;
                    }
                }
                _ => break,
            }
        }
        trivia
    }

    /// Collect trailing trivia: whitespace and comments on the same line after a token.
    fn collect_trailing_trivia(&mut self) -> Vec<Trivia> {
        let mut trivia = Vec::new();
        loop {
            match self.peek() {
                Some((_, ' ')) | Some((_, '\t')) => {
                    // Collect horizontal whitespace
                    let mut ws = String::new();
                    while let Some((_, ch)) = self.peek() {
                        if ch == ' ' || ch == '\t' {
                            ws.push(ch);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    if !ws.is_empty() {
                        trivia.push(Trivia::Whitespace(ws));
                    }
                }
                Some((start, '/')) => {
                    // Check if this is a line comment
                    let mut clone = self.chars.clone();
                    if let Some((_, '/')) = clone.next() {
                        // It's a trailing line comment
                        let comment_start = start;
                        self.bump(); // consume first '/'
                        self.bump(); // consume second '/'

                        // Collect until end of line
                        while let Some((_, ch)) = self.peek() {
                            if ch == '\n' {
                                break;
                            }
                            self.bump();
                        }

                        // Extract the comment text from source
                        let comment_end = self.peek().map(|(i, _)| i).unwrap_or(self.end);
                        let comment = &self.src[comment_start..comment_end];
                        trivia.push(Trivia::LineComment(comment.to_string()));
                        // After a line comment, stop collecting trailing trivia
                        break;
                    } else {
                        // Not a comment, done collecting trailing trivia
                        break;
                    }
                }
                _ => {
                    // Newline or other character - stop collecting trailing trivia
                    break;
                }
            }
        }
        trivia
    }

    fn classify_ident_or_keyword(&self, _span: Span, text: &str) -> TokenKind {
        match text {
            "as" => TokenKind::Keyword(Keyword::As),
            "pub" => TokenKind::Keyword(Keyword::Pub),
            "use" => TokenKind::Keyword(Keyword::Use),
            "fn" => TokenKind::Keyword(Keyword::Fn),
            "let" => TokenKind::Keyword(Keyword::Let),
            "mod" => TokenKind::Keyword(Keyword::Mod),
            "mut" => TokenKind::Keyword(Keyword::Mut),
            "struct" => TokenKind::Keyword(Keyword::Struct),
            "enum" => TokenKind::Keyword(Keyword::Enum),
            "type" => TokenKind::Keyword(Keyword::Type),
            "extern" => TokenKind::Keyword(Keyword::Extern),
            "if" => TokenKind::Keyword(Keyword::If),
            "else" => TokenKind::Keyword(Keyword::Else),
            "while" => TokenKind::Keyword(Keyword::While),
            "loop" => TokenKind::Keyword(Keyword::Loop),
            "match" => TokenKind::Keyword(Keyword::Match),
            "break" => TokenKind::Keyword(Keyword::Break),
            "continue" => TokenKind::Keyword(Keyword::Continue),
            "return" => TokenKind::Keyword(Keyword::Return),
            "true" => TokenKind::Keyword(Keyword::True),
            "false" => TokenKind::Keyword(Keyword::False),
            "trait" => TokenKind::Keyword(Keyword::Trait),
            "impl" => TokenKind::Keyword(Keyword::Impl),
            "for" => TokenKind::Keyword(Keyword::For),
            "in" => TokenKind::Keyword(Keyword::In),
            "Self" => TokenKind::Keyword(Keyword::SelfType),
            "static" => TokenKind::Keyword(Keyword::Static),
            "const" => TokenKind::Keyword(Keyword::Const),
            "global" => TokenKind::Keyword(Keyword::Global),
            "js" => TokenKind::Keyword(Keyword::Js),
            _ => TokenKind::Ident(text.to_string()),
        }
    }

    fn lex_number(&mut self, start: usize, first_ch: char) -> (TokenKind, Span) {
        let (span, _text) = self.consume_while(start, |c| c.is_ascii_digit());
        let mut end = if span.range.start == span.range.end {
            // only first_ch
            start + first_ch.len_utf8()
        } else {
            span.range.end
        };

        // Check for decimal point followed by digits (float literal)
        let mut is_float = false;
        if let Some((dot_idx, '.')) = self.peek() {
            // Look ahead to see if there's a digit after the dot
            // We need to check if the next character after '.' is a digit
            let after_dot = self.src.get(dot_idx + 1..dot_idx + 2);
            if let Some(ch_str) = after_dot
                && let Some(ch) = ch_str.chars().next()
                && ch.is_ascii_digit()
            {
                // Consume the dot
                self.bump();
                // Consume the fractional digits
                let (frac_span, _) = self.consume_while(dot_idx + 1, |c| c.is_ascii_digit());
                end = frac_span.range.end;
                is_float = true;
            }
        }

        let full_span = Span::new(start, end);
        let lexeme = &self.src[full_span.range.clone()];
        let kind = if is_float {
            TokenKind::FloatLiteral(lexeme.to_string())
        } else {
            TokenKind::IntLiteral(lexeme.to_string())
        };
        (kind, full_span)
    }

    fn lex_ident_or_keyword(&mut self, start: usize) -> (TokenKind, Span) {
        let (span, text) = self.consume_while(start, |c| c.is_alphanumeric() || c == '_');
        let kind = self.classify_ident_or_keyword(span.clone(), text);
        (kind, span)
    }

    fn lex_string(&mut self, start: usize) -> (TokenKind, Span) {
        // Assumes opening quote has already been consumed.
        let mut end = start;
        let mut value = String::new();

        while let Some((idx, ch)) = self.bump() {
            if ch == '"' {
                end = idx + 1;
                break;
            } else if ch == '\\' {
                // Handle escape sequences
                if let Some((esc_idx, esc_ch)) = self.bump() {
                    end = esc_idx + 1;
                    match esc_ch {
                        'n' => value.push('\n'),
                        't' => value.push('\t'),
                        'r' => value.push('\r'),
                        '0' => value.push('\0'),
                        '\\' => value.push('\\'),
                        '"' => value.push('"'),
                        // For unknown escapes, keep as-is
                        other => {
                            value.push('\\');
                            value.push(other);
                        }
                    }
                }
            } else {
                value.push(ch);
                end = idx + 1;
            }
        }

        let span = self.make_span(start, end);
        (TokenKind::StringLiteral(value), span)
    }
}

impl<'src> Iterator for Lexer<'src> {
    type Item = Token;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        // Collect leading trivia (whitespace, newlines, comments before this token)
        let leading_trivia = self.collect_leading_trivia();

        let (start, ch) = match self.bump() {
            Some(pair) => pair,
            None => {
                let span = Span::new(self.end, self.end);
                self.finished = true;
                return Some(Token::with_trivia(
                    TokenKind::Eof,
                    span,
                    leading_trivia,
                    Vec::new(),
                ));
            }
        };

        // Get the token kind and span
        let (kind, span) = match ch {
            c if c.is_ascii_alphabetic() || c == '_' => self.lex_ident_or_keyword(start),
            c if c.is_ascii_digit() => self.lex_number(start, c),
            '"' => self.lex_string(start),
            '(' => (TokenKind::LParen, Span::new(start, start + 1)),
            ')' => (TokenKind::RParen, Span::new(start, start + 1)),
            '{' => (TokenKind::LBrace, Span::new(start, start + 1)),
            '}' => (TokenKind::RBrace, Span::new(start, start + 1)),
            ',' => (TokenKind::Comma, Span::new(start, start + 1)),
            ':' => {
                if let Some((idx2, ':')) = self.peek() {
                    self.bump();
                    (TokenKind::ColonColon, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Colon, Span::new(start, start + 1))
                }
            }
            ';' => (TokenKind::Semicolon, Span::new(start, start + 1)),
            '.' => {
                if let Some((idx2, '.')) = self.peek() {
                    self.bump(); // consume second '.'

                    if let Some((idx3, '=')) = self.peek() {
                        self.bump(); // consume '='
                        (TokenKind::DotDotEq, Span::new(start, idx3 + 1))
                    } else {
                        (TokenKind::DotDot, Span::new(start, idx2 + 1))
                    }
                } else {
                    (TokenKind::Dot, Span::new(start, start + 1))
                }
            }
            '-' => {
                if let Some((idx2, '>')) = self.peek() {
                    self.bump();
                    (TokenKind::Arrow, Span::new(start, idx2 + 1))
                } else if let Some((idx2, '=')) = self.peek() {
                    self.bump();
                    (TokenKind::MinusEq, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Minus, Span::new(start, start + 1))
                }
            }
            '=' => {
                if let Some((idx2, next)) = self.peek() {
                    match next {
                        '>' => {
                            self.bump();
                            (TokenKind::FatArrow, Span::new(start, idx2 + 1))
                        }
                        '=' => {
                            self.bump();
                            (TokenKind::EqEq, Span::new(start, idx2 + 1))
                        }
                        _ => (TokenKind::Eq, Span::new(start, start + 1)),
                    }
                } else {
                    (TokenKind::Eq, Span::new(start, start + 1))
                }
            }
            '+' => {
                if let Some((idx2, '=')) = self.peek() {
                    self.bump();
                    (TokenKind::PlusEq, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Plus, Span::new(start, start + 1))
                }
            }
            '*' => (TokenKind::Star, Span::new(start, start + 1)),
            '/' => (TokenKind::Slash, Span::new(start, start + 1)),
            '%' => {
                if let Some((idx2, '=')) = self.peek() {
                    self.bump();
                    (TokenKind::PercentEq, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Percent, Span::new(start, start + 1))
                }
            }
            '!' => {
                if let Some((idx2, '=')) = self.peek() {
                    self.bump();
                    (TokenKind::BangEq, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Bang, Span::new(start, start + 1))
                }
            }
            '?' => (TokenKind::Question, Span::new(start, start + 1)),
            '<' => {
                if let Some((idx2, '=')) = self.peek() {
                    self.bump();
                    (TokenKind::Le, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Lt, Span::new(start, start + 1))
                }
            }
            '>' => {
                if let Some((idx2, '=')) = self.peek() {
                    self.bump();
                    (TokenKind::Ge, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Gt, Span::new(start, start + 1))
                }
            }
            '&' => {
                if let Some((idx2, '&')) = self.peek() {
                    self.bump();
                    (TokenKind::AndAnd, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Amp, Span::new(start, start + 1))
                }
            }
            '|' => {
                if let Some((idx2, '|')) = self.peek() {
                    self.bump();
                    (TokenKind::OrOr, Span::new(start, idx2 + 1))
                } else {
                    (TokenKind::Pipe, Span::new(start, start + 1))
                }
            }
            '#' => (TokenKind::Hash, Span::new(start, start + 1)),
            '[' => (TokenKind::LBracket, Span::new(start, start + 1)),
            ']' => (TokenKind::RBracket, Span::new(start, start + 1)),
            _ => {
                // Unknown character, skip for now; in the future we will emit diagnostics.
                (TokenKind::Eof, Span::new(start, start + 1))
            }
        };

        // Collect trailing trivia (whitespace and comments on the same line after the token)
        let trailing_trivia = self.collect_trailing_trivia();

        Some(Token::with_trivia(
            kind,
            span,
            leading_trivia,
            trailing_trivia,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trivia_leading_whitespace() {
        let src = "   foo";
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();

        assert!(matches!(token.kind, TokenKind::Ident(ref s) if s == "foo"));
        assert_eq!(token.leading_trivia.len(), 1);
        assert!(matches!(&token.leading_trivia[0], Trivia::Whitespace(ws) if ws == "   "));
    }

    #[test]
    fn test_trivia_leading_newlines() {
        let src = "\n\nfoo";
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();

        assert!(matches!(token.kind, TokenKind::Ident(ref s) if s == "foo"));
        assert_eq!(token.leading_trivia.len(), 2);
        assert!(matches!(&token.leading_trivia[0], Trivia::Newline(nl) if nl == "\n"));
        assert!(matches!(&token.leading_trivia[1], Trivia::Newline(nl) if nl == "\n"));
    }

    #[test]
    fn test_trivia_leading_comment() {
        let src = "// this is a comment\nfoo";
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();

        assert!(matches!(token.kind, TokenKind::Ident(ref s) if s == "foo"));
        assert_eq!(token.leading_trivia.len(), 2);
        assert!(
            matches!(&token.leading_trivia[0], Trivia::LineComment(c) if c == "// this is a comment")
        );
        assert!(matches!(&token.leading_trivia[1], Trivia::Newline(nl) if nl == "\n"));
    }

    #[test]
    fn test_trivia_trailing_comment() {
        let src = "foo // trailing\nbar";
        let mut lexer = Lexer::new(src);

        let foo = lexer.next().unwrap();
        assert!(matches!(foo.kind, TokenKind::Ident(ref s) if s == "foo"));
        assert_eq!(foo.trailing_trivia.len(), 2);
        assert!(matches!(&foo.trailing_trivia[0], Trivia::Whitespace(ws) if ws == " "));
        assert!(matches!(&foo.trailing_trivia[1], Trivia::LineComment(c) if c == "// trailing"));

        let bar = lexer.next().unwrap();
        assert!(matches!(bar.kind, TokenKind::Ident(ref s) if s == "bar"));
        assert_eq!(bar.leading_trivia.len(), 1);
        assert!(matches!(&bar.leading_trivia[0], Trivia::Newline(nl) if nl == "\n"));
    }

    #[test]
    fn test_trivia_blank_lines() {
        let src = "\n\n\nfoo";
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();

        assert!(matches!(token.kind, TokenKind::Ident(ref s) if s == "foo"));
        // 3 newlines = 2 blank lines
        assert_eq!(token.leading_blank_lines(), 2);
    }

    #[test]
    fn test_trivia_has_leading_comments() {
        let src = "// comment\nfoo";
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();

        assert!(token.has_leading_comments());
    }

    #[test]
    fn test_trivia_has_trailing_comment() {
        let src = "foo // comment\n";
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();

        assert!(token.has_trailing_comment());
    }

    #[test]
    fn test_trivia_complex_mixed() {
        let src = "  // header comment\n\n  fn main() {}";
        let mut lexer = Lexer::new(src);

        // First token: 'fn'
        let fn_token = lexer.next().unwrap();
        assert!(matches!(fn_token.kind, TokenKind::Keyword(Keyword::Fn)));
        // Leading: whitespace, comment, newline, newline, whitespace
        assert_eq!(fn_token.leading_trivia.len(), 5);
        assert!(matches!(&fn_token.leading_trivia[0], Trivia::Whitespace(_)));
        assert!(matches!(
            &fn_token.leading_trivia[1],
            Trivia::LineComment(_)
        ));
        assert!(matches!(&fn_token.leading_trivia[2], Trivia::Newline(_)));
        assert!(matches!(&fn_token.leading_trivia[3], Trivia::Newline(_)));
        assert!(matches!(&fn_token.leading_trivia[4], Trivia::Whitespace(_)));
    }

    #[test]
    fn test_trivia_no_trivia() {
        let src = "foo";
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();

        assert!(matches!(token.kind, TokenKind::Ident(ref s) if s == "foo"));
        assert!(token.leading_trivia.is_empty());
        assert!(token.trailing_trivia.is_empty());
    }

    #[test]
    fn test_trivia_eof_preserves_trivia() {
        let src = "foo\n// final comment\n";
        let mut lexer = Lexer::new(src);

        let foo = lexer.next().unwrap();
        assert!(matches!(foo.kind, TokenKind::Ident(ref s) if s == "foo"));

        let eof = lexer.next().unwrap();
        assert!(matches!(eof.kind, TokenKind::Eof));
        // EOF should have the trailing comment as leading trivia
        assert!(eof.has_leading_comments());
    }

    #[test]
    fn test_trivia_between_tokens() {
        let src = "a + b";
        let mut lexer = Lexer::new(src);

        let a = lexer.next().unwrap();
        assert!(matches!(a.kind, TokenKind::Ident(ref s) if s == "a"));
        assert_eq!(a.trailing_trivia.len(), 1);
        assert!(matches!(&a.trailing_trivia[0], Trivia::Whitespace(ws) if ws == " "));

        let plus = lexer.next().unwrap();
        assert!(matches!(plus.kind, TokenKind::Plus));
        assert!(plus.leading_trivia.is_empty()); // trailing of 'a' consumed it
        assert_eq!(plus.trailing_trivia.len(), 1);
        assert!(matches!(&plus.trailing_trivia[0], Trivia::Whitespace(ws) if ws == " "));

        let b = lexer.next().unwrap();
        assert!(matches!(b.kind, TokenKind::Ident(ref s) if s == "b"));
        assert!(b.leading_trivia.is_empty()); // trailing of '+' consumed it
    }

    #[test]
    fn test_string_escape_sequences() {
        // Test newline escape
        let src = r#""\n""#;
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();
        assert!(
            matches!(token.kind, TokenKind::StringLiteral(ref s) if s == "\n"),
            "Expected newline character, got {:?}",
            token.kind
        );

        // Test tab escape
        let src = r#""\t""#;
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();
        assert!(
            matches!(token.kind, TokenKind::StringLiteral(ref s) if s == "\t"),
            "Expected tab character, got {:?}",
            token.kind
        );

        // Test backslash escape
        let src = r#""\\""#;
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();
        assert!(
            matches!(token.kind, TokenKind::StringLiteral(ref s) if s == "\\"),
            "Expected backslash character, got {:?}",
            token.kind
        );

        // Test quote escape
        let src = r#""\"""#;
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();
        assert!(
            matches!(token.kind, TokenKind::StringLiteral(ref s) if s == "\""),
            "Expected quote character, got {:?}",
            token.kind
        );

        // Test carriage return escape
        let src = r#""\r""#;
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();
        assert!(
            matches!(token.kind, TokenKind::StringLiteral(ref s) if s == "\r"),
            "Expected carriage return character, got {:?}",
            token.kind
        );

        // Test null escape
        let src = r#""\0""#;
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();
        assert!(
            matches!(token.kind, TokenKind::StringLiteral(ref s) if s == "\0"),
            "Expected null character, got {:?}",
            token.kind
        );

        // Test mixed content with escapes
        let src = r#""hello\nworld""#;
        let mut lexer = Lexer::new(src);
        let token = lexer.next().unwrap();
        assert!(
            matches!(token.kind, TokenKind::StringLiteral(ref s) if s == "hello\nworld"),
            "Expected 'hello\\nworld', got {:?}",
            token.kind
        );
    }
}
