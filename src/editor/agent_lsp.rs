//! Session-owned LSP queries and text-only rename plans.
//!
//! Requests are registered here and completed from the normal editor poll loop.
//! Disk snapshots and diff construction run off-thread. Only the editor owner
//! validates current buffer revisions and commits text; no tool here saves files.

use super::*;
use crate::agent_tools::{LspDiagnosticScope, PendingEditorTool};
use crate::lsp::{PreparedWorkspaceEdit, WorkspaceEditOperation};
use std::collections::BTreeSet;
use tokio::task::JoinHandle;

const REQUEST_DEADLINE: Duration = Duration::from_secs(20);
const PLAN_LIFETIME: Duration = Duration::from_secs(120);
const MAX_PENDING: usize = 8;
const MAX_PLANS: usize = 4;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 64;
const MAX_PREVIEW_BYTES: usize = 64 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 128 * 1024;

#[derive(Default)]
pub(super) struct AgentLsp {
    pending: HashMap<i64, PendingQuery>,
    workers: Vec<EditWorker>,
    plans: HashMap<String, EditPlan>,
    retired: BTreeSet<i64>,
    reports: HashMap<String, ReportStamp>,
    report_sequence: u64,
}

impl AgentLsp {
    pub(super) fn has_work(&self) -> bool {
        !self.pending.is_empty() || !self.workers.is_empty() || !self.plans.is_empty()
    }
}

#[derive(Clone)]
struct BufferSnapshot {
    id: BufferId,
    path: PathBuf,
    uri: String,
    revision: u64,
    version: Option<i64>,
    contents: ropey::Rope,
    dirty: bool,
}

struct QueryContext {
    session: String,
    turn_id: Option<String>,
    root: PathBuf,
    server_root: PathBuf,
    file: String,
    instance: u64,
    source: BufferId,
    snapshots: Vec<BufferSnapshot>,
    allow_sensitive_paths: bool,
}

struct PendingQuery {
    tool: PendingEditorTool,
    context: QueryContext,
    deadline: Instant,
    push_sequence: Option<u64>,
}

struct EditPlan {
    context: QueryContext,
    prepared: PreparedWorkspaceEdit,
    expires: Instant,
}

struct EditWorker {
    tool: PendingEditorTool,
    context: QueryContext,
    task: JoinHandle<anyhow::Result<(PreparedWorkspaceEdit, Value)>>,
    applying: bool,
    deadline: Instant,
}

struct ReportStamp {
    sequence: u64,
    revision: Option<u64>,
    received_at_ms: u128,
}

enum StartQuery {
    Immediate(Value),
    Pending {
        id: i64,
        context: QueryContext,
        wait: Duration,
        push_sequence: Option<u64>,
    },
    Apply(EditPlan),
}

fn result_status(status: &str, message: &str) -> Value {
    json!({"ok": status == "ok", "status": status, "message": message})
}

fn bounded_text(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end]
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

impl Editor {
    /// Starts an LSP tool without waiting, for production-loop integration tests.
    #[doc(hidden)]
    pub async fn test_start_agent_lsp_tool(
        &mut self,
        request: EditorToolRequest,
    ) -> tokio::sync::oneshot::Receiver<Result<Value, String>> {
        self.agent_manager
            .mark_session_active(request.session_id.clone());
        let (response, receiver) = tokio::sync::oneshot::channel();
        let mut render = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.dispatch_agent_lsp(
            PendingEditorTool { request, response },
            &mut render,
            &mut Runtime::new(),
        )
        .await;
        receiver
    }

    // Keep the large request future out of nested editor action/replay frames.
    pub(super) fn dispatch_agent_lsp<'a>(
        &'a mut self,
        tool: PendingEditorTool,
        render_buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.dispatch_agent_lsp_impl(tool, render_buffer, runtime))
    }

    async fn dispatch_agent_lsp_impl(
        &mut self,
        tool: PendingEditorTool,
        _render_buffer: &mut RenderBuffer,
        _runtime: &mut Runtime,
    ) {
        if tool.response.is_closed() {
            return;
        }
        match self.start_agent_lsp(&tool.request).await {
            Ok(StartQuery::Immediate(result)) => {
                let _ = tool.response.send(Ok(result));
            }
            Ok(StartQuery::Pending {
                id,
                context,
                wait,
                push_sequence,
            }) => {
                self.agent_lsp.pending.insert(
                    id,
                    PendingQuery {
                        tool,
                        context,
                        deadline: Instant::now() + wait,
                        push_sequence,
                    },
                );
            }
            Ok(StartQuery::Apply(plan)) => {
                let context = plan.context;
                let root = context.root.clone();
                let allow_sensitive = context.allow_sensitive_paths;
                let prepared = plan.prepared;
                let task = tokio::task::spawn_blocking(move || {
                    validate_prepared_paths(&prepared, &root, allow_sensitive)?;
                    // Even without resource operations this verifies pinned disk snapshots.
                    crate::lsp::apply_workspace_resource_operations(&prepared)?;
                    Ok((prepared, Value::Null))
                });
                self.agent_lsp.workers.push(EditWorker {
                    tool,
                    context,
                    task,
                    applying: true,
                    deadline: Instant::now() + REQUEST_DEADLINE,
                });
            }
            Err(error) => {
                let _ = tool.response.send(Err(error.to_string()));
            }
        }
    }

    async fn start_agent_lsp(&mut self, request: &EditorToolRequest) -> anyhow::Result<StartQuery> {
        anyhow::ensure!(
            self.agent_manager.is_session_active(&request.session_id),
            "inactive agent session"
        );
        let root = self
            .agent_manager
            .root()
            .ok_or_else(|| anyhow::anyhow!("no agent workspace is active"))?
            .to_path_buf();
        anyhow::ensure!(
            self.agent_lsp.pending.len() + self.agent_lsp.workers.len() < MAX_PENDING,
            "too many pending LSP tools"
        );
        if let EditorToolCall::LspApplyEdit { plan_id } = &request.call {
            let plan = self.agent_lsp.plans.get(plan_id).ok_or_else(|| {
                anyhow::anyhow!("unknown or expired LSP plan; request a new preview")
            })?;
            anyhow::ensure!(
                plan.context.session == request.session_id,
                "LSP plan belongs to another session"
            );
            anyhow::ensure!(
                Instant::now() < plan.expires,
                "LSP plan expired; request a new preview"
            );
            self.validate_agent_lsp_context(&plan.context, Some(&plan.prepared))?;
            return Ok(StartQuery::Apply(
                self.agent_lsp
                    .plans
                    .remove(plan_id)
                    .expect("validated plan"),
            ));
        }
        let path = match &request.call {
            EditorToolCall::LspStatus { path }
            | EditorToolCall::LspPrepareRename { path, .. }
            | EditorToolCall::LspPreviewRename { path, .. } => Some(path.as_str()),
            EditorToolCall::LspDiagnostics { path, .. } => path.as_deref(),
            _ => anyhow::bail!("not an LSP tool"),
        };
        let path = path
            .map(|path| {
                resolve_agent_tool_path_with_policy(
                    &root,
                    path,
                    self.config.agent.allow_sensitive_paths,
                )
            })
            .transpose()?;
        if let EditorToolCall::LspDiagnostics {
            scope,
            limit,
            severity,
            offset,
            expected_generation,
            refresh,
            wait_ms,
            range,
            ..
        } = &request.call
        {
            anyhow::ensure!((1..=100).contains(limit), "diagnostic limit must be 1..100");
            anyhow::ensure!(
                severity.is_none_or(|s| (1..=4).contains(&s)),
                "diagnostic severity must be 1..4"
            );
            anyhow::ensure!(*wait_ms <= 20_000, "diagnostic wait_ms must be 0..20000");
            if let Some(range) = range {
                anyhow::ensure!(
                    *scope == LspDiagnosticScope::File,
                    "diagnostic range requires file scope"
                );
                anyhow::ensure!(
                    (range.start.line, range.start.character)
                        <= (range.end.line, range.end.character),
                    "diagnostic range end precedes start"
                );
            }
            anyhow::ensure!(
                (*scope == LspDiagnosticScope::File) == path.is_some(),
                "only file scope requires a path"
            );
            anyhow::ensure!(
                !refresh || (*scope == LspDiagnosticScope::File && *offset == 0),
                "refresh requires file scope and offset 0"
            );
            anyhow::ensure!(
                *offset == 0 || expected_generation.is_some(),
                "diagnostic continuation requires expected_generation"
            );
            if let Some(generation) = expected_generation {
                anyhow::ensure!(
                    *generation == self.diagnostic_reports.generation(),
                    "diagnostics changed; restart at offset 0"
                );
            }
            if !refresh {
                return Ok(StartQuery::Immediate(self.agent_diagnostics_payload(
                    &request.call,
                    &root,
                    "cached",
                )?));
            }
        }
        let path = path.ok_or_else(|| anyhow::anyhow!("LSP operation requires a path"))?;
        let file = path.to_string_lossy().to_string();
        if matches!(request.call, EditorToolCall::LspStatus { .. }) {
            let caps = self
                .lsp
                .server_capabilities_for_file(&file)
                .map(serde_json::to_value)
                .transpose()?
                .unwrap_or(Value::Null);
            let supported = |name: &str| {
                caps.get(name)
                    .is_some_and(|value| value == true || value.is_object())
            };
            return Ok(StartQuery::Immediate(json!({
                "ok": true, "path": path.strip_prefix(&root).unwrap_or(&path),
                "status": self.lsp.server_status_for_file(&file),
                "server": self.lsp.server_name_for_file(&file),
                "workspace_root": self.lsp.workspace_root_for_file(&file),
                "capabilities": {
                    "rename": supported("renameProvider"),
                    "prepare_rename": caps["renameProvider"]["prepareProvider"] == true,
                    "pull_diagnostics": supported("diagnosticProvider"),
                    "hover": supported("hoverProvider"), "definition": supported("definitionProvider"),
                    "references": supported("referencesProvider"), "document_symbols": supported("documentSymbolProvider"),
                    "workspace_symbols": supported("workspaceSymbolProvider"), "code_actions": supported("codeActionProvider"),
                    "formatting": supported("documentFormattingProvider")
                }
            })));
        }
        let index = self.file_buffer_index(&path).ok_or_else(|| {
            anyhow::anyhow!("read_file must open this file before requesting LSP results")
        })?;
        if let EditorToolCall::LspPrepareRename {
            position,
            expected_revision,
            ..
        }
        | EditorToolCall::LspPreviewRename {
            position,
            expected_revision,
            ..
        } = &request.call
        {
            anyhow::ensure!(
                self.buffer_manager[index].revision() == *expected_revision,
                "stale editor revision; read the file again"
            );
            anyhow::ensure!(
                self.buffer_manager[index].byte_len() <= MAX_BYTES,
                "LSP source exceeds byte budget"
            );
            utf16_byte_offset(&self.buffer_manager[index].contents(), *position)?;
        }
        self.ensure_buffer_lsp_opened(index).await?;
        if self.lsp.server_status_for_file(&file) != "ready" {
            return Ok(StartQuery::Immediate(result_status(
                self.lsp.server_status_for_file(&file),
                "language server is not ready; inspect lsp_status and retry",
            )));
        }
        let caps = serde_json::to_value(self.lsp.server_capabilities_for_file(&file))?;
        anyhow::ensure!(
            caps["positionEncoding"].is_null() || caps["positionEncoding"] == "utf-16",
            "language server did not negotiate UTF-16 positions"
        );
        let server_root = self
            .lsp
            .workspace_root_for_file(&file)
            .ok_or_else(|| anyhow::anyhow!("language server has no workspace root"))?;
        let server = self.lsp.server_name_for_file(&file);
        // Flush all visible documents owned by this server before asking it to rename.
        let indices = self
            .buffer_manager
            .iter()
            .enumerate()
            .filter_map(|(i, buffer)| {
                let file = buffer.file.as_deref()?;
                resolve_agent_tool_path_with_policy(
                    &root,
                    file,
                    self.config.agent.allow_sensitive_paths,
                )
                .ok()?;
                (self.lsp.workspace_root_for_file(file).as_ref() == Some(&server_root)
                    && self.lsp.server_name_for_file(file) == server)
                    .then_some(i)
            })
            .collect::<Vec<_>>();
        for i in indices {
            anyhow::ensure!(
                self.buffer_manager[i].byte_len() <= MAX_BYTES,
                "open LSP document exceeds byte budget"
            );
            self.ensure_buffer_lsp_opened(i).await?;
            let buffer = &self.buffer_manager[i];
            if self.lsp_coordinator.last_notified_revision(buffer.id()) != Some(buffer.revision()) {
                self.lsp
                    .did_change(
                        buffer.file.as_deref().expect("named buffer"),
                        buffer.contents(),
                    )
                    .await?;
                self.lsp_coordinator
                    .record_notified_revision(buffer.id(), buffer.revision());
            }
        }
        let mut bytes = 0;
        let mut snapshots = Vec::new();
        for buffer in self.buffer_manager.iter() {
            let Some(file) = buffer.file.as_deref() else {
                continue;
            };
            let Ok(path) = resolve_agent_tool_path_with_policy(
                &root,
                file,
                self.config.agent.allow_sensitive_paths,
            ) else {
                continue;
            };
            bytes += buffer.byte_len();
            anyhow::ensure!(
                bytes <= MAX_BYTES,
                "open documents exceed LSP snapshot budget"
            );
            snapshots.push(BufferSnapshot {
                id: buffer.id(),
                uri: crate::lsp::file_uri(&path)?,
                path,
                revision: buffer.revision(),
                version: self.lsp.document_version(file),
                contents: buffer.contents_snapshot(),
                dirty: buffer.is_dirty(),
            });
        }
        let context = QueryContext {
            session: request.session_id.clone(),
            turn_id: self
                .agent_manager
                .turn_id(&request.session_id)
                .map(str::to_string),
            root,
            server_root,
            file: file.clone(),
            instance: self
                .lsp
                .server_instance_for_file(&file)
                .ok_or_else(|| anyhow::anyhow!("language server became unavailable"))?,
            source: self.buffer_manager[index].id(),
            snapshots,
            allow_sensitive_paths: self.config.agent.allow_sensitive_paths,
        };
        let uri = crate::lsp::file_uri(&path)?;
        let (method, params) = match &request.call {
            EditorToolCall::LspPrepareRename { position, .. } => {
                if caps["renameProvider"]["prepareProvider"] != true {
                    return Ok(StartQuery::Immediate(result_status(
                        "unsupported",
                        "server does not support prepareRename; check rename capability separately",
                    )));
                }
                (
                    "textDocument/prepareRename",
                    json!({"textDocument": {"uri": uri}, "position": position}),
                )
            }
            EditorToolCall::LspPreviewRename {
                position, new_name, ..
            } => {
                anyhow::ensure!(
                    !new_name.is_empty()
                        && new_name.len() <= 256
                        && !new_name.chars().any(char::is_control),
                    "invalid rename name"
                );
                if caps["renameProvider"] != true && !caps["renameProvider"].is_object() {
                    return Ok(StartQuery::Immediate(result_status(
                        "unsupported",
                        "server does not support rename",
                    )));
                }
                (
                    "textDocument/rename",
                    json!({"textDocument": {"uri": uri}, "position": position, "newName": new_name}),
                )
            }
            EditorToolCall::LspDiagnostics { wait_ms, .. } => {
                if !caps["diagnosticProvider"].is_object() {
                    if *wait_ms == 0 {
                        return Ok(StartQuery::Immediate(self.agent_diagnostics_payload(
                            &request.call,
                            &context.root,
                            "push_only",
                        )?));
                    }
                    let sequence = self
                        .agent_lsp
                        .reports
                        .get(&uri)
                        .map_or(0, |stamp| stamp.sequence);
                    return Ok(StartQuery::Pending {
                        id: -(crate::lsp::next_id() as i64),
                        context,
                        wait: Duration::from_millis(*wait_ms),
                        push_sequence: Some(sequence),
                    });
                }
                let mut params = json!({"textDocument": {"uri": uri}});
                if let Some(identifier) = caps["diagnosticProvider"].get("identifier") {
                    params["identifier"] = identifier.clone();
                }
                ("textDocument/diagnostic", params)
            }
            _ => unreachable!("validated query"),
        };
        let id = self
            .lsp
            .send_request_for_file(&file, method, params, false)
            .await?;
        anyhow::ensure!(id > 0, "language server is unavailable");
        Ok(StartQuery::Pending {
            id,
            context,
            wait: REQUEST_DEADLINE,
            push_sequence: None,
        })
    }

    fn validate_agent_lsp_context(
        &self,
        context: &QueryContext,
        prepared: Option<&PreparedWorkspaceEdit>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.agent_manager.is_session_active(&context.session),
            "inactive agent session"
        );
        anyhow::ensure!(
            self.agent_manager.turn_id(&context.session) == context.turn_id.as_deref(),
            "agent turn changed"
        );
        anyhow::ensure!(
            self.agent_manager.root() == Some(context.root.as_path()),
            "agent workspace changed"
        );
        anyhow::ensure!(
            self.config.agent.allow_sensitive_paths == context.allow_sensitive_paths,
            "agent access policy changed"
        );
        anyhow::ensure!(
            self.lsp.server_instance_for_file(&context.file) == Some(context.instance),
            "language server changed or stopped; request a new preview"
        );
        for snapshot in &context.snapshots {
            if snapshot.id != context.source
                && !prepared
                    .is_some_and(|edit| edit.documents.iter().any(|d| d.uri == snapshot.uri))
            {
                continue;
            }
            anyhow::ensure!(
                self.buffer_manager
                    .iter()
                    .any(|buffer| buffer.id() == snapshot.id
                        && buffer.revision() == snapshot.revision
                        && buffer
                            .file
                            .as_deref()
                            .is_some_and(|file| same_file_path(Path::new(file), &snapshot.path))),
                "stale LSP result: an affected buffer changed or closed; request a new preview"
            );
        }
        if let Some(prepared) = prepared {
            for document in &prepared.documents {
                let path = crate::lsp::normalized_file_path(&document.uri)?;
                if !context
                    .snapshots
                    .iter()
                    .any(|snapshot| snapshot.uri == document.uri)
                {
                    anyhow::ensure!(
                        self.file_buffer_index(Path::new(&path)).is_none(),
                        "LSP target opened after preview; request a new preview"
                    );
                }
            }
        }
        Ok(())
    }

    /// Consume only responses owned by an agent; leave UI/plugin requests alone.
    pub(super) fn handle_agent_lsp_message(&mut self, message: &InboundMessage) -> bool {
        let id = match message {
            InboundMessage::Message(response) => response.id,
            InboundMessage::RequestError { id, .. } => *id,
            InboundMessage::Error(error) => error.request.as_ref().map_or(0, |request| request.id),
            _ => return false,
        };
        if self.agent_lsp.retired.remove(&id) {
            return true;
        }
        let Some(pending) = self.agent_lsp.pending.remove(&id) else {
            return false;
        };
        if pending.tool.response.is_closed() {
            return true;
        }
        let result = self
            .validate_agent_lsp_context(&pending.context, None)
            .and_then(|()| {
                anyhow::ensure!(Instant::now() < pending.deadline, "LSP request timed out");
                match message {
                    InboundMessage::Message(response) => Ok(response.result.clone()),
                    InboundMessage::RequestError { error, .. } => {
                        Err(anyhow::anyhow!(error.to_string()))
                    }
                    InboundMessage::Error(error) => {
                        Err(anyhow::anyhow!(bounded_text(&error.message, 4096)))
                    }
                    _ => unreachable!(),
                }
            });
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let _ = pending.tool.response.send(Err(error.to_string()));
                return true;
            }
        };
        if matches!(
            pending.tool.request.call,
            EditorToolCall::LspPreviewRename { .. }
        ) {
            if result.is_null() {
                let _ = pending.tool.response.send(Ok(result_status(
                    "no_changes",
                    "server returned no rename edits",
                )));
                return true;
            }
            let snapshots = pending.context.snapshots.clone();
            let root = pending.context.root.clone();
            let server_root = pending.context.server_root.clone();
            let allow_sensitive = pending.context.allow_sensitive_paths;
            let task = tokio::task::spawn_blocking(move || {
                prepare_rename_preview(result, &root, &server_root, &snapshots, allow_sensitive)
            });
            self.agent_lsp.workers.push(EditWorker {
                tool: pending.tool,
                context: pending.context,
                task,
                applying: false,
                deadline: pending.deadline,
            });
            return true;
        }
        let result = if matches!(
            pending.tool.request.call,
            EditorToolCall::LspDiagnostics { .. }
        ) {
            (|| -> anyhow::Result<Value> {
                anyhow::ensure!(
                    result["kind"] == "full",
                    "expected a full diagnostic report (no previous result ID was sent)"
                );
                let diagnostics: Vec<Diagnostic> = serde_json::from_value(result["items"].clone())?;
                let uri = crate::lsp::file_uri(&pending.context.file)?;
                self.update_diagnostics(
                    Some(&uri),
                    &diagnostics,
                    diagnostics::DiagnosticReportKind::Pull,
                );
                self.agent_diagnostics_payload(
                    &pending.tool.request.call,
                    &pending.context.root,
                    "received",
                )
            })()
        } else {
            prepare_rename_payload(result, &pending.context)
        };
        let _ = pending
            .tool
            .response
            .send(result.map_err(|error| error.to_string()));
        true
    }

    pub(super) fn service_agent_lsp<'a>(
        &'a mut self,
        render: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.service_agent_lsp_impl(render, runtime))
    }

    async fn service_agent_lsp_impl(
        &mut self,
        render: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let now = Instant::now();
        let ids = self.agent_lsp.pending.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let pending = &self.agent_lsp.pending[&id];
            let invalid = self
                .validate_agent_lsp_context(&pending.context, None)
                .err();
            let received = pending.push_sequence.is_some_and(|sequence| {
                crate::lsp::file_uri(&pending.context.file)
                    .ok()
                    .and_then(|uri| self.agent_lsp.reports.get(&uri))
                    .is_some_and(|stamp| stamp.sequence > sequence)
            });
            if !pending.tool.response.is_closed()
                && invalid.is_none()
                && now < pending.deadline
                && !received
            {
                continue;
            }
            let pending = self.agent_lsp.pending.remove(&id).expect("pending query");
            let result = if let Some(error) = invalid {
                Err(error.to_string())
            } else if received {
                self.agent_diagnostics_payload(
                    &pending.tool.request.call,
                    &pending.context.root,
                    "received",
                )
                .map_err(|error| error.to_string())
            } else {
                Ok(result_status("timeout", "no fresh LSP result arrived before the deadline; this is not a clean diagnostic report"))
            };
            let _ = pending.tool.response.send(result);
            if id > 0 {
                self.agent_lsp.retired.insert(id);
                let _ = self
                    .lsp
                    .cancel_request_for_file(&pending.context.file, id)
                    .await;
            }
        }
        while self.agent_lsp.retired.len() > 256 {
            self.agent_lsp.retired.pop_first();
        }
        let workers = std::mem::take(&mut self.agent_lsp.workers);
        for worker in workers {
            if worker.tool.response.is_closed() || now >= worker.deadline {
                worker.task.abort();
                let _ = worker.tool.response.send(Ok(result_status(
                    "timeout",
                    "LSP edit preparation expired without applying changes",
                )));
                continue;
            }
            if !worker.task.is_finished() {
                self.agent_lsp.workers.push(worker);
                continue;
            }
            let result = async {
                let (prepared, mut preview) = worker.task.await??;
                self.validate_agent_lsp_context(&worker.context, Some(&prepared))?;
                if worker.applying {
                    self.apply_agent_lsp_plan(&worker.context, prepared, render, runtime)
                        .await
                } else {
                    anyhow::ensure!(
                        self.agent_lsp.plans.len() < MAX_PLANS,
                        "too many retained LSP previews; apply one or wait for expiry"
                    );
                    let id = uuid::Uuid::new_v4().to_string();
                    preview["plan_id"] = json!(id);
                    preview["expires_in_seconds"] = json!(PLAN_LIFETIME.as_secs());
                    self.agent_lsp.plans.insert(
                        id,
                        EditPlan {
                            context: worker.context,
                            prepared,
                            expires: Instant::now() + PLAN_LIFETIME,
                        },
                    );
                    Ok(preview)
                }
            }
            .await;
            let _ = worker
                .tool
                .response
                .send(result.map_err(|error: anyhow::Error| error.to_string()));
        }
        let expired = self
            .agent_lsp
            .plans
            .iter()
            .filter_map(|(id, plan)| {
                (now >= plan.expires
                    || self
                        .validate_agent_lsp_context(&plan.context, Some(&plan.prepared))
                        .is_err())
                .then_some(id.clone())
            })
            .collect::<Vec<_>>();
        for id in expired {
            self.agent_lsp.plans.remove(&id);
        }
        Ok(())
    }

    async fn apply_agent_lsp_plan(
        &mut self,
        context: &QueryContext,
        prepared: PreparedWorkspaceEdit,
        render: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<Value> {
        self.validate_agent_lsp_context(context, Some(&prepared))?;
        let receipts = prepared
            .documents
            .iter()
            .map(|document| {
                (
                    document.original_contents.as_str(),
                    document.contents.as_str(),
                )
            })
            .collect::<Vec<_>>();
        self.check_inline_agent_receipts_capacity(&context.session, &receipts)?;
        let original_index = self.buffer_manager.active_index();
        let view = (self.cx, self.cy, self.vtop, self.vleft, self.skipcol);
        let mut changed = Vec::new();
        // All targets and snapshots have been validated. Do not await between mutations.
        for document in prepared.documents {
            if !document.text_changed {
                continue;
            }
            let path = crate::lsp::file_path(&document.uri)?;
            let before = document.original_contents;
            let index = self.file_buffer_index(Path::new(&path)).unwrap_or_else(|| {
                self.buffer_manager
                    .push_buffer(Buffer::new(Some(path.clone()), before.clone()));
                self.buffer_manager.len() - 1
            });
            self.select_buffer_for_lsp_edit(index);
            if self.transaction_active() {
                self.commit_transaction(self.cursor_snapshot());
            }
            self.begin_transaction_with_origin(
                "LSP rename",
                EditOrigin::Agent {
                    session_id: context.session.clone(),
                    turn_id: self
                        .agent_manager
                        .turn_id(&context.session)
                        .unwrap_or("unattributed")
                        .to_string(),
                },
            );
            for batch in document.edit_batches {
                for edit in batch.into_iter().rev() {
                    let start = self.current_buffer().char_idx_to_position(edit.start);
                    let end = self.current_buffer().char_idx_to_position(edit.end);
                    self.replace_range(TextRange::new(start, end), &edit.new_text);
                }
            }
            self.commit_transaction(self.cursor_snapshot());
            if let Some(transaction) = self.current_buffer().undo_history.latest_transaction() {
                let transaction_id = transaction.id.clone();
                self.record_inline_agent_edit(
                    &context.session,
                    Path::new(&path),
                    false,
                    crate::inline_history::InlineAgentEdit::new(
                        before,
                        document.contents,
                        transaction_id,
                        false,
                    ),
                );
            }
            changed.push(index);
        }
        let mut files = Vec::new();
        let mut errors = Vec::new();
        for index in changed {
            self.select_buffer_for_lsp_edit(index);
            if let Err(error) = self.notify_change(runtime).await {
                errors.push(bounded_text(&error.to_string(), 1024));
            }
            let buffer = self.current_buffer();
            files.push(json!({"path": buffer.file.as_deref().and_then(|file| Path::new(file).strip_prefix(&context.root).ok()), "revision": buffer.revision(), "dirty": buffer.is_dirty()}));
        }
        self.select_buffer_for_lsp_edit(original_index);
        (self.cx, self.cy, self.vtop, self.vleft, self.skipcol) = view;
        self.check_bounds();
        self.sync_inline_change_summaries();
        if let Err(error) = self.render(render) {
            errors.push(bounded_text(&error.to_string(), 1024));
        }
        Ok(
            json!({"ok": errors.is_empty(), "applied": true, "saved": false, "files": files, "errors": errors, "message": "Rename applied to buffers. Files remain unsaved; undo is per file."}),
        )
    }

    pub(super) fn record_agent_diagnostic_report(&mut self, uri: &str) {
        self.agent_lsp.report_sequence = self.agent_lsp.report_sequence.wrapping_add(1);
        let revision = self
            .buffer_manager
            .iter()
            .find(|buffer| buffer.uri().ok().flatten().as_deref() == Some(uri))
            .map(Buffer::revision);
        self.agent_lsp.reports.insert(
            uri.to_string(),
            ReportStamp {
                sequence: self.agent_lsp.report_sequence,
                revision,
                received_at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            },
        );
    }

    fn agent_diagnostics_payload(
        &self,
        call: &EditorToolCall,
        root: &Path,
        refresh_status: &str,
    ) -> anyhow::Result<Value> {
        let EditorToolCall::LspDiagnostics {
            scope,
            path,
            severity,
            source,
            code,
            range,
            offset,
            limit,
            ..
        } = call
        else {
            unreachable!()
        };
        let requested = path
            .as_deref()
            .map(|path| {
                resolve_agent_tool_path_with_policy(
                    root,
                    path,
                    self.config.agent.allow_sensitive_paths,
                )
            })
            .transpose()?;
        let mut items = Vec::new();
        let mut documents = Vec::new();
        let mut uris = self.diagnostics.keys().cloned().collect::<BTreeSet<_>>();
        if let Some(path) = &requested {
            uris.insert(crate::lsp::file_uri(path)?);
        }
        let mut total = 0;
        let mut bytes = 0;
        for uri in uris {
            let Ok(path) = crate::lsp::file_path(&uri).and_then(|path| {
                resolve_agent_tool_path_with_policy(
                    root,
                    &path,
                    self.config.agent.allow_sensitive_paths,
                )
                .map_err(|error| crate::lsp::LspError::ProtocolError(error.to_string()))
            }) else {
                continue;
            };
            if requested
                .as_ref()
                .is_some_and(|requested| requested != &path)
            {
                continue;
            }
            let index = self.file_buffer_index(&path);
            if *scope == LspDiagnosticScope::OpenBuffers && index.is_none() {
                continue;
            }
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let stamp = self.agent_lsp.reports.get(&uri);
            let revision = index.map(|i| self.buffer_manager[i].revision());
            let freshness = if !self.diagnostics.contains_key(&uri) {
                "not_received"
            } else if self.diagnostic_reports.is_provisional(&uri) {
                "provisional"
            } else if let Some(stamp) = stamp {
                if stamp.revision != revision {
                    "stale"
                } else {
                    "unversioned"
                }
            } else {
                "not_received"
            };
            if documents.len() < 100 {
                documents.push(json!({"path": relative, "revision": revision, "freshness": freshness,
                    "received_at_ms": stamp.map(|stamp| stamp.received_at_ms), "observed_revision": stamp.and_then(|stamp| stamp.revision)}));
            }
            for diagnostic in self.diagnostics.get(&uri).into_iter().flatten() {
                if let Some(range) = range {
                    let start = (
                        diagnostic.range.start.line,
                        diagnostic.range.start.character,
                    );
                    let end = (diagnostic.range.end.line, diagnostic.range.end.character);
                    let filter_start = (range.start.line, range.start.character);
                    let filter_end = (range.end.line, range.end.character);
                    let intersects = if filter_start == filter_end {
                        start <= filter_start && filter_start <= end
                    } else if start == end {
                        filter_start <= start && start < filter_end
                    } else {
                        start < filter_end && filter_start < end
                    };
                    if !intersects {
                        continue;
                    }
                }
                let diagnostic_severity = serde_json::to_value(&diagnostic.severity)?;
                let diagnostic_code = serde_json::to_value(&diagnostic.code)?;
                if severity.is_some_and(|s| diagnostic_severity != json!(s))
                    || source
                        .as_ref()
                        .is_some_and(|s| diagnostic.source.as_ref() != Some(s))
                    || code.as_ref().is_some_and(|c| {
                        diagnostic_code
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| diagnostic_code.to_string())
                            != *c
                    })
                {
                    continue;
                }
                let ordinal = total;
                total += 1;
                if ordinal < *offset || items.len() >= *limit || bytes >= MAX_DIAGNOSTIC_BYTES {
                    continue;
                }
                let related = diagnostic.related_information.iter().flatten().take(8).filter_map(|info| {
                    let path = crate::lsp::file_path(&info.location.uri).ok()?;
                    let path = resolve_agent_tool_path_with_policy(root, &path, self.config.agent.allow_sensitive_paths).ok()?;
                    Some(json!({"path": path.strip_prefix(root).unwrap_or(&path), "range": info.location.range, "message": bounded_text(&info.message, 2048)}))
                }).collect::<Vec<_>>();
                let item = json!({"path": relative, "range": diagnostic.range, "severity": diagnostic_severity,
                    "code": diagnostic_code, "source": diagnostic.source.as_deref().map(|source| bounded_text(source, 256)),
                    "message": bounded_text(&diagnostic.message, 8192), "message_truncated": diagnostic.message.len() > 8192,
                    "tags": diagnostic.tags, "related_information": related, "freshness": freshness});
                let size = serde_json::to_vec(&item)?.len();
                anyhow::ensure!(
                    size <= MAX_DIAGNOSTIC_BYTES,
                    "one diagnostic exceeds the output budget"
                );
                if bytes + size > MAX_DIAGNOSTIC_BYTES {
                    bytes = MAX_DIAGNOSTIC_BYTES;
                    continue;
                }
                bytes += size;
                items.push(item);
            }
        }
        let next = offset.saturating_add(items.len());
        Ok(
            json!({"ok": true, "scope": scope, "coverage": "known_reports_only", "workspace_complete": false,
            "generation": self.diagnostic_reports.generation(), "refresh_status": refresh_status,
            "items": items, "total": total, "next_offset": (next < total).then_some(next), "truncated": next < total,
            "documents": documents, "document_metadata_truncated": self.diagnostics.len() > 100,
            "freshness_note": "Reports may be unversioned or provisional. Receipt does not prove all compiler or workspace checks completed."}),
        )
    }
}

fn prepare_rename_payload(result: Value, context: &QueryContext) -> anyhow::Result<Value> {
    if result.is_null() {
        return Ok(result_status(
            "not_renameable",
            "no renameable symbol at this position",
        ));
    }
    if result["defaultBehavior"] == true {
        return Ok(json!({"ok": true, "can_rename": true, "default_behavior": true}));
    }
    let range: crate::lsp::Range =
        serde_json::from_value(result.get("range").unwrap_or(&result).clone())?;
    let snapshot = context
        .snapshots
        .iter()
        .find(|s| s.id == context.source)
        .expect("source snapshot");
    let contents = snapshot.contents.to_string();
    let start = utf16_byte_offset(
        &contents,
        crate::agent_tools::EditorPosition {
            line: range.start.line,
            character: range.start.character,
        },
    )?;
    let end = utf16_byte_offset(
        &contents,
        crate::agent_tools::EditorPosition {
            line: range.end.line,
            character: range.end.character,
        },
    )?;
    anyhow::ensure!(start <= end, "invalid prepareRename range");
    Ok(
        json!({"ok": true, "can_rename": true, "range": range, "placeholder": bounded_text(result["placeholder"].as_str().unwrap_or(&contents[start..end]), 256), "revision": snapshot.revision}),
    )
}

fn validate_prepared_paths(
    prepared: &PreparedWorkspaceEdit,
    root: &Path,
    allow_sensitive: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        prepared.resource_operations.is_empty(),
        "agent LSP plans cannot create, rename, or delete files"
    );
    for document in &prepared.documents {
        resolve_agent_tool_path_with_policy(
            root,
            &crate::lsp::file_path(&document.uri)?,
            allow_sensitive,
        )?;
    }
    Ok(())
}

fn prepare_rename_preview(
    result: Value,
    root: &Path,
    server_root: &Path,
    snapshots: &[BufferSnapshot],
    allow_sensitive: bool,
) -> anyhow::Result<(PreparedWorkspaceEdit, Value)> {
    let operations = crate::lsp::workspace_edit_operations(&result)?;
    anyhow::ensure!(
        !operations.is_empty() && operations.len() <= MAX_DOCUMENTS,
        "rename must contain 1..64 text document operations"
    );
    for operation in &operations {
        let WorkspaceEditOperation::Document { edit } = operation else {
            anyhow::bail!("agent rename supports text edits only; resource operations require explicit editor handling");
        };
        let path = resolve_agent_tool_path_with_policy(
            root,
            &crate::lsp::file_path(&edit.uri)?,
            allow_sensitive,
        )?;
        anyhow::ensure!(
            path.starts_with(server_root),
            "rename target is outside the originating server workspace"
        );
    }
    let open = snapshots
        .iter()
        .enumerate()
        .map(|(index, snapshot)| crate::lsp::OpenWorkspaceDocument {
            index,
            uri: snapshot.uri.clone(),
            contents: snapshot.contents.to_string(),
            revision: snapshot.revision,
            version: snapshot.version,
            dirty: snapshot.dirty,
        })
        .collect();
    let revisions = snapshots
        .iter()
        .map(|snapshot| (snapshot.uri.clone(), snapshot.revision))
        .collect::<Vec<_>>();
    let prepared = crate::lsp::prepare_workspace_edit(&operations, &revisions, open, Some(root))?;
    anyhow::ensure!(
        prepared
            .documents
            .iter()
            .all(|document| !document.contents.contains('\0')),
        "rename cannot insert NUL bytes"
    );
    let bytes = prepared
        .documents
        .iter()
        .map(|d| d.original_contents.len() + d.contents.len())
        .sum::<usize>();
    anyhow::ensure!(bytes <= MAX_BYTES, "rename exceeds LSP preview byte budget");
    let mut preview = String::new();
    let mut files = Vec::new();
    let mut truncated = false;
    for document in &prepared.documents {
        if !document.text_changed {
            continue;
        }
        let path = crate::lsp::file_path(&document.uri)?;
        let path = Path::new(&path)
            .strip_prefix(root)?
            .to_string_lossy()
            .to_string();
        files.push(json!({"path": path, "edit_count": document.edit_batches.iter().map(Vec::len).sum::<usize>()}));
        let diff = similar::TextDiff::configure()
            .timeout(Duration::from_millis(200))
            .diff_lines(&document.original_contents, &document.contents);
        let diff = diff
            .unified_diff()
            .context_radius(3)
            .header(&path, &path)
            .to_string();
        let remaining = MAX_PREVIEW_BYTES.saturating_sub(preview.len());
        truncated |= diff.len() > remaining;
        preview.push_str(&bounded_text(&diff, remaining));
    }
    anyhow::ensure!(!files.is_empty(), "rename returned no text changes");
    let payload = json!({"ok": true, "applied": false, "saved": false, "files": files, "diff": preview, "diff_truncated": truncated});
    Ok((prepared, payload))
}
