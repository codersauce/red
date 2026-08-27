//! Strict editor-tool contract shared by Red and Codex dynamic tools.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;

use async_trait::async_trait;
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

use crate::codex::CodexToolHost;

/// Maximum number of edits accepted in one atomic editor operation.
pub const MAX_EDITOR_EDITS: usize = 128;

/// Maximum number of source lines returned by one full-Agent file read.
pub const MAX_AGENT_READ_LINES: usize = 1_000;

/// Maximum source bytes returned by one full-Agent file read.
pub const MAX_AGENT_READ_BYTES: usize = 256 * 1024;

/// Maximum annotations accepted in one Agent tool call.
pub const MAX_AGENT_ANNOTATIONS_PER_CALL: usize = crate::inline_assist::MAX_COMMENTS;

/// One zero-based, inclusive source-line range rendered as an inline annotation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditorAnnotationInput {
    /// Inclusive zero-based start line in the file.
    pub start_line: usize,
    /// Inclusive zero-based end line; defaults to `start_line`.
    #[serde(default)]
    pub end_line: Option<usize>,
    /// Plain-text annotation body.
    pub message: String,
}

impl EditorAnnotationInput {
    #[must_use]
    pub fn last_line(&self) -> usize {
        self.end_line.unwrap_or(self.start_line)
    }
}

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

/// Scope of a bounded diagnostic query; workspace means known reports, not a scan.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LspDiagnosticScope {
    File,
    OpenBuffers,
    Workspace,
}

/// A half-open UTF-16 range used to filter diagnostics in one file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditorLspRange {
    pub start: EditorPosition,
    pub end: EditorPosition,
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
    /// Select the next source annotation in the active buffer.
    NextAnnotation,
    /// Select the previous source annotation in the active buffer.
    PreviousAnnotation,
    /// Select the next annotation overlapping the current source location.
    NextOverlappingAnnotation,
    /// Select the previous annotation overlapping the current source location.
    PreviousOverlappingAnnotation,
}

/// Semantic operation executed by Red's editor owner task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditorToolCall {
    /// Inspect routing and negotiated capabilities without moving editor focus.
    LspStatus { path: String },
    /// Read known diagnostics, optionally refreshing one already-open file.
    LspDiagnostics {
        scope: LspDiagnosticScope,
        path: Option<String>,
        severity: Option<u8>,
        source: Option<String>,
        code: Option<String>,
        range: Option<EditorLspRange>,
        #[serde(default)]
        offset: usize,
        limit: usize,
        expected_generation: Option<u64>,
        #[serde(default)]
        refresh: bool,
        #[serde(default)]
        wait_ms: u64,
    },
    /// Check rename eligibility at a revision-checked UTF-16 position.
    LspPrepareRename {
        path: String,
        position: EditorPosition,
        expected_revision: u64,
    },
    /// Compute, validate, and retain a text-only rename without applying it.
    LspPreviewRename {
        path: String,
        position: EditorPosition,
        expected_revision: u64,
        new_name: String,
    },
    /// Apply a session-owned preview to buffers, without saving any files.
    LspApplyEdit { plan_id: String },
    /// Internal, request-bound read-only inspection for an inline provider.
    #[serde(skip)]
    InlineContext {
        request_id: String,
        call: crate::inline_context::InlineContextCall,
    },
    /// Read the authoritative visible contents of a workspace file.
    ReadFile {
        /// Workspace-relative or accepted absolute path.
        path: String,
        /// One-based first source line to return.
        start_line: usize,
        /// Maximum number of source lines to return.
        line_count: usize,
    },
    /// Replace a file with complete contents and persist it.
    WriteFile {
        /// Workspace-relative or accepted absolute path.
        path: String,
        /// Visible buffer revision returned by the preceding read.
        expected_revision: u64,
        /// Complete replacement contents.
        content: String,
    },
    /// Create workspace directories without changing an editor buffer.
    CreateDirectory {
        /// Workspace-relative or accepted absolute path. Missing parents are created.
        path: String,
    },
    /// Add source-linked annotations without changing file contents.
    AddAnnotations {
        /// Existing workspace file to annotate.
        path: String,
        /// Visible buffer revision returned by the preceding read.
        expected_revision: u64,
        /// Bounded, zero-based inclusive line ranges and messages.
        annotations: Vec<EditorAnnotationInput>,
    },
    /// Hide existing source annotations by their stable identifiers.
    DismissAnnotations {
        /// Annotation UUIDs returned by add or editor-state tools.
        annotation_ids: Vec<String>,
    },
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
    /// Apply atomic, revision-checked replacements and persist them.
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
    /// Returns whether the call changes text.
    pub fn is_edit(&self) -> bool {
        matches!(
            self,
            Self::WriteFile { .. } | Self::ApplyEdits { .. } | Self::LspApplyEdit { .. }
        )
    }

    /// LSP calls retain their response while the editor continues polling servers.
    pub fn is_lsp(&self) -> bool {
        matches!(
            self,
            Self::LspStatus { .. }
                | Self::LspDiagnostics { .. }
                | Self::LspPrepareRename { .. }
                | Self::LspPreviewRename { .. }
                | Self::LspApplyEdit { .. }
        )
    }

    #[must_use]
    /// Formats a bounded user-facing description of the in-progress call.
    pub fn activity_title(&self) -> String {
        match self {
            Self::LspStatus { path } => format!("Inspecting language server for {path}"),
            Self::LspDiagnostics { .. } => "Reading language-server diagnostics".to_string(),
            Self::LspPrepareRename { path, .. } => format!("Checking rename in {path}"),
            Self::LspPreviewRename { path, .. } => format!("Previewing rename in {path}"),
            Self::LspApplyEdit { .. } => "Applying language-server rename (unsaved)".to_string(),
            Self::InlineContext { .. } => "Inspecting inline context".to_string(),
            Self::ReadFile { path, .. } => format!("Reading {path}"),
            Self::WriteFile { path, .. } => format!("Writing {path}"),
            Self::CreateDirectory { path } => format!("Creating {path}/"),
            Self::AddAnnotations {
                path, annotations, ..
            } => format!("Annotating {path} ({} comment(s))", annotations.len()),
            Self::DismissAnnotations { annotation_ids } => {
                format!("Dismissing {} annotation(s)", annotation_ids.len())
            }
            Self::GetEditorState {} => "Inspecting editor state".to_string(),
            Self::OpenFile { path, .. } => format!("Opening {path}"),
            Self::SelectText { path, .. } => format!("Selecting text in {path}"),
            Self::ApplyEdits { path, edits, .. } => {
                format!("Editing {path} ({} change(s))", edits.len())
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
    /// Active Codex session that owns the call.
    pub session_id: String,
    /// Strictly parsed semantic operation.
    #[serde(flatten)]
    pub call: EditorToolCall,
}

/// Codex tool host that forwards all editor-aware reads and writes to Red's owner task.
#[derive(Debug, Clone)]
pub struct EditorToolHost {
    sender: mpsc::Sender<PendingEditorTool>,
}

impl EditorToolHost {
    #[must_use]
    pub fn new(sender: mpsc::Sender<PendingEditorTool>) -> Self {
        Self { sender }
    }

    async fn request(&self, request: EditorToolRequest) -> anyhow::Result<Value> {
        let (response_tx, response_rx) = oneshot::channel();
        timeout(
            Duration::from_secs(30),
            self.sender.send(PendingEditorTool {
                request,
                response: response_tx,
            }),
        )
        .await
        .map_err(|_| anyhow::anyhow!("editor tool dispatcher is backpressured"))?
        .map_err(|_| anyhow::anyhow!("editor tool dispatcher stopped"))?;
        timeout(Duration::from_secs(30), response_rx)
            .await
            .map_err(|_| anyhow::anyhow!("editor tool request timed out"))?
            .map_err(|_| anyhow::anyhow!("editor tool dispatcher dropped the response"))?
            .map_err(anyhow::Error::msg)
    }
}

#[async_trait]
impl CodexToolHost for EditorToolHost {
    async fn read_file(
        &mut self,
        session_id: &str,
        path: &str,
        start_line: usize,
        line_count: usize,
    ) -> anyhow::Result<Value> {
        self.request(EditorToolRequest {
            session_id: session_id.to_string(),
            call: EditorToolCall::ReadFile {
                path: path.to_string(),
                start_line,
                line_count,
            },
        })
        .await
    }

    async fn write_file(
        &mut self,
        session_id: &str,
        path: &str,
        expected_revision: u64,
        content: String,
    ) -> anyhow::Result<Value> {
        self.request(EditorToolRequest {
            session_id: session_id.to_string(),
            call: EditorToolCall::WriteFile {
                path: path.to_string(),
                expected_revision,
                content,
            },
        })
        .await
    }

    async fn editor_tool(&mut self, request: EditorToolRequest) -> anyhow::Result<Value> {
        self.request(request).await
    }
}

/// One bounded request waiting for the editor main loop to produce a result.
#[derive(Debug)]
pub struct PendingEditorTool {
    /// Request to execute on the editor owner task.
    pub request: EditorToolRequest,
    /// One-shot result channel back to the Codex worker.
    pub response: oneshot::Sender<Result<Value, String>>,
}

/// Completed editor tool held briefly so its visible result can be followed.
#[derive(Debug)]
pub struct PendingEditorToolResponse {
    pub response: oneshot::Sender<Result<Value, String>>,
    pub result: Result<Value, String>,
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
            "Atomically apply up to 128 non-overlapping, half-open UTF-16 text edits through Red and save the file, creating missing parent directories.",
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
                            "jump_back", "jump_forward", "next_buffer", "previous_buffer",
                            "next_annotation", "previous_annotation",
                            "next_overlapping_annotation", "previous_overlapping_annotation"
                        ]
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        ),
        (
            "create_directory",
            "Create a directory and missing parents inside the workspace. Existing directories are accepted. Does not open or change a buffer.",
            json!({"type": "object", "properties": {"path": {"type": "string", "minLength": 1}}, "required": ["path"], "additionalProperties": false}),
        ),
        (
            "add_annotations",
            "Add source-linked annotation cards without changing file contents. Read the file first and use its current revision. Lines are zero-based and inclusive. Each returned annotation includes a stable ID and canonical href for linking to the card from Agent Markdown.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "expected_revision": {"type": "integer", "minimum": 0},
                    "annotations": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_AGENT_ANNOTATIONS_PER_CALL,
                        "items": {
                            "type": "object",
                            "properties": {
                                "start_line": {"type": "integer", "minimum": 0},
                                "end_line": {"type": "integer", "minimum": 0, "description": "Inclusive end; omit for one line."},
                                "message": {"type": "string", "minLength": 1, "maxLength": crate::inline_assist::MAX_COMMENT_BYTES}
                            },
                            "required": ["start_line", "message"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "expected_revision", "annotations"],
                "additionalProperties": false
            }),
        ),
        (
            "dismiss_annotations",
            "Hide source annotation cards by stable ID. This never edits source or deletes retained conversations.",
            json!({
                "type": "object",
                "properties": {
                    "annotation_ids": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_AGENT_ANNOTATIONS_PER_CALL,
                        "items": {"type": "string", "format": "uuid"}
                    }
                },
                "required": ["annotation_ids"],
                "additionalProperties": false
            }),
        ),
    ];
    let mut tools = definitions
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
        .collect::<Vec<_>>();
    let lsp_definitions = [
        ("lsp_status", "Inspect a file's language-server status and supported operations. Does not start a server. Use read_file first to open and synchronize a document.", json!({"path": {"type": "string"}}), vec!["path"]),
        ("lsp_diagnostics", "Read bounded diagnostics for a file, open buffers, or known workspace reports. Workspace results are not a complete project check. For refresh, scope must be file and read_file must have opened it. wait_ms (0..20000) waits for a new push-only report; it never saves. Continue using expected_generation; restart at offset 0 if it changes.", json!({
            "scope": {"type": "string", "enum": ["file", "open_buffers", "workspace"]},
            "path": {"type": ["string", "null"]},
            "severity": {"type": ["integer", "null"], "enum": [1, 2, 3, 4, null]},
            "source": {"type": ["string", "null"]}, "code": {"type": ["string", "null"]},
            "range": {"anyOf": [{"type": "null"}, {"type": "object", "properties": {"start": position, "end": position}, "required": ["start", "end"], "additionalProperties": false}]},
            "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 100},
            "expected_generation": {"type": ["integer", "null"], "minimum": 0},
            "refresh": {"type": "boolean"}, "wait_ms": {"type": "integer", "minimum": 0, "maximum": 20000}
        }), vec!["scope", "path", "severity", "source", "code", "range", "offset", "limit", "expected_generation", "refresh", "wait_ms"]),
        ("lsp_prepare_rename", "Check whether an already-read symbol can be renamed. Positions are zero-based UTF-16. An unsupported prepare operation does not necessarily mean rename is unsupported.", json!({"path": {"type": "string"}, "position": position, "expected_revision": {"type": "integer", "minimum": 0}}), vec!["path", "position", "expected_revision"]),
        ("lsp_preview_rename", "Preview a semantic rename across workspace files. Read the source first and pass its revision. Returns a bounded diff and a session-owned plan_id valid for 120 seconds while captured documents remain unchanged. Does not edit or save.", json!({"path": {"type": "string"}, "position": position, "expected_revision": {"type": "integer", "minimum": 0}, "new_name": {"type": "string", "minLength": 1, "maxLength": 256}}), vec!["path", "position", "expected_revision", "new_name"]),
        ("lsp_apply_edit", "Apply a previously returned rename plan once, after revalidating all targets. Updates visible buffers and undo history but NEVER saves files. Report unsaved changes to the user. Expired or stale plans require a new preview.", json!({"plan_id": {"type": "string"}}), vec!["plan_id"]),
    ];
    for (name, description, properties, required) in lsp_definitions {
        let mut tool = json!({"type": "function", "name": name, "description": description});
        tool[schema_key] = json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false});
        tools.push(tool);
    }
    tools
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
            assert_eq!(tools.len(), 13);
            assert!(tools
                .iter()
                .all(|tool| tool[schema_key]["additionalProperties"] == false));
            assert_eq!(tools[3][schema_key]["properties"]["edits"]["maxItems"], 128);
            assert_eq!(
                tools[6][schema_key]["properties"]["annotations"]["maxItems"],
                MAX_AGENT_ANNOTATIONS_PER_CALL
            );
            assert_eq!(
                tools[7][schema_key]["properties"]["annotation_ids"]["maxItems"],
                MAX_AGENT_ANNOTATIONS_PER_CALL
            );
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
        assert_eq!(
            EditorToolCall::parse("create_directory", json!({"path":"go/examples"})).unwrap(),
            EditorToolCall::CreateDirectory {
                path: "go/examples".to_string()
            }
        );
        assert!(
            EditorToolCall::parse("create_directory", json!({"path":"go", "recursive":true}))
                .is_err()
        );
        assert!(EditorToolCall::parse("run_editor_action", json!({"action": "quit"})).is_err());
        assert!(EditorToolCall::parse("get_editor_state", json!({"extra": true})).is_err());
        assert!(
            EditorToolCall::parse("open_file", json!({"path": "main.rs", "tool": "quit"})).is_err()
        );
        assert_eq!(
            EditorToolCall::parse(
                "add_annotations",
                json!({
                    "path": "main.rs",
                    "expected_revision": 3,
                    "annotations": [{"start_line": 1, "message": "Review this."}]
                })
            )
            .unwrap(),
            EditorToolCall::AddAnnotations {
                path: "main.rs".to_string(),
                expected_revision: 3,
                annotations: vec![EditorAnnotationInput {
                    start_line: 1,
                    end_line: None,
                    message: "Review this.".to_string(),
                }],
            }
        );
        assert!(EditorToolCall::parse(
            "dismiss_annotations",
            json!({"annotation_ids": ["id"], "delete_history": true})
        )
        .is_err());
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
