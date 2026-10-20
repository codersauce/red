//! Buffer-management actions isolated from recursive editing dispatch frames.

use super::*;

impl Editor {
    /// Keep buffer management's larger async frame off nested edit/replay stacks.
    #[inline(never)]
    pub(super) fn execute_buffer_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(self.execute_buffer_action_impl(action, buffer))
    }

    async fn execute_buffer_action_impl(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<bool> {
        match action {
            Action::ListBuffers => {
                let active = self.buffer_manager.active_index();
                let alternate = self.buffer_manager.alternate_index();
                let listing = self
                    .buffer_manager
                    .iter()
                    .enumerate()
                    .map(|(index, source)| {
                        let status = if index == active {
                            "%a"
                        } else if alternate == Some(index) {
                            " #"
                        } else {
                            "  "
                        };
                        let modified = if source.is_dirty() { " [+]" } else { "" };
                        format!(
                            "{} {status} {:?}{modified}",
                            source.id().as_u64(),
                            source.name()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("  |  ");
                self.set_legacy_message(Some(listing));
                self.render(buffer)?;
            }
            Action::NewBuffer => {
                if !self.current_buffer().is_unnamed()
                    || !self.current_buffer().is_blank()
                    || self.current_buffer().is_dirty()
                {
                    self.buffer_manager
                        .push_buffer(Buffer::new(/*file*/ None, String::new()));
                    let index = self.buffer_manager.len() - 1;
                    self.set_current_buffer(buffer, index).await?;
                }
            }
            Action::OpenBufferById(id) => {
                if let Some(index) = self
                    .buffer_manager
                    .iter()
                    .position(|source| source.id().as_u64() == *id)
                {
                    self.set_current_buffer(buffer, index).await?;
                } else {
                    self.set_legacy_message(Some(format!("No buffer matching {id}")));
                    self.render(buffer)?;
                }
            }
            Action::OpenBuffer(name) => match self.named_buffer_index(name) {
                Ok(index) => self.set_current_buffer(buffer, index).await?,
                Err(message) => {
                    self.set_legacy_message(Some(message));
                    self.render(buffer)?;
                }
            },
            Action::SetBufferName(name) => {
                if self
                    .scratch_buffers
                    .contains_key(&self.current_buffer().id())
                {
                    self.set_legacy_message(Some("Scratch buffers cannot be renamed".to_string()));
                    self.render(buffer)?;
                    return Ok(false);
                }

                let path = match normalized_file_path(name) {
                    Ok(path) => path,
                    Err(error) => {
                        self.set_legacy_message(Some(error.to_string()));
                        self.render(buffer)?;
                        return Ok(false);
                    }
                };
                let index = self.buffer_manager.active_index();
                if self
                    .file_buffer_index(&path)
                    .is_some_and(|existing| existing != index)
                {
                    self.set_legacy_message(Some(format!(
                        "A buffer already exists for {:?}",
                        path.display().to_string()
                    )));
                    self.render(buffer)?;
                    return Ok(false);
                }

                let previous_uri = self.current_buffer().uri()?;
                let file = path.to_string_lossy().into_owned();
                self.current_buffer_mut().file = Some(file.clone());
                self.rebind_inline_history_file(&file);
                if let Err(error) = self
                    .sync_lsp_document_identity(previous_uri.as_deref(), index)
                    .await
                {
                    self.report_diagnostics_lsp_failure("rename document", &error);
                } else {
                    self.set_legacy_message(Some(format!("{file:?}")));
                }
                self.render(buffer)?;
            }
            Action::SplitHorizontalNewBuffer | Action::SplitVerticalNewBuffer => {
                self.buffer_manager
                    .push_buffer(Buffer::new(/*file*/ None, String::new()));
                let index = self.buffer_manager.len() - 1;
                let vertical = matches!(action, Action::SplitVerticalNewBuffer);
                let created = self.update_window_layout(|windows| {
                    if vertical {
                        windows.split_vertical(index)
                    } else {
                        windows.split_horizontal(index)
                    }
                });
                if created {
                    self.request_diagnostics().await?;
                    self.render(buffer)?;
                } else {
                    self.buffer_manager.pop_buffer();
                }
            }
            _ => unreachable!("only buffer-management actions are dispatched here"),
        }

        Ok(true)
    }
}
