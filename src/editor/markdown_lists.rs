//! Task edits use the same block grammar as highlighting and the normal undo seam.

use anyhow::{anyhow, bail, Context as _};
use futures::future::BoxFuture;
use tree_sitter::{Parser, Point, Query, QueryCursor, StreamingIterator as _};

use super::{render_buffer::RenderBuffer, Editor, Mode, Runtime, TextPosition, TextRange};

/// Match the synchronous document-size limit used by Markdown tables and links.
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

/// Returns the state character's UTF-8 offset for a task beginning on this line.
fn task_state_byte(source: &str, line: usize) -> anyhow::Result<Option<usize>> {
    let language = tree_sitter_md::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .context("Cannot parse Markdown task list")?;
    let query = Query::new(
        &language,
        "[(task_list_marker_checked) (task_list_marker_unchecked)] @task",
    )?;
    let mut cursor = QueryCursor::new();
    cursor.set_point_range(Point::new(line, 0)..Point::new(line + 1, 0));
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    Ok(matches
        .next()
        .map(|matched| matched.captures[0].node.start_byte() + 1))
}

impl Editor {
    // Change notifications have a large future. Keep its construction and polling
    // outside recursive action dispatch so visual-block dot-repeat fits 2 MiB.
    #[inline(never)]
    pub(super) fn execute_toggle_markdown_task<'a>(
        &'a mut self,
        screen: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            match self.toggle_markdown_task() {
                Ok(()) => self.notify_change(runtime).await?,
                Err(error) => self.set_legacy_message(Some(error.to_string())),
            }
            self.render(screen)
        })
    }

    pub(super) fn toggle_markdown_task(&mut self) -> anyhow::Result<()> {
        if self.mode != Mode::Normal {
            bail!("Toggle task is available in Normal mode");
        }
        if self
            .highlight_language_id_for_buffer_index(self.buffer_manager.active_index())
            .as_deref()
            != Some("markdown")
        {
            bail!("Toggle task is available in Markdown buffers");
        }
        let document = self.current_buffer();
        if document.byte_len() > MAX_DOCUMENT_BYTES {
            bail!("Toggle task is available for Markdown documents up to 2 MiB");
        }
        let source = document.contents();
        let byte = task_state_byte(&source, self.buffer_line())?
            .ok_or_else(|| anyhow!("No Markdown task on this line"))?;
        let position = document
            .byte_idx_to_position(byte)
            .context("Markdown task position is no longer valid")?;
        let replacement = if matches!(source.as_bytes()[byte], b'x' | b'X') {
            " "
        } else {
            "x"
        };
        let range = TextRange::new(
            position,
            TextPosition::new(position.line, position.character + 1),
        );
        self.begin_transaction("toggle task");
        self.replace_range(range, replacement);
        self.commit_transaction(self.cursor_snapshot());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer, config::Config, editor::Action, test_utils::EditorTestExt as _,
        theme::Theme,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn editor(file: &str, source: &str) -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            80,
            25,
            config,
            Theme::default(),
            vec![Buffer::new(Some(file.to_string()), source.to_string())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    async fn key(editor: &mut Editor, character: char) {
        editor
            .test_execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
    }

    #[test]
    fn task_lookup_recognizes_nested_ordered_quoted_and_crlf_items() {
        for (source, line, state) in [
            ("- [ ] pending\n", 0, b' '),
            ("- [x] done\n", 0, b'x'),
            ("- parent\n  * [X] child\n", 1, b'X'),
            ("10) [ ] ordered\n", 0, b' '),
            ("> 1. [x] quoted\n", 0, b'x'),
            ("# Heading\r\n\r\n+ [ ] café\r\n", 2, b' '),
            ("- [\t] pending\n", 0, b'\t'),
        ] {
            let byte = task_state_byte(source, line).unwrap().unwrap();
            assert_eq!(source.as_bytes()[byte], state, "{source}");
            assert!(
                task_state_byte(source, line + 1).unwrap().is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn task_lookup_excludes_code_escaped_markers_and_continuation_lines() {
        for (source, line) in [
            ("```\n- [x] fenced\n```\n", 1),
            ("    - [ ] indented\n", 0),
            ("`- [x] inline`\n", 0),
            ("[ ] plain\n", 0),
            ("- \\[x] escaped\n", 0),
            ("- [x]missing-space\n", 0),
            ("- item\n  [ ] continuation\n", 1),
            ("- [ ] task\n  continuation\n", 1),
            ("- item\n\n  [x] second paragraph\n", 2),
        ] {
            assert_eq!(task_state_byte(source, line).unwrap(), None, "{source}");
        }
    }

    #[tokio::test]
    async fn task_toggle_preserves_cursor_unicode_crlf_and_each_undo_step() {
        let source = "> - [ ] café 👩‍💻 end\r\n> - [X] second\r\n";
        let mut editor = editor("tasks.md", source);
        editor.cx = 13;
        editor.refresh_cursor_goal();
        let cursor = editor.cursor_snapshot();
        key(&mut editor, 'g').await;
        key(&mut editor, 'T').await;
        assert_eq!(
            editor.current_buffer().contents(),
            source.replacen("[ ]", "[x]", 1)
        );
        assert_eq!(editor.cursor_snapshot(), cursor);
        assert!(editor.current_buffer().is_dirty());
        editor.test_execute_action(Action::Undo).await.unwrap();
        assert_eq!(editor.current_buffer().contents(), source);
        assert_eq!(editor.cursor_snapshot(), cursor);
        assert!(!editor.current_buffer().is_dirty());
        editor.test_execute_action(Action::Redo).await.unwrap();
        assert_eq!(editor.cursor_snapshot(), cursor);
        editor.test_execute_action(Action::MoveDown).await.unwrap();
        key(&mut editor, '.').await;
        assert_eq!(
            editor.current_buffer().contents(),
            source.replacen("[ ]", "[x]", 1).replacen("[X]", "[ ]", 1)
        );
        editor.test_execute_action(Action::Undo).await.unwrap();
        assert_eq!(
            editor.current_buffer().contents(),
            source.replacen("[ ]", "[x]", 1)
        );
    }

    #[tokio::test]
    async fn task_toggle_rejects_non_tasks_and_non_markdown_without_edits() {
        for (file, source) in [
            ("tasks.md", "- plain\n"),
            ("example.rs", "- [ ] literal\n"),
            ("tasks.md", "```\n- [ ] code\n```\n"),
        ] {
            let mut editor = editor(file, source);
            if source.starts_with("```") {
                editor.cy = 1;
            }
            let revision = editor.current_buffer().revision();
            let cursor = editor.cursor_snapshot();
            editor
                .test_execute_action(Action::ToggleMarkdownTask)
                .await
                .unwrap();
            assert_eq!(editor.current_buffer().contents(), source);
            assert_eq!(editor.current_buffer().revision(), revision);
            assert_eq!(editor.cursor_snapshot(), cursor);
            assert!(!editor.current_buffer().is_dirty());
        }
    }

    #[test]
    fn task_toggle_checks_mode_and_size_before_editing() {
        let mut editor = editor("tasks.md", "- [ ] task\n");
        for mode in [
            Mode::Insert,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            editor.mode = mode;
            assert!(editor
                .toggle_markdown_task()
                .unwrap_err()
                .to_string()
                .contains("Normal mode"));
        }
        let source = format!("- [ ] {}", "x".repeat(MAX_DOCUMENT_BYTES));
        let mut oversized = self::editor("large.md", &source);
        assert!(oversized
            .toggle_markdown_task()
            .unwrap_err()
            .to_string()
            .contains("2 MiB"));
        assert!(!oversized.current_buffer().is_dirty());
    }
}
