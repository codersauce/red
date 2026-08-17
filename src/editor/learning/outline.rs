//! A real document-symbol request confined to the owned Husk fixture.

use super::*;
use crate::ui::{PickerItem, PickerPreview};

#[derive(Clone, Copy)]
struct OutlineRequest {
    id: i64,
    buffer_id: BufferId,
    revision: u64,
}

#[derive(Default)]
pub(super) struct LearnOutlineState {
    pending: Option<OutlineRequest>,
    superseded: HashSet<i64>,
    pub received: bool,
}

impl Editor {
    #[inline(never)]
    pub(super) fn intercept_learn_outline_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            if !matches!(action, Action::PluginCommand(name) if name == "LspDocumentSymbols")
                || self
                    .learn_session
                    .as_ref()
                    .is_none_or(|session| session.outline.is_none())
            {
                return Ok(false);
            }
            if self.current_buffer().contents() != crate::learn::HUSK_SYMBOL_CONTENTS {
                self.set_quiet_message(Some(
                    "restore the practice text or use :tutorial restart".into(),
                ));
                self.render(buffer)?;
                return Ok(true);
            }
            let file = self
                .current_buffer()
                .file
                .clone()
                .expect("outline lesson is file-backed");
            self.ensure_current_buffer_lsp_opened().await?;
            let id = self.lsp.document_symbols(&file).await?;
            if id > 0 {
                let request = OutlineRequest {
                    id,
                    buffer_id: self.current_buffer().id(),
                    revision: self.current_buffer().revision(),
                };
                let outline = self
                    .learn_session
                    .as_mut()
                    .and_then(|session| session.outline.as_mut())
                    .expect("outline lesson was checked");
                if let Some(previous) = outline.pending.replace(request) {
                    outline.superseded.insert(previous.id);
                }
                self.set_quiet_message(Some("Asking the bundled Husk server…".into()));
            } else {
                self.set_quiet_message(Some("Husk is not ready yet; try the command again".into()));
            }
            self.render(buffer)?;
            Ok(true)
        })
    }

    pub(super) fn handle_learn_outline_response(
        &mut self,
        message: &InboundMessage,
    ) -> Option<Option<Action>> {
        let id = match message {
            InboundMessage::Message(response) => response.id,
            InboundMessage::Error(error) => error.id?,
            InboundMessage::RequestError { id, .. } => *id,
            _ => return None,
        };
        let outline = self.learn_session.as_mut()?.outline.as_mut()?;
        if outline.superseded.remove(&id) {
            return Some(None);
        }
        if outline.pending.is_none_or(|request| request.id != id) {
            return None;
        }
        let request = outline.pending.take()?;
        let failure = |message: &str| Some(Some(Action::Print(message.into())));
        if request.buffer_id != self.current_buffer().id()
            || request.revision != self.current_buffer().revision()
            || self.current_buffer().contents() != crate::learn::HUSK_SYMBOL_CONTENTS
        {
            return failure("practice outline is stale; retry or restart the lesson");
        }
        let InboundMessage::Message(response) = message else {
            return failure("Husk could not load the outline; try again");
        };
        let file = self.current_buffer().file.as_ref()?;
        let Ok(symbols) = self.normalize_document_symbols(&response.result, file) else {
            return failure("Husk returned an invalid outline; try again");
        };
        let workspace = self.learn_session.as_ref()?.workspace.as_ref()?;
        let mut symbols = symbols
            .into_iter()
            .filter(|symbol| {
                workspace.permits_file(Path::new(&symbol.file))
                    && same_file_path(Path::new(&symbol.file), Path::new(file))
                    && match symbol.name.as_str() {
                        "add_score" => {
                            symbol.selection_range.start.line == 0
                                && symbol.selection_range.start.character == 3
                        }
                        "main" => {
                            symbol.selection_range.start.line == 4
                                && symbol.selection_range.start.character == 3
                        }
                        _ => false,
                    }
            })
            .collect::<Vec<_>>();
        symbols.sort_by_key(|symbol| symbol.selection_range.start.line);
        symbols.dedup_by(|a, b| a.name == b.name);
        if symbols.len() != 2 {
            return failure("Husk did not return both practice functions; try again");
        }
        let mut actions = HashMap::new();
        let items = symbols
            .into_iter()
            .map(|symbol| {
                let line = symbol.selection_range.start.line;
                let column = symbol.selection_range.start.character;
                actions.insert(
                    symbol.id.clone(),
                    Action::OpenLocation(
                        plugin::PluginLocation {
                            path: symbol.file.clone(),
                            line,
                            column,
                            column_encoding: plugin::LocationColumnEncoding::Utf16,
                        },
                        plugin::OpenLocationTarget::Current,
                    ),
                );
                PickerItem {
                    id: symbol.id,
                    icon: None,
                    label: symbol.name,
                    kind: Some(symbol.kind_name),
                    annotation: Some(format!("main.hk:{}:{}", line + 1, column + 1)),
                    detail: symbol.detail,
                    data: Value::Null,
                    matches: Vec::new(),
                    detail_matches: Vec::new(),
                    preview: Some(PickerPreview::Location {
                        path: symbol.file,
                        line: Some(line),
                        column: Some(column),
                        matches: Vec::new(),
                    }),
                }
            })
            .collect();
        let preview = HashMap::from([(file.clone(), self.current_buffer().contents_snapshot())]);
        self.current_dialog = Some(Box::new(
            Picker::builder()
                .title("Document Symbols")
                .structured_items(items)
                .placeholder("Filter document symbols")
                .status("2 symbols · owned practice file")
                .location_preview_contents(preview)
                .select_action(move |id| {
                    actions.get(&id).cloned().unwrap_or_else(|| {
                        Action::Print("practice symbol is no longer available".into())
                    })
                })
                .build(self),
        ));
        self.learn_session.as_mut()?.outline.as_mut()?.received = true;
        Some(Some(Action::ShowDialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(id: i64, file: &Path) -> InboundMessage {
        let symbol = |name: &str, line| json!({"name":name,"kind":12,"location":{"uri":crate::lsp::file_uri(file).unwrap(),"range":{"start":{"line":line,"character":3},"end":{"line":line,"character":12}}}});
        InboundMessage::Message(ResponseMessage {
            id,
            request: None,
            result: json!([symbol("add_score", 0), symbol("main", 4)]),
        })
    }

    fn request(editor: &mut Editor, id: i64, stale: bool) {
        let pending = OutlineRequest {
            id,
            buffer_id: editor.current_buffer().id(),
            revision: editor.current_buffer().revision() + u64::from(stale),
        };
        editor
            .learn_session
            .as_mut()
            .unwrap()
            .outline
            .as_mut()
            .unwrap()
            .pending = Some(pending);
    }

    #[tokio::test]
    async fn learn_outline_requires_current_owned_server_results() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            100,
            30,
            config,
            Theme::default(),
            vec![Buffer::new(None, "original".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut buffer = RenderBuffer::new(100, 30, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .start_learn_lesson(Lesson::SaveAPracticeFile, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let workspace = editor
            .learn_session
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap();
        workspace
            .write_fixture("main.hk", crate::learn::HUSK_SYMBOL_CONTENTS)
            .unwrap();
        let file = workspace.path("main.hk");
        let practice = Buffer::new(
            Some(file.to_string_lossy().into_owned()),
            crate::learn::HUSK_SYMBOL_CONTENTS.into(),
        );
        let id = practice.id();
        *editor.current_buffer_mut() = practice;
        let session = editor.learn_session.as_mut().unwrap();
        session.practice_buffer_id = id;
        session.lesson = Lesson::FollowSymbols;
        session.step = PracticeStep::OutlineOpen;
        session.outline = Some(LearnOutlineState::default());
        request(&mut editor, 10, true);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(10, &file)),
            Some(Some(Action::Print(_)))
        ));
        request(&mut editor, 11, false);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(11, Path::new("/outside/main.hk"))),
            Some(Some(Action::Print(_)))
        ));
        request(&mut editor, 12, false);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(12, &file)),
            Some(Some(Action::ShowDialog))
        ));
        assert_eq!(
            editor.current_dialog.as_ref().unwrap().shortcut_context(),
            "Document Symbols"
        );
        editor
            .observe_learn_action(&Action::ShowDialog, &mut buffer)
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::OutlineChoose
        );
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "original");
        assert!(!file.exists());
    }
}
