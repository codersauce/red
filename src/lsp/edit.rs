//! LSP URI, UTF-16 range, text-edit, and workspace-operation conversion.
//!
//! Functions in this module convert protocol edits into buffer character ranges without
//! applying them. UTF-16 positions are checked against real scalar boundaries, edits are
//! ordered so earlier replacements cannot invalidate later ranges, and overlapping
//! operations are rejected.
//!
//! URI normalization is a protocol boundary, not a general filesystem authorization
//! check. Multi-file confinement, resource-operation safety, revisions, and rollback are
//! enforced by [`super::workspace_edit`].

use std::{collections::HashMap, path::Path};

use path_absolutize::Absolutize;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{LspError, Position, Range, TextEdit};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentEdit {
    pub uri: String,
    pub version: Option<i64>,
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceEditOperation {
    Document {
        edit: DocumentEdit,
    },
    Create {
        uri: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Rename {
        old_uri: String,
        new_uri: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Delete {
        uri: String,
        recursive: bool,
        ignore_if_not_exists: bool,
    },
}

impl WorkspaceEditOperation {
    pub fn document(&self) -> Option<&DocumentEdit> {
        match self {
            Self::Document { edit } => Some(edit),
            Self::Create { .. } | Self::Rename { .. } | Self::Delete { .. } => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceEdit {
    changes: Option<HashMap<String, Vec<TextEdit>>>,
    document_changes: Option<Vec<Value>>,
    change_annotations: Option<HashMap<String, ChangeAnnotation>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeAnnotation {
    #[serde(default)]
    needs_confirmation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentEdit {
    text_document: VersionedTextDocument,
    edits: Vec<TextEdit>,
}

#[derive(Deserialize)]
struct VersionedTextDocument {
    uri: String,
    version: Option<i64>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateFileOptions {
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    ignore_if_exists: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameFileOptions {
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    ignore_if_exists: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFileOptions {
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    ignore_if_not_exists: bool,
}

pub fn file_uri(path: impl AsRef<Path>) -> Result<String, LspError> {
    let path = path.as_ref().absolutize()?;
    let path = path.to_string_lossy();
    #[cfg(windows)]
    let path = {
        let path = path.replace('\\', "/");
        let path = path.strip_prefix("//?/").unwrap_or(&path).to_string();
        if path.starts_with("UNC/") || path.starts_with("//") {
            return Err(LspError::ProtocolError(
                "UNC paths are not supported as LSP document URIs".to_string(),
            ));
        }
        path
    };

    let mut uri = String::with_capacity(path.len() + 8);
    uri.push_str("file://");
    if !path.starts_with('/') {
        uri.push('/');
    }
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            uri.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            uri.push('%');
            uri.push(char::from(HEX[(byte >> 4) as usize]));
            uri.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    Ok(uri)
}

pub fn file_path(uri: &str) -> Result<String, LspError> {
    let path = uri
        .strip_prefix("file://")
        .ok_or_else(|| LspError::ProtocolError(format!("unsupported LSP document URI: {uri}")))?;
    let path = path.strip_prefix("localhost").unwrap_or(path);
    if !path.starts_with('/') {
        return Err(LspError::ProtocolError(format!(
            "unsupported LSP document URI authority: {uri}"
        )));
    }

    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let Some(value) = bytes
            .get(index + 1..index + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        else {
            return Err(LspError::ProtocolError(format!(
                "invalid percent escape in LSP document URI: {uri}"
            )));
        };
        decoded.push(value);
        index += 3;
    }

    let path = String::from_utf8(decoded).map_err(|_| {
        LspError::ProtocolError(format!("LSP document URI is not valid UTF-8: {uri}"))
    })?;
    #[cfg(windows)]
    let path = path
        .strip_prefix('/')
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
        .map(|path| path.replace('/', "\\"))
        .unwrap_or(path);
    Ok(path)
}

pub fn workspace_edit_operations(value: &Value) -> Result<Vec<WorkspaceEditOperation>, LspError> {
    let workspace_edit: WorkspaceEdit = serde_json::from_value(value.clone())?;
    if workspace_edit.changes.is_some() && workspace_edit.document_changes.is_some() {
        return Err(LspError::ProtocolError(
            "LSP workspace edit included both changes and documentChanges".to_string(),
        ));
    }
    if workspace_edit
        .change_annotations
        .as_ref()
        .is_some_and(|annotations| {
            annotations
                .values()
                .any(|annotation| annotation.needs_confirmation)
        })
    {
        return Err(LspError::ProtocolError(
            "LSP workspace edit requires change-annotation confirmation".to_string(),
        ));
    }

    if let Some(changes) = workspace_edit.changes {
        let mut documents = changes
            .into_iter()
            .map(|(uri, edits)| {
                file_path(&uri)?;
                Ok(WorkspaceEditOperation::Document {
                    edit: DocumentEdit {
                        uri,
                        version: None,
                        edits,
                    },
                })
            })
            .collect::<Result<Vec<_>, LspError>>()?;
        documents.sort_by(|left, right| {
            left.document()
                .map(|document| &document.uri)
                .cmp(&right.document().map(|document| &document.uri))
        });
        return Ok(documents);
    }

    workspace_edit
        .document_changes
        .unwrap_or_default()
        .into_iter()
        .map(|change| {
            if let Some(kind) = change.get("kind").and_then(Value::as_str) {
                return match kind {
                    "create" => {
                        let uri = required_uri(&change, "uri")?;
                        let options = optional_options::<CreateFileOptions>(&change)?;
                        Ok(WorkspaceEditOperation::Create {
                            uri,
                            overwrite: options.overwrite,
                            ignore_if_exists: options.ignore_if_exists,
                        })
                    }
                    "rename" => {
                        let old_uri = required_uri(&change, "oldUri")?;
                        let new_uri = required_uri(&change, "newUri")?;
                        let options = optional_options::<RenameFileOptions>(&change)?;
                        Ok(WorkspaceEditOperation::Rename {
                            old_uri,
                            new_uri,
                            overwrite: options.overwrite,
                            ignore_if_exists: options.ignore_if_exists,
                        })
                    }
                    "delete" => {
                        let uri = required_uri(&change, "uri")?;
                        let options = optional_options::<DeleteFileOptions>(&change)?;
                        Ok(WorkspaceEditOperation::Delete {
                            uri,
                            recursive: options.recursive,
                            ignore_if_not_exists: options.ignore_if_not_exists,
                        })
                    }
                    _ => Err(LspError::ProtocolError(format!(
                        "unsupported LSP workspace resource operation: {kind}"
                    ))),
                };
            }
            let change: TextDocumentEdit = serde_json::from_value(change)?;
            file_path(&change.text_document.uri)?;
            Ok(WorkspaceEditOperation::Document {
                edit: DocumentEdit {
                    uri: change.text_document.uri,
                    version: change.text_document.version,
                    edits: change.edits,
                },
            })
        })
        .collect()
}

pub fn workspace_edits(value: &Value) -> Result<Vec<DocumentEdit>, LspError> {
    workspace_edit_operations(value)?
        .into_iter()
        .map(|operation| match operation {
            WorkspaceEditOperation::Document { edit } => Ok(edit),
            WorkspaceEditOperation::Create { .. }
            | WorkspaceEditOperation::Rename { .. }
            | WorkspaceEditOperation::Delete { .. } => Err(LspError::ProtocolError(
                "LSP workspace resource operations require ordered application".to_string(),
            )),
        })
        .collect()
}

fn required_uri(value: &Value, field: &str) -> Result<String, LspError> {
    let uri = value.get(field).and_then(Value::as_str).ok_or_else(|| {
        LspError::ProtocolError(format!("LSP resource operation is missing {field}"))
    })?;
    file_path(uri)?;
    Ok(uri.to_string())
}

fn optional_options<T: for<'de> Deserialize<'de> + Default>(value: &Value) -> Result<T, LspError> {
    value
        .get("options")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(LspError::JsonError)
}

pub fn apply_text_edits(contents: &str, edits: &[TextEdit]) -> Result<String, LspError> {
    let mut starts = Vec::with_capacity(
        contents
            .as_bytes()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1,
    );
    starts.push(0);
    starts.extend(
        contents
            .as_bytes()
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
    );

    let mut ranges = edits
        .iter()
        .enumerate()
        .map(|(index, edit)| {
            let start = byte_offset(contents, &starts, edit.range.start)?;
            let end = byte_offset(contents, &starts, edit.range.end)?;
            if start > end {
                return Err(LspError::ProtocolError(format!(
                    "LSP text edit {index} ends before it starts"
                )));
            }
            Ok((start, end, index, edit.new_text.as_str()))
        })
        .collect::<Result<Vec<_>, LspError>>()?;
    ranges.sort_by_key(|(start, end, index, _)| (*start, *end, *index));

    if ranges.windows(2).any(|ranges| ranges[0].1 > ranges[1].0) {
        return Err(LspError::ProtocolError(
            "LSP text edits overlap".to_string(),
        ));
    }

    let mut result = contents.to_string();
    for (start, end, _, text) in ranges.into_iter().rev() {
        result.replace_range(start..end, text);
    }
    Ok(result)
}

pub fn text_edit_char_range(contents: &str, range: &Range) -> Result<(usize, usize), LspError> {
    let mut starts = Vec::with_capacity(
        contents
            .as_bytes()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1,
    );
    starts.push(0);
    starts.extend(
        contents
            .as_bytes()
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
    );
    let start = byte_offset(contents, &starts, range.start)?;
    let end = byte_offset(contents, &starts, range.end)?;
    if start > end {
        return Err(LspError::ProtocolError(
            "LSP text edit ends before it starts".to_string(),
        ));
    }
    Ok((
        contents[..start].chars().count(),
        contents[..end].chars().count(),
    ))
}

fn byte_offset(contents: &str, starts: &[usize], position: Position) -> Result<usize, LspError> {
    let Some(&start) = starts.get(position.line) else {
        return Err(LspError::ProtocolError(format!(
            "LSP position line {} is outside the document",
            position.line
        )));
    };
    let mut end = starts
        .get(position.line + 1)
        .copied()
        .unwrap_or(contents.len());
    if contents.as_bytes().get(end.saturating_sub(1)) == Some(&b'\n') {
        end -= 1;
    }
    if contents.as_bytes().get(end.saturating_sub(1)) == Some(&b'\r') {
        end -= 1;
    }

    let line = &contents[start..end];
    let mut units = 0;
    for (offset, character) in line.char_indices() {
        if units == position.character {
            return Ok(start + offset);
        }
        let next = units + character.len_utf16();
        if next > position.character {
            return Err(LspError::ProtocolError(format!(
                "LSP position {}:{} splits a UTF-16 character",
                position.line, position.character
            )));
        }
        units = next;
    }
    if units == position.character {
        return Ok(end);
    }
    Err(LspError::ProtocolError(format!(
        "LSP position {}:{} is outside its line",
        position.line, position.character
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::lsp::Range;

    fn edit(start: (usize, usize), end: (usize, usize), text: &str) -> TextEdit {
        TextEdit {
            range: Range {
                start: Position {
                    line: start.0,
                    character: start.1,
                },
                end: Position {
                    line: end.0,
                    character: end.1,
                },
            },
            new_text: text.to_string(),
        }
    }

    #[test]
    fn file_uris_roundtrip_reserved_and_unicode_paths() {
        let path = std::env::current_dir()
            .unwrap()
            .join("folder with spaces")
            .join("café #1%.rs");

        let uri = file_uri(&path).unwrap();

        assert!(uri.contains("folder%20with%20spaces/caf%C3%A9%20%231%25.rs"));
        assert_eq!(file_path(&uri).unwrap(), path.to_string_lossy().as_ref());
    }

    #[test]
    fn file_uris_reject_invalid_escapes_and_remote_authorities() {
        assert!(file_path("file:///tmp/bad%2.rs").is_err());
        assert!(file_path("file:///tmp/bad%GG.rs").is_err());
        assert!(file_path("file://remote/tmp/file.rs").is_err());
        assert!(file_path("untitled:notes").is_err());
        assert_eq!(
            file_path("file://localhost/tmp/a%20b.rs").unwrap(),
            "/tmp/a b.rs"
        );

        #[cfg(windows)]
        assert_eq!(
            file_path("file:///D:/folder%20with%20spaces/caf%C3%A9%20%231%25.rs").unwrap(),
            r"D:\folder with spaces\café #1%.rs"
        );
    }

    #[test]
    fn text_edits_apply_with_utf16_crlf_and_multiple_documents_lines() {
        let contents = "first\r\n👋 café\r\nlast";
        let edits = vec![
            edit((0, 5), (0, 5), "!"),
            edit((1, 3), (1, 7), "tea"),
            edit((2, 0), (2, 4), "done"),
        ];

        assert_eq!(
            apply_text_edits(contents, &edits).unwrap(),
            "first!\r\n👋 tea\r\ndone"
        );
    }

    #[test]
    fn text_edits_preserve_same_position_insert_order() {
        let edits = vec![edit((0, 1), (0, 1), "A"), edit((0, 1), (0, 1), "B")];

        assert_eq!(apply_text_edits("xy", &edits).unwrap(), "xABy");
    }

    #[test]
    fn text_edits_reject_overlap_invalid_ranges_and_split_surrogates() {
        assert!(apply_text_edits(
            "abcdef",
            &[edit((0, 1), (0, 4), "x"), edit((0, 3), (0, 5), "y")]
        )
        .is_err());
        assert!(apply_text_edits("abcdef", &[edit((0, 3), (0, 2), "x")]).is_err());
        assert!(apply_text_edits("👋", &[edit((0, 1), (0, 2), "x")]).is_err());
        assert!(apply_text_edits("a", &[edit((1, 0), (1, 0), "x")]).is_err());
    }

    #[test]
    fn workspace_changes_parse_and_sort_documents() {
        let value = json!({
            "changes": {
                "file:///tmp/z.rs": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "newText": "z" }],
                "file:///tmp/a%20b.rs": [{ "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 3 } }, "newText": "a" }]
            }
        });

        let documents = workspace_edits(&value).unwrap();

        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].uri, "file:///tmp/a%20b.rs");
        assert_eq!(documents[1].uri, "file:///tmp/z.rs");
        assert_eq!(documents[0].version, None);
    }

    #[test]
    fn workspace_document_changes_preserve_versions_and_annotations() {
        let value = json!({
            "documentChanges": [{
                "textDocument": { "uri": "file:///tmp/a.rs", "version": 7 },
                "edits": [{
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                    "newText": "a",
                    "annotationId": "safe-change"
                }]
            }]
        });

        let documents = workspace_edits(&value).unwrap();

        assert_eq!(documents[0].version, Some(7));
        assert_eq!(documents[0].edits[0].new_text, "a");
    }

    #[test]
    fn workspace_edits_reject_resource_operations_ambiguous_forms_and_non_file_uris() {
        assert!(workspace_edits(&json!({
            "documentChanges": [{ "kind": "rename", "oldUri": "file:///tmp/a.rs", "newUri": "file:///tmp/b.rs" }]
        }))
        .is_err());
        assert!(workspace_edits(&json!({ "changes": {}, "documentChanges": [] })).is_err());
        assert!(workspace_edits(&json!({ "changes": { "untitled:notes": [] } })).is_err());
    }

    #[test]
    fn workspace_operations_preserve_resource_order_and_options() {
        let value = json!({
            "documentChanges": [
                { "kind": "create", "uri": "file:///tmp/new%20file.rs", "options": { "overwrite": true } },
                {
                    "textDocument": { "uri": "file:///tmp/new%20file.rs", "version": null },
                    "edits": [{
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                        "newText": "fn main() {}"
                    }]
                },
                { "kind": "rename", "oldUri": "file:///tmp/new%20file.rs", "newUri": "file:///tmp/renamed.rs", "options": { "ignoreIfExists": true } },
                { "kind": "delete", "uri": "file:///tmp/old.rs", "options": { "ignoreIfNotExists": true } }
            ]
        });

        let operations = workspace_edit_operations(&value).unwrap();

        assert!(matches!(
            &operations[0],
            WorkspaceEditOperation::Create { uri, overwrite: true, ignore_if_exists: false }
                if uri == "file:///tmp/new%20file.rs"
        ));
        assert_eq!(
            operations[1]
                .document()
                .map(|document| document.uri.as_str()),
            Some("file:///tmp/new%20file.rs")
        );
        assert!(matches!(
            &operations[2],
            WorkspaceEditOperation::Rename { old_uri, new_uri, overwrite: false, ignore_if_exists: true }
                if old_uri == "file:///tmp/new%20file.rs" && new_uri == "file:///tmp/renamed.rs"
        ));
        assert!(matches!(
            &operations[3],
            WorkspaceEditOperation::Delete { uri, recursive: false, ignore_if_not_exists: true }
                if uri == "file:///tmp/old.rs"
        ));
    }

    #[test]
    fn workspace_operations_reject_unknown_kinds_missing_uris_and_bad_options() {
        assert!(workspace_edit_operations(&json!({
            "documentChanges": [{ "kind": "copy", "uri": "file:///tmp/a.rs" }]
        }))
        .is_err());
        assert!(workspace_edit_operations(&json!({
            "documentChanges": [{ "kind": "rename", "oldUri": "file:///tmp/a.rs" }]
        }))
        .is_err());
        assert!(workspace_edit_operations(&json!({
            "documentChanges": [{ "kind": "create", "uri": "file:///tmp/a.rs", "options": "overwrite" }]
        }))
        .is_err());
    }

    #[test]
    fn workspace_operations_reject_change_annotations_that_require_confirmation() {
        let value = json!({
            "documentChanges": [{
                "textDocument": { "uri": "file:///tmp/a.rs", "version": 1 },
                "edits": [{
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                    "newText": "value",
                    "annotationId": "dangerous"
                }]
            }],
            "changeAnnotations": {
                "dangerous": { "label": "Overwrite generated file", "needsConfirmation": true }
            }
        });

        let error = workspace_edit_operations(&value).unwrap_err();

        assert!(error.to_string().contains("confirmation"));
    }
}
