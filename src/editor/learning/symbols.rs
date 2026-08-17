//! Real, revision-checked LSP navigation inside the owned practice file.

use super::*;
use crate::ui::{PickerItem, PickerPreview};

#[derive(Clone, Copy)]
struct SymbolRequest {
    id: i64,
    buffer_id: BufferId,
    revision: u64,
}

#[derive(Default)]
pub(super) struct LearnSymbolState {
    definition: Option<SymbolRequest>,
    references: Option<SymbolRequest>,
    superseded: HashSet<i64>,
    pub definition_received: bool,
    pub references_received: bool,
}

impl LearnSymbolState {
    fn queue(&mut self, definition: bool, request: SymbolRequest) {
        let pending = if definition {
            &mut self.definition
        } else {
            &mut self.references
        };
        if let Some(previous) = pending.replace(request) {
            self.superseded.insert(previous.id);
        }
    }
}

impl Editor {
    pub(in crate::editor) fn learn_symbol_at(&self, line: usize) -> bool {
        if self.current_buffer().contents() != crate::learn::HUSK_SYMBOL_CONTENTS {
            return false;
        }
        let position = self.cursor_text_position();
        let Some(text) = self.current_buffer().get(line) else {
            return false;
        };
        let Some(start) = text.find("add_score") else {
            return false;
        };
        position.line == line && (start..start + "add_score".len()).contains(&position.character)
    }

    #[inline(never)]
    pub(in crate::editor) fn intercept_learn_symbol_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            if self
                .learn_session
                .as_ref()
                .is_none_or(|session| session.symbols.is_none())
            {
                return Ok(false);
            }
            let definition = match action {
                Action::GoToDefinition => true,
                Action::PluginCommand(name) if name == "LspReferences" => false,
                _ => return Ok(false),
            };
            if !self.learn_symbol_at(if definition { 5 } else { 0 }) {
                self.set_legacy_message(Some(
                    if definition {
                        "place the cursor on the first add_score call (:6, then ^3w)"
                    } else {
                        "find references from the add_score definition first"
                    }
                    .into(),
                ));
                self.render(buffer)?;
                return Ok(true);
            }
            let file = self
                .current_buffer()
                .file
                .clone()
                .expect("symbol lesson is file-backed");
            let position = self.cursor_lsp_position();
            self.ensure_current_buffer_lsp_opened().await?;
            let id = if definition {
                self.lsp
                    .goto_definition(&file, position.character, position.line)
                    .await?
            } else {
                self.lsp
                    .references(
                        &file,
                        position.character,
                        position.line,
                        /*include_declaration*/ false,
                    )
                    .await?
            };
            if id > 0 {
                let request = SymbolRequest {
                    id,
                    buffer_id: self.current_buffer().id(),
                    revision: self.current_buffer().revision(),
                };
                let symbols = self
                    .learn_session
                    .as_mut()
                    .and_then(|session| session.symbols.as_mut())
                    .expect("symbol lesson was checked");
                symbols.queue(definition, request);
                self.set_quiet_message(Some("Asking the bundled Husk server…".into()));
            } else {
                self.set_legacy_message(Some(
                    "Husk is not ready yet; try the command again".into(),
                ));
            }
            self.render(buffer)?;
            Ok(true)
        })
    }

    /// `Some` means this response belongs to the lesson and must not reach the
    /// user's plugin request handlers, even when it is stale or unsuccessful.
    pub(in crate::editor) fn handle_learn_symbol_response(
        &mut self,
        message: &InboundMessage,
    ) -> Option<Option<Action>> {
        let id = match message {
            InboundMessage::Message(response) => response.id,
            InboundMessage::Error(error) => error.id?,
            InboundMessage::RequestError { id, .. } => *id,
            _ => return None,
        };
        let symbols = self.learn_session.as_mut()?.symbols.as_mut()?;
        if symbols.superseded.remove(&id) {
            return Some(None);
        }
        let (definition, request) = if symbols.definition.is_some_and(|request| request.id == id) {
            (true, symbols.definition.take()?)
        } else if symbols.references.is_some_and(|request| request.id == id) {
            (false, symbols.references.take()?)
        } else {
            return None;
        };
        if request.buffer_id != self.current_buffer().id()
            || request.revision != self.current_buffer().revision()
            || self.current_buffer().contents() != crate::learn::HUSK_SYMBOL_CONTENTS
        {
            return Some(Some(Action::Print(
                "practice symbol response is stale; retry or restart the lesson".into(),
            )));
        }
        let InboundMessage::Message(response) = message else {
            return Some(Some(Action::Print(
                "Husk could not complete the symbol request; try again".into(),
            )));
        };
        let result = if definition && response.result.is_object() {
            Value::Array(vec![response.result.clone()])
        } else {
            response.result.clone()
        };
        let locations = match self.normalize_locations(&result) {
            Ok(locations) => locations,
            Err(error) => {
                return Some(Some(Action::Print(format!(
                    "invalid practice symbol response: {error}"
                ))))
            }
        };
        let Some(workspace) = self
            .learn_session
            .as_ref()
            .and_then(|session| session.workspace.as_ref())
        else {
            return Some(None);
        };
        let Some(current_file) = self.current_buffer().file.as_deref() else {
            return Some(None);
        };
        let mut locations = locations
            .into_iter()
            .filter(|location| {
                workspace.permits_file(Path::new(&location.file))
                    && same_file_path(Path::new(&location.file), Path::new(current_file))
            })
            .collect::<Vec<_>>();
        locations
            .sort_by_key(|location| (location.range.start.line, location.range.start.character));
        locations.dedup_by(|left, right| left.range == right.range);
        if definition {
            let Some(location) = locations.into_iter().find(|location| {
                location.range.start.line == 0 && location.range.start.character == 3
            }) else {
                return Some(Some(Action::Print(
                    "Husk did not return the practice definition; try again".into(),
                )));
            };
            self.learn_session
                .as_mut()?
                .symbols
                .as_mut()?
                .definition_received = true;
            return Some(Some(open_location(&location)));
        }
        locations.retain(|location| match location.range.start.line {
            5 => location.range.start.character == 16,
            6 => location.range.start.character == 15,
            _ => false,
        });
        if ![5, 6].into_iter().all(|line| {
            locations
                .iter()
                .any(|location| location.range.start.line == line)
        }) {
            return Some(Some(Action::Print(
                "Husk did not find both practice calls; retry or restart the lesson".into(),
            )));
        }
        let mut actions = HashMap::new();
        let mut items = Vec::new();
        for location in locations {
            let line = location.range.start.line;
            let column = location.range.start.character;
            let id = format!("reference:{line}:{column}");
            actions.insert(id.clone(), open_location(&location));
            items.push(PickerItem {
                id,
                icon: None,
                label: self
                    .current_buffer()
                    .get(line)
                    .unwrap_or_default()
                    .trim()
                    .into(),
                kind: Some("Reference".into()),
                annotation: Some(format!("main.hk:{}:{}", line + 1, column + 1)),
                detail: None,
                data: Value::Null,
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: Some(PickerPreview::Location {
                    path: location.file,
                    line: Some(line),
                    column: Some(column),
                    matches: Vec::new(),
                }),
            });
        }
        let preview = HashMap::from([(
            current_file.to_string(),
            self.current_buffer().contents_snapshot(),
        )]);
        self.current_dialog = Some(Box::new(
            Picker::builder()
                .title("References")
                .structured_items(items)
                .placeholder("Filter references")
                .status("2 references · owned practice file")
                .location_preview_contents(preview)
                .select_action(move |id| {
                    actions.get(&id).cloned().unwrap_or_else(|| {
                        Action::Print("practice reference is no longer available".into())
                    })
                })
                .build(self),
        ));
        self.learn_session
            .as_mut()?
            .symbols
            .as_mut()?
            .references_received = true;
        Some(Some(Action::ShowDialog))
    }
}

fn open_location(location: &PluginLocation) -> Action {
    Action::OpenLocation(
        plugin::PluginLocation {
            path: location.file.clone(),
            line: location.range.start.line,
            column: location.range.start.character,
            column_encoding: plugin::LocationColumnEncoding::Utf16,
        },
        plugin::OpenLocationTarget::Current,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(id: i64, result: Value) -> InboundMessage {
        InboundMessage::Message(ResponseMessage {
            id,
            result,
            request: None,
        })
    }

    fn location(file: &Path, line: usize, character: usize) -> Value {
        json!({"uri": crate::lsp::file_uri(file).unwrap(), "range": {
            "start": {"line":line,"character":character},
            "end": {"line":line,"character":character+9}
        }})
    }

    fn set_request(editor: &mut Editor, id: i64, definition: bool, stale: bool) {
        let request = SymbolRequest {
            id,
            buffer_id: editor.current_buffer().id(),
            revision: editor.current_buffer().revision() + u64::from(stale),
        };
        let symbols = editor
            .learn_session
            .as_mut()
            .unwrap()
            .symbols
            .as_mut()
            .unwrap();
        symbols.queue(definition, request);
    }

    #[tokio::test]
    async fn learn_symbol_responses_are_revision_checked_and_confined() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            140,
            38,
            config,
            Theme::default(),
            vec![Buffer::new(None, "original".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut buffer = RenderBuffer::new(140, 38, &Style::default());
        let mut runtime = Runtime::new();
        // Install the real lesson lifecycle without spawning the test binary
        // as a server. The response handler below receives exact LSP payloads.
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
        let path = workspace.path("main.hk");
        let practice = Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            crate::learn::HUSK_SYMBOL_CONTENTS.into(),
        );
        let id = practice.id();
        *editor.current_buffer_mut() = practice;
        let session = editor.learn_session.as_mut().unwrap();
        session.practice_buffer_id = id;
        session.lesson = Lesson::FollowTheSymbol;
        session.step = PracticeStep::SymbolDefinition;
        session.symbols = Some(LearnSymbolState::default());
        let outside = tempfile::NamedTempFile::new().unwrap();

        set_request(&mut editor, 39, true, false);
        set_request(&mut editor, 40, true, false);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(39, location(&path, 0, 3))),
            Some(None)
        ));

        set_request(&mut editor, 41, true, true);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(41, location(&path, 0, 3))),
            Some(Some(Action::Print(_)))
        ));
        assert!(
            !editor
                .learn_session
                .as_ref()
                .unwrap()
                .symbols
                .as_ref()
                .unwrap()
                .definition_received
        );
        set_request(&mut editor, 42, true, false);
        assert!(editor
            .handle_learn_symbol_response(&response(900, location(&path, 0, 3)))
            .is_none());
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(42, location(outside.path(), 0, 3))),
            Some(Some(Action::Print(_)))
        ));
        set_request(&mut editor, 43, true, false);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(43, location(&path, 0, 3))),
            Some(Some(Action::OpenLocation(
                _,
                plugin::OpenLocationTarget::Current
            )))
        ));

        set_request(&mut editor, 44, false, false);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(
                44,
                json!([location(&path, 5, 16), location(outside.path(), 6, 15)])
            )),
            Some(Some(Action::Print(_)))
        ));
        set_request(&mut editor, 45, false, false);
        assert!(matches!(
            editor.handle_learn_symbol_response(&response(
                45,
                json!([location(&path, 6, 15), location(&path, 5, 16)])
            )),
            Some(Some(Action::ShowDialog))
        ));
        assert_eq!(
            editor.current_dialog.as_ref().unwrap().shortcut_context(),
            "References"
        );
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!path.exists());
        assert_eq!(editor.current_buffer().contents(), "original");
    }
}
