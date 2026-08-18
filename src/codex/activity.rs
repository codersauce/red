//! Bounded, human-readable activity derived from app-server item lifecycles.

use serde_json::{json, Value};

/// Convert only recognized items; never forward raw arguments or file contents.
pub(super) fn item_update(item: &Value, completed: bool) -> Option<Value> {
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
    let path = label(arguments["path"].as_str().unwrap_or("file"), 100);
    let (kind, title) = match tool {
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
        _ => ("tool", format!("Running {}", label(tool, 80))),
    };
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
        item["contentItems"]
            .as_array()
            .and_then(|items| items.iter().find_map(|content| content["text"].as_str()))
            .map(|text| label(text, 240))
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
        "title": title,
        "status": status,
        "detail": detail,
    }))
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
        let start = item_update(&item, false).unwrap();
        assert_eq!(start["title"], "Editing src/main.rs next");
        assert_eq!(start["kind"], "edit");
        assert_eq!(start["status"], "in_progress");
        let end = item_update(&item, true).unwrap();
        assert_eq!(end["tool_call_id"], "call");
        assert_eq!(end["detail"], "12 ms");
        assert!(!end.to_string().contains("private"));
    }

    #[test]
    fn failures_and_reasoning_have_safe_presentations() {
        let item = json!({"id":"call", "type":"dynamicToolCall", "tool":"read_file",
            "success":false, "contentItems":[{"text":"é".repeat(400)}]});
        let update = item_update(&item, true).unwrap();
        assert_eq!(update["status"], "failed");
        assert_eq!(update["detail"].as_str().unwrap().chars().count(), 241);
        assert_eq!(
            item_update(&json!({"type":"reasoning", "text":"private"}), false),
            Some(json!({"session_update":"agent_thought_chunk"}))
        );
        assert!(item_update(&json!({"type":"unknown"}), false).is_none());
    }
}
