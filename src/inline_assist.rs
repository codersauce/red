//! Bounded, editor-independent results returned by inline-assist tools.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_REPLACEMENT_BYTES: usize = 128 * 1024;
pub const MAX_COMMENTS: usize = 16;
pub const MAX_COMMENT_BYTES: usize = 4096;

/// One inclusive, one-based line range relative to the target or replacement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InlineCommentInput {
    pub start_line: usize,
    #[serde(default)]
    pub end_line: Option<usize>,
    pub message: String,
}

impl InlineCommentInput {
    pub fn last_line(&self) -> usize {
        self.end_line.unwrap_or(self.start_line)
    }
}

/// Exactly one completed submission per turn. An empty comment list is a valid
/// review result: the model must not invent findings merely to finish a turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InlineAssistResult {
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub comments: Vec<InlineCommentInput>,
}

impl InlineAssistResult {
    pub fn from_tool(tool: &str, arguments: Value) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Replacement {
            replacement: String,
            #[serde(default)]
            comments: Vec<InlineCommentInput>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Comments {
            comments: Vec<InlineCommentInput>,
        }
        let mut result = match tool {
            "submit_replacement" => {
                let input: Replacement = serde_json::from_value(arguments)
                    .context("invalid submit_replacement arguments")?;
                Self {
                    replacement: Some(input.replacement),
                    comments: input.comments,
                }
            }
            "submit_comments" => {
                let input: Comments = serde_json::from_value(arguments)
                    .context("invalid submit_comments arguments")?;
                Self {
                    replacement: None,
                    comments: input.comments,
                }
            }
            _ => {
                anyhow::bail!("inline assist only supports submit_replacement and submit_comments")
            }
        };
        for comment in &mut result.comments {
            comment.message = comment
                .message
                .replace("\r\n", "\n")
                .replace('\t', "    ")
                .trim()
                .to_string();
        }
        result.validate()?;
        if let Some(replacement) = &result.replacement {
            result.validate_for_target(replacement)?;
        }
        Ok(result)
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(replacement) = &self.replacement {
            ensure!(
                !replacement.is_empty() || self.comments.is_empty(),
                "comments cannot target a deleted replacement"
            );
            ensure!(
                replacement.len() <= MAX_REPLACEMENT_BYTES,
                "inline replacement exceeds the 128 KiB limit"
            );
            ensure!(
                !replacement.contains('\0'),
                "inline replacement contains binary data"
            );
        }
        ensure!(
            self.comments.len() <= MAX_COMMENTS,
            "inline result exceeds {MAX_COMMENTS} comments"
        );
        for comment in &self.comments {
            ensure!(
                comment.start_line > 0 && comment.last_line() >= comment.start_line,
                "invalid inline comment range"
            );
            ensure!(
                !comment.message.trim().is_empty(),
                "inline comment cannot be empty"
            );
            ensure!(
                comment.message.len() <= MAX_COMMENT_BYTES,
                "inline comment exceeds {MAX_COMMENT_BYTES} bytes"
            );
            ensure!(
                !comment
                    .message
                    .chars()
                    .any(|ch| (ch.is_control() && ch != '\n')
                        || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
                "inline comment contains control characters"
            );
        }
        Ok(())
    }

    pub fn validate_for_target(&self, target: &str) -> Result<()> {
        self.validate()?;
        let lines = target_line_count(target);
        ensure!(
            self.comments
                .iter()
                .all(|comment| comment.last_line() <= lines),
            "inline comment is outside the {lines}-line target"
        );
        Ok(())
    }
}

pub fn target_line_count(text: &str) -> usize {
    text.lines().count().max(1)
}

pub fn tool_definitions() -> Value {
    let comments = json!({
        "type": "array", "maxItems": MAX_COMMENTS,
        "items": {
            "type": "object",
            "properties": {
                "start_line": {"type": "integer", "minimum": 1},
                "end_line": {"type": "integer", "minimum": 1, "description": "Inclusive end; omit for one line."},
                "message": {"type": "string", "minLength": 1, "maxLength": MAX_COMMENT_BYTES}
            },
            "required": ["start_line", "message"], "additionalProperties": false
        }
    });
    json!([
        {"type": "function", "name": "submit_replacement",
         "description": "Submit the complete replacement and optional comments. Comment lines are one-based and inclusive, relative to the replacement text. Call exactly one submission tool per turn.",
         "inputSchema": {"type": "object", "properties": {"replacement": {"type": "string"}, "comments": comments.clone()}, "required": ["replacement"], "additionalProperties": false}},
        {"type": "function", "name": "submit_comments",
         "description": "Leave inline comments without editing code. Lines are one-based and inclusive, relative to the supplied target (not the surrounding file). An empty list means no findings. Call exactly one submission tool per turn.",
         "inputSchema": {"type": "object", "properties": {"comments": comments}, "required": ["comments"], "additionalProperties": false}}
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comment_only_and_mixed_results() {
        let comments = json!([{"start_line": 1, "end_line": 2, "message": "A useful note"}]);
        let result =
            InlineAssistResult::from_tool("submit_comments", json!({"comments": comments.clone()}))
                .unwrap();
        assert!(result.replacement.is_none());
        assert!(result.validate_for_target("one\ntwo\n").is_ok());
        assert!(result.validate_for_target("one\n").is_err());
        assert!(InlineAssistResult::from_tool(
            "submit_replacement",
            json!({"replacement": "one\ntwo\n", "comments": comments})
        )
        .is_ok());
        assert!(InlineAssistResult::from_tool("submit_comments", json!({"comments": []})).is_ok());
    }

    #[test]
    fn rejects_invalid_ranges_controls_and_unknown_fields() {
        for comment in [
            json!({"start_line": 0, "message": "note"}),
            json!({"start_line": 2, "end_line": 1, "message": "note"}),
            json!({"start_line": 1, "message": "\u{1b}[31mhidden"}),
            json!({"start_line": 1, "message": "   "}),
            json!({"start_line": 1, "message": "x".repeat(MAX_COMMENT_BYTES + 1)}),
            json!({"start_line": 1, "message": "note", "path": "elsewhere"}),
        ] {
            assert!(InlineAssistResult::from_tool(
                "submit_comments",
                json!({"comments": [comment]})
            )
            .is_err());
        }
        assert!(InlineAssistResult::from_tool(
            "submit_comments",
            json!({"comments": [], "replacement": "oops"})
        )
        .is_err());
        assert!(InlineAssistResult::from_tool(
            "submit_replacement",
            json!({"replacement": "one\n", "comments": [{"start_line": 2, "message": "note"}]})
        )
        .is_err());
    }
}
