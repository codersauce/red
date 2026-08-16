//! Presentation data stays separate from retained conversation and source identity.
use super::*;

pub(super) fn relative_location(location: &InlineLocation, cwd: &Path) -> String {
    let file = Path::new(&location.file)
        .strip_prefix(cwd)
        .unwrap_or(Path::new(&location.file));
    let start = location.range.start.line + 1;
    let end = (location.range.end.line + usize::from(location.range.end.character > 0)).max(start);
    crate::notification::single_line(&if start == end {
        format!("{}:{start}", file.display())
    } else {
        format!("{}:{start}–{end}", file.display())
    })
}

pub(super) fn source_status(state: InlineSourceState) -> HistoryStatus {
    HistoryStatus::new(
        state.label(),
        match state {
            InlineSourceState::Unchanged => HistoryTone::Muted,
            InlineSourceState::Changed | InlineSourceState::Detached => HistoryTone::Warning,
        },
    )
}

impl Editor {
    pub(super) fn history_turn_detail(
        &self,
        turn: &InlineHistoryTurn,
        state: InlineSourceState,
    ) -> HistoryDetail {
        let cwd = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| {
                conversation
                    .turns
                    .iter()
                    .any(|candidate| candidate.request_id == turn.request_id)
            })
            .map_or_else(get_workspace_path, |conversation| {
                PathBuf::from(&conversation.cwd)
            });
        let view = self
            .inline_history_browser
            .as_ref()
            .map_or(HistoryView::default(), |browser| browser.view);
        let has_change = turn.has_code_change();
        let tone = match turn.state {
            InlineTurnState::Failed | InlineTurnState::Rejected => HistoryTone::Error,
            InlineTurnState::Ready => HistoryTone::Warning,
            InlineTurnState::Completed if turn.disposition == InlineDisposition::Kept => {
                HistoryTone::Success
            }
            InlineTurnState::Pending => HistoryTone::Info,
            _ => HistoryTone::Muted,
        };
        let status = if turn.state == InlineTurnState::Pending {
            "Working".into()
        } else if has_change && turn.disposition == InlineDisposition::Kept {
            "✓ Applied".into()
        } else {
            turn.status().to_owned()
        };
        let mut statuses = vec![HistoryStatus::new(status, tone)];
        if has_change {
            if let Some((index, _, InlineSourceState::Unchanged)) =
                self.resolve_inline_change_source(turn)
            {
                if self.buffer_manager[index].is_dirty() {
                    statuses.push(HistoryStatus::new("Unsaved", HistoryTone::Warning));
                }
            }
        }
        statuses.push(source_status(state));
        let file = &turn.location.file;
        let code = |source: &str| HistoryBlock::Code {
            file: file.clone(),
            source: source.into(),
        };
        let diff = |before: &str, after: &str, label: &str| HistoryBlock::Diff {
            file: file.clone(),
            before: before.into(),
            after: after.into(),
            label: label.into(),
        };
        let mut blocks = Vec::new();
        match view {
            HistoryView::Conversation => {
                blocks.push(HistoryBlock::Request(turn.prompt.clone()));
                if has_change {
                    let count = turn.change_summary.as_ref().map_or_else(
                        || {
                            crate::inline_history::InlineChangeSummary::new(
                                &turn.before,
                                turn.reviewed(),
                            )
                            .hunks
                            .len()
                        },
                        |summary| summary.hunks.len(),
                    );
                    blocks.push(HistoryBlock::Plain(format!(
                        "1 file · {count} changed location{}",
                        if count == 1 { "" } else { "s" }
                    )));
                }
                if let Some((before, after)) = turn.proposed_edit() {
                    blocks.push(HistoryBlock::Markdown(turn.proposal_description()));
                    blocks.push(diff(before, after, "proposed"));
                } else if !has_change
                    || !turn.answer.is_empty()
                    || turn
                        .result
                        .as_ref()
                        .is_some_and(|result| !result.comments.is_empty())
                {
                    blocks.push(HistoryBlock::Markdown(turn.answer_text()));
                }
                if let Some(error) = &turn.error {
                    blocks.push(HistoryBlock::Plain(format!("Outcome: {error}")));
                }
                if !turn.context_reads.is_empty() {
                    blocks.push(HistoryBlock::Plain(format!(
                        "Context read:\n{}",
                        turn.context_reads.join("\n")
                    )));
                }
            }
            HistoryView::Reviewed => blocks.extend([
                HistoryBlock::Plain(format!(
                    "{}\nReviewed source · read-only",
                    history_location_label(&turn.location)
                )),
                code(turn.reviewed()),
            ]),
            HistoryView::Before => blocks.extend([
                HistoryBlock::Plain(format!(
                    "{}\nBefore edit · read-only",
                    history_location_label(&turn.location)
                )),
                code(&turn.before),
            ]),
            HistoryView::Compare => {
                if let Some((index, range, _)) = self.resolve_history_turn(turn) {
                    let current = self.buffer_manager[index].text_in_range(range);
                    blocks.push(diff(turn.reviewed(), &current, "current source"));
                } else {
                    blocks.push(HistoryBlock::Plain(
                        "Source detached; current comparison unavailable.".into(),
                    ));
                }
            }
            HistoryView::Changes if has_change => {
                blocks.push(diff(&turn.before, turn.reviewed(), "after inline edit"))
            }
            HistoryView::Changes => blocks.push(HistoryBlock::Plain(
                "No applied code changes in this turn.".into(),
            )),
        }
        HistoryDetail {
            location: Some(relative_location(&turn.location, &cwd)),
            can_jump: state != InlineSourceState::Detached,
            cwd,
            statuses,
            blocks,
            view,
            open_label: if has_change {
                "review changes"
            } else if turn.proposed_edit().is_some() {
                "review proposal"
            } else if turn.state == InlineTurnState::Pending {
                "reopen request"
            } else {
                "open answer"
            },
        }
    }
}
