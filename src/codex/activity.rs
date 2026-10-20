//! Bounded, human-readable activity derived from app-server item lifecycles.

use serde_json::{json, Value};
use std::path::Path;

/// Convert only recognized items; never forward raw arguments or file contents.
pub(super) fn item_update(item: &Value, completed: bool, cwd: &Path) -> Option<Value> {
    match item["type"].as_str()? {
        "reasoning" if !completed => {
            return Some(json!({"session_update": "agent_thought_chunk"}));
        }
        "dynamicToolCall" => {}
        _ => return None,
    }
    let id = item["id"].as_str().filter(|id| !id.is_empty())?;
    let tool = item["tool"].as_str().unwrap_or("tool");
    let arguments = &item["arguments"];
    let full_path = label(arguments["path"].as_str().unwrap_or("file"), 2048);
    let path = compact_path(&full_path, cwd);
    let (kind, title) = tool_title(tool, arguments, &path);
    let (_, full_title) = tool_title(tool, arguments, &full_path);
    let content_text = item["contentItems"]
        .as_array()
        .and_then(|items| items.iter().find_map(|content| content["text"].as_str()));
    let status = if !completed {
        "in_progress"
    } else if item["success"].as_bool() == Some(false) || item["status"].as_str() == Some("failed")
    {
        "failed"
    } else if matches!(item["status"].as_str(), Some("cancelled" | "declined")) {
        "cancelled"
    } else {
        "completed"
    };
    let detail = if status == "failed" {
        content_text
            .map(|text| label(text, 2048))
            .unwrap_or_else(|| "Tool failed".to_string())
    } else if completed {
        item["durationMs"]
            .as_u64()
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some(json!({
        "session_update": if completed { "tool_call_update" } else { "tool_call" },
        "tool_call_id": id,
        "kind": kind,
        "title": label(&title, 72),
        "full_title": full_title,
        "status": status,
        "detail": detail,
    }))
}

fn tool_title(tool: &str, arguments: &Value, path: &str) -> (&'static str, String) {
    match tool {
        "list_files" => ("list", "Listing workspace files".to_string()),
        "search_files" => (
            "search",
            format!(
                "Searching for {}",
                label(arguments["query"].as_str().unwrap_or("text"), 80)
            ),
        ),
        "read_file" => ("read", format!("Reading {path}")),
        "write_file" | "apply_edits" => ("edit", format!("Editing {path}")),
        "create_directory" => (
            "create",
            format!("Creating {}/", path.trim_end_matches('/')),
        ),
        "get_editor_state" => ("inspect", "Inspecting editor state".to_string()),
        "open_file" => ("inspect", format!("Opening {path}")),
        "select_text" => ("inspect", format!("Selecting text in {path}")),
        "run_editor_action" => (
            "inspect",
            format!(
                "Running editor action {}",
                label(arguments["action"].as_str().unwrap_or(""), 80)
            ),
        ),
        "lsp_status" => (
            "inspect",
            format!("Checking language-server status for {path}"),
        ),
        "lsp_diagnostics" => match arguments["scope"].as_str().unwrap_or("workspace") {
            "file" => ("diagnostics", format!("Checking diagnostics for {path}")),
            "open_buffers" => (
                "diagnostics",
                "Checking diagnostics for open buffers".to_string(),
            ),
            _ => ("diagnostics", "Checking workspace diagnostics".to_string()),
        },
        "lsp_prepare_rename" => ("rename", format!("Checking rename support for {path}")),
        "lsp_preview_rename" => ("rename", format!("Previewing rename in {path}")),
        "lsp_apply_edit" => ("edit", "Applying language-server edits".to_string()),
        _ => ("tool", format!("Running {}", label(tool, 80))),
    }
}

fn compact_path(path: &str, cwd: &Path) -> String {
    let path = Path::new(path);
    let display = path.strip_prefix(cwd).unwrap_or_else(|_| {
        // Rooted paths need shortening even without a Windows drive prefix.
        if path.has_root() {
            path.file_name().map(Path::new).unwrap_or(path)
        } else {
            path
        }
    });
    let text = display.to_string_lossy();
    if text.chars().count() <= 52 {
        text.into_owned()
    } else {
        let tail: String = text.chars().rev().take(49).collect();
        format!("…{}", tail.chars().rev().collect::<String>())
    }
}

fn label(text: &str, limit: usize) -> String {
    let mut chars = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch });
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_lifecycle_is_bounded_and_does_not_expose_file_contents() {
        let item = json!({"id":"call", "type":"dynamicToolCall", "tool":"write_file",
            "arguments":{"path":"src/main.rs\nnext", "content":"private file contents"},
            "status":"completed", "success":true, "durationMs":12,
            "contentItems":[{"text":"private returned contents"}]});
        let start = item_update(&item, false, Path::new("/workspace")).unwrap();
        assert_eq!(start["title"], "Editing src/main.rs next");
        assert_eq!(start["kind"], "edit");
        assert_eq!(start["status"], "in_progress");
        let end = item_update(&item, true, Path::new("/workspace")).unwrap();
        assert_eq!(end["tool_call_id"], "call");
        assert_eq!(end["detail"], "12 ms");
        assert!(!end.to_string().contains("private"));
    }

    #[test]
    fn failures_and_reasoning_have_safe_presentations() {
        let item = json!({"id":"call", "type":"dynamicToolCall", "tool":"read_file",
            "success":false, "contentItems":[{"text":"é".repeat(3000)}]});
        let update = item_update(&item, true, Path::new("/workspace")).unwrap();
        assert_eq!(update["status"], "failed");
        assert_eq!(update["detail"].as_str().unwrap().chars().count(), 2049);
        assert_eq!(
            item_update(
                &json!({"type":"reasoning", "text":"private"}),
                false,
                Path::new("/workspace")
            ),
            Some(json!({"session_update":"agent_thought_chunk"}))
        );
        assert!(item_update(&json!({"type":"unknown"}), false, Path::new("/workspace")).is_none());
    }

    #[test]
    fn transport_failures_include_the_domain_error_detail() {
        let item = json!({
            "id": "rename",
            "type": "dynamicToolCall",
            "tool": "lsp_apply_edit",
            "status": "completed",
            "success": false,
            "contentItems": [{
                "text": serde_json::to_string(&json!({
                    "ok": false,
                    "status": "not_ready",
                    "message": "language server is not ready; retry"
                })).unwrap()
            }]
        });

        let update = item_update(&item, true, Path::new("/workspace")).unwrap();
        assert_eq!(update["status"], "failed");
        assert!(update["detail"].as_str().unwrap().contains("not ready"));
    }

    #[test]
    fn paths_are_compact_but_inspectable() {
        let cwd = Path::new("/workspace/project");
        for (path, expected) in [
            ("/workspace/project/src/main.rs", "Reading src/main.rs"),
            (
                "/Users/someone/.codex/memories/MEMORY.md",
                "Reading MEMORY.md",
            ),
        ] {
            let item = json!({"id":"call", "type":"dynamicToolCall", "tool":"read_file", "arguments":{"path":path}});
            let update = item_update(&item, false, cwd).unwrap();
            assert_eq!(update["title"], expected);
            assert_eq!(update["full_title"], format!("Reading {path}"));
        }
        assert_eq!(compact_path("src/main.rs", cwd), "src/main.rs");
        assert_eq!(compact_path("/outside/main.rs", cwd), "main.rs");
    }

    #[cfg(windows)]
    #[test]
    fn windows_activity_paths_keep_native_relative_paths_and_shorten_other_roots() {
        let cwd = Path::new(r"C:\workspace\project");
        for (path, expected) in [
            (r"C:\workspace\project\src\main.rs", r"src\main.rs"),
            (r"D:\other\main.rs", "main.rs"),
            (r"\other\main.rs", "main.rs"),
            (r"\\server\share\main.rs", "main.rs"),
            (r"src\main.rs", r"src\main.rs"),
            (r"C:main.rs", "C:main.rs"),
        ] {
            assert_eq!(compact_path(path, cwd), expected, "{path}");
        }
    }

    #[test]
    fn directory_creation_has_a_compact_activity_label() {
        let item = json!({"id":"mkdir", "type":"dynamicToolCall", "tool":"create_directory",
            "arguments":{"path":"/workspace/go/"}});
        let update = item_update(&item, false, Path::new("/workspace")).unwrap();
        assert_eq!(update["title"], "Creating go/");
        assert_eq!(update["kind"], "create");
        assert_eq!(update["full_title"], "Creating /workspace/go/");
    }

    #[test]
    fn lsp_tools_have_explicit_activity_titles_and_kinds() {
        let cases = [
            (
                "lsp_status",
                json!({"path":"src/main.rs"}),
                "inspect",
                "Checking language-server status for src/main.rs",
            ),
            (
                "lsp_diagnostics",
                json!({"scope":"file", "path":"src/main.rs"}),
                "diagnostics",
                "Checking diagnostics for src/main.rs",
            ),
            (
                "lsp_prepare_rename",
                json!({"path":"src/main.rs"}),
                "rename",
                "Checking rename support for src/main.rs",
            ),
            (
                "lsp_preview_rename",
                json!({"path":"src/main.rs", "position":{"line":4,"character":2}, "new_name":"private_symbol"}),
                "rename",
                "Previewing rename in src/main.rs",
            ),
            (
                "lsp_apply_edit",
                json!({"plan_id":"private-plan"}),
                "edit",
                "Applying language-server edits",
            ),
        ];

        for (tool, arguments, kind, title) in cases {
            let item = json!({
                "id": "call",
                "type": "dynamicToolCall",
                "tool": tool,
                "arguments": arguments,
            });
            let update = item_update(&item, false, Path::new("/workspace")).unwrap();
            assert_eq!(update["kind"], kind, "{tool}");
            assert_eq!(update["title"], title, "{tool}");
            assert!(!update.to_string().contains("private"), "{tool}");
        }
    }

    #[test]
    fn diagnostics_activity_titles_describe_the_requested_scope() {
        for (arguments, title) in [
            (
                json!({"scope":"open_buffers"}),
                "Checking diagnostics for open buffers",
            ),
            (
                json!({"scope":"workspace"}),
                "Checking workspace diagnostics",
            ),
        ] {
            let item = json!({
                "id": "diagnostics",
                "type": "dynamicToolCall",
                "tool": "lsp_diagnostics",
                "arguments": arguments,
            });
            let update = item_update(&item, false, Path::new("/workspace")).unwrap();
            assert_eq!(update["title"], title);
            assert_eq!(update["kind"], "diagnostics");
        }
    }
}
