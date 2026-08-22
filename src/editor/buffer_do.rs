//! Save-all and safe iteration of non-interactive Ex commands over open buffers.

use super::*;

impl Editor {
    #[inline(never)]
    pub(super) fn execute_write_all<'a>(
        &'a mut self,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.execute_write_all_impl(buffer, runtime))
    }

    async fn execute_write_all_impl(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let original_id = self.current_buffer().id();
        let original_view = (self.cx, self.cy, self.vtop, self.vleft, self.skipcol);
        let targets = self
            .buffer_manager
            .iter()
            .filter(|source| source.is_dirty())
            .map(Buffer::id)
            .collect::<Vec<_>>();
        let practice_buffer_id = self
            .tutorial_controller
            .as_ref()
            .map(|tutorial| tutorial.practice_buffer_id);
        let mut first_error = None;
        let mut operation_error = None;

        for target in targets {
            let Some(index) = self
                .buffer_manager
                .iter()
                .position(|source| source.id() == target)
            else {
                continue;
            };
            if practice_buffer_id == Some(target) {
                first_error.get_or_insert_with(|| {
                    "the Red tutorial practice buffer cannot be saved".to_string()
                });
                continue;
            }
            if self.buffer_manager[index].file.is_none() {
                first_error
                    .get_or_insert_with(|| format!("No file name for buffer {}", target.as_u64()));
                continue;
            }

            self.select_buffer_for_lsp_edit(index);
            match self.save_action(buffer, runtime).await {
                Ok(_) => {
                    if self.current_buffer().is_dirty()
                        && !self.buffer_has_pending_format_save(target)
                    {
                        first_error.get_or_insert_with(|| {
                            self.last_error.clone().unwrap_or_else(|| {
                                format!("Could not write buffer {}", target.as_u64())
                            })
                        });
                    }
                }
                Err(error) => {
                    operation_error = Some(error);
                    break;
                }
            }
        }

        if let Some(index) = self
            .buffer_manager
            .iter()
            .position(|source| source.id() == original_id)
        {
            self.select_buffer_for_lsp_edit(index);
        }
        (self.cx, self.cy, self.vtop, self.vleft, self.skipcol) = original_view;
        self.check_bounds();
        self.sync_inline_change_summaries();

        if let Some(error) = operation_error {
            return Err(error);
        }
        if let Some(error) = first_error {
            self.set_notification_message(Severity::Error, Some(error));
        }
        self.render(buffer)?;
        Ok(())
    }

    #[inline(never)]
    pub(super) fn execute_buffer_do<'a>(
        &'a mut self,
        command: &'a str,
        start: Option<u64>,
        end: Option<u64>,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.execute_buffer_do_impl(command, start, end, buffer, runtime))
    }

    async fn execute_buffer_do_impl(
        &mut self,
        command: &str,
        start: Option<u64>,
        end: Option<u64>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let mut targets = self
            .buffer_manager
            .iter()
            .map(Buffer::id)
            .filter(|id| start.is_none_or(|start| id.as_u64() >= start))
            .filter(|id| end.is_none_or(|end| id.as_u64() <= end))
            .collect::<Vec<_>>();
        targets.sort_unstable_by_key(|id| id.as_u64());

        if targets.is_empty() {
            self.set_legacy_message(Some("No buffers in range".to_string()));
            self.render(buffer)?;
            return Ok(());
        }

        for target in targets {
            let Some(index) = self
                .buffer_manager
                .iter()
                .position(|source| source.id() == target)
            else {
                self.set_legacy_message(Some(
                    "bufdo stopped because the buffer list changed".to_string(),
                ));
                self.render(buffer)?;
                return Ok(());
            };
            self.set_current_buffer(buffer, index).await?;

            let actions = self.handle_command(command, runtime);
            if actions.is_empty() {
                if self.last_error.is_none() {
                    self.set_legacy_message(Some(format!("bufdo could not execute {command:?}")));
                    self.render(buffer)?;
                }
                return Ok(());
            }
            if let Some(action) = actions
                .iter()
                .find(|action| !Self::action_is_buffer_do_safe(action))
            {
                let reason = if matches!(action, Action::Substitute(command) if command.confirm) {
                    "interactive substitute confirmation"
                } else {
                    "commands outside its supported non-interactive subset"
                };
                self.set_legacy_message(Some(format!(
                    "bufdo does not support {reason}: {command:?}"
                )));
                self.render(buffer)?;
                return Ok(());
            }

            for action in actions {
                if let Action::Substitute(substitute) = &action {
                    let substitutions = match self.plan_substitutions(substitute) {
                        Ok(substitutions) => substitutions,
                        Err(error) => {
                            self.set_legacy_message(Some(error.to_string()));
                            self.render(buffer)?;
                            return Ok(());
                        }
                    };
                    if substitutions.is_empty() {
                        if substitute.suppress_errors {
                            self.set_legacy_message(None);
                            continue;
                        }
                        self.set_legacy_message(Some("pattern not found".to_string()));
                        self.render(buffer)?;
                        return Ok(());
                    }
                }

                let save_without_file =
                    matches!(action, Action::Save) && self.current_buffer().file.is_none();
                let invalid_syntax = matches!(
                    &action,
                    Action::SetSyntax(syntax)
                        if !matches!(syntax.trim().to_ascii_lowercase().as_str(), "auto" | "off")
                            && self.highlighter.language_id_for_name(syntax).is_none()
                );
                let dirty_before = self.current_buffer().is_dirty();
                let blocked_reload = matches!(action, Action::ReloadFile(false)) && dirty_before;
                let blocked_delete = matches!(action, Action::DeleteBuffer(false)) && dirty_before;
                if self.execute(&action, buffer, runtime).await? {
                    return Ok(());
                }
                if blocked_reload || blocked_delete || save_without_file || invalid_syntax {
                    return Ok(());
                }
                if matches!(action, Action::Save)
                    && dirty_before
                    && self.current_buffer().is_dirty()
                    && !self.buffer_has_pending_format_save(target)
                {
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    fn action_is_buffer_do_safe(action: &Action) -> bool {
        matches!(
            action,
            Action::Save
                | Action::ReloadFile(_)
                | Action::Substitute(SubstituteCommand { confirm: false, .. })
                | Action::JoinLines(_)
                | Action::JoinLinesKeepSpaces(_)
                | Action::JoinLinesInRange { .. }
                | Action::GoToLine(_)
                | Action::MoveToBottom
                | Action::Print(_)
                | Action::ClearSearchHighlight
                | Action::SetWrap(_)
                | Action::SetRelativeLineNumbers(_)
                | Action::SetSyntax(_)
                | Action::DeleteBuffer(_)
        )
    }

    fn buffer_has_pending_format_save(&self, buffer_id: BufferId) -> bool {
        self.pending_lsp_format_saves.keys().any(|request_id| {
            self.pending_lsp_edit_requests
                .get(request_id)
                .is_some_and(|pending| pending.buffer_id == buffer_id)
        })
    }
}
