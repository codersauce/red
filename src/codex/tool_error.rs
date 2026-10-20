//! Safe, actionable dynamic-tool errors returned through the Codex harness.

use serde_json::{json, Value};

/// Retain only labels that are safe and useful when an asynchronous call fails.
pub(super) fn context(arguments: &Value) -> Value {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .map(|path| json!({"path": bounded_label(path, 2048)}))
        .unwrap_or_else(|| json!({}))
}

pub(super) fn encode(tool: &str, arguments: &Value, message: impl Into<String>) -> String {
    let message = message.into();
    if serde_json::from_str::<Value>(&message)
        .ok()
        .is_some_and(|value| value["error"].is_object())
    {
        return message;
    }
    let code = classify(&message);
    let retryable = matches!(
        code,
        "invalid_argument" | "stale_revision" | "timeout" | "editor_conflict" | "limit_exceeded"
    );
    let recovery = match code {
        "invalid_argument" => "Correct the arguments and retry.",
        "stale_revision" => "Read the file again and retry from the new revision.",
        "timeout" => "Retry after the editor or language server becomes ready.",
        "editor_conflict" => "Read the current editor state before retrying.",
        "limit_exceeded" => "Narrow the request and retry.",
        "not_found" => "Check the workspace-relative path before retrying.",
        "outside_workspace" | "sensitive_path" => {
            "Choose a path allowed by the active Agent workspace policy."
        }
        "unsupported" => "Use another exposed Agent tool for this operation.",
        "cancelled" => "Start a new request if the operation is still needed.",
        _ => "Inspect the error and current editor state before retrying.",
    };
    let mut error = json!({
        "code": code,
        "tool": tool,
        "message": message,
        "retryable": retryable,
        "recovery": recovery,
    });
    if let Some(path) = arguments.get("path").and_then(Value::as_str) {
        error["path"] = json!(bounded_label(path, 2048));
    }
    json!({"ok": false, "error": error}).to_string()
}

pub(super) fn encoded_code(error: &str) -> Option<String> {
    serde_json::from_str::<Value>(error)
        .ok()?
        .pointer("/error/code")?
        .as_str()
        .map(str::to_string)
}

fn classify(message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    if message.contains("stale") || message.contains("revision changed") {
        "stale_revision"
    } else if message.contains("outside") && message.contains("workspace") {
        "outside_workspace"
    } else if message.contains("sensitive") || message.contains("restricted") {
        "sensitive_path"
    } else if message.contains("not found") || message.contains("does not exist") {
        "not_found"
    } else if message.contains("timed out")
        || message.contains("timeout")
        || message.contains("deadline")
    {
        "timeout"
    } else if message.contains("inactive")
        || message.contains("cancelled")
        || message.contains("canceled")
    {
        "cancelled"
    } else if message.contains("unsupported") {
        "unsupported"
    } else if message.contains("unsaved editor changes") || message.contains("changed on disk") {
        "editor_conflict"
    } else if message.contains("too large")
        || message.contains("exceeds")
        || message.contains("size limit")
        || message.contains("byte budget")
    {
        "limit_exceeded"
    } else if message.contains("invalid")
        || message.contains("must ")
        || message.contains("requires ")
        || message.contains("out of bounds")
    {
        "invalid_argument"
    } else {
        "operation_failed"
    }
}

fn bounded_label(text: &str, limit: usize) -> String {
    let mut chars = text.chars().map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    });
    let mut result = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_identify_the_tool_path_and_recovery() {
        let encoded = encode(
            "read_file",
            &json!({"path":"src/main.rs"}),
            "start_line must be >= 1; received 0",
        );
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert_eq!(value["error"]["tool"], "read_file");
        assert_eq!(value["error"]["path"], "src/main.rs");
        assert_eq!(value["error"]["retryable"], true);
        assert!(value["error"]["recovery"].as_str().is_some());
    }

    #[test]
    fn stale_revisions_and_policy_failures_have_distinct_codes() {
        for (message, code, retryable) in [
            (
                "editor revision changed during paged read",
                "stale_revision",
                true,
            ),
            ("path is outside workspace", "outside_workspace", false),
            ("editor tool request timed out", "timeout", true),
        ] {
            let value: Value =
                serde_json::from_str(&encode("read_file", &json!({}), message)).unwrap();
            assert_eq!(value["error"]["code"], code);
            assert_eq!(value["error"]["retryable"], retryable);
        }
    }

    #[test]
    fn asynchronous_context_discards_content_and_keeps_a_bounded_path() {
        let context = context(&json!({
            "path": "src/main.rs",
            "content": "private file contents",
            "query": "private search"
        }));
        assert_eq!(context, json!({"path": "src/main.rs"}));
        assert!(!context.to_string().contains("private"));
    }
}
