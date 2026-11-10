//! Heading outline picker. The list is built from the buffer text and a jump
//! does not edit it or push an undo entry.

use std::collections::HashMap;

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use serde_json::json;

use crate::{
    markdown_outline::{self, MarkdownHeading},
    ui::{Picker, PickerItem, PickerPreview},
};

use super::{Action, Editor};

impl Editor {
    pub(super) fn open_markdown_outline(&mut self) {
        if self.current_language_id().as_deref() != Some("markdown") {
            self.set_legacy_message(Some(
                "Markdown outline works in Markdown buffers".to_string(),
            ));
            return;
        }
        let source = self.current_buffer().contents();
        if source.len() > markdown_outline::MAX_DOCUMENT_BYTES {
            self.set_legacy_message(Some(
                "Markdown outline supports documents up to 2 MiB".to_string(),
            ));
            return;
        }
        let headings = markdown_outline::headings(&source);
        let buffer_id = self.current_buffer().id().as_u64();
        let path = self
            .current_buffer()
            .file
            .clone()
            .unwrap_or_else(|| format!("buffer:{buffer_id}"));
        let preview = HashMap::from([(path.clone(), self.current_buffer().contents_snapshot())]);
        let line = self.buffer_line();
        let active = markdown_outline::active_heading(&headings, line);
        let model = outline_picker_model(&headings, &path);
        let actions = model.actions;
        let mut picker = Picker::builder()
            .title("Markdown outline")
            .structured_items(model.items)
            .filter_action(outline_filter_score)
            .filter_tie_breaker(|item| {
                item.data
                    .get("order")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize
            })
            .placeholder("Filter headings")
            .history_key("markdown-outline")
            .location_preview_contents(preview)
            .select_action(move |item| {
                actions.get(&item).cloned().unwrap_or_else(|| {
                    Action::Print("heading is no longer in this buffer".to_string())
                })
            });
        if let Some(active) = active {
            picker = picker.initial_selection(active.to_string());
        }
        let mut picker = picker.build(self);
        picker.set_empty_message(Some("No headings".to_string()));
        self.current_dialog = Some(Box::new(picker));
    }
}

struct OutlinePickerModel {
    items: Vec<PickerItem>,
    actions: HashMap<String, Action>,
}

fn outline_picker_model(headings: &[MarkdownHeading], path: &str) -> OutlinePickerModel {
    let mut items = Vec::with_capacity(headings.len());
    let mut actions = HashMap::with_capacity(headings.len());
    for (index, heading) in headings.iter().enumerate() {
        let id = index.to_string();
        let indent = "  ".repeat(usize::from(heading.level.saturating_sub(1)));
        let label = if heading.title.is_empty() {
            format!("{indent}(empty)")
        } else {
            format!("{indent}{}", heading.title)
        };
        items.push(PickerItem {
            id: id.clone(),
            icon: None,
            label,
            kind: None,
            annotation: Some((heading.line + 1).to_string()),
            detail: None,
            data: json!({
                "search_text": heading.title,
                "order": index,
            }),
            matches: Vec::new(),
            detail_matches: Vec::new(),
            preview: Some(PickerPreview::Location {
                path: path.to_string(),
                line: Some(heading.line),
                column: Some(0),
                matches: Vec::new(),
            }),
        });
        actions.insert(id, Action::MoveTo(0, heading.line + 1));
    }
    OutlinePickerModel { items, actions }
}

fn outline_filter_score(item: &PickerItem, query: &str) -> Option<i64> {
    if query.trim().is_empty() {
        return Some(0);
    }
    let matcher = SkimMatcherV2::default();
    let search_text = item
        .data
        .get("search_text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&item.label);
    query.split_whitespace().try_fold(0_i64, |total, token| {
        matcher
            .fuzzy_match(search_text, token)
            .map(|score| total.saturating_add(score))
    })
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyModifiers};

    use super::Editor;
    use crate::{
        buffer::Buffer, config::Config, editor::Action, test_utils::EditorTestExt as _,
        theme::Theme, unicode_utils::char_to_grapheme,
    };

    fn outline_editor(path: &str, source: &str) -> Editor {
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

    async fn key(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) {
        editor
            .test_execute_event(TerminalEvent::Key(KeyEvent::new(code, modifiers)))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn outline_jumps_to_the_active_heading_and_control_o_returns() {
        let source = "# One\n\nintro\n\n## Two\n\nmore\n\n### Three\n\nend\n";
        let mut editor = outline_editor("/tmp/red-outline.md", source);
        cursor_at(&mut editor, "more");
        let origin = editor.current_jump_entry();
        let revision = editor.current_buffer().revision();
        key(&mut editor, KeyCode::Char(' '), KeyModifiers::NONE).await;
        key(&mut editor, KeyCode::Char('o'), KeyModifiers::NONE).await;
        assert!(editor.current_dialog.is_some());
        key(&mut editor, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(editor.current_dialog.is_none());
        assert_eq!(
            editor.buffer_line(),
            source.lines().position(|line| line == "## Two").unwrap()
        );
        assert_eq!(editor.active_jump_list().entries.len(), 1);
        key(&mut editor, KeyCode::Char('o'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.current_jump_entry(), origin);
        assert_eq!(editor.current_buffer().contents(), source);
        assert_eq!(editor.current_buffer().revision(), revision);
        assert!(!editor.current_buffer().is_dirty());
    }

    #[tokio::test]
    async fn command_opens_the_outline_and_other_languages_do_not_take_space_o() {
        let mut editor = outline_editor("/tmp/red-outline.md", "# Title\n\nbody\n");
        editor
            .test_execute_action(Action::Command("MarkdownOutline".into()))
            .await
            .unwrap();
        assert!(editor.current_dialog.is_some());

        let mut rust = outline_editor("/tmp/red-outline.rs", "fn main() {}\n");
        let before = rust.current_buffer().contents();
        key(&mut rust, KeyCode::Char(' '), KeyModifiers::NONE).await;
        key(&mut rust, KeyCode::Char('o'), KeyModifiers::NONE).await;
        assert!(rust.current_dialog.is_none());
        assert_eq!(rust.current_buffer().contents(), before);
        rust.test_execute_action(Action::MarkdownOutline)
            .await
            .unwrap();
        assert!(rust.current_dialog.is_none());
    }
}
