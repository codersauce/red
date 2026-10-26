//! Translate Vimlet operations into Red buffer transactions and prompt layout requests.

use super::{normalize_newlines, TextArea, TextAreaOutcome};
use crate::{
    editing::{apply_transactional_replacement, plain_line, reflow_text},
    editor::Mode,
    keyboard::is_word_backspace,
    text_layout::{LayoutOptions, TextLayout},
    undo::{CursorSnapshot, TextRange},
    unicode_utils::delete_last_word,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ropey::Rope;
use vimlet::{
    Action, AppliedEdit, Binding, BindingContext, ByteOffset, Command, DocumentVersion,
    DocumentView, HistoryCommand, HostOperation, HostOutcome, HostResult, Input, Key,
    MotionResolver, OperationKind, Operator, Replacement, ResolvedRange, Selection, Shape, Step,
    TargetRequest, TerminalGeometry, TextSource, Transaction,
};

const ROW_UP: u64 = 0;
const ROW_DOWN: u64 = 1;

#[derive(Debug, Clone)]
pub(super) struct Search {
    pub(super) pattern: String,
    operation: HostOperation,
    operator: bool,
    backward: bool,
    count: usize,
}

impl TextArea {
    pub(super) fn dispatch_event(
        &mut self,
        event: &Event,
        layout: LayoutOptions,
    ) -> TextAreaOutcome {
        let key = match event {
            Event::Key(key) if key.kind == KeyEventKind::Release => {
                return TextAreaOutcome::Changed
            }
            Event::Key(key) if self.search.is_some() => return self.search_key(*key, layout),
            Event::Key(key) if is_word_backspace(*key) => {
                self.delete_previous_word();
                return TextAreaOutcome::Changed;
            }
            Event::Key(key) => match modal_key(*key) {
                Some(key) => key,
                None => return TextAreaOutcome::Unhandled,
            },
            Event::Paste(text) if self.mode() == Mode::Insert => {
                Key::Text(normalize_newlines(text).into_owned())
            }
            Event::Paste(text) if self.search.is_none() => {
                self.session
                    .lifecycle(&mut self.engine, vimlet::Lifecycle::CancelPending);
                self.state.pending = false;
                return if self.insert(text) {
                    TextAreaOutcome::Changed
                } else {
                    TextAreaOutcome::Unhandled
                };
            }
            _ => return TextAreaOutcome::Unhandled,
        };
        if key == Key::Escape && !self.engine.owns_escape() {
            return TextAreaOutcome::Unhandled;
        }
        let inserted = self.mode() == Mode::Insert
            && matches!(key, Key::Text(_) | Key::Char(_) | Key::Tab | Key::Enter);
        let revision = self.buffer.revision();
        let outcome = self.drive(Step::Replay(Input::key(key)), layout);
        if inserted && self.buffer.revision() == revision {
            TextAreaOutcome::Unhandled
        } else {
            outcome
        }
    }

    fn version(&self) -> DocumentVersion {
        DocumentVersion {
            id: self.buffer.id().as_u64(),
            revision: self.buffer.revision(),
        }
    }

    fn view<'a>(&self, rope: &'a Rope, geometry: &'a TerminalGeometry) -> DocumentView<'a> {
        DocumentView {
            geometry,
            selection: self.selection,
            ..DocumentView::new(rope, self.version(), self.cursor_byte)
        }
    }

    fn drive(&mut self, mut step: Step, layout: LayoutOptions) -> TextAreaOutcome {
        let geometry = TerminalGeometry {
            tabstop: layout.tab_width,
        };
        let mut operator = false;
        let mut key = None;
        self.register.take();
        loop {
            step = match step {
                Step::Replay(mut input) => {
                    let context = self.engine.binding_context();
                    operator = context == BindingContext::OperatorPending;
                    if matches!(context, BindingContext::Normal | BindingContext::Visual) {
                        let id = match input.key {
                            Key::Up | Key::Char('k') => Some(ROW_UP),
                            Key::Down | Key::Char('j') => Some(ROW_DOWN),
                            _ => None,
                        };
                        if let Some(id) = id {
                            input.binding = Binding::Command(Command::new(
                                Action::Target(TargetRequest::Geometry { id }),
                                0,
                            ));
                        }
                    }
                    key = Some(input.key.clone());
                    let rope = self.buffer.contents_snapshot();
                    let view = self.view(&rope, &geometry);
                    self.session.handle(&mut self.engine, input, view)
                }
                Step::HostOperation(op) => {
                    if op.is_current(self.version()) {
                        if let OperationKind::Resolve {
                            request:
                                TargetRequest::Search {
                                    backward,
                                    repeat: false,
                                },
                            count,
                        } = op.kind
                        {
                            self.search = Some(Search {
                                pattern: String::new(),
                                operation: op,
                                backward,
                                count,
                                operator,
                            });
                            self.state.mode = Mode::Search;
                            self.state.pending = true;
                            return TextAreaOutcome::Changed;
                        }
                    }
                    let outcome = self.apply_operation(&op, layout, key.as_ref(), operator);
                    self.acknowledge(&op, outcome, &geometry)
                }
                Step::Consumed | Step::AwaitingInput if self.session.is_playing() => {
                    self.session.next_macro_input(&self.engine, self.version())
                }
                terminal => {
                    self.state.mode = match self.engine.mode() {
                        vimlet::Mode::Normal => Mode::Normal,
                        vimlet::Mode::Insert | vimlet::Mode::Replace => Mode::Insert,
                        vimlet::Mode::Visual => Mode::Visual,
                        vimlet::Mode::VisualLine => Mode::VisualLine,
                        vimlet::Mode::VisualBlock => Mode::VisualBlock,
                    };
                    self.state.pending = self.engine.has_pending_input();
                    return match terminal {
                        Step::Unhandled => TextAreaOutcome::Unhandled,
                        Step::Error(error) => {
                            crate::log_warn!("embedded Vim input failed: {error}");
                            TextAreaOutcome::Changed
                        }
                        _ => TextAreaOutcome::Changed,
                    };
                }
            }
        }
    }

    fn acknowledge(
        &mut self,
        op: &HostOperation,
        outcome: HostOutcome,
        geometry: &TerminalGeometry,
    ) -> Step {
        let rope = self.buffer.contents_snapshot();
        let view = self.view(&rope, geometry);
        let result = HostResult {
            operation: op.id,
            before: op.expected,
            after: view.version,
            cursor: view.cursor,
            selection: view.selection,
            outcome,
        };
        self.session.resume(&mut self.engine, result, view)
    }

    fn apply_operation(
        &mut self,
        op: &HostOperation,
        layout: LayoutOptions,
        key: Option<&Key>,
        operator: bool,
    ) -> HostOutcome {
        if !op.is_current(self.version()) {
            return HostOutcome::Rejected;
        }
        let rope = self.buffer.contents_snapshot();
        let geometry = TerminalGeometry {
            tabstop: layout.tab_width,
        };
        match &op.kind {
            OperationKind::Move { cursor, selection } => {
                let preferred = self.state.preferred_column;
                self.place(
                    &rope,
                    (self.cursor_byte.0, self.cursor()),
                    *cursor,
                    *selection,
                );
                if matches!(key, Some(Key::Up | Key::Down | Key::Char('j' | 'k'))) {
                    self.state.preferred_column = preferred;
                }
            }
            OperationKind::Edit {
                replacements,
                cursor,
                selection,
                transaction,
            } => {
                let mut cursor = *cursor;
                let mut column = None;
                if *transaction == Transaction::BreakInsert {
                    match key {
                        Some(Key::Left) => {
                            cursor = ByteOffset(rope.prev_grapheme(self.cursor_byte.0))
                        }
                        Some(Key::Right) => {
                            cursor = ByteOffset(rope.next_grapheme(self.cursor_byte.0))
                        }
                        Some(Key::Home) => cursor = ByteOffset(0),
                        Some(Key::End) => cursor = ByteOffset(rope.len_bytes()),
                        Some(Key::Up | Key::Down) => {
                            cursor = self.row_target(layout, key == Some(&Key::Up), 1);
                            column = self.state.preferred_column;
                        }
                        _ => {}
                    }
                }
                let outcome =
                    self.apply_edits(&rope, replacements, cursor, *selection, *transaction);
                self.state.preferred_column = column;
                return outcome;
            }
            OperationKind::History { command, count } => {
                for _ in 0..*count {
                    let changed = match command {
                        HistoryCommand::Undo => self.undo(),
                        HistoryCommand::Redo => self.redo(),
                        HistoryCommand::UndoLine => return HostOutcome::Rejected,
                    };
                    if !changed {
                        break;
                    }
                }
                self.selection = None;
                self.state.selection_anchor = None;
            }
            OperationKind::Transform {
                operator: Operator::Format,
                range,
                ..
            } => {
                let first = range.ranges.first().unwrap().start.0;
                let last = range.ranges.last().unwrap().end.0;
                let start = rope.line_start(first);
                let end = if last > start && rope.line_start(last) == last {
                    last
                } else {
                    rope.next_line(last)
                };
                let formatted = reflow_text(
                    &rope.byte_slice(start..end).to_string(),
                    layout.width.clamp(1, 79),
                    layout.tab_width,
                    plain_line,
                );
                let body = formatted.trim_end_matches(['\n', '\r']);
                let line_start = body.rfind('\n').map_or(0, |i| i + 1);
                let cursor = start + line_start + body[line_start..].len()
                    - body[line_start..].trim_start().len();
                return self.apply_edits(
                    &rope,
                    &[Replacement {
                        range: vimlet::TextRange::new(start, end),
                        text: formatted,
                    }],
                    ByteOffset(cursor),
                    None,
                    Transaction::Atomic,
                );
            }
            OperationKind::Resolve {
                request:
                    TargetRequest::Geometry {
                        id: ROW_UP | ROW_DOWN,
                    },
                count,
            } => {
                let up = matches!(
                    op.kind,
                    OperationKind::Resolve {
                        request: TargetRequest::Geometry { id: ROW_UP },
                        ..
                    }
                );
                let target = self.row_target(layout, up, *count).0;
                return HostOutcome::Target(ResolvedRange::character(target, target));
            }
            OperationKind::Resolve {
                request: TargetRequest::Block { selection },
                ..
            } => {
                let resolver = MotionResolver::from_view(self.view(&rope, &geometry));
                let a = resolver.column(selection.anchor.0);
                let b = resolver.column(selection.cursor.0);
                let first = rope.byte_to_line(selection.anchor.0.min(selection.cursor.0));
                let last = rope.byte_to_line(selection.anchor.0.max(selection.cursor.0));
                let mut ranges = Vec::new();
                for line in first..=last {
                    let start = rope.line_to_byte(line);
                    let end = rope.line_end(start);
                    if resolver.column(end) <= a.min(b) {
                        continue;
                    }
                    let left = resolver.at_column(start, a.min(b));
                    let right = resolver.at_column(start, a.max(b));
                    ranges.push(vimlet::TextRange::new(left, resolver.next(right).min(end)));
                }
                return HostOutcome::Target(ResolvedRange {
                    ranges,
                    shape: Shape::Block {
                        width: a.abs_diff(b) + 1,
                    },
                });
            }
            OperationKind::Resolve {
                request:
                    TargetRequest::Search {
                        backward,
                        repeat: true,
                    },
                count,
            } => {
                let Some((pattern, original_backward)) = &self.last_search else {
                    return HostOutcome::Rejected;
                };
                return self.search_target(
                    pattern,
                    *original_backward ^ backward,
                    *count,
                    operator,
                );
            }
            _ => return HostOutcome::Rejected,
        }
        HostOutcome::Applied {
            edits: Vec::new(),
            changed: self.version() != op.expected,
        }
    }

    fn row_target(&mut self, options: LayoutOptions, up: bool, count: usize) -> ByteOffset {
        let text = self.text();
        let layout = TextLayout::new(&text, options);
        let Some(position) = layout.position(self.cursor()) else {
            return self.cursor_byte;
        };
        let column = *self.state.preferred_column.get_or_insert(position.column);
        let row = if up {
            position.row.saturating_sub(count)
        } else {
            position
                .row
                .saturating_add(count)
                .min(layout.rows().len().saturating_sub(1))
        };
        layout
            .nearest_offset_on_row(row, column)
            .map_or(self.cursor_byte, |offset| {
                ByteOffset(crate::unicode_utils::grapheme_to_byte(&text, offset))
            })
    }

    fn apply_edits(
        &mut self,
        rope: &Rope,
        replacements: &[Replacement],
        cursor: ByteOffset,
        selection: Option<Selection>,
        transaction: Transaction,
    ) -> HostOutcome {
        if replacements.iter().any(|r| {
            r.range.start > r.range.end
                || !rope.is_char_boundary(r.range.start.0)
                || !rope.is_char_boundary(r.range.end.0)
        }) || replacements
            .windows(2)
            .any(|pair| pair[0].range.end > pair[1].range.start)
        {
            return HostOutcome::Rejected;
        }
        let mut edits = Vec::with_capacity(replacements.len());
        let mut new_len = rope.len_bytes();
        for r in replacements {
            let removed = rope.byte_slice(r.range.as_range()).to_string();
            new_len = new_len - removed.len() + r.text.len();
            edits.push(AppliedEdit {
                range: r.range,
                removed,
                inserted: r.text.clone(),
            });
        }
        if new_len > self.max_bytes {
            // Prompt input at capacity is consumed. Playback can still reach the following Escape.
            return if self.engine.mode() == vimlet::Mode::Insert
                && matches!(transaction, Transaction::Begin | Transaction::Continue)
            {
                HostOutcome::Applied {
                    edits: Vec::new(),
                    changed: false,
                }
            } else {
                HostOutcome::Rejected
            };
        }
        let mut reference = (self.cursor_byte.0, self.cursor());
        if let Some(first) = edits.first() {
            let byte = rope.prev_grapheme(first.range.start.0.min(reference.0));
            reference = (byte, index_from(rope, reference, byte));
        }
        // Text typed into a freshly opened prompt has its own undo entry. An explicit
        // Vim insert/change already opened a transaction before issuing its first edit.
        let prompt_edit = self.engine.mode() == vimlet::Mode::Insert
            && !self.buffer.undo_history.is_transaction_active()
            && matches!(transaction, Transaction::Begin | Transaction::Continue);
        if !self.buffer.undo_history.is_transaction_active()
            && (!edits.is_empty() || transaction == Transaction::Begin)
        {
            let (x, y) = self.buffer.pos;
            self.buffer
                .undo_history
                .begin_transaction("vim edit", CursorSnapshot::new(x, y, 0));
        }
        let mut changed = false;
        for edit in edits.iter().rev() {
            let start = self
                .buffer
                .char_idx_to_position(rope.byte_to_char(edit.range.start.0));
            let end = self
                .buffer
                .char_idx_to_position(rope.byte_to_char(edit.range.end.0));
            changed |= apply_transactional_replacement(
                &mut self.buffer,
                TextRange::new(start, end),
                &edit.inserted,
            )
            .is_some();
        }
        let updated = self.buffer.contents_snapshot();
        let cursor = if matches!(transaction, Transaction::Atomic | Transaction::End) {
            ByteOffset(MotionResolver::new(&updated, cursor).normal_cursor(cursor.0))
        } else {
            cursor
        };
        self.place(&updated, reference, cursor, selection);
        if prompt_edit
            || matches!(
                transaction,
                Transaction::Atomic | Transaction::BreakInsert | Transaction::End
            )
        {
            self.close_transaction();
        }
        HostOutcome::Applied { edits, changed }
    }

    fn place(
        &mut self,
        rope: &Rope,
        reference: (usize, usize),
        cursor: ByteOffset,
        selection: Option<Selection>,
    ) {
        // An inserted scalar can join an existing grapheme (for example before a combining mark).
        let mut byte = cursor.0.min(rope.len_bytes());
        if byte > 0 {
            byte = rope.next_grapheme(rope.prev_grapheme(byte));
        }
        self.cursor_byte = ByteOffset(byte);
        self.state.cursor = index_from(rope, reference, byte);
        self.selection = selection.map(|s| Selection {
            cursor: self.cursor_byte,
            ..s
        });
        self.state.selection_anchor =
            selection.map(|s| index_from(rope, (byte, self.state.cursor), s.anchor.0));
        self.state.preferred_column = None;
        let line = rope.byte_to_line(byte);
        self.buffer.pos = (index_from(rope, (rope.line_to_byte(line), 0), byte), line);
    }

    fn search_key(&mut self, key: KeyEvent, layout: LayoutOptions) -> TextAreaOutcome {
        let search = self.search.as_mut().unwrap();
        if is_word_backspace(key)
            || (key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            delete_last_word(&mut search.pattern);
            return TextAreaOutcome::Changed;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                let search = self.search.take().unwrap();
                let outcome = if key.code == KeyCode::Esc {
                    HostOutcome::Rejected
                } else {
                    if !search.pattern.is_empty() {
                        self.last_search = Some((search.pattern, search.backward));
                    }
                    self.last_search
                        .as_ref()
                        .map_or(HostOutcome::Rejected, |(pattern, _)| {
                            self.search_target(
                                pattern,
                                search.backward,
                                search.count,
                                search.operator,
                            )
                        })
                };
                let step = self.acknowledge(
                    &search.operation,
                    outcome,
                    &TerminalGeometry {
                        tabstop: layout.tab_width,
                    },
                );
                self.drive(step, layout)
            }
            KeyCode::Backspace => {
                search.pattern.pop();
                TextAreaOutcome::Changed
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                search.pattern.push(c);
                TextAreaOutcome::Changed
            }
            _ => TextAreaOutcome::Unhandled,
        }
    }

    fn search_target(
        &self,
        pattern: &str,
        backward: bool,
        count: usize,
        operator: bool,
    ) -> HostOutcome {
        let text = self.text();
        let origin = self.cursor_byte.0;
        let mut pos = origin;
        for _ in 0..count {
            let found = if backward {
                text[..pos].rfind(pattern).or_else(|| text.rfind(pattern))
            } else {
                let after = text.next_grapheme(pos);
                text[after..]
                    .find(pattern)
                    .map(|i| after + i)
                    .or_else(|| text.find(pattern))
            };
            let Some(next) = found else {
                return HostOutcome::Rejected;
            };
            pos = next;
        }
        if operator {
            MotionResolver::new(&text, ByteOffset(origin))
                .exclusive_range(ByteOffset(pos))
                .map_or(HostOutcome::Rejected, HostOutcome::Target)
        } else {
            HostOutcome::Target(ResolvedRange::character(pos, pos))
        }
    }
}

pub(super) fn index_from(rope: &Rope, (mut byte, index): (usize, usize), target: usize) -> usize {
    if rope.len_bytes() == rope.len_chars() {
        return if target >= byte {
            index + (target - byte)
        } else {
            index - (byte - target)
        };
    }
    let mut index = index;
    if byte < target {
        while byte < target {
            byte = rope.next_grapheme(byte);
            index += 1;
        }
    } else {
        while byte > target {
            byte = rope.prev_grapheme(byte);
            index -= 1;
        }
    }
    index
}

fn modal_key(key: KeyEvent) -> Option<Key> {
    Some(match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Key::Ctrl(c.to_ascii_lowercase())
        }
        KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::ALT) => return None,
        KeyCode::Char('\n') | KeyCode::Enter => Key::Enter,
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Esc => Key::Escape,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Tab => Key::Tab,
        _ => return None,
    })
}
