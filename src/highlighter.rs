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
}

struct LanguageHighlighter {
    parser: Parser,
    query: Query,
}

pub struct Highlighter {
    languages: HashMap<&'static str, LanguageDefinition>,
    extensions: HashMap<&'static str, &'static str>,
    highlighters: HashMap<&'static str, LanguageHighlighter>,
    theme: Theme,
}

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
        let Some(definition) = self.languages.get(language_id).copied() else {
            return Ok(Vec::new());
        };

        if !self.highlighters.contains_key(definition.id) {
            let language = (definition.language)();
            let mut parser = Parser::new();
            parser.set_language(&language)?;
            let query = Query::new(&language, definition.highlight_query)?;
            self.highlighters
                .insert(definition.id, LanguageHighlighter { parser, query });
        }

        let Some(highlighter) = self.highlighters.get_mut(definition.id) else {
            return Ok(Vec::new());
        };
        let Some(tree) = highlighter.parser.parse(code, None) else {
            return Ok(Vec::new());
        };

        let mut colors = Vec::new();
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

        Ok(colors)
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
        },
        LanguageDefinition {
            id: "markdown",
            extensions: &["md", "markdown"],
            language: || tree_sitter_md::LANGUAGE.into(),
            highlight_query: MARKDOWN_HIGHLIGHT_QUERY,
        },
        LanguageDefinition {
            id: "javascript",
            extensions: &["js", "jsx", "mjs", "cjs"],
            language: || tree_sitter_javascript::LANGUAGE.into(),
            highlight_query: tree_sitter_javascript::HIGHLIGHT_QUERY,
        },
        LanguageDefinition {
            id: "typescript",
            extensions: &["ts"],
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlight_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        },
        LanguageDefinition {
            id: "tsx",
            extensions: &["tsx"],
            language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlight_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        },
        LanguageDefinition {
            id: "json",
            extensions: &["json"],
            language: || tree_sitter_json::LANGUAGE.into(),
            highlight_query: tree_sitter_json::HIGHLIGHTS_QUERY,
        },
        LanguageDefinition {
            id: "toml",
            extensions: &["toml"],
            language: || tree_sitter_toml_ng::LANGUAGE.into(),
            highlight_query: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        },
        LanguageDefinition {
            id: "yaml",
            extensions: &["yml", "yaml"],
            language: || tree_sitter_yaml::LANGUAGE.into(),
            highlight_query: tree_sitter_yaml::HIGHLIGHTS_QUERY,
        },
        LanguageDefinition {
            id: "python",
            extensions: &["py", "pyw"],
            language: || tree_sitter_python::LANGUAGE.into(),
            highlight_query: tree_sitter_python::HIGHLIGHTS_QUERY,
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
