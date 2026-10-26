//! Apply revision-checked operations to Red's native transaction, history and plugin services.

use super::REPEAT_HOST_CHANGE;
use crate::editor::{Action, Editor, KeyAction, Mode, RenderBuffer, Runtime, TextRange};
use crossterm::event::{Event, KeyCode, KeyModifiers};
use vimlet::{
    AppliedEdit, ByteOffset, Command, EditTarget, HostOperation, HostOutcome, OperationKind,
    Operator, Shape, Transaction,
};

impl Editor {
    pub(super) async fn apply_file_operation(
        &mut self,
        op: &HostOperation,
        command: Option<&Command>,
        native: Option<&Action>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<HostOutcome> {
        if !op.is_current(self.file_version()) {
            return Ok(HostOutcome::Rejected);
        }
        if let OperationKind::Host { command, count } = op.kind {
            let Some(change) = self
                .last_semantic_change
                .clone()
                .filter(|_| command.id == REPEAT_HOST_CHANGE)
            else {
                return Ok(HostOutcome::Rejected);
            };
            for n in 0..count {
                let checkpoint = self.rpc_command_checkpoint();
                self.replay_semantic_change(change.clone(), buffer, runtime)
                    .await?;
                if let Some(rpc) = self.rpc_command_started_since(checkpoint) {
                    if n + 1 < count {
                        self.defer_rpc_action(
                            rpc,
                            crate::editor::rpc_commands::RpcContinuation::Keys(
                                Event::Key(crossterm::event::KeyEvent::new(
                                    KeyCode::Char('.'),
                                    KeyModifiers::NONE,
                                )),
                                KeyAction::Repeating(
                                    u16::try_from(count - n - 1).unwrap_or(u16::MAX),
                                    Box::new(KeyAction::Single(Action::RepeatLastChange)),
                                ),
                            ),
                        );
                    }
                    break;
                }
            }
            return Ok(HostOutcome::Applied {
                edits: vec![],
                changed: self.file_version() != op.expected,
            });
        }
        if let OperationKind::History { command, count } = op.kind {
            self.snippet_session = None;
            for _ in 0..count {
                match command {
                    vimlet::HistoryCommand::Undo => self.undo_transaction(buffer, runtime).await?,
                    vimlet::HistoryCommand::Redo => self.redo_transaction(buffer, runtime).await?,
                    vimlet::HistoryCommand::UndoLine => return Ok(HostOutcome::Rejected),
                };
            }
            return Ok(HostOutcome::Applied {
                edits: vec![],
                changed: self.file_version() != op.expected,
            });
        }
        if let OperationKind::Resolve {
            request: vimlet::TargetRequest::Mark { name, linewise },
            ..
        } = op.kind
        {
            let Some(mark) = self
                .named_mark(name)
                .filter(|mark| mark.buffer_id == self.current_buffer().id())
            else {
                return Ok(HostOutcome::Rejected);
            };
            let rope = self.current_buffer().contents_snapshot();
            let cursor = self.file_cursor_byte();
            let resolver = vimlet::MotionResolver::new(&rope, cursor);
            let target = rope.char_to_byte(mark.char_index.min(rope.len_chars()));
            let range = if linewise {
                Some(resolver.lines(cursor.0, target))
            } else {
                resolver.exclusive_range(ByteOffset(target))
            };
            return Ok(range.map_or(HostOutcome::Rejected, HostOutcome::Target));
        }
        if let OperationKind::Resolve {
            request: vimlet::TargetRequest::Block { selection },
            ..
        } = op.kind
        {
            return Ok(self.file_block_target(selection));
        }
        if let OperationKind::Resolve {
            request: request @ vimlet::TargetRequest::Syntax { id },
            count,
        } = op.kind
        {
            return Ok(self.file_syntax_target(request, id, count));
        }
        if let OperationKind::Transform {
            operator,
            range,
            count,
        } = &op.kind
        {
            let source = self.current_buffer();
            let target = TextRange::new(
                source
                    .byte_idx_to_position(range.ranges[0].start.0)
                    .unwrap(),
                source
                    .byte_idx_to_position(range.ranges.last().unwrap().end.0)
                    .unwrap(),
            );
            let visual = self.is_visual();
            if visual {
                self.execute(&Action::EnterMode(Mode::Normal), buffer, runtime)
                    .await?;
            }
            if matches!(operator, Operator::Indent | Operator::Outdent) {
                let first = target.start.line;
                let last = if target.end.character == 0 && target.end.line > first {
                    target.end.line - 1
                } else {
                    target.end.line
                };
                let levels = if matches!(
                    command.map(|c| &c.action),
                    Some(vimlet::Action::Operate {
                        target: EditTarget::Selection,
                        ..
                    })
                ) {
                    *count
                } else {
                    1
                };
                let levels = u16::try_from(levels).unwrap_or(u16::MAX);
                let changed = if *operator == Operator::Indent {
                    self.indent_line_range(first, last, levels)
                } else {
                    self.unindent_line_range(first, last, levels)
                };
                if changed {
                    self.notify_change(runtime).await?;
                }
                self.render(buffer)?;
                return Ok(HostOutcome::Applied {
                    edits: vec![],
                    changed,
                });
            }
            let action = match operator {
                Operator::Comment => Action::ToggleCommentRange(target),
                Operator::Format => Action::FormatTextRange(target),
                _ => return Ok(HostOutcome::Rejected),
            };
            self.execute(&action, buffer, runtime).await?;
            return Ok(HostOutcome::Applied {
                edits: vec![],
                changed: self.file_version() != op.expected,
            });
        }
        if let OperationKind::Move { cursor, selection } = op.kind {
            if let Some(selection) = selection {
                let mode = match selection.shape {
                    Shape::Character => Mode::Visual,
                    Shape::Line => Mode::VisualLine,
                    Shape::Block { .. } => Mode::VisualBlock,
                };
                if self.mode != mode {
                    self.execute(&Action::EnterMode(mode), buffer, runtime)
                        .await?;
                }
                let anchor = self
                    .current_buffer()
                    .byte_idx_to_position(selection.anchor.0)
                    .unwrap();
                self.selection_start = Some(crate::editor::Point::new(
                    self.char_to_grapheme_on_line(anchor.character, anchor.line),
                    anchor.line,
                ));
            } else if self.is_visual() {
                self.execute(&Action::EnterMode(Mode::Normal), buffer, runtime)
                    .await?;
            }
            let Some(pos) = self.current_buffer().byte_idx_to_position(cursor.0) else {
                return Ok(HostOutcome::Rejected);
            };
            self.move_to_text_position(pos);
            if self.is_visual() {
                self.update_selection();
            }
            self.finish_cursor_motion(buffer, false)?;
            return Ok(HostOutcome::Applied {
                edits: vec![],
                changed: false,
            });
        }
        let semantic = command.map(|command| &command.action);
        let open = match semantic {
            Some(vimlet::Action::Insert(vimlet::InsertStyle::OpenAbove)) => {
                Some(Action::InsertLineAtCursor)
            }
            Some(vimlet::Action::Insert(vimlet::InsertStyle::OpenBelow)) => {
                Some(Action::InsertLineBelowCursor)
            }
            _ => None,
        };
        if let Some(action) = open {
            let before = self.current_buffer().contents_snapshot();
            self.execute(&action, buffer, runtime).await?;
            return Ok(HostOutcome::Applied {
                edits: actual_file_edit(&before, &self.current_buffer().contents_snapshot())
                    .into_iter()
                    .collect(),
                changed: self.file_version() != op.expected,
            });
        }
        let paste = matches!(semantic, Some(vimlet::Action::Paste { .. }));
        if self.is_visual() {
            self.execute(&Action::EnterMode(Mode::Normal), buffer, runtime)
                .await?;
        }
        let OperationKind::Edit {
            replacements,
            cursor,
            transaction,
            ..
        } = &op.kind
        else {
            return Ok(HostOutcome::Rejected);
        };
        if *transaction == Transaction::BreakInsert {
            self.commit_transaction(self.cursor_snapshot());
            if let Some(action) = native {
                self.execute(action, buffer, runtime).await?;
            } else if let Some(pos) = self.current_buffer().byte_idx_to_position(cursor.0) {
                self.move_to_insert_text_position(pos);
                self.finish_cursor_motion(buffer, false)?;
            }
            self.insert_entry_cursor = Some(self.cursor_snapshot());
            return Ok(HostOutcome::Applied {
                edits: vec![],
                changed: false,
            });
        }
        let before = self.current_buffer().contents_snapshot();
        let mut edits = Vec::with_capacity(replacements.len());
        for r in replacements {
            let Some(old) = before.get_byte_slice(r.range.as_range()) else {
                return Ok(HostOutcome::Rejected);
            };
            edits.push(AppliedEdit {
                range: r.range,
                removed: old.to_string(),
                inserted: r.text.clone(),
            });
        }
        if let Some(action) = native.filter(|_| {
            self.is_insert() && matches!(transaction, Transaction::Continue | Transaction::Begin)
        }) {
            if *transaction == Transaction::Begin {
                self.begin_transaction("insert");
            }
            self.execute(action, buffer, runtime).await?;
            let after = self.current_buffer().contents_snapshot();
            if self.file_version().revision == op.expected.revision {
                edits.clear();
            } else {
                let matches = edits.as_slice().first().is_some_and(|edit| {
                    edits.len() == 1
                        && self.file_version().revision == op.expected.revision + 1
                        && self.file_cursor_byte() == *cursor
                        && after.len_bytes()
                            == before.len_bytes() - edit.removed.len() + edit.inserted.len()
                        && after
                            .get_byte_slice(
                                edit.range.start.0..edit.range.start.0 + edit.inserted.len(),
                            )
                            .is_some_and(|actual| actual == edit.inserted.as_str())
                });
                if !matches {
                    edits = actual_file_edit(&before, &after).into_iter().collect();
                }
            }
        } else {
            if matches!(transaction, Transaction::Begin | Transaction::Atomic) {
                self.begin_transaction("vim edit");
            }
            for edit in edits.iter().rev() {
                let source = self.current_buffer();
                let range = TextRange::new(
                    source.byte_idx_to_position(edit.range.start.0).unwrap(),
                    source.byte_idx_to_position(edit.range.end.0).unwrap(),
                );
                self.replace_range(range, &edit.inserted);
            }
            if paste {
                if let [edit] = edits.as_slice() {
                    if edit.inserted.contains('\n') {
                        let first = self
                            .current_buffer()
                            .byte_idx_to_position(edit.range.start.0)
                            .unwrap()
                            .line;
                        self.reindent_pasted_lines(first, &edit.inserted);
                    }
                }
            }
            let pos = self.current_buffer().byte_idx_to_position(cursor.0);
            if *transaction == Transaction::End {
                self.execute(&Action::EnterMode(Mode::Normal), buffer, runtime)
                    .await?;
            }
            if let Some(pos) = pos {
                if matches!(transaction, Transaction::Continue | Transaction::Begin) {
                    self.move_to_insert_text_position(pos);
                } else {
                    self.move_to_text_position(pos);
                    self.cx = self.cx.min(self.line_length().saturating_sub(1));
                }
            }
            if *transaction == Transaction::Begin && !self.is_insert() {
                self.execute(&Action::EnterMode(Mode::Insert), buffer, runtime)
                    .await?;
            } else if *transaction == Transaction::Atomic {
                self.commit_transaction(self.cursor_snapshot());
            }
            if self.file_version() != op.expected {
                self.notify_change(runtime).await?;
            }
            if paste && self.file_version() != op.expected {
                self.request_format_on_paste(&before.to_string()).await;
            }
            self.refresh_cursor_goal();
            self.render(buffer)?;
        }
        Ok(HostOutcome::Applied {
            edits,
            changed: self.file_version() != op.expected,
        })
    }
}

// Native indentation and snippet hooks can replace a larger range than the requested key.
fn actual_file_edit(before: &ropey::Rope, after: &ropey::Rope) -> Option<AppliedEdit> {
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let remaining = before.len_chars().min(after.len_chars()) - prefix;
    let suffix = before
        .chars_at(before.len_chars())
        .reversed()
        .zip(after.chars_at(after.len_chars()).reversed())
        .take(remaining)
        .take_while(|(a, b)| a == b)
        .count();
    let old = before.slice(prefix..before.len_chars() - suffix);
    let new = after.slice(prefix..after.len_chars() - suffix);
    if old.len_bytes() == 0 && new.len_bytes() == 0 {
        return None;
    }
    let start = before.char_to_byte(prefix);
    Some(AppliedEdit {
        range: vimlet::TextRange::new(start, start + old.len_bytes()),
        removed: old.to_string(),
        inserted: new.to_string(),
    })
}
