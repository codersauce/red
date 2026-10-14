//! Resume only the action tails that depend on a completed RPC command.

use super::*;

#[derive(Default)]
pub(super) struct RpcCommands {
    latest: Option<plugin::RpcCommandToken>,
    waiting: VecDeque<RpcSequence>,
    resuming: bool,
}

struct RpcSequence {
    command: plugin::RpcCommandToken,
    tails: VecDeque<RpcContinuation>,
    replay: RpcReplayContext,
}

struct RpcReplayContext {
    macro_events: VecDeque<MacroReplayEvent>,
    macro_depth: usize,
    macro_budget: usize,
    semantic: bool,
}

pub(super) enum RpcContinuation {
    Keys(Event, KeyAction),
    Commands(Vec<Action>),
    Macro,
    Semantic(SemanticChange),
}

impl RpcCommands {
    pub(super) fn started(&mut self, command: plugin::RpcCommandToken) {
        self.latest = Some(command);
    }

    pub(super) fn interrupt(&mut self, event: &Event) {
        if matches!(
            event,
            Event::Key(KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            }) | Event::Paste(_)
                | Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(_)
                        | MouseEventKind::Up(_)
                        | MouseEventKind::Drag(_)
                        | MouseEventKind::ScrollDown
                        | MouseEventKind::ScrollUp
                        | MouseEventKind::ScrollLeft
                        | MouseEventKind::ScrollRight,
                    ..
                })
        ) {
            self.waiting.clear();
        }
    }
}

impl Editor {
    fn take_rpc_replay_context(&mut self) -> RpcReplayContext {
        RpcReplayContext {
            macro_events: std::mem::take(&mut self.macro_replay_queue),
            macro_depth: self.macro_replay_depth,
            macro_budget: self.macro_instructions_remaining,
            semantic: self.replaying_semantic_change,
        }
    }

    fn restore_rpc_replay_context(&mut self, context: RpcReplayContext) {
        self.macro_replay_queue = context.macro_events;
        self.macro_replay_depth = context.macro_depth;
        self.macro_instructions_remaining = context.macro_budget;
        self.replaying_semantic_change = context.semantic;
    }

    pub(super) fn validate_rpc_document_context(
        &self,
        owner: &str,
        path: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<(), plugin::rpc::RpcErrorObject> {
        if path.is_none()
            && document_id.is_none()
            && self
                .plugin_registry
                .rpc_command_document(owner)
                .is_some_and(|origin| origin != self.current_buffer().id())
        {
            return Err(Self::rpc_error(
                -32003,
                "current document changed during RPC command; use document_id or path",
            ));
        }
        Ok(())
    }

    pub(super) fn begin_rpc_document_mutation(&mut self, index: usize) -> RpcDocumentMutationState {
        let state = RpcDocumentMutationState {
            index: self.buffer_manager.active_index(),
            view: (self.cx, self.cy, self.vtop, self.vleft, self.skipcol),
            resume_insert: self.transaction_active(),
            terminal_output: self.terminal_output_enabled,
        };
        if index != state.index {
            self.select_buffer_for_lsp_edit(index);
            self.terminal_output_enabled = false;
        }
        self.commit_active_transaction_before_save();
        state
    }

    pub(super) fn finish_rpc_document_mutation(
        &mut self,
        state: RpcDocumentMutationState,
        buffer: &mut RenderBuffer,
    ) -> Result<(), plugin::rpc::RpcErrorObject> {
        self.select_buffer_for_lsp_edit(state.index);
        (self.cx, self.cy, self.vtop, self.vleft, self.skipcol) = state.view;
        self.terminal_output_enabled = state.terminal_output;
        self.check_bounds();
        self.resume_insert_transaction_after_save(state.resume_insert);
        self.render(buffer)
            .map_err(|error| Self::rpc_error(-32003, error))
    }

    pub(super) fn rpc_command_checkpoint(&self) -> Option<tokio::task::Id> {
        self.rpc_commands
            .latest
            .as_ref()
            .map(plugin::RpcCommandToken::id)
    }

    pub(super) fn rpc_command_started_since(
        &self,
        checkpoint: Option<tokio::task::Id>,
    ) -> Option<plugin::RpcCommandToken> {
        self.rpc_commands
            .latest
            .as_ref()
            .filter(|command| Some(command.id()) != checkpoint)
            .cloned()
    }

    pub(super) fn defer_rpc_action(
        &mut self,
        command: plugin::RpcCommandToken,
        tail: RpcContinuation,
    ) {
        self.defer_rpc_tails(command, [tail]);
    }

    fn defer_rpc_tails(
        &mut self,
        command: plugin::RpcCommandToken,
        tails: impl IntoIterator<Item = RpcContinuation>,
    ) {
        if let Some(sequence) = self
            .rpc_commands
            .waiting
            .iter_mut()
            .find(|sequence| sequence.command.id() == command.id())
        {
            sequence.tails.extend(tails);
        } else {
            let replay = self.take_rpc_replay_context();
            self.rpc_commands.waiting.push_back(RpcSequence {
                command,
                tails: tails.into_iter().collect(),
                replay,
            });
        }
    }

    /// Completion is published by the registry before its dependent actions resume.
    pub(super) async fn resume_rpc_actions(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self.rpc_commands.resuming
            || self.macro_replay_depth > 0
            || self.replaying_semantic_change
        {
            return Ok(());
        }
        self.rpc_commands.resuming = true;
        let result = self.resume_ready_rpc_actions(buffer, runtime).await;
        self.rpc_commands.resuming = false;
        result
    }

    async fn resume_ready_rpc_actions(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        for _ in 0..PLUGIN_REQUESTS_PER_TICK {
            let Some(index) = self
                .rpc_commands
                .waiting
                .iter()
                .position(|sequence| !sequence.command.is_pending(&self.plugin_registry))
            else {
                break;
            };
            let sequence = self
                .rpc_commands
                .waiting
                .remove(index)
                .expect("ready sequence");
            if sequence.command.is_current(&self.plugin_registry) {
                self.resume_rpc_sequence(sequence, buffer, runtime).await?;
            }
            if self.deferred_quit == DeferredQuit::Ready {
                break;
            }
        }
        Ok(())
    }

    async fn resume_rpc_sequence(
        &mut self,
        sequence: RpcSequence,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let previous = self.take_rpc_replay_context();
        self.restore_rpc_replay_context(sequence.replay);
        let result = self.run_rpc_sequence(sequence.tails, buffer, runtime).await;
        self.restore_rpc_replay_context(previous);
        result
    }

    async fn run_rpc_sequence(
        &mut self,
        mut tails: VecDeque<RpcContinuation>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        while let Some(tail) = tails.pop_front() {
            let checkpoint = self.rpc_command_checkpoint();
            if self.resume_rpc_action(tail, buffer, runtime).await? {
                self.deferred_quit = DeferredQuit::Ready;
                break;
            }
            if self.replay_is_cancelled() {
                break;
            }
            if let Some(command) = self.rpc_command_started_since(checkpoint) {
                self.defer_rpc_tails(command, tails);
                break;
            }
        }
        Ok(())
    }

    async fn resume_rpc_action(
        &mut self,
        tail: RpcContinuation,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        match tail {
            RpcContinuation::Keys(event, action) => {
                self.handle_key_action(&event, &action, buffer, runtime)
                    .await
            }
            RpcContinuation::Commands(actions) => {
                self.execute_command_actions(actions, buffer, runtime).await
            }
            RpcContinuation::Macro => self
                .drain_macro_replay(buffer, runtime)
                .await
                .map(|()| false),
            RpcContinuation::Semantic(change) => self
                .replay_semantic_change(change, buffer, runtime)
                .await
                .map(|()| false),
        }
    }

    pub(super) async fn execute_key_actions(
        &mut self,
        event: &Event,
        actions: &[Action],
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        for (index, action) in actions.iter().enumerate() {
            let checkpoint = self.rpc_command_checkpoint();
            let recipe = self.pending_semantic_recipe_token();
            let quit = self.execute(action, buffer, runtime).await?;
            self.append_pending_semantic_action(recipe, action);
            if quit {
                return Ok(true);
            }
            if self.replay_is_cancelled() {
                break;
            }
            if let Some(command) = self.rpc_command_started_since(checkpoint) {
                self.defer_rpc_action(
                    command,
                    RpcContinuation::Keys(
                        event.clone(),
                        KeyAction::Multiple(actions[index + 1..].to_vec()),
                    ),
                );
                break;
            }
        }
        Ok(false)
    }

    pub(super) async fn repeat_key_action(
        &mut self,
        event: &Event,
        times: u16,
        action: &KeyAction,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        self.repeater = None;
        for index in 0..times {
            let checkpoint = self.rpc_command_checkpoint();
            if self
                .handle_key_action(event, action, buffer, runtime)
                .await?
            {
                return Ok(true);
            }
            if let Some(command) = self.rpc_command_started_since(checkpoint) {
                self.defer_rpc_action(
                    command,
                    RpcContinuation::Keys(
                        event.clone(),
                        KeyAction::Repeating(times - index - 1, Box::new(action.clone())),
                    ),
                );
                break;
            }
            if self.replay_checkpoint(buffer, runtime).await? {
                break;
            }
        }
        Ok(false)
    }

    pub(super) async fn execute_command_actions(
        &mut self,
        actions: Vec<Action>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        for (index, action) in actions.iter().enumerate() {
            let checkpoint = self.rpc_command_checkpoint();
            self.set_legacy_message(None);
            if self.execute(action, buffer, runtime).await? {
                return Ok(true);
            }
            if let Some(command) = self.rpc_command_started_since(checkpoint) {
                self.defer_rpc_action(
                    command,
                    RpcContinuation::Commands(actions[index + 1..].to_vec()),
                );
                break;
            }
        }
        Ok(false)
    }
}
