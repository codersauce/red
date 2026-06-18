use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

use husk_lexer::{Keyword, Lexer, TokenKind, Trivia};
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use crate::{editor::StyleInfo, theme::Theme};

#[derive(Clone, Copy)]
struct LanguageDefinition {
    id: &'static str,
    extensions: &'static [&'static str],
    language: fn() -> Language,
    highlight_queries: &'static [&'static str],
    injection_query: Option<&'static str>,
}

struct LanguageHighlighter {
    parser: Parser,
    query: Query,
    injection_query: Option<Query>,
}

struct Injection {
    language_id: &'static str,
    content_start: usize,
    content_end: usize,
}

struct RawInjection {
    language_name: String,
    content_start: usize,
    content_end: usize,
}

pub struct Highlighter {
    languages: HashMap<&'static str, LanguageDefinition>,
    extensions: HashMap<&'static str, &'static str>,
    highlighters: HashMap<&'static str, LanguageHighlighter>,
    theme: Theme,
}

const MAX_INJECTION_DEPTH: usize = 3;

impl Highlighter {
    pub fn new(theme: &Theme) -> anyhow::Result<Self> {
        let languages = language_definitions()
            .into_iter()
            .map(|definition| (definition.id, definition))
            .collect::<HashMap<_, _>>();
        let extensions = languages
            .values()
            .flat_map(|definition| {
                definition
                    .extensions
                    .iter()
                    .map(|extension| (*extension, definition.id))
            })
            .collect::<HashMap<_, _>>();

        Ok(Self {
            languages,
            extensions,
            highlighters: HashMap::new(),
            theme: theme.clone(),
        })
    }

    pub fn language_id_for_file(&self, file: Option<&str>) -> Option<&'static str> {
        let extension = file_extension(file?)?;
        self.language_id_for_extension(&extension)
    }

    pub fn language_id_for_extension(&self, extension: &str) -> Option<&'static str> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        self.extensions.get(extension.as_str()).copied()
    }

    pub fn language_id_for_name(&self, name: &str) -> Option<&'static str> {
        language_id_for_name(name).or_else(|| self.language_id_for_extension(name))
    }

    pub fn highlight_for_file(
        &mut self,
        file: Option<&str>,
        code: &str,
    ) -> anyhow::Result<Vec<StyleInfo>> {
        let Some(language_id) = self.language_id_for_file(file) else {
            return Ok(Vec::new());
        };
        self.highlight(language_id, code)
    }

    pub fn highlight(&mut self, language_id: &str, code: &str) -> anyhow::Result<Vec<StyleInfo>> {
        self.highlight_with_depth(language_id, code, 0)
    }

    fn highlight_with_depth(
        &mut self,
        language_id: &str,
        code: &str,
        depth: usize,
    ) -> anyhow::Result<Vec<StyleInfo>> {
        if language_id == "husk" {
            return Ok(highlight_husk(code, &self.theme));
        }

        let Some(definition) = self.languages.get(language_id).copied() else {
            return Ok(Vec::new());
        };

        if !self.highlighters.contains_key(definition.id) {
            let language = (definition.language)();
            let mut parser = Parser::new();
            parser.set_language(&language)?;
            let highlight_query = definition.highlight_queries.join("\n");
            let query = Query::new(&language, &highlight_query)?;
            let injection_query = definition
                .injection_query
                .map(|query| Query::new(&language, query))
                .transpose()?;
            self.highlighters.insert(
                definition.id,
                LanguageHighlighter {
                    parser,
                    query,
                    injection_query,
                },
            );
        }

        let mut colors = Vec::new();
        let mut raw_injections = Vec::new();

        {
            let Some(highlighter) = self.highlighters.get_mut(definition.id) else {
                return Ok(Vec::new());
            };
            let Some(tree) = highlighter.parser.parse(code, None) else {
                return Ok(Vec::new());
            };

            let mut cursor = QueryCursor::new();
            let mut matches = cursor.matches(&highlighter.query, tree.root_node(), code.as_bytes());

            while let Some(mat) = matches.next() {
                for cap in mat.captures {
                    let node = cap.node;
                    let start = node.start_byte();
                    let end = node.end_byte();
                    let scope = highlighter.query.capture_names()[cap.index as usize];

                    if let Some(style) = self.theme.get_style(scope) {
                        colors.push(StyleInfo { start, end, style });
                    }
                }
            }

            if depth < MAX_INJECTION_DEPTH {
                if let Some(injection_query) = &highlighter.injection_query {
                    raw_injections = collect_injections(injection_query, tree.root_node(), code);
                }
            }
        }

        let injections = raw_injections
            .into_iter()
            .filter_map(|injection| {
                let language_id = self.language_id_for_name(&injection.language_name)?;
                Some(Injection {
                    language_id,
                    content_start: injection.content_start,
                    content_end: injection.content_end,
                })
            })
            .collect::<Vec<_>>();

        for injection in injections {
            let Some(injected_code) = code.get(injection.content_start..injection.content_end)
            else {
                continue;
            };
            let mut injected_colors =
                self.highlight_with_depth(injection.language_id, injected_code, depth + 1)?;
            for color in &mut injected_colors {
                color.start += injection.content_start;
                color.end += injection.content_start;
            }
            colors.extend(injected_colors);
        }

        Ok(colors)
    }
}

fn collect_injections(
    query: &Query,
    root_node: tree_sitter::Node<'_>,
    code: &str,
) -> Vec<RawInjection> {
    let mut injections = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root_node, code.as_bytes());

    while let Some(mat) = matches.next() {
        let mut language_name = None;
        let mut content = None;

        for capture in mat.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            match capture_name {
                "injection.language" => {
                    language_name = capture.node.utf8_text(code.as_bytes()).ok();
                }
                "injection.content" => {
                    content = Some((capture.node.start_byte(), capture.node.end_byte()));
                }
                _ => {}
            }
        }

        let (Some(language_name), Some((content_start, content_end))) = (language_name, content)
        else {
            continue;
        };

        injections.push(RawInjection {
            language_name: language_name.to_string(),
            content_start,
            content_end,
        });
    }

    injections
}

fn language_id_for_name(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    let name = name.split_whitespace().next().unwrap_or_default();

    match name {
        "rs" | "rust" => Some("rust"),
        "js" | "javascript" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("jsx"),
        "ts" | "typescript" => Some("typescript"),
        "tsx" => Some("tsx"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "py" | "python" => Some("python"),
        "md" | "markdown" => Some("markdown"),
        "bash" | "sh" | "shell" | "zsh" => Some("bash"),
        "powershell" | "pwsh" | "ps1" => Some("powershell"),
        "lua" => Some("lua"),
        "hk" | "husk" => Some("husk"),
        _ => None,
    }
}

fn file_extension(file: &str) -> Option<String> {
    Path::new(file)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
}

fn language_definitions() -> Vec<LanguageDefinition> {
    vec![
        LanguageDefinition {
            id: "rust",
            extensions: &["rs"],
            language: || tree_sitter_rust::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_rust::HIGHLIGHTS_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "markdown",
            extensions: &["md", "markdown"],
            language: || tree_sitter_md::LANGUAGE.into(),
            highlight_queries: &[MARKDOWN_HIGHLIGHT_QUERY],
            injection_query: Some(MARKDOWN_INJECTION_QUERY),
        },
        LanguageDefinition {
            id: "javascript",
            extensions: &["js", "mjs", "cjs"],
            language: || tree_sitter_javascript::LANGUAGE.into(),
            highlight_queries: JAVASCRIPT_HIGHLIGHT_QUERIES,
            injection_query: None,
        },
        LanguageDefinition {
            id: "jsx",
            extensions: &["jsx"],
            language: || tree_sitter_javascript::LANGUAGE.into(),
            highlight_queries: JSX_HIGHLIGHT_QUERIES,
            injection_query: None,
        },
        LanguageDefinition {
            id: "typescript",
            extensions: &["ts"],
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlight_queries: TYPESCRIPT_HIGHLIGHT_QUERIES,
            injection_query: None,
        },
        LanguageDefinition {
            id: "tsx",
            extensions: &["tsx"],
            language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlight_queries: TSX_HIGHLIGHT_QUERIES,
            injection_query: None,
        },
        LanguageDefinition {
            id: "json",
            extensions: &["json"],
            language: || tree_sitter_json::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_json::HIGHLIGHTS_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "toml",
            extensions: &["toml"],
            language: || tree_sitter_toml_ng::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "yaml",
            extensions: &["yml", "yaml"],
            language: || tree_sitter_yaml::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "python",
            extensions: &["py", "pyw"],
            language: || tree_sitter_python::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_python::HIGHLIGHTS_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "bash",
            extensions: &["sh", "bash", "zsh"],
            language: || tree_sitter_bash::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_bash::HIGHLIGHT_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "powershell",
            extensions: &["ps1", "psm1", "psd1"],
            language: || tree_sitter_powershell::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_powershell::HIGHLIGHTS_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "lua",
            extensions: &["lua"],
            language: || tree_sitter_lua::LANGUAGE.into(),
            highlight_queries: &[tree_sitter_lua::HIGHLIGHTS_QUERY],
            injection_query: None,
        },
        LanguageDefinition {
            id: "husk",
            extensions: &["hk", "husk"],
            language: || tree_sitter_rust::LANGUAGE.into(),
            highlight_queries: &[],
            injection_query: None,
        },
    ]
}

fn highlight_husk(code: &str, theme: &Theme) -> Vec<StyleInfo> {
    let mut styles = Vec::new();
    let mut cursor = 0;

    for token in Lexer::new(code) {
        highlight_trivia(&token.leading_trivia, cursor, theme, &mut styles);

        let token_start = token.span.range.start;
        let token_end = token.span.range.end;
        if !matches!(token.kind, TokenKind::Eof) {
            highlight_husk_token(
                &token.kind,
                token_start..token_end,
                code,
                theme,
                &mut styles,
            );
        }
        cursor = token_end;

        cursor = highlight_trivia(&token.trailing_trivia, cursor, theme, &mut styles);
    }

    styles
}

fn highlight_trivia(
    trivia: &[Trivia],
    mut cursor: usize,
    theme: &Theme,
    styles: &mut Vec<StyleInfo>,
) -> usize {
    for item in trivia {
        let len = trivia_len(item);
        let start = cursor;
        cursor += len;
        if matches!(item, Trivia::LineComment(_)) {
            push_style(theme, "comment", start..cursor, styles);
        }
    }
    cursor
}

fn trivia_len(trivia: &Trivia) -> usize {
    match trivia {
        Trivia::Whitespace(value) | Trivia::Newline(value) | Trivia::LineComment(value) => {
            value.len()
        }
    }
}

fn highlight_husk_token(
    kind: &TokenKind,
    range: Range<usize>,
    code: &str,
    theme: &Theme,
    styles: &mut Vec<StyleInfo>,
) {
    match kind {
        TokenKind::Keyword(Keyword::True | Keyword::False) => {
            push_style(theme, "constant.builtin", range, styles);
        }
        TokenKind::Keyword(Keyword::SelfType) => {
            push_style(theme, "variable.builtin", range, styles);
        }
        TokenKind::Keyword(_) => {
            push_style(theme, "keyword", range, styles);
        }
        TokenKind::IntLiteral(_) | TokenKind::FloatLiteral(_) => {
            push_style(theme, "constant.numeric", range, styles);
        }
        TokenKind::StringLiteral(_) => {
            push_style(theme, "string", range, styles);
        }
        TokenKind::Ident(_) => {
            if let Some(text) = code.get(range.clone()) {
                if is_husk_builtin_type(text) {
                    push_style(theme, "type.builtin", range, styles);
                }
            }
        }
        TokenKind::Plus
        | TokenKind::PlusEq
        | TokenKind::Minus
        | TokenKind::MinusEq
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::PercentEq
        | TokenKind::Eq
        | TokenKind::EqEq
        | TokenKind::Bang
        | TokenKind::BangEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::Le
        | TokenKind::Ge
        | TokenKind::AndAnd
        | TokenKind::Amp
        | TokenKind::OrOr
        | TokenKind::Pipe
        | TokenKind::Arrow
        | TokenKind::FatArrow
        | TokenKind::Question
        | TokenKind::DotDot
        | TokenKind::DotDotEq => {
            push_style(theme, "operator", range, styles);
        }
        _ => {}
    }
}

fn is_husk_builtin_type(text: &str) -> bool {
    matches!(
        text,
        "bool"
            | "char"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "isize"
            | "Json"
            | "str"
            | "String"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
    )
}

fn push_style(theme: &Theme, scope: &str, range: Range<usize>, styles: &mut Vec<StyleInfo>) {
    if range.start >= range.end {
        return;
    }

    if let Some(style) = theme.get_style(scope) {
        styles.push(StyleInfo {
            start: range.start,
            end: range.end,
            style,
        });
    }
}

const JAVASCRIPT_PARAMETER_HIGHLIGHT_QUERY: &str = r#"
(formal_parameters
  (pattern/identifier) @variable.parameter)

(formal_parameters
  (pattern/array_pattern
    (identifier) @variable.parameter))

(formal_parameters
  (pattern/object_pattern
    [
      (pair_pattern value: (identifier) @variable.parameter)
      (shorthand_property_identifier_pattern) @variable.parameter
    ]))
"#;

const JAVASCRIPT_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    JAVASCRIPT_PARAMETER_HIGHLIGHT_QUERY,
];
const JSX_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    JAVASCRIPT_PARAMETER_HIGHLIGHT_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
];
const TYPESCRIPT_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
];
const TSX_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
];

const MARKDOWN_HIGHLIGHT_QUERY: &str = r#"
(atx_heading
  (atx_h1_marker) @punctuation.definition.heading.markdown
  (inline) @heading.1.markdown)

(atx_heading
  (atx_h2_marker) @punctuation.definition.heading.markdown
  (inline) @heading.2.markdown)

(atx_heading
  (atx_h3_marker) @punctuation.definition.heading.markdown
  (inline) @heading.3.markdown)

(atx_heading
  (atx_h4_marker) @punctuation.definition.heading.markdown
  (inline) @heading.4.markdown)

(atx_heading
  (atx_h5_marker) @punctuation.definition.heading.markdown
  (inline) @heading.5.markdown)

(atx_heading
  (atx_h6_marker) @punctuation.definition.heading.markdown
  (inline) @heading.6.markdown)

(setext_heading
  (paragraph) @markup.heading.setext.1.markdown
  (setext_h1_underline) @punctuation.definition.heading.markdown)

(setext_heading
  (paragraph) @markup.heading.setext.2.markdown
  (setext_h2_underline) @punctuation.definition.heading.markdown)

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
] @punctuation.definition.list.begin.markdown

[
  (indented_code_block)
  (fenced_code_block)
] @markup.raw.block.markdown

(fenced_code_block_delimiter) @punctuation.definition.raw.markdown

(link_destination) @markup.underline.link.markdown
(link_label) @constant.other.reference.link.markdown
(thematic_break) @meta.separator.markdown

[
  (block_continuation)
  (block_quote_marker)
] @punctuation.definition.quote.begin.markdown

(backslash_escape) @escape
"#;

const MARKDOWN_INJECTION_QUERY: &str = r#"
(fenced_code_block
  (info_string
    (language) @injection.language)
  (code_fence_content) @injection.content)
"#;

pub fn normalized_extension(file: &str) -> Option<String> {
    PathBuf::from(file)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use crate::{
        color::Color,
        theme::{parse_vscode_theme, Style, Theme, TokenStyle},
    };

    use super::*;

    fn highlighter() -> Highlighter {
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        Highlighter::new(&theme).unwrap()
    }

    fn theme_with_markdown_textmate_scopes() -> Theme {
        let markdown_heading = Style {
            fg: Some(Color::Rgb {
                r: 139,
                g: 164,
                b: 176,
            }),
            ..Default::default()
        };
        let markdown_plain = Style {
            fg: Some(Color::Rgb {
                r: 197,
                g: 201,
                b: 199,
            }),
            ..Default::default()
        };

        Theme {
            token_styles: vec![
                TokenStyle {
                    name: None,
                    scope: vec!["markup.heading.markdown".to_string()],
                    style: markdown_heading,
                },
                TokenStyle {
                    name: None,
                    scope: vec!["punctuation.definition.list_item.markdown".to_string()],
                    style: markdown_plain,
                },
            ],
            ..Theme::default()
        }
    }

    fn theme_with_scopes(scopes: &[&str]) -> Theme {
        let style = Style {
            fg: Some(Color::Rgb {
                r: 139,
                g: 164,
                b: 176,
            }),
            ..Default::default()
        };

        Theme {
            token_styles: scopes
                .iter()
                .map(|scope| TokenStyle {
                    name: None,
                    scope: vec![(*scope).to_string()],
                    style: style.clone(),
                })
                .collect(),
            ..Theme::default()
        }
    }

    fn assert_token_highlighted(styles: &[StyleInfo], code: &str, token: &str) {
        let start = code.find(token).unwrap();
        let end = start + token.len();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= start && style.end >= end),
            "`{token}` should be highlighted"
        );
    }

    #[test]
    fn resolves_language_by_file_extension() {
        let highlighter = highlighter();

        assert_eq!(
            highlighter.language_id_for_file(Some("main.rs")),
            Some("rust")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("README.MD")),
            Some("markdown")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("component.tsx")),
            Some("tsx")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("component.jsx")),
            Some("jsx")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("config.yml")),
            Some("yaml")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("script.sh")),
            Some("bash")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("bootstrap.ps1")),
            Some("powershell")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("theme.lua")),
            Some("lua")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("plugin.hk")),
            Some("husk")
        );
        assert_eq!(highlighter.language_id_for_file(Some("LICENSE")), None);
    }

    #[test]
    fn highlights_supported_languages() {
        let samples = [
            ("rust", "fn main() { let value = true; }\n"),
            ("markdown", "# Heading\n\n```rust\nfn main() {}\n```\n"),
            ("javascript", "const value = true;\n"),
            ("jsx", "export const View = () => <div />;\n"),
            ("typescript", "const value: boolean = true;\n"),
            ("tsx", "export const View = () => <div />;\n"),
            ("json", r#"{"value": true}"#),
            ("toml", "value = true\n"),
            ("yaml", "value: true\n"),
            ("python", "def main():\n    return True\n"),
            ("bash", "if [ -f Cargo.toml ]; then\n  echo yes\nfi\n"),
            (
                "powershell",
                "function Invoke-Greeting { param([string]$Name) Write-Host \"Hello $Name\" }\n",
            ),
            (
                "lua",
                "local function greet(name) return 'hello ' .. name end\n",
            ),
            (
                "husk",
                "pub fn activate() { red::add_command(\"Hello\", hello); }\n",
            ),
        ];
        let mut highlighter = highlighter();

        for (language_id, code) in samples {
            let styles = highlighter.highlight(language_id, code).unwrap();
            assert!(
                !styles.is_empty(),
                "{language_id} should produce syntax highlight spans"
            );
        }
    }

    #[test]
    fn resolves_fenced_code_language_aliases() {
        let highlighter = highlighter();

        assert_eq!(highlighter.language_id_for_name("rs"), Some("rust"));
        assert_eq!(highlighter.language_id_for_name("py"), Some("python"));
        assert_eq!(highlighter.language_id_for_name("yml"), Some("yaml"));
        assert_eq!(highlighter.language_id_for_name("ts"), Some("typescript"));
        assert_eq!(highlighter.language_id_for_name("jsx"), Some("jsx"));
        assert_eq!(highlighter.language_id_for_name("sh"), Some("bash"));
        assert_eq!(highlighter.language_id_for_name("shell"), Some("bash"));
        assert_eq!(highlighter.language_id_for_name("pwsh"), Some("powershell"));
        assert_eq!(highlighter.language_id_for_name("lua"), Some("lua"));
        assert_eq!(highlighter.language_id_for_name("hk"), Some("husk"));
        assert_eq!(highlighter.language_id_for_name("husk"), Some("husk"));
        assert_eq!(highlighter.language_id_for_name("unknown"), None);
    }

    #[test]
    fn husk_highlights_tokens_from_lexer() {
        let theme = theme_with_scopes(&[
            "comment",
            "constant.builtin",
            "constant.numeric",
            "keyword",
            "operator",
            "string",
            "type.builtin",
        ]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = r#"// activate plugin
pub fn activate(event: Json) {
    let enabled = true;
    let count: i32 = 42;
    red::execute("Print", "hello");
}
"#;

        let styles = highlighter
            .highlight_for_file(Some("plugin.hk"), code)
            .unwrap();

        for token in [
            "// activate plugin",
            "pub",
            "fn",
            "Json",
            "let",
            "true",
            "i32",
            "42",
            "=",
            "\"Print\"",
        ] {
            assert_token_highlighted(&styles, code, token);
        }
    }

    #[test]
    fn typescript_inherits_javascript_highlights() {
        let theme = theme_with_scopes(&["keyword", "string", "function", "function.method"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = r#"import fs from "node:fs/promises";
describe("StateStore", async () => {
    const store = new StateStore();
    await store.initialize();
});
"#;

        for language_id in ["typescript", "tsx"] {
            let styles = highlighter.highlight(language_id, code).unwrap();

            for token in [
                "import",
                "\"node:fs/promises\"",
                "describe",
                "async",
                "const",
                "new",
                "await",
                "initialize",
            ] {
                assert_token_highlighted(&styles, code, token);
            }
        }
    }

    #[test]
    fn javascript_family_highlights_parameters() {
        let theme = theme_with_scopes(&["variable.parameter"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "function greet(person) { return person; }";

        for language_id in ["javascript", "jsx", "typescript", "tsx"] {
            let styles = highlighter.highlight(language_id, code).unwrap();
            assert_token_highlighted(&styles, code, "person");
        }
    }

    #[test]
    fn jsx_languages_highlight_tags_and_attributes() {
        let theme = theme_with_scopes(&["tag", "attribute"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = r#"const view = <section data-id="value" />;"#;

        for language_id in ["jsx", "tsx"] {
            let styles = highlighter.highlight(language_id, code).unwrap();
            assert_token_highlighted(&styles, code, "section");
            assert_token_highlighted(&styles, code, "data-id");
        }
    }

    #[test]
    fn markdown_uses_theme_compatible_scopes() {
        let mut highlighter = highlighter();
        let styles = highlighter
            .highlight_for_file(Some("CLAUDE.md"), "### Debugging\n- `dh` - History\n")
            .unwrap();

        assert!(
            !styles.is_empty(),
            "markdown should produce themed highlight spans"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= 4 && style.end >= 13),
            "markdown heading text should be highlighted"
        );
        assert!(
            styles.iter().any(|style| style.start == 14),
            "markdown list marker should be highlighted"
        );
    }

    #[test]
    fn markdown_highlights_with_textmate_markdown_theme_scopes() {
        let theme = theme_with_markdown_textmate_scopes();
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "## Determining the PR(s)\n- Use `gh`\n";
        let styles = highlighter
            .highlight_for_file(Some("SKILL.md"), code)
            .unwrap();
        let list_marker_start = code.find("- ").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= 3 && style.end >= 21),
            "markdown heading should use TextMate-compatible theme scopes"
        );
        assert!(
            styles.iter().any(|style| style.start == list_marker_start),
            "markdown list marker should use TextMate-compatible theme scopes"
        );
    }

    #[test]
    fn markdown_highlights_rust_fenced_code() {
        let mut highlighter = highlighter();
        let code = "# Example\n\n```rust\nfn main() {\n    let value = true;\n}\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let fn_start = code.find("fn").unwrap();
        let let_start = code.find("let").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= fn_start && style.end >= fn_start + 2),
            "fenced Rust `fn` keyword should be highlighted at Markdown byte offsets"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= let_start && style.end >= let_start + 3),
            "fenced Rust `let` keyword should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_highlights_json_fenced_code() {
        let mut highlighter = highlighter();
        let code = "```json\n{\"enabled\": true}\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let bool_start = code.find("true").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= bool_start && style.end >= bool_start + 4),
            "fenced JSON boolean should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_highlights_bash_fenced_code() {
        let mut highlighter = highlighter();
        let code = "```sh\nif [ -f Cargo.toml ]; then\n  echo yes\nfi\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let if_start = code.find("if").unwrap();
        let echo_start = code.find("echo").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= if_start && style.end >= if_start + 2),
            "fenced shell `if` keyword should be highlighted at Markdown byte offsets"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= echo_start && style.end >= echo_start + 4),
            "fenced shell command should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_highlights_husk_fenced_code() {
        let theme = theme_with_scopes(&["keyword", "string"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "```husk\npub fn activate() { red::log(\"ready\"); }\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let pub_start = code.find("pub").unwrap();
        let ready_start = code.find("\"ready\"").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= pub_start && style.end >= pub_start + 3),
            "fenced Husk `pub` keyword should be highlighted at Markdown byte offsets"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= ready_start && style.end >= ready_start + 7),
            "fenced Husk string should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_resolves_fenced_code_by_registered_extension() {
        let mut highlighter = highlighter();
        let code = "```pyw\nprint(True)\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let true_start = code.find("True").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= true_start && style.end >= true_start + 4),
            "fenced language names should resolve through registered extensions"
        );
    }

    #[test]
    fn markdown_ignores_unknown_fenced_code_language() {
        let mut highlighter = highlighter();
        let code = "```madeup\nhello\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let content_start = code.find("hello").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= content_start && style.end >= content_start + 5),
            "unknown fenced language should keep Markdown raw block styling"
        );
    }

    #[test]
    fn unknown_languages_do_not_error() {
        let mut highlighter = highlighter();

        assert!(highlighter
            .highlight("unknown", "plain text")
            .unwrap()
            .is_empty());
        assert!(highlighter
            .highlight_for_file(Some("notes.txt"), "plain text")
            .unwrap()
            .is_empty());
    }
}
