use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use crate::{editor::StyleInfo, theme::Theme};

#[derive(Clone, Copy)]
struct LanguageDefinition {
    id: &'static str,
    extensions: &'static [&'static str],
    language: fn() -> Language,
    highlight_query: &'static str,
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
        let Some(definition) = self.languages.get(language_id).copied() else {
            return Ok(Vec::new());
        };

        if !self.highlighters.contains_key(definition.id) {
            let language = (definition.language)();
            let mut parser = Parser::new();
            parser.set_language(&language)?;
            let query = Query::new(&language, definition.highlight_query)?;
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
        "ts" | "typescript" => Some("typescript"),
        "tsx" => Some("tsx"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "py" | "python" => Some("python"),
        "md" | "markdown" => Some("markdown"),
        "bash" | "sh" | "shell" | "zsh" => Some("bash"),
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
            highlight_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "markdown",
            extensions: &["md", "markdown"],
            language: || tree_sitter_md::LANGUAGE.into(),
            highlight_query: MARKDOWN_HIGHLIGHT_QUERY,
            injection_query: Some(MARKDOWN_INJECTION_QUERY),
        },
        LanguageDefinition {
            id: "javascript",
            extensions: &["js", "jsx", "mjs", "cjs"],
            language: || tree_sitter_javascript::LANGUAGE.into(),
            highlight_query: tree_sitter_javascript::HIGHLIGHT_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "typescript",
            extensions: &["ts"],
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlight_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "tsx",
            extensions: &["tsx"],
            language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlight_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "json",
            extensions: &["json"],
            language: || tree_sitter_json::LANGUAGE.into(),
            highlight_query: tree_sitter_json::HIGHLIGHTS_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "toml",
            extensions: &["toml"],
            language: || tree_sitter_toml_ng::LANGUAGE.into(),
            highlight_query: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "yaml",
            extensions: &["yml", "yaml"],
            language: || tree_sitter_yaml::LANGUAGE.into(),
            highlight_query: tree_sitter_yaml::HIGHLIGHTS_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "python",
            extensions: &["py", "pyw"],
            language: || tree_sitter_python::LANGUAGE.into(),
            highlight_query: tree_sitter_python::HIGHLIGHTS_QUERY,
            injection_query: None,
        },
        LanguageDefinition {
            id: "bash",
            extensions: &["sh", "bash", "zsh"],
            language: || tree_sitter_bash::LANGUAGE.into(),
            highlight_query: tree_sitter_bash::HIGHLIGHT_QUERY,
            injection_query: None,
        },
    ]
}

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
    use crate::theme::parse_vscode_theme;

    use super::*;

    fn highlighter() -> Highlighter {
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        Highlighter::new(&theme).unwrap()
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
            highlighter.language_id_for_file(Some("config.yml")),
            Some("yaml")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("script.sh")),
            Some("bash")
        );
        assert_eq!(highlighter.language_id_for_file(Some("LICENSE")), None);
    }

    #[test]
    fn highlights_supported_languages() {
        let samples = [
            ("rust", "fn main() { let value = true; }\n"),
            ("markdown", "# Heading\n\n```rust\nfn main() {}\n```\n"),
            ("javascript", "const value = true;\n"),
            ("typescript", "const value: boolean = true;\n"),
            ("tsx", "export const View = () => <div />;\n"),
            ("json", r#"{"value": true}"#),
            ("toml", "value = true\n"),
            ("yaml", "value: true\n"),
            ("python", "def main():\n    return True\n"),
            ("bash", "if [ -f Cargo.toml ]; then\n  echo yes\nfi\n"),
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
        assert_eq!(highlighter.language_id_for_name("sh"), Some("bash"));
        assert_eq!(highlighter.language_id_for_name("shell"), Some("bash"));
        assert_eq!(highlighter.language_id_for_name("unknown"), None);
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
