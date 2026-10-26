//! Drive Vimlet operations against Red file buffers, native edit transactions and window state.

mod targets;

pub(super) use targets::syntax_target;

use super::{
    Action, EditOperator, Editor, Mode, PendingOperator, RenderBuffer, Runtime, TextRange,
};
use vimlet::{
    AppliedEdit, Binding, ByteOffset, Command, DocumentVersion, DocumentView, EditTarget,
    EditingState, HostOperation, HostOutcome, HostResult, Input, Key, Motion, OperationKind,
    Operator, Session, Shape, Step, Transaction, WordClass, WordDestination,
};

#[derive(Default)]
pub(super) struct FileVim {
    session: Session,
    state: EditingState,
    document: Option<DocumentVersion>,
    pub(super) search: Option<HostOperation>,
    last_search: Option<String>,
}

impl FileVim {
    pub(super) fn inserting(&self) -> bool {
        self.state.mode() == vimlet::Mode::Insert
    }
}

pub(super) fn word_target(key: char) -> EditTarget {
    let destination = match key.to_ascii_lowercase() {
        'e' => WordDestination::NextEnd,
        'b' => WordDestination::PreviousStart,
        _ => WordDestination::NextStart,
    };
    let class = if key.is_ascii_uppercase() {
        WordClass::BigWord
    } else {
        WordClass::Word
    };
    EditTarget::Motion(Motion::Word { destination, class })
}

impl PendingOperator {
    pub(super) fn vim_command(self, key: char, target: EditTarget) -> Option<super::KeyAction> {
        let operator = match self.operator {
            EditOperator::Change => Operator::Change,
            EditOperator::Delete => Operator::Delete,
            EditOperator::Yank => Operator::Yank,
            _ => return None,
        };
        let command = Command::new(
            vimlet::Action::Operate { operator, target },
            usize::from(self.count()),
        );
        Some(super::KeyAction::Single(Action::Vimlet { key, command }))
    }
}

impl Editor {
    pub(super) fn file_version(&self) -> DocumentVersion {
        let buffer = self.current_buffer();
        DocumentVersion {
            id: buffer.id().as_u64(),
            revision: buffer.revision(),
        }
    }

    fn file_cursor_byte(&self) -> ByteOffset {
        ByteOffset(
            self.current_buffer()
                .position_to_byte_idx(self.cursor_text_position()),
        )
    }

    fn file_view<'a>(
        &self,
        rope: &'a ropey::Rope,
        geometry: &'a vimlet::TerminalGeometry,
    ) -> DocumentView<'a> {
        let mut view = DocumentView::new(rope, self.file_version(), self.file_cursor_byte());
        view.geometry = geometry;
        if self.mode == Mode::VisualBlock {
            if let Some(anchor) = self.selection_start {
                let pos = super::TextPosition::new(
                    anchor.y,
                    self.grapheme_to_char_on_line(anchor.x, anchor.y),
                );
                let anchor = ByteOffset(self.current_buffer().position_to_byte_idx(pos));
                let resolver = vimlet::MotionResolver::from_view(view);
                let width = resolver
                    .column(anchor.0)
                    .abs_diff(resolver.column(view.cursor.0))
                    + 1;
                view.selection = Some(vimlet::Selection {
                    anchor,
                    cursor: view.cursor,
                    shape: Shape::Block { width },
                });
            }
        }
        view
    }

    pub(super) fn file_block_operator<'a>(
        &'a mut self,
        key: char,
        operator: Operator,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> super::BoxFuture<'a, anyhow::Result<()>> {
        self.file_vim.state =
            EditingState::new(vimlet::Mode::VisualBlock, vimlet::Capabilities::default());
        self.run_file_vim_command(
            key,
            Command::new(
                vimlet::Action::Operate {
                    operator,
                    target: EditTarget::Selection,
                },
                0,
            ),
            buffer,
            runtime,
        )
    }

    #[inline(never)]
    pub(super) fn run_file_vim_command<'a>(
        &'a mut self,
        key: char,
        command: Command,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> super::BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut input = Input::key(Key::Char(key));
            input.binding = Binding::Command(command);
            self.drive_file_vim(input, None, buffer, runtime).await
        })
    }

    #[inline(never)]
    pub(super) fn repeat_file_vim<'a>(
        &'a mut self,
        count: u16,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> super::BoxFuture<'a, anyhow::Result<()>> {
        self.run_file_vim_command(
            '.',
            Command::new(vimlet::Action::Repeat, usize::from(count)),
            buffer,
            runtime,
        )
    }

    #[inline(never)]
    pub(super) fn continue_file_vim<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> super::BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            let key = match action {
                Action::InsertCharAtCursorPos(c) => Some(Key::Char(*c)),
                Action::EnterMode(Mode::Normal) => Some(Key::Escape),
                _ => None,
            };
            if let Some(key) = key
                .filter(|_| self.is_insert() && self.file_vim.document == Some(self.file_version()))
            {
                self.drive_file_vim(Input::key(key), Some(action), buffer, runtime)
                    .await?;
                return Ok(true);
            }
            self.file_vim
                .session
                .lifecycle(&mut self.file_vim.state, vimlet::Lifecycle::LeaveMode);
            if let Some(pending) = &mut self.pending_semantic_change {
                pending.vimlet = false;
            }
            Ok(false)
        })
    }

    async fn drive_file_vim(
        &mut self,
        input: Input,
        native: Option<&Action>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self.current_buffer().directory.is_some() {
            self.set_legacy_message(Some("Directory buffers are read-only".into()));
            return Ok(());
        }
        // Native insert and mode hooks dispatch nested actions. They share the open Red
        // transaction, while this invocation retains the outstanding Vimlet operation.
        let mut vim = std::mem::take(&mut self.file_vim);
        let rope = self.current_buffer().contents_snapshot();
        let geometry = vimlet::TerminalGeometry {
            tabstop: self.active_tab_width(),
        };
        let view = self.file_view(&rope, &geometry);
        let step = vim.session.handle(&mut vim.state, input, view);
        let result = self
            .finish_file_steps(&mut vim, step, native, buffer, runtime)
            .await;
        self.file_vim = vim;
        result
    }

    async fn finish_file_steps(
        &mut self,
        vim: &mut FileVim,
        mut step: Step,
        native: Option<&Action>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let mut sync_register = native.is_none();
        while let Step::HostOperation(op) = step {
            let outcome = if let OperationKind::Resolve {
                request: vimlet::TargetRequest::Search { backward, repeat },
                count,
            } = op.kind
            {
                let direction = if backward {
                    super::SearchDirection::Backward
                } else {
                    super::SearchDirection::Forward
                };
                if !repeat {
                    self.begin_search(direction);
                    vim.search = Some(op);
                    self.render(buffer)?;
                    return Ok(());
                }
                match &vim.last_search {
                    Some(pattern) => self.file_search_target(pattern, direction, count),
                    None => HostOutcome::Rejected,
                }
            } else {
                self.apply_file_operation(&op, native, buffer, runtime)
                    .await?
            };
            let accepted = outcome != HostOutcome::Rejected;
            let rope = self.current_buffer().contents_snapshot();
            let geometry = vimlet::TerminalGeometry {
                tabstop: self.active_tab_width(),
            };
            let view = self.file_view(&rope, &geometry);
            let result = HostResult {
                operation: op.id,
                before: op.expected,
                after: view.version,
                cursor: view.cursor,
                selection: view.selection,
                outcome,
            };
            step = vim.session.resume(&mut vim.state, result, view);
            if !accepted {
                vim.session
                    .lifecycle(&mut vim.state, vimlet::Lifecycle::LeaveMode);
            }
            if let Some(pending) = &mut self.pending_semantic_change {
                pending.vimlet = accepted;
            }
            if accepted && sync_register && !matches!(op.kind, OperationKind::Resolve { .. }) {
                if let Some(register) = vim.session.registers().get('"') {
                    let text = register.text().to_owned();
                    self.set_default_register(match register.shape() {
                        Shape::Line => super::Content::linewise(text),
                        Shape::Block { .. } => {
                            // Red's clipboard representation terminates every rectangular row.
                            super::Content::blockwise(format!("{text}\n"))
                        }
                        Shape::Character => super::Content::charwise(text),
                    });
                }
            }
            if !matches!(op.kind, OperationKind::Resolve { .. }) {
                sync_register = false;
            }
        }
        vim.document = Some(self.file_version());
        if let Step::Error(error) = step {
            anyhow::bail!("file Vim input: {error}");
        }
        Ok(())
    }

    async fn apply_file_operation(
        &mut self,
        op: &HostOperation,
        native: Option<&Action>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<HostOutcome> {
        if !op.is_current(self.file_version()) {
            return Ok(HostOutcome::Rejected);
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
        if self.mode == Mode::VisualBlock {
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
            if let OperationKind::Move { cursor, .. } = op.kind {
                if let Some(pos) = self.current_buffer().byte_idx_to_position(cursor.0) {
                    self.move_to_text_position(pos);
                    self.finish_cursor_motion(buffer, false)?;
                    return Ok(HostOutcome::Applied {
                        edits: vec![],
                        changed: false,
                    });
                }
            }
            return Ok(HostOutcome::Rejected);
        };
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
        if *transaction == Transaction::Continue
            && matches!(native, Some(Action::InsertCharAtCursorPos(_)))
        {
            self.execute(native.unwrap(), buffer, runtime).await?;
            // Snippets, indentation and comment wrapping can expand an ordinary insertion.
            // Red's recorded input recipe owns those host-specific effects until they are modeled.
            if self.file_version().revision != op.expected.revision + 1
                || self.file_cursor_byte() != *cursor
            {
                return Ok(HostOutcome::Rejected);
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
            if *transaction == Transaction::Begin {
                self.execute(&Action::EnterMode(Mode::Insert), buffer, runtime)
                    .await?;
            } else if *transaction == Transaction::Atomic {
                self.commit_transaction(self.cursor_snapshot());
            }
            if self.file_version() != op.expected {
                self.notify_change(runtime).await?;
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
