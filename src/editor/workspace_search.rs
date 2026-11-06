//! Schedules content searches and converts their byte locations to plugin picker rows.
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{PluginRequest, RequestId, Runtime};
use red_workspace_search::{Outcome, Query, SearchMatch, WorkspaceSearch};

#[derive(Debug, Deserialize)]
pub struct Options {
    #[serde(flatten)]
    pub query: Query,
    #[serde(default)]
    generation: i64,
    #[serde(default = "preview_default")]
    preview: bool,
}

fn preview_default() -> bool {
    true
}

#[derive(Default)]
pub(super) struct Service {
    engine: Arc<Mutex<WorkspaceSearch>>,
    pending: HashMap<RequestId, Pending>,
}

struct Pending {
    cancelled: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.task.abort();
    }
}

impl Service {
    pub(super) fn start(&mut self, runtime: Runtime, request_id: RequestId, options: Options) {
        let cwd = std::env::current_dir();
        let engine = self.engine.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let generation = options.generation;
        let task = tokio::spawn(async move {
            let mut engine = engine.lock_owned().await;
            let progress = runtime.clone();
            let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                let root = cwd?.canonicalize()?;
                let outcome =
                    engine.search_with_preview(&root, &options.query, &signal, |row| {
                        if let Some(item) = picker_item(&root, row, options.preview) {
                            progress.send_request(PluginRequest::WorkspaceSearchFirstMatch {
                                request_id,
                                item,
                            });
                        }
                    })?;
                Ok(result_payload(&root, generation, options.preview, outcome))
            })
            .await;
            let payload = match result {
                Ok(Ok(payload)) => payload,
                Ok(Err(err)) => error_payload(generation, err.to_string()),
                Err(err) => error_payload(generation, err.to_string()),
            };
            runtime.send_request(PluginRequest::WorkspaceContentSearched {
                request_id,
                payload,
            });
        });
        self.pending.insert(request_id, Pending { cancelled, task });
    }

    pub(super) fn remove(&mut self, request_id: RequestId) -> bool {
        self.pending.remove(&request_id).is_some()
    }
}

fn error_payload(generation: i64, error: String) -> Value {
    json!({"preview_enabled":false, "generation":generation, "root":"", "items":[], "total":0, "truncated":false, "error":error})
}

fn result_payload(root: &Path, generation: i64, preview: bool, outcome: Outcome) -> Value {
    let total = outcome.matches.len();
    let mut items = Vec::with_capacity(total);
    let mut bytes = 0;
    let mut truncated = outcome.truncated;
    let mut error = outcome.error.unwrap_or_default();
    for row in outcome.matches {
        let Some(item) = picker_item(root, &row, preview) else {
            error = "Some matching paths cannot be represented as UTF-8".to_owned();
            continue;
        };
        bytes += item.to_string().len();
        if bytes > 2 * 1024 * 1024 {
            truncated = true;
            break;
        }
        items.push(item);
    }
    json!({"preview_enabled":preview, "generation":generation, "root":root, "items":items, "total":total, "truncated":truncated, "error":error})
}

fn picker_item(root: &Path, row: &SearchMatch, preview: bool) -> Option<Value> {
    let path = root.join(&row.path);
    let path = path.to_str()?;
    let column = row.spans.first().map_or(0, |s| s[0]);
    let location =
        json!({"path":path, "line":row.line, "column":column, "column_encoding":"utf8-byte"});
    let stored = json!({"path":path, "line":row.line, "column":column, "matches":row.spans});
    let mut parent = row
        .path
        .parent()
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");
    if !parent.is_empty() {
        parent.push('/');
    }
    let label = row.path.file_name().unwrap_or_default().to_string_lossy();
    Some(json!({
        "id":format!("{path}:{}:{column}", row.line),
        "label":label, "kind":"FileMatch",
        "annotation":format!("{parent}:{}:{}", row.line + 1, column + 1),
        "detail":row.detail, "detail_matches":row.detail_matches,
        "data":{"location":location, "preview":stored},
        "preview":if preview {stored} else {Value::Null}
    }))
}
