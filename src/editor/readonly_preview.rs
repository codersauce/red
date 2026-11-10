//! Read-only split previews opened by plugins.
//!
//! The buffer name is a label, not a path Red writes. Opening a preview suspends
//! a modal workspace so the split can be seen, and closing that window resumes it.

use serde_json::json;

use super::{render_buffer::RenderBuffer, Action, Editor, Mode};
use crate::{
    buffer::{Buffer, BufferId, SyntaxSelection},
    config::MarkdownLinkSplitFocus,
    plugin::RequestId,
    window::{WindowId, WindowManager},
};

const PREVIEW_HINT: &str = "read-only preview · :q returns to the diff";

#[derive(Clone)]
pub(super) struct PreviewPane {
    key: String,
    window: WindowId,
    buffer_id: BufferId,
}

pub(super) struct SplitPreview {
    pub key: String,
    pub name: String,
    pub text: String,
    pub line: usize,
    pub syntax: Option<String>,
    pub side: Option<String>,
}

impl Editor {
    pub(super) async fn open_readonly_split(
        &mut self,
        screen: &mut RenderBuffer,
        runtime: &mut crate::plugin::Runtime,
        request_id: RequestId,
        preview: SplitPreview,
    ) -> anyhow::Result<()> {
        if let Some(existing) = self
            .readonly_previews
            .iter()
            .find(|pane| pane.key == preview.key)
            .cloned()
        {
            let window_open = self.window_manager.window_index(existing.window).is_some();
            let buffer_open = self.preview_buffer_index(existing.buffer_id).is_some();
            if window_open && buffer_open {
                self.reuse_readonly_split(screen, runtime, request_id, &preview, existing)
                    .await?;
                return Ok(());
            }
            self.readonly_previews
                .retain(|pane| pane.window != existing.window);
        }

        let source = self.window_manager.active_stable_window_id();
        let mut buffer = Buffer::readonly_text(preview.name.clone(), preview.text.clone());
        self.apply_preview_syntax(&mut buffer, preview.syntax.as_deref());
        let buffer_id = buffer.id();
        self.buffer_manager.push_buffer(buffer);
        let buffer_index = self.buffer_manager.len() - 1;
        if !self.update_window_layout(|windows| windows.split_vertical(buffer_index)) {
            self.buffer_manager.pop_buffer();
            self.sync_with_window();
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    json!({ "error": "Cannot create a read-only preview split" }),
                )
                .await?;
            return Ok(());
        }
        let Some(destination) = self.window_manager.active_stable_window_id() else {
            self.buffer_manager.pop_buffer();
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    json!({ "error": "Cannot create a read-only preview split" }),
                )
                .await?;
            return Ok(());
        };
        self.reveal_preview_line(preview.line);
        self.readonly_previews.push(PreviewPane {
            key: preview.key,
            window: destination,
            buffer_id,
        });
        self.place_preview_focus(source, destination);
        self.finish_preview_open(
            screen,
            runtime,
            request_id,
            buffer_index,
            false,
            preview.side,
        )
        .await
    }

    async fn reuse_readonly_split(
        &mut self,
        screen: &mut RenderBuffer,
        runtime: &mut crate::plugin::Runtime,
        request_id: RequestId,
        preview: &SplitPreview,
        existing: PreviewPane,
    ) -> anyhow::Result<()> {
        let source = self.window_manager.active_stable_window_id();
        let Some(index) = self.window_manager.window_index(existing.window) else {
            return Ok(());
        };
        self.set_active_window(index);
        if self.current_buffer().id() != existing.buffer_id {
            if let Some(buffer_index) = self.preview_buffer_index(existing.buffer_id) {
                self.buffer_manager.set_active_index(buffer_index);
                if let Some(window) = self.window_manager.active_window_mut() {
                    window.buffer_index = buffer_index;
                }
            }
        }
        let language = self.preview_language(preview.syntax.as_deref());
        {
            let buffer = self.current_buffer_mut();
            buffer.replace_generated_text(&preview.text);
            if !preview.name.is_empty() {
                buffer.set_display_name(preview.name.clone());
            }
            if let Some(language) = language {
                buffer.set_syntax_selection(SyntaxSelection::Language(language));
            }
        }
        self.reveal_preview_line(preview.line);
        self.place_preview_focus(source, existing.window);
        let buffer_index = self
            .preview_buffer_index(existing.buffer_id)
            .unwrap_or_else(|| self.buffer_manager.active_index());
        self.finish_preview_open(
            screen,
            runtime,
            request_id,
            buffer_index,
            true,
            preview.side.clone(),
        )
        .await
    }

    async fn finish_preview_open(
        &mut self,
        screen: &mut RenderBuffer,
        runtime: &mut crate::plugin::Runtime,
        request_id: RequestId,
        buffer_index: usize,
        reused: bool,
        side: Option<String>,
    ) -> anyhow::Result<()> {
        self.workspace_manager.suspend();
        self.set_legacy_message(Some(PREVIEW_HINT.to_string()));
        self.plugin_registry
            .resolve_request(
                runtime,
                request_id,
                json!({
                    "buffer_index": buffer_index,
                    "reused": reused,
                    "side": side,
                }),
            )
            .await?;
        self.render(screen)
    }

    pub(super) fn close_active_window(&mut self) -> bool {
        let closing_preview = self.active_window_is_readonly_preview();
        if !self.update_window_layout(WindowManager::close_window) {
            return false;
        }
        if closing_preview {
            self.readonly_previews
                .retain(|preview| self.window_manager.window_index(preview.window).is_some());
            if self.readonly_previews.is_empty() {
                self.workspace_manager.resume();
            }
        }
        true
    }

    pub(super) fn active_window_is_readonly_preview(&self) -> bool {
        self.window_manager
            .active_stable_window_id()
            .is_some_and(|id| {
                self.readonly_previews
                    .iter()
                    .any(|preview| preview.window == id)
            })
    }

    pub(super) fn block_readonly_mutation(&mut self, action: &Action) -> bool {
        let directory = self.current_buffer().directory.is_some();
        let preview = self.current_buffer().is_read_only();
        if !directory && !preview {
            return false;
        }
        let structural = matches!(
            action,
            Action::Save
                | Action::ForceSave
                | Action::SaveAs(_)
                | Action::ForceSaveAs(_)
                | Action::SetBufferName(_)
                | Action::EnterMode(Mode::Insert)
                | Action::SetSyntax(_)
        );
        let typed = preview
            && matches!(
                action,
                Action::InsertCharAtCursorPos(_)
                    | Action::InsertNewLine
                    | Action::InsertString(_)
                    | Action::InsertPastedText(_)
                    | Action::DeleteCharAtCursorPos
                    | Action::DeletePreviousChar
                    | Action::DeleteCharAt(_, _)
                    | Action::ReplaceCharsAtCursor { .. }
                    | Action::ReplaceSelection(_)
                    | Action::Paste
                    | Action::PasteBefore
                    | Action::ChangeSelection
                    | Action::Delete
            );
        if !(structural || typed) {
            return false;
        }
        let message = if directory {
            "Directory buffers are read-only"
        } else {
            "Preview is read-only"
        };
        self.set_legacy_message(Some(message.to_string()));
        true
    }

    pub(super) fn note_readonly_text_edit(&mut self) -> bool {
        let message = if self.current_buffer().directory.is_some() {
            "Directory buffers are read-only"
        } else if self.current_buffer().is_read_only() {
            "Preview is read-only"
        } else {
            return false;
        };
        self.set_legacy_message(Some(message.to_string()));
        true
    }

    fn reveal_preview_line(&mut self, line: usize) {
        let line = line.min(self.current_buffer().len());
        self.vtop = line;
        self.cy = 0;
        self.cx = 0;
        self.sync_to_window();
    }

    fn place_preview_focus(&mut self, source: Option<WindowId>, destination: WindowId) {
        let want_source = self.config.markdown_link_split_focus == MarkdownLinkSplitFocus::Source;
        let target = if want_source {
            source.filter(|id| *id != destination).or(source)
        } else {
            Some(destination)
        };
        if let Some(id) = target {
            if let Some(index) = self.window_manager.window_index(id) {
                self.set_active_window(index);
            }
        }
    }

    fn apply_preview_syntax(&self, buffer: &mut Buffer, syntax: Option<&str>) {
        if let Some(language) = self.preview_language(syntax) {
            buffer.set_syntax_selection(SyntaxSelection::Language(language));
        }
    }

    fn preview_language(&self, syntax: Option<&str>) -> Option<String> {
        self.highlighter
            .language_id_for_file(syntax)
            .map(str::to_string)
    }

    fn preview_buffer_index(&self, buffer_id: BufferId) -> Option<usize> {
        self.buffer_manager
            .iter()
            .position(|buffer| buffer.id() == buffer_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        plugin::{Runtime, WorkspaceConfig},
        test_utils::EditorTestExt as _,
        theme::{Style, Theme},
    };

    fn editor() -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        config.markdown_link_split_focus = MarkdownLinkSplitFocus::Source;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            80,
            24,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some("source.txt".to_string()),
                "source\n".to_string(),
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
            .workspace_manager
            .open("git-dashboard".to_string(), WorkspaceConfig::default());
        editor
    }

    fn preview(text: &str, line: usize) -> SplitPreview {
        SplitPreview {
            key: "before:src/main.rs".to_string(),
            name: "[Git before] src/main.rs@HEAD".to_string(),
            text: text.to_string(),
            line,
            syntax: Some("src/main.rs".to_string()),
            side: Some("before".to_string()),
        }
    }

    async fn open(editor: &mut Editor, preview: SplitPreview) {
        let mut screen = RenderBuffer::new(80, 24, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .open_readonly_split(&mut screen, &mut runtime, RequestId::from_raw(1), preview)
            .await
            .unwrap();
    }

    fn preview_buffer(editor: &Editor) -> &Buffer {
        let id = editor.readonly_previews[0].buffer_id;
        editor
            .buffer_manager
            .iter()
            .find(|buffer| buffer.id() == id)
            .unwrap()
    }

    #[tokio::test]
    async fn split_suspends_workspace_reuses_the_key_and_resumes_on_quit() {
        let mut editor = editor();
        let source = editor.window_manager.active_stable_window_id().unwrap();
        open(&mut editor, preview("one\ntwo\nthree\n", 1)).await;

        assert!(!editor.workspace_manager.is_active());
        assert!(editor.workspace_manager.is_suspended());
        assert_eq!(editor.window_manager.window_count(), 2);
        assert_eq!(
            editor.window_manager.active_stable_window_id(),
            Some(source)
        );
        let pane = editor.readonly_previews[0].clone();
        let buffer = preview_buffer(&editor);
        assert!(buffer.is_read_only());
        assert_eq!(buffer.contents(), "one\ntwo\nthree\n");
        assert_eq!(buffer.path(), Some("[Git before] src/main.rs@HEAD"));
        assert_eq!(
            buffer.syntax_selection(),
            &SyntaxSelection::Language("rust".to_string())
        );
        assert_eq!(editor.current_buffer().contents(), "source\n");
        assert_eq!(editor.window_manager.window(pane.window).unwrap().vtop, 1);

        open(&mut editor, preview("after\n", 0)).await;
        assert_eq!(editor.window_manager.window_count(), 2);
        assert_eq!(editor.readonly_previews[0].window, pane.window);
        assert_eq!(editor.readonly_previews[0].buffer_id, pane.buffer_id);
        assert_eq!(preview_buffer(&editor).contents(), "after\n");
        assert!(editor.workspace_manager.is_suspended());

        editor.set_active_window(editor.window_manager.window_index(pane.window).unwrap());
        assert_eq!(editor.test_buffer_line(), 0);
        editor.test_execute_action(Action::MoveDown).await.unwrap();
        assert_eq!(editor.current_buffer().contents(), "after\n");
        let before = editor.current_buffer().contents();
        editor
            .test_execute_action(Action::InsertCharAtCursorPos('x'))
            .await
            .unwrap();
        editor.test_execute_action(Action::Save).await.unwrap();
        editor
            .test_execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), before);
        assert_eq!(editor.last_error.as_deref(), Some("Preview is read-only"));
        assert_eq!(editor.test_mode(), Mode::Normal);

        editor
            .test_execute_action(Action::Quit(false))
            .await
            .unwrap();
        assert_eq!(editor.window_manager.window_count(), 1);
        assert!(editor.workspace_manager.is_active());
        assert!(!editor.workspace_manager.is_suspended());
        assert!(editor.readonly_previews.is_empty());
        assert_eq!(editor.current_buffer().contents(), "source\n");
    }

    #[tokio::test]
    async fn target_focus_lands_in_the_preview_pane() {
        let mut editor = editor();
        editor.config.markdown_link_split_focus = MarkdownLinkSplitFocus::Target;
        let source = editor.window_manager.active_stable_window_id().unwrap();
        open(&mut editor, preview("alpha\n", 0)).await;
        let destination = editor.readonly_previews[0].window;
        assert_ne!(destination, source);
        assert_eq!(
            editor.window_manager.active_stable_window_id(),
            Some(destination)
        );
        assert_eq!(editor.current_buffer().contents(), "alpha\n");
        editor
            .test_execute_action(Action::CloseWindow)
            .await
            .unwrap();
        assert_eq!(editor.window_manager.window_count(), 1);
        assert!(editor.workspace_manager.is_active());
    }
}
