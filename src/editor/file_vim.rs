//! Drive Vimlet operations against Red file buffers, native edit transactions and window state.

mod apply;
mod bindings;
mod targets;

pub(super) use targets::syntax_target;

use super::{Action, Editor, Mode, RenderBuffer, Runtime};
use vimlet::{
    Binding, ByteOffset, Command, DocumentVersion, DocumentView, EditTarget, EditingState,
    HostOperation, HostOutcome, HostResult, Input, Key, OperationKind, Operator, Session, Shape,
    Step,
};

pub(super) const REPEAT_HOST_CHANGE: u64 = 1;

#[derive(Default)]
pub(super) struct FileVim {
    pub(super) session: Session,
    pub(super) state: EditingState,
    document: Option<DocumentVersion>,
    pub(super) search: Option<HostOperation>,
    last_search: Option<String>,
    register_revision: u64,
}

impl FileVim {
    pub(super) fn inserting(&self) -> bool {
        self.state.mode() == vimlet::Mode::Insert
    }

    pub(super) fn pending(&self) -> bool {
        self.state.has_pending_input() && self.search.is_none()
    }

    pub(super) fn cancel_pending(&mut self) {
        if self.pending() {
            self.session
                .lifecycle(&mut self.state, vimlet::Lifecycle::CancelPending);
        }
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
        if self.is_visual() {
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
                    shape: match self.mode {
                        Mode::VisualLine => Shape::Line,
                        Mode::VisualBlock => Shape::Block { width },
                        _ => Shape::Character,
                    },
                });
            }
        }
        view
    }

    pub(super) fn run_file_visual_action<'a>(
        &'a mut self,
        action: &Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> super::BoxFuture<'a, anyhow::Result<()>> {
        let (key, command) = match action {
            Action::InsertBlock => ('I', vimlet::Action::BlockInsert { append: false }),
            Action::AppendBlock => ('A', vimlet::Action::BlockInsert { append: true }),
            _ => {
                let (key, operator) = match action {
                    Action::Yank => ('y', Operator::Yank),
                    Action::ChangeSelection => ('c', Operator::Change),
                    _ => ('d', Operator::Delete),
                };
                (
                    key,
                    vimlet::Action::Operate {
                        operator,
                        target: EditTarget::Selection,
                    },
                )
            }
        };
        self.sync_file_mode();
        self.run_file_vim_command(key, Command::new(command, 0), buffer, runtime)
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
        if !self.file_vim.session.has_last_change() {
            self.set_legacy_message(Some("no change to repeat".into()));
            return Box::pin(async { Ok(()) });
        }
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
                Action::InsertPastedText(s) | Action::InsertString(s) => Some(Key::Text(s.clone())),
                Action::InsertNewLine => Some(Key::Enter),
                Action::InsertTab => Some(Key::Tab),
                Action::DeletePreviousChar => Some(Key::Backspace),
                Action::MoveLeft => Some(Key::Left),
                Action::MoveRight => Some(Key::Right),
                Action::MoveUp => Some(Key::Up),
                Action::MoveDown => Some(Key::Down),
                Action::MoveToLineStart => Some(Key::Home),
                Action::MoveToLineEnd => Some(Key::End),
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

    pub(super) fn execute_file_or_native_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
        tracking: bool,
    ) -> super::BoxFuture<'a, anyhow::Result<bool>> {
        if let Action::VimletInput(input) = action {
            self.set_legacy_message(None);
            return Box::pin(async move {
                self.drive_file_vim(input.clone(), None, buffer, runtime)
                    .await?;
                Ok(false)
            });
        }
        // Native commands can recursively dispatch more actions on the detached editor's stack.
        if !self.file_vim.inserting() {
            return self.execute_action_inner(action, buffer, runtime, tracking);
        }
        Box::pin(async move {
            if self.continue_file_vim(action, buffer, runtime).await? {
                return Ok(false);
            }
            self.execute_action_inner(action, buffer, runtime, tracking)
                .await
        })
    }

    pub(super) async fn drive_file_vim(
        &mut self,
        mut input: Input,
        native: Option<&Action>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self.current_buffer().directory.is_some() {
            self.set_legacy_message(Some("Directory buffers are read-only".into()));
            return Ok(());
        }
        let paste = matches!(
            input.binding,
            Binding::Command(Command {
                action: vimlet::Action::Paste { .. },
                ..
            })
        );
        if paste {
            self.refresh_default_register_from_system_clipboard().await;
            if let Some(content) = self.registers.get(&super::DEFAULT_REGISTER) {
                let (text, shape) = match content.kind {
                    super::ContentKind::Linewise => (content.text.as_str(), Shape::Line),
                    super::ContentKind::Charwise => (content.text.as_str(), Shape::Character),
                    super::ContentKind::Blockwise => (
                        content.text.strip_suffix('\n').unwrap_or(&content.text),
                        Shape::Block { width: 1 },
                    ),
                };
                let bank = self.file_vim.session.registers_mut();
                if !bank.get('"').is_some_and(|r| {
                    r.text() == text
                        && std::mem::discriminant(&r.shape()) == std::mem::discriminant(&shape)
                }) {
                    bank.set('"', vimlet::Register::from_text(text, shape));
                    self.file_vim.register_revision = bank.unnamed_revision();
                }
            } else {
                return Ok(());
            }
        }
        // Native insert and mode hooks dispatch nested actions. They share the open Red
        // transaction, while this invocation retains the outstanding Vimlet operation.
        let mut vim = std::mem::take(&mut self.file_vim);
        let rope = self.current_buffer().contents_snapshot();
        let geometry = vimlet::TerminalGeometry {
            tabstop: self.active_tab_width(),
        };
        let view = self.file_view(&rope, &geometry);
        let literal = vim.state.pending_character_motion();
        let pending = vim.state.has_pending_input();
        let escape = input.key == Key::Escape;
        let key = if let Key::Char(c) = input.key {
            Some(c)
        } else {
            None
        };
        input.origin = vimlet::InputOrigin::Injected;
        let step = vim.session.handle(&mut vim.state, input, view);
        if pending {
            if step == Step::AwaitingInput {
                if let Some(c) = key {
                    self.waiting_command.get_or_insert_with(String::new).push(c);
                }
            } else {
                self.waiting_command = None;
                if step == Step::Consumed && !escape {
                    if literal.is_some() {
                        self.set_routine_warning(Some("character not found".into()));
                    } else {
                        self.set_legacy_message(Some("invalid operator motion".into()));
                    }
                }
            }
        }
        let result = self
            .finish_file_steps(&mut vim, step, native, buffer, runtime)
            .await;
        if result.is_err() {
            vim.session
                .lifecycle(&mut vim.state, vimlet::Lifecycle::LeaveMode);
        }
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
                Box::pin(self.apply_file_operation(
                    &op,
                    vim.state.pending_edit_command(),
                    native,
                    buffer,
                    runtime,
                ))
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
                if matches!(
                    op.kind,
                    OperationKind::Edit { .. } | OperationKind::Transform { .. }
                ) {
                    pending.vimlet = accepted && pending.repeat_recipe.is_none();
                }
            }
            if accepted && vim.register_revision != vim.session.registers().unnamed_revision() {
                vim.register_revision = vim.session.registers().unnamed_revision();
                for (name, register) in vim
                    .session
                    .registers()
                    .iter()
                    .filter(|(name, _)| name.is_ascii_digit() || *name == '-')
                {
                    self.registers.insert(name, red_register(register));
                }
                if let Some(register) = vim.session.registers().get('"') {
                    self.set_default_register(red_register(register));
                }
            }
            if self.clipboard_notice.is_some() {
                self.render(buffer)?;
            }
        }
        vim.document = Some(self.file_version());
        if let Step::Error(error) = step {
            anyhow::bail!("file Vim input: {error}");
        }
        Ok(())
    }
}

fn red_register(register: &vimlet::Register) -> super::Content {
    match register.shape() {
        Shape::Line => super::Content::linewise(register.text().to_owned()),
        // Red's clipboard notation terminates every rectangular row.
        Shape::Block { .. } => super::Content::blockwise(format!("{}\n", register.text())),
        Shape::Character => super::Content::charwise(register.text().to_owned()),
    }
}
