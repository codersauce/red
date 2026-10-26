//! Resolve file searches, syntax queries and rectangular selections against the active buffer.

use crate::editor::{Editor, Mode, RenderBuffer, Runtime};
use vimlet::{ByteOffset, EditTarget, HostOutcome, HostResult, OperationKind, Shape, TextSource};

// These two ASCII bytes identify the same syntax target when dot runs at a new cursor.
pub(in crate::editor) fn syntax_target(prefix: u8, key: char) -> EditTarget {
    EditTarget::Host(vimlet::TargetRequest::Syntax {
        id: u64::from(u16::from_be_bytes([prefix, key as u8])),
    })
}

impl Editor {
    pub(super) fn file_block_target(&self, selection: vimlet::Selection) -> HostOutcome {
        let rope = self.current_buffer().contents_snapshot();
        let geometry = vimlet::TerminalGeometry {
            tabstop: self.active_tab_width(),
        };
        let resolver = vimlet::MotionResolver::from_view(self.file_view(&rope, &geometry));
        let first_line = rope.byte_to_line(selection.anchor.0.min(selection.cursor.0));
        let last_line = rope.byte_to_line(selection.anchor.0.max(selection.cursor.0));
        let a = resolver.column(selection.anchor.0);
        let b = resolver.column(selection.cursor.0);
        let left = a.min(b);
        let right = a.max(b);
        let ranges = (first_line..=last_line)
            .map(|line| {
                let start = rope.line_to_byte(line);
                let end = rope.line_end(start);
                if resolver.column(end) <= left {
                    return vimlet::TextRange::new(end, end);
                }
                let from = resolver.at_column(start, left);
                let to = resolver.next(resolver.at_column(start, right)).min(end);
                vimlet::TextRange::new(from, to)
            })
            .collect();
        HostOutcome::Target(vimlet::ResolvedRange {
            ranges,
            shape: Shape::Block {
                width: right - left + 1,
            },
        })
    }

    pub(in crate::editor) fn finish_file_search<'a>(
        &'a mut self,
        commit: bool,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> crate::editor::BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let Some(op) = self.file_vim.search.take() else {
                return Ok(());
            };
            let session = self.active_search.take();
            self.mode = Mode::Normal;
            if !op.is_current(self.file_version()) || session.is_none() {
                self.file_vim
                    .session
                    .lifecycle(&mut self.file_vim.state, vimlet::Lifecycle::CancelPending);
                self.set_legacy_message(Some("buffer changed during operator search".to_owned()));
                self.render(buffer)?;
                return Ok(());
            }
            let session = session.unwrap();
            self.restore_search_origin(&session.origin, session.origin_vtop);
            let OperationKind::Resolve { count, .. } = op.kind else {
                unreachable!()
            };
            let pattern = if session.draft.is_empty() {
                self.search_term.clone()
            } else {
                session.draft
            };
            let outcome = if commit && !pattern.is_empty() {
                self.record_search_history(&pattern);
                self.search_direction = session.direction;
                self.search_term.clone_from(&pattern);
                self.search_highlights_suppressed = false;
                let outcome = self.file_search_target(&pattern, session.direction, count);
                if matches!(outcome, HostOutcome::RememberedTarget { .. }) {
                    self.file_vim.last_search = Some(pattern);
                }
                outcome
            } else {
                HostOutcome::Rejected
            };
            let rope = self.current_buffer().contents_snapshot();
            let geometry = vimlet::TerminalGeometry {
                tabstop: self.active_tab_width(),
            };
            let view = self.file_view(&rope, &geometry);
            let result = HostResult {
                operation: op.id,
                before: op.expected,
                after: view.version,
                selection: view.selection,
                cursor: view.cursor,
                outcome,
            };
            let mut vim = std::mem::take(&mut self.file_vim);
            let step = vim.session.resume(&mut vim.state, result, view);
            let result = self
                .finish_file_steps(&mut vim, step, None, buffer, runtime)
                .await;
            self.file_vim = vim;
            result?;
            self.render(buffer)
        })
    }

    pub(super) fn file_search_target(
        &mut self,
        pattern: &str,
        direction: crate::editor::SearchDirection,
        count: usize,
    ) -> HostOutcome {
        let mut origin = self.current_history_entry();
        let cursor = self.file_cursor_byte().0;
        let mut target = cursor;
        for _ in 0..count {
            match self.search_match_from_origin(
                pattern,
                &origin,
                direction,
                self.config.search.wrapscan,
            ) {
                Ok(Some(found)) => {
                    origin.y = found.start_y;
                    origin.x = self.char_to_grapheme_on_line(found.start_x, found.start_y);
                    target = self.current_buffer().position_to_byte_idx(
                        crate::editor::TextPosition::new(found.start_y, found.start_x),
                    );
                }
                Ok(None) => {
                    self.set_routine_warning(Some(Self::pattern_not_found_message(pattern)));
                    return HostOutcome::Rejected;
                }
                Err(error) => {
                    self.set_legacy_message(Some(error.to_string()));
                    return HostOutcome::Rejected;
                }
            }
        }
        let rope = self.current_buffer().contents_snapshot();
        let Some(range) = vimlet::MotionResolver::new(&rope, ByteOffset(cursor))
            .exclusive_range(ByteOffset(target))
        else {
            return HostOutcome::Rejected;
        };
        HostOutcome::RememberedTarget {
            range,
            replay: vimlet::TargetRequest::Search {
                backward: direction == crate::editor::SearchDirection::Backward,
                repeat: true,
            },
        }
    }

    pub(super) fn file_syntax_target(
        &mut self,
        request: vimlet::TargetRequest,
        id: u64,
        count: usize,
    ) -> HostOutcome {
        let Ok(id) = u16::try_from(id) else {
            return HostOutcome::Rejected;
        };
        let [prefix, key] = id.to_be_bytes();
        let key = char::from(key);
        let (range, shape) = match prefix {
            b'g' if key == 'c' => {
                let Some(range) = self.comment_text_object_range() else {
                    return HostOutcome::Rejected;
                };
                (range, Shape::Line)
            }
            b'%' => {
                let Some(range) = self.matchit_motion_range(crate::editor::MatchDirection::Forward)
                else {
                    self.set_routine_warning(Some("match not found".into()));
                    return HostOutcome::Rejected;
                };
                (range, Shape::Character)
            }
            b'[' | b']' => {
                let Some(kind) = crate::editor::structural_motion_kind(key) else {
                    return HostOutcome::Rejected;
                };
                let Some(range) = self.syntax_motion_range(
                    kind,
                    prefix == b'[',
                    u16::try_from(count).unwrap_or(u16::MAX),
                ) else {
                    if self.last_error.is_none() {
                        self.set_navigation_boundary_warning(
                            crate::editor::structural_motion_label(key),
                        );
                    }
                    return HostOutcome::Rejected;
                };
                (range, Shape::Character)
            }
            b'i' | b'a' => {
                let Some(kind) = crate::editor::SyntaxObjectKind::for_text_object_key(key) else {
                    return HostOutcome::Rejected;
                };
                let scope = if prefix == b'i' {
                    crate::editor::TextObjectScope::Inner
                } else {
                    crate::editor::TextObjectScope::Around
                };
                let Some(object) = self.syntax_text_object(scope, kind) else {
                    self.set_legacy_message(Some("text object not found".to_owned()));
                    return HostOutcome::Rejected;
                };
                if object.linewise {
                    (self.linewise_text_object_range(object.range), Shape::Line)
                } else {
                    (object.range, Shape::Character)
                }
            }
            _ => return HostOutcome::Rejected,
        };
        let source = self.current_buffer();
        let range = vimlet::ResolvedRange {
            ranges: vec![vimlet::TextRange::new(
                source.position_to_byte_idx(range.start),
                source.position_to_byte_idx(range.end),
            )],
            shape,
        };
        HostOutcome::RememberedTarget {
            range,
            replay: request,
        }
    }
}
