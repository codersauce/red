//! Strict editor-tool contract shared by Red and Codex dynamic tools.

use std::{ffi::OsStr, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot};

/// Maximum number of edits accepted in one atomic proposal operation.
pub const MAX_EDITOR_EDITS: usize = 128;

/// Returns whether a workspace path names credentials or other sensitive data.
pub(crate) fn agent_path_is_sensitive(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return true;
    };
    let name = name.to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.contains("secret")
        || name.contains("credential")
        || matches!(name.as_str(), "id_rsa" | "id_ed25519")
        || matches!(
            path.extension()
                .and_then(OsStr::to_str)
                .map(|extension| extension.to_ascii_lowercase())
                .as_deref(),
            Some("pem" | "key" | "p12" | "pfx")
        )
}

/// Evaluates workspace-local ignore files for an agent-visible absolute path.
pub(crate) fn agent_path_is_ignored(path: &Path, root: &Path) -> bool {
    let mut ignored = false;
    let mut directories = path
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .take_while(|directory| directory.starts_with(root))
        .collect::<Vec<_>>();
    directories.reverse();
    for directory in directories {
        for name in [".gitignore", ".ignore"] {
            let (matcher, _) = ignore::gitignore::Gitignore::new(directory.join(name));
            match matcher.matched_path_or_any_parents(path, /* is_dir */ false) {
                ignore::Match::Ignore(_) => ignored = true,
                ignore::Match::Whitelist(_) => ignored = false,
                ignore::Match::None => {}
            }
        }
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    builder.add(root.join(".git/info/exclude"));
    if let Ok(exclude) = builder.build() {
        match exclude.matched_path_or_any_parents(path, /* is_dir */ false) {
            ignore::Match::Ignore(_) => ignored = true,
            ignore::Match::Whitelist(_) => ignored = false,
            ignore::Match::None => {}
        }
    }
    ignored
}

/// Applies the same fail-closed path-disclosure policy to every Red agent tool.
pub(crate) fn ensure_agent_path_disclosable(root: &Path, path: &Path) -> anyhow::Result<()> {
    let full_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let relative = full_path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("agent path is outside the workspace"))?;
    anyhow::ensure!(
        relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "agent path contains an unsafe workspace component"
    );
    anyhow::ensure!(
        !agent_path_is_sensitive(&full_path),
        "agent cannot disclose a sensitive file"
    );
    anyhow::ensure!(
        !agent_path_is_ignored(&full_path, root),
        "agent cannot disclose an ignored file"
    );
    Ok(())
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
    fn sensitive_agent_paths_fail_closed() {
        let workspace = tempfile::tempdir().unwrap();

        for name in [
            ".env",
            ".env.local",
            "service-secret.json",
            "credentials.json",
            "id_rsa",
            "id_ed25519",
            "certificate.pem",
            "private.key",
            "identity.p12",
            "identity.pfx",
        ] {
            let error = ensure_agent_path_disclosable(workspace.path(), Path::new(name))
                .expect_err("sensitive agent paths must be rejected");
            assert!(error.to_string().contains("sensitive"), "{name}: {error}");
        }

        ensure_agent_path_disclosable(workspace.path(), Path::new("src/main.rs")).unwrap();
    }

    #[test]
    fn agent_paths_honor_workspace_and_nested_ignore_rules() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(workspace.path().join(".ignore"), "local.rs\n").unwrap();
        let nested = workspace.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join(".gitignore"), "blocked.rs\n").unwrap();

        for path in ["ignored.rs", "local.rs", "nested/blocked.rs"] {
            let error = ensure_agent_path_disclosable(workspace.path(), Path::new(path))
                .expect_err("ignored agent paths must be rejected");
            assert!(error.to_string().contains("ignored"), "{path}: {error}");
        }

        ensure_agent_path_disclosable(workspace.path(), Path::new("nested/included.rs")).unwrap();
    }

    #[test]
    fn agent_paths_honor_git_info_exclude() {
        let workspace = tempfile::tempdir().unwrap();
        let info = workspace.path().join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("exclude"), "machine-local.rs\n").unwrap();

        assert!(
            ensure_agent_path_disclosable(workspace.path(), Path::new("machine-local.rs")).is_err()
        );
        ensure_agent_path_disclosable(workspace.path(), Path::new("shared.rs")).unwrap();
    }

    #[test]
    fn agent_paths_reject_workspace_escape_and_non_normal_components() {
        let workspace = tempfile::tempdir().unwrap();

        for path in [Path::new("../outside.rs"), Path::new("src/../outside.rs")] {
            assert!(ensure_agent_path_disclosable(workspace.path(), path).is_err());
        }

        let outside = tempfile::tempdir().unwrap();
        assert!(ensure_agent_path_disclosable(
            workspace.path(),
            &outside.path().join("outside.rs")
        )
        .is_err());
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
