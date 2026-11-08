//! Copy a fenced code block without editing the buffer.

use super::{Content, Editor};

impl Editor {
    pub(super) fn yank_code_block(&mut self) -> bool {
        let source = self.current_buffer().contents();
        let line = self.buffer_line();
        let Some(content) = crate::markdown_fence::fenced_block_content(&source, line) else {
            self.set_legacy_message(Some("Cursor is not inside a fenced code block".to_string()));
            return false;
        };
        self.set_default_register(Content::linewise(content));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use crate::{
        buffer::Buffer, config::Config, editor::Action, test_utils::EditorTestExt as _,
        theme::Theme, unicode_utils::char_to_grapheme,
    };

    fn fence_editor(path: &str, source: &str) -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(Some(path.to_string()), source.to_string());
        let mut editor =
            Editor::with_size(lsp, 100, 30, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn cursor_at(editor: &mut Editor, needle: &str) {
        let byte = editor.current_buffer().contents().find(needle).unwrap();
        let position = editor.current_buffer().byte_idx_to_position(byte).unwrap();
        let line = editor.current_buffer().get(position.line).unwrap();
        editor.cx = char_to_grapheme(&line, position.character);
        editor.vtop = 0;
        editor.cy = position.line;
        editor.refresh_cursor_goal();
        editor.sync_to_window();
    }

    fn register_text(editor: &Editor) -> Option<&str> {
        editor
            .registers
            .get(&'"')
            .map(|content| content.text.as_str())
    }

    #[test]
    fn keymap_name_is_yank_code_block() {
        let action: Action = serde_json::from_str("\"yank-code-block\"").unwrap();
        assert_eq!(action, Action::YankCodeBlock);
    }

    #[tokio::test]
    async fn yank_copies_the_fence_around_the_cursor_without_editing() {
        let source = "before\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\nafter\n";
        let mut editor = fence_editor("/tmp/red-fence.md", source);
        cursor_at(&mut editor, "println");
        let revision = editor.current_buffer().revision();

        editor
            .test_execute_action(Action::YankCodeBlock)
            .await
            .unwrap();

        assert_eq!(
            register_text(&editor),
            Some("fn main() {\n    println!(\"hi\");\n}\n")
        );
        assert_eq!(editor.current_buffer().contents(), source);
        assert_eq!(editor.current_buffer().revision(), revision);
        assert!(editor.last_error.is_none());

        cursor_at(&mut editor, "```rust");
        editor
            .test_execute_action(Action::Command("ycb".into()))
            .await
            .unwrap();
        assert_eq!(
            register_text(&editor),
            Some("fn main() {\n    println!(\"hi\");\n}\n")
        );
        assert_eq!(editor.current_buffer().revision(), revision);
    }

    #[tokio::test]
    async fn yank_outside_a_fence_leaves_the_register_and_buffer_alone() {
        let source = "plain text\n    ```\n    hidden\n    ```\n";
        let mut editor = fence_editor("/tmp/red-fence.rs", source);
        cursor_at(&mut editor, "hidden");
        let revision = editor.current_buffer().revision();

        editor
            .test_execute_action(Action::Command("yank-code-block".into()))
            .await
            .unwrap();

        assert_eq!(register_text(&editor), None);
        assert_eq!(
            editor.last_error.as_deref(),
            Some("Cursor is not inside a fenced code block")
        );
        assert_eq!(editor.current_buffer().contents(), source);
        assert_eq!(editor.current_buffer().revision(), revision);
    }

    #[tokio::test]
    async fn an_empty_fence_copies_an_empty_string() {
        let source = "```\n```\n";
        let mut editor = fence_editor("/tmp/red-fence.md", source);
        cursor_at(&mut editor, "```");
        editor
            .test_execute_action(Action::YankCodeBlock)
            .await
            .unwrap();
        assert_eq!(register_text(&editor), Some(""));
        assert_eq!(editor.current_buffer().contents(), source);
    }
}
