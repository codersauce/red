//! Bounded, editor-independent results returned by inline-assist tools.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_REPLACEMENT_BYTES: usize = 128 * 1024;
pub const MAX_COMMENTS: usize = 16;
pub const MAX_COMMENT_BYTES: usize = 4096;
pub const MAX_EXPANDED_SOURCE_BYTES: usize = 64 * 1024;

/// A same-file linewise proposal, verified by the editor before review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExpandedInlineScope {
    pub start_line: usize,
    pub end_line: usize,
    pub expected_revision: u64,
    pub before: String,
    pub reason: String,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_scope: Option<ExpandedInlineScope>,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub comments: Vec<InlineCommentInput>,
    /// A broader request that should continue in the full Agent workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_agent: Option<String>,
}

impl InlineAssistResult {
    /// Whether accepting this result would change the reviewed source text.
    pub fn changes_text(&self, before: &str) -> bool {
        let before = self
            .expanded_scope
            .as_ref()
            .map_or(before, |scope| scope.before.as_str());
        self.replacement
            .as_deref()
            .is_some_and(|replacement| replacement != before)
    }

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
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct NeedsAgent {
            reason: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ExpandedReplacement {
            start_line: usize,
            end_line: usize,
            expected_revision: u64,
            before: String,
            replacement: String,
            reason: String,
            #[serde(default)]
            comments: Vec<InlineCommentInput>,
        }
        let mut result = match tool {
            "submit_replacement" => {
                let input: Replacement = serde_json::from_value(arguments)
                    .context("invalid submit_replacement arguments")?;
                Self {
                    replacement: Some(input.replacement),
                    expanded_scope: None,
                    comments: input.comments,
                    needs_agent: None,
                }
            }
            "submit_comments" => {
                let input: Comments = serde_json::from_value(arguments)
                    .context("invalid submit_comments arguments")?;
                Self {
                    replacement: None,
                    comments: input.comments,
                    expanded_scope: None,
                    needs_agent: None,
                }
            }
            "request_agent" => {
                let input: NeedsAgent =
                    serde_json::from_value(arguments).context("invalid request_agent arguments")?;
                Self {
                    replacement: None,
                    comments: Vec::new(),
                    expanded_scope: None,
                    needs_agent: Some(input.reason.trim().to_string()),
                }
            }
            "propose_expanded_replacement" => {
                let input: ExpandedReplacement = serde_json::from_value(arguments)
                    .context("invalid propose_expanded_replacement arguments")?;
                Self {
                    expanded_scope: Some(ExpandedInlineScope {
                        start_line: input.start_line,
                        end_line: input.end_line,
                        expected_revision: input.expected_revision,
                        before: input.before,
                        reason: input.reason.trim().to_string(),
                    }),
                    replacement: Some(input.replacement),
                    comments: input.comments,
                    needs_agent: None,
                }
            }
            _ => {
                anyhow::bail!("unsupported inline-assist submission tool")
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
        if let Some(scope) = &self.expanded_scope {
            ensure!(
                self.needs_agent.is_none() && self.replacement.is_some(),
                "a wider proposal requires a replacement, not an Agent handoff"
            );
            ensure!(
                scope.start_line > 0 && scope.end_line >= scope.start_line,
                "invalid wider edit range"
            );
            ensure!(
                !scope.before.is_empty()
                    && scope.before.len() <= MAX_EXPANDED_SOURCE_BYTES
                    && !scope.before.contains('\0'),
                "invalid wider edit source"
            );
            ensure!(
                target_line_count(&scope.before) == scope.end_line - scope.start_line + 1,
                "wider edit source does not match its line range"
            );
            ensure!(
                !scope.reason.trim().is_empty()
                    && scope.reason.len() <= MAX_COMMENT_BYTES
                    && !scope
                        .reason
                        .chars()
                        .any(|ch| (ch.is_control() && ch != '\n')
                            || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
                "invalid wider edit reason"
            );
            ensure!(
                self.replacement.as_deref() != Some(scope.before.as_str()),
                "wider proposal does not change source"
            );
        }
        if let Some(reason) = &self.needs_agent {
            ensure!(
                self.replacement.is_none() && self.comments.is_empty(),
                "an Agent handoff cannot also edit code or submit comments"
            );
            ensure!(
                !reason.trim().is_empty()
                    && reason.len() <= MAX_COMMENT_BYTES
                    && !reason.chars().any(|ch| (ch.is_control() && ch != '\n')
                        || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
                "invalid Agent handoff reason"
            );
        }
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
         "inputSchema": {"type": "object", "properties": {"comments": comments.clone()}, "required": ["comments"], "additionalProperties": false}},
        {"type": "function", "name": "request_agent",
         "description": "Request a contextual handoff when the user's task needs code or edits outside the supplied target. Explain what needs to change. This does not edit code, add a comment, or start the Agent automatically. Call exactly one submission tool per turn.",
         "inputSchema": {"type": "object", "properties": {"reason": {"type": "string", "minLength": 1, "maxLength": MAX_COMMENT_BYTES}}, "required": ["reason"], "additionalProperties": false}},
        {"type": "function", "name": "propose_expanded_replacement",
         "description": "Propose a wider linewise replacement in the SAME active file when scope expansion is allowed. Read the source first. The inclusive one-based file range must contain and extend the original target. Supply the exact original text and editor revision. This only proposes an edit: the user must review its diff and approve it. Comments refer to replacement-relative lines. Call exactly one submission tool per turn.",
         "inputSchema": {"type": "object", "properties": {"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1},"expected_revision":{"type":"integer","minimum":0},"before":{"type":"string","minLength":1},"replacement":{"type":"string"},"reason":{"type":"string","minLength":1,"maxLength":MAX_COMMENT_BYTES},"comments":comments},"required":["start_line","end_line","expected_revision","before","replacement","reason"],"additionalProperties":false}}
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_expansion_schema_is_bounded_and_cannot_choose_a_file() {
        let input = json!({"start_line":2,"end_line":3,"expected_revision":4,"before":"one\ntwo\n","replacement":"changed\n","reason":"Update the helper."});
        let result =
            InlineAssistResult::from_tool("propose_expanded_replacement", input.clone()).unwrap();
        assert!(result.changes_text("unrelated narrow target"));
        assert_eq!(result.expanded_scope.unwrap().expected_revision, 4);
        for (key, value) in [
            ("path", json!("other.c")),
            ("start_line", json!(0)),
            ("end_line", json!(4)),
            ("before", json!("x".repeat(MAX_EXPANDED_SOURCE_BYTES + 1))),
            ("replacement", json!("one\ntwo\n")),
            ("reason", json!("")),
        ] {
            let mut invalid = input.clone();
            invalid[key] = value;
            assert!(
                InlineAssistResult::from_tool("propose_expanded_replacement", invalid).is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn agent_handoff_is_a_separate_bounded_result() {
        let mut result = InlineAssistResult::from_tool(
            "request_agent",
            json!({"reason": "Update both functions."}),
        )
        .unwrap();
        assert_eq!(
            result.needs_agent.as_deref(),
            Some("Update both functions.")
        );
        assert!(result.validate_for_target("one line").is_ok());
        result.replacement = Some("changed".into());
        assert!(result.validate().is_err());
        for reason in [
            "".to_string(),
            "x".repeat(MAX_COMMENT_BYTES + 1),
            "hidden\u{202e}".into(),
        ] {
            assert!(
                InlineAssistResult::from_tool("request_agent", json!({"reason": reason})).is_err()
            );
        }
        let old: InlineAssistResult =
            serde_json::from_value(json!({"replacement": null, "comments": []})).unwrap();
        assert!(old.needs_agent.is_none());
    }

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
