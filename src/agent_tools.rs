//! Strict editor-tool contract shared by Red and Codex dynamic tools.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot};

/// Maximum number of edits accepted in one atomic proposal operation.
pub const MAX_EDITOR_EDITS: usize = 128;

/// A zero-based UTF-16 position, compatible with LSP coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditorPosition {
    /// Zero-based line.
    pub line: usize,
    /// Zero-based UTF-16 code-unit offset within the line.
    pub character: usize,
}

/// One half-open text replacement expressed in UTF-16 coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditorTextEdit {
    /// Inclusive start position.
    pub start: EditorPosition,
    /// Exclusive end position.
    pub end: EditorPosition,
    /// Replacement UTF-8 text.
    pub new_text: String,
}

/// The visual-selection mode requested by an agent.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditorSelectionKind {
    /// Characterwise visual selection.
    #[default]
    Character,
    /// Whole-line visual selection.
    Line,
    /// Rectangular visual-block selection.
    Block,
}

/// Safe, explicitly registered editor and LSP actions an agent may invoke.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditorActionName {
    /// Request the active language server's definition target.
    GoToDefinition,
    /// Request hover information at the active cursor.
    Hover,
    /// Request fresh diagnostics for the active document.
    RefreshDiagnostics,
    /// Request signature help at the active cursor.
    SignatureHelp,
    /// Move backward in the editor jumplist.
    JumpBack,
    /// Move forward in the editor jumplist.
    JumpForward,
    /// Activate the next buffer.
    NextBuffer,
    /// Activate the previous buffer.
    PreviousBuffer,
}

/// Semantic editor operation. Text changes always stage a proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditorToolCall {
    /// Read a bounded snapshot of active editor state.
    GetEditorState {},
    /// Open a workspace file and reveal a UTF-16 position.
    OpenFile {
        /// Workspace-relative or accepted absolute path.
        path: String,
        /// Zero-based destination line.
        #[serde(default)]
        line: usize,
        /// Zero-based UTF-16 destination offset.
        #[serde(default)]
        character: usize,
        /// Window placement requested for the file.
        #[serde(default)]
        target: EditorOpenTarget,
    },
    /// Open a file and create a visual selection.
    SelectText {
        /// Workspace file containing the selection.
        path: String,
        /// Inclusive UTF-16 selection start.
        start: EditorPosition,
        /// Exclusive UTF-16 selection end.
        end: EditorPosition,
        /// Requested visual selection mode.
        #[serde(default)]
        kind: EditorSelectionKind,
    },
    /// Stage atomic, revision-checked replacements as a reviewable proposal.
    ApplyEdits {
        /// Workspace file to change.
        path: String,
        /// Visible buffer revision on which the edits were based.
        expected_revision: u64,
        /// Non-overlapping half-open UTF-16 replacements.
        edits: Vec<EditorTextEdit>,
    },
    /// Invoke one allow-listed non-mutating editor or LSP action.
    RunEditorAction {
        /// Registered safe action.
        action: EditorActionName,
    },
}

impl EditorToolCall {
    /// Parse an adapter tool name and its strict argument object.
    pub fn parse(name: &str, arguments: Value) -> anyhow::Result<Self> {
        let Value::Object(mut arguments) = arguments else {
            anyhow::bail!("editor tool arguments must be an object");
        };
        anyhow::ensure!(
            !arguments.contains_key("tool"),
            "editor tool arguments cannot override the tool name"
        );
        arguments.insert("tool".to_string(), Value::String(name.to_string()));
        serde_json::from_value(Value::Object(arguments))
            .map_err(|error| anyhow::anyhow!("invalid {name} arguments: {error}"))
    }

    #[must_use]
    /// Returns whether the call stages textual edits.
    pub fn is_edit(&self) -> bool {
        matches!(self, Self::ApplyEdits { .. })
    }

    #[must_use]
    /// Formats a bounded user-facing description of the in-progress call.
    pub fn activity_title(&self) -> String {
        match self {
            Self::GetEditorState {} => "Inspecting editor state".to_string(),
            Self::OpenFile { path, .. } => format!("Opening {path}"),
            Self::SelectText { path, .. } => format!("Selecting text in {path}"),
            Self::ApplyEdits { path, edits, .. } => {
                format!("Proposing {} edit(s) in {path}", edits.len())
            }
            Self::RunEditorAction { action } => format!("Running editor action {action:?}"),
        }
    }
}

/// Destination used when opening a file from an editor tool.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditorOpenTarget {
    /// Reuse the active window.
    #[default]
    Current,
    /// Open in a horizontal split.
    Horizontal,
    /// Open in a vertical split.
    Vertical,
}

/// One Codex editor-tool request tied to an active session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EditorToolRequest {
    /// Active Codex session that owns the call and any resulting proposal.
    pub session_id: String,
    /// Strictly parsed semantic operation.
    #[serde(flatten)]
    pub call: EditorToolCall,
}

/// One bounded request waiting for the editor main loop to produce a result.
#[derive(Debug)]
pub struct PendingEditorTool {
    /// Request to execute on the editor owner task.
    pub request: EditorToolRequest,
    /// One-shot result channel back to the Codex worker.
    pub response: oneshot::Sender<Result<Value, String>>,
}

/// Create the bounded editor-tool request channel owned by one editor instance.
#[must_use]
pub fn editor_tool_channel(
    capacity: usize,
) -> (
    mpsc::Sender<PendingEditorTool>,
    mpsc::Receiver<PendingEditorTool>,
) {
    mpsc::channel(capacity)
}

/// Return strict schemas for Codex dynamic editor tools.
#[must_use]
pub fn editor_tool_schemas(schema_key: &str) -> Vec<Value> {
    let position = json!({
        "type": "object",
        "properties": {
            "line": {"type": "integer", "minimum": 0},
            "character": {"type": "integer", "minimum": 0}
        },
        "required": ["line", "character"],
        "additionalProperties": false
    });
    let definitions = [
        (
            "get_editor_state",
            "Inspect the active editor file, cursor, selection, windows, diagnostics, and bounded context.",
            json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
        ),
        (
            "open_file",
            "Open a workspace file in the editor and reveal a zero-based UTF-16 location.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "line": {"type": "integer", "minimum": 0},
                    "character": {"type": "integer", "minimum": 0},
                    "target": {"type": "string", "enum": ["current", "horizontal", "vertical"]}
                },
                "required": ["path", "line", "character", "target"],
                "additionalProperties": false
            }),
        ),
        (
            "select_text",
            "Open a workspace file and create a visual selection using a half-open, zero-based UTF-16 range.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start": position,
                    "end": position,
                    "kind": {"type": "string", "enum": ["character", "line", "block"]}
                },
                "required": ["path", "start", "end", "kind"],
                "additionalProperties": false
            }),
        ),
        (
            "apply_edits",
            "Atomically stage up to 128 non-overlapping, half-open UTF-16 text edits as a reviewable editor proposal. This never saves or writes to disk.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "expected_revision": {"type": "integer", "minimum": 0},
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_EDITOR_EDITS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "start": position,
                                "end": position,
                                "new_text": {"type": "string"}
                            },
                            "required": ["start", "end", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "expected_revision", "edits"],
                "additionalProperties": false
            }),
        ),
        (
            "run_editor_action",
            "Run a safe editor or LSP action. This cannot invoke arbitrary commands, shell, save, quit, or live text mutations.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "go_to_definition", "hover", "refresh_diagnostics", "signature_help",
                            "jump_back", "jump_forward", "next_buffer", "previous_buffer"
                        ]
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        ),
    ];
    definitions
        .into_iter()
        .map(|(name, description, schema)| {
            let mut tool = Map::from_iter([
                ("type".to_string(), json!("function")),
                ("name".to_string(), json!(name)),
                ("description".to_string(), json!(description)),
            ]);
            tool.insert(schema_key.to_string(), schema);
            Value::Object(tool)
        })
        .collect()
}

/// Validate and atomically apply half-open UTF-16 edits to text.
pub fn apply_text_edits(contents: &str, edits: &[EditorTextEdit]) -> anyhow::Result<String> {
    anyhow::ensure!(!edits.is_empty(), "editor edit list cannot be empty");
    anyhow::ensure!(
        edits.len() <= MAX_EDITOR_EDITS,
        "editor edit list exceeds {MAX_EDITOR_EDITS} entries"
    );

    let mut resolved = edits
        .iter()
        .map(|edit| {
            anyhow::ensure!(
                !edit.new_text.contains('\0'),
                "editor edit text cannot contain NUL bytes"
            );
            let start = utf16_byte_offset(contents, edit.start)?;
            let end = utf16_byte_offset(contents, edit.end)?;
            anyhow::ensure!(start <= end, "editor edit end precedes its start");
            Ok((start, end, edit.new_text.as_str()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    resolved.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in resolved.windows(2) {
        anyhow::ensure!(
            pair[0].1 <= pair[1].0 && (pair[0].0 != pair[1].0 || pair[0].1 != pair[1].1),
            "editor edits overlap or share an ambiguous insertion point"
        );
    }

    let mut output = contents.to_string();
    for (start, end, replacement) in resolved.into_iter().rev() {
        output.replace_range(start..end, replacement);
    }
    Ok(output)
}

/// Convert a zero-based UTF-16 position to a byte offset and reject split surrogates.
pub fn utf16_byte_offset(contents: &str, position: EditorPosition) -> anyhow::Result<usize> {
    let mut line_start = 0usize;
    let mut lines = contents.split('\n');
    for _ in 0..position.line {
        let line = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("editor position line is out of bounds"))?;
        line_start = line_start.saturating_add(line.len() + 1);
    }
    let line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("editor position line is out of bounds"))?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut utf16 = 0usize;
    for (byte, character) in line.char_indices() {
        if utf16 == position.character {
            return Ok(line_start + byte);
        }
        utf16 += character.len_utf16();
        anyhow::ensure!(
            utf16 <= position.character,
            "editor position splits a UTF-16 surrogate pair"
        );
    }
    anyhow::ensure!(
        utf16 == position.character,
        "editor position character is out of bounds"
    );
    Ok(line_start + line.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(line: usize, character: usize) -> EditorPosition {
        EditorPosition { line, character }
    }

    #[test]
    fn tool_schemas_are_strict_and_bounded() {
        for schema_key in ["parameters", "inputSchema"] {
            let tools = editor_tool_schemas(schema_key);
            assert_eq!(tools.len(), 5);
            assert!(tools
                .iter()
                .all(|tool| tool[schema_key]["additionalProperties"] == false));
            assert_eq!(tools[3][schema_key]["properties"]["edits"]["maxItems"], 128);
            assert_eq!(
                tools[1][schema_key]["required"],
                json!(["path", "line", "character", "target"])
            );
            assert_eq!(
                tools[2][schema_key]["required"],
                json!(["path", "start", "end", "kind"])
            );
        }
    }

    #[test]
    fn tool_parser_rejects_unknown_actions_and_fields() {
        assert!(EditorToolCall::parse("run_editor_action", json!({"action": "quit"})).is_err());
        assert!(EditorToolCall::parse("get_editor_state", json!({"extra": true})).is_err());
        assert!(
            EditorToolCall::parse("open_file", json!({"path": "main.rs", "tool": "quit"})).is_err()
        );
    }

    #[test]
    fn editor_tool_request_round_trips_the_flat_dynamic_tool_shape() {
        let request = EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::GetEditorState {},
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded,
            json!({"sessionId": "session-1", "tool": "get_editor_state"})
        );
        assert_eq!(
            serde_json::from_value::<EditorToolRequest>(encoded).unwrap(),
            request
        );
        assert!(serde_json::from_value::<EditorToolRequest>(json!({
            "sessionId": "session-1",
            "tool": "get_editor_state",
            "unexpected": true
        }))
        .is_err());
    }

    #[test]
    fn utf16_edits_replace_unicode_and_preserve_crlf() {
        let contents = "a😀b\r\nsecond\n";
        let edits = [
            EditorTextEdit {
                start: position(0, 1),
                end: position(0, 3),
                new_text: "λ".to_string(),
            },
            EditorTextEdit {
                start: position(1, 6),
                end: position(1, 6),
                new_text: "!".to_string(),
            },
        ];
        assert_eq!(
            apply_text_edits(contents, &edits).unwrap(),
            "aλb\r\nsecond!\n"
        );
    }

    #[test]
    fn invalid_utf16_and_overlapping_edits_fail_closed() {
        assert!(utf16_byte_offset("😀", position(0, 1)).is_err());
        assert!(utf16_byte_offset("abc", position(1, 0)).is_err());
        assert!(utf16_byte_offset("abc", position(0, 4)).is_err());
        let edit = EditorTextEdit {
            start: position(0, 0),
            end: position(0, 2),
            new_text: String::new(),
        };
        assert!(apply_text_edits("abc", &[edit.clone(), edit]).is_err());
    }
}
