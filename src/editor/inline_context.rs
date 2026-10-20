//! Snapshot inline reads on the editor owner, then perform I/O off the UI path.

use super::*;
use crate::{
    agent_tools::PendingEditorTool,
    inline_context::{
        resolve_path_with_policy, InlineContextCall, InlineContextSnapshot, VisibleText,
        MAX_FILE_BYTES, MAX_SNAPSHOT_BYTES,
    },
};

#[cfg(test)]
mod tests;

impl Editor {
    fn snapshot_inline_context(
        &self,
        provider: &str,
        request: &str,
        call: &InlineContextCall,
    ) -> anyhow::Result<InlineContextSnapshot> {
        // The worker can enqueue its first read before the editor has consumed
        // InlineSessionCreated. An unbound pending session is valid, but a
        // different bound provider or a completed request is never valid.
        anyhow::ensure!(
            self.inline_assist
                .iter()
                .chain(self.inline_jobs.values().map(|job| &job.session))
                .any(|session| session.request_id.as_deref() == Some(request)
                    && session
                        .session_id
                        .as_deref()
                        .is_none_or(|id| id == provider)),
            "inline context references an inactive provider"
        );
        let conversation = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| {
                conversation
                    .turns
                    .iter()
                    .any(|turn| turn.request_id == request)
            })
            .ok_or_else(|| anyhow::anyhow!("inline context conversation is unavailable"))?;
        anyhow::ensure!(
            conversation
                .turns
                .iter()
                .any(|turn| turn.request_id == request
                    && turn.state == InlineTurnState::Pending
                    && turn.session_id.as_deref().is_none_or(|id| id == provider)),
            "inline context references an inactive request"
        );
        let root = PathBuf::from(&conversation.cwd).canonicalize()?;
        let allow_sensitive_paths = self.config.agent.allow_sensitive_paths;
        let requested = call
            .path()
            .map(|path| {
                resolve_path_with_policy(&root, path, allow_sensitive_paths)
                    .map(|(_, relative)| relative)
            })
            .transpose()?;
        let mut snapshot = InlineContextSnapshot {
            root,
            visible: BTreeMap::new(),
            allow_sensitive_paths,
        };
        let mut remaining = MAX_SNAPSHOT_BYTES;
        for buffer in self.buffer_manager.iter() {
            let Some((_, relative)) = buffer.file.as_deref().and_then(|path| {
                resolve_path_with_policy(&snapshot.root, path, snapshot.allow_sensitive_paths).ok()
            }) else {
                continue;
            };
            if requested
                .as_ref()
                .is_some_and(|requested| requested != &relative)
            {
                continue;
            }
            let size = buffer.byte_len();
            let contents = if !matches!(call, InlineContextCall::ListFiles {})
                && size <= MAX_FILE_BYTES
                && size <= remaining
            {
                remaining -= size;
                Some(VisibleText {
                    content: buffer.contents(),
                    revision: buffer.revision(),
                    dirty: buffer.is_dirty(),
                })
            } else {
                None
            };
            snapshot.visible.insert(relative, contents);
        }
        Ok(snapshot)
    }

    pub(super) fn dispatch_inline_context_request(&self, pending: PendingEditorTool) {
        let EditorToolCall::InlineContext { request_id, call } = pending.request.call else {
            unreachable!("inline dispatcher requires an inline call")
        };
        let snapshot =
            self.snapshot_inline_context(&pending.request.session_id, &request_id, &call);
        tokio::spawn(async move {
            let result = match snapshot {
                Ok(snapshot) => snapshot.execute(call).await,
                Err(error) => Err(error),
            };
            let _ = pending
                .response
                .send(result.map_err(|error| error.to_string()));
        });
    }
}
