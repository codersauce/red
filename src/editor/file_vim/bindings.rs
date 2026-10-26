//! Translate file key mappings and pending operator suffixes into Vimlet input.

use super::syntax_target;
use crate::editor::{Action, Editor, KeyAction, Mode};
use crossterm::event::{Event, KeyCode, KeyModifiers};
use vimlet::{
    Binding, Command, EditTarget, Input, Key, Motion, Operator, Shape, WordClass, WordDestination,
};

fn file_motion(mapping: &KeyAction) -> Option<Motion> {
    let action = match mapping {
        KeyAction::Single(action) => action,
        KeyAction::Multiple(actions) if actions.len() == 1 => &actions[0],
        _ => return None,
    };
    Some(match action {
        Action::MoveLeft => Motion::Left,
        Action::MoveRight => Motion::Right,
        Action::MoveDown => Motion::Down,
        Action::MoveUp => Motion::Up,
        Action::MoveToLineStart => Motion::LineStart,
        Action::MoveToLineEnd => Motion::LineEnd,
        Action::MoveToFirstLineChar => Motion::FirstNonBlank,
        Action::MoveToNextWord
        | Action::MoveToNextBigWord
        | Action::MoveToPreviousWord
        | Action::MoveToPreviousBigWord
        | Action::MoveToNextWordEnd
        | Action::MoveToNextBigWordEnd => {
            let class = match action {
                Action::MoveToNextBigWord
                | Action::MoveToPreviousBigWord
                | Action::MoveToNextBigWordEnd => WordClass::BigWord,
                _ => WordClass::Word,
            };
            let destination = match action {
                Action::MoveToPreviousWord | Action::MoveToPreviousBigWord => {
                    WordDestination::PreviousStart
                }
                Action::MoveToNextWordEnd | Action::MoveToNextBigWordEnd => {
                    WordDestination::NextEnd
                }
                _ => WordDestination::NextStart,
            };
            Motion::Word { class, destination }
        }
        _ => return None,
    })
}

impl Editor {
    pub(super) fn sync_file_mode(&mut self) {
        let mode = match self.mode {
            Mode::Visual => vimlet::Mode::Visual,
            Mode::VisualBlock => vimlet::Mode::VisualBlock,
            Mode::VisualLine => vimlet::Mode::VisualLine,
            Mode::Insert => vimlet::Mode::Insert,
            _ => vimlet::Mode::Normal,
        };
        self.file_vim.state.sync_mode(mode);
    }

    pub(in crate::editor) fn file_binding(&mut self, ev: &Event, mapping: KeyAction) -> KeyAction {
        if self.learn_session.is_some() || self.mouse_selection.returns_to_insert() {
            return mapping;
        }
        let Event::Key(event) = ev else {
            return mapping;
        };
        let Some(key) = crate::keyboard::modal_key(*event) else {
            return mapping;
        };
        let (mapping_ref, prefix) = match &mapping {
            KeyAction::Repeating(n, inner) => (inner.as_ref(), usize::from(*n)),
            other => (other, 0),
        };
        let actions = match mapping_ref {
            KeyAction::Single(action) => std::slice::from_ref(action),
            KeyAction::Multiple(actions) => actions.as_slice(),
            _ => return mapping,
        };
        use vimlet::{Action as Vim, InsertStyle as Insert};
        let (action, count) = match actions {
            [Action::EnterMode(Mode::Insert)] if self.is_normal() => {
                (Vim::Insert(Insert::Before), prefix)
            }
            [Action::EnterMode(Mode::Insert), Action::MoveRight] if self.is_normal() => {
                (Vim::Insert(Insert::After), prefix)
            }
            [Action::MoveToLineEnd, Action::EnterMode(Mode::Insert), Action::MoveRight]
                if self.is_normal() =>
            {
                (Vim::Insert(Insert::LineEnd), prefix)
            }
            [Action::MoveToFirstLineChar, Action::EnterMode(Mode::Insert)] if self.is_normal() => {
                (Vim::Insert(Insert::FirstNonBlank), prefix)
            }
            [Action::InsertLineAtCursor] => (Vim::Insert(Insert::OpenAbove), prefix),
            [Action::InsertLineBelowCursor] => (Vim::Insert(Insert::OpenBelow), prefix),
            [Action::EnterMode(Mode::Visual)] => (Vim::Visual(Shape::Character), prefix),
            [Action::EnterMode(Mode::VisualLine)] => (Vim::Visual(Shape::Line), prefix),
            [Action::EnterMode(Mode::VisualBlock)] => {
                (Vim::Visual(Shape::Block { width: 1 }), prefix)
            }
            [Action::DeleteCharAtCursorPos] => (Vim::DeleteChars { backward: false }, prefix),
            [Action::DeletePreviousChars(n)] => {
                (Vim::DeleteChars { backward: true }, usize::from(*n))
            }
            [Action::ChangeCharsAtCursor(n)] => (
                Vim::Operate {
                    operator: Operator::Change,
                    target: EditTarget::Motion(Motion::Right),
                },
                usize::from(*n),
            ),
            [Action::ChangeCurrentLines(n)] => (
                Vim::Operate {
                    operator: Operator::Change,
                    target: EditTarget::Lines,
                },
                usize::from(*n),
            ),
            [Action::DeleteToLineEnd(n)] => (
                Vim::Operate {
                    operator: Operator::Delete,
                    target: EditTarget::Motion(Motion::LineEnd),
                },
                usize::from(*n),
            ),
            [Action::ChangeToLineEnd(n)] => (
                Vim::Operate {
                    operator: Operator::Change,
                    target: EditTarget::Motion(Motion::LineEnd),
                },
                usize::from(*n),
            ),
            [Action::YankToLineEnd(n)] => (
                Vim::Operate {
                    operator: Operator::Yank,
                    target: EditTarget::Motion(Motion::LineEnd),
                },
                usize::from(*n),
            ),
            [Action::Undo] => (Vim::History(vimlet::HistoryCommand::Undo), prefix),
            [Action::Redo] => (Vim::History(vimlet::HistoryCommand::Redo), prefix),
            [Action::Paste] | [Action::Paste, Action::EnterMode(Mode::Normal)] => {
                (Vim::Paste { before: false }, prefix)
            }
            [Action::PasteBefore] | [Action::PasteBefore, Action::EnterMode(Mode::Normal)] => {
                (Vim::Paste { before: true }, prefix)
            }
            [Action::ToggleCharCase(n)] => (Vim::ToggleCase, usize::from(*n)),
            [Action::JoinLines(n)] => (Vim::Join { spaces: true }, usize::from(*n)),
            [Action::JoinLinesKeepSpaces(n)] => (Vim::Join { spaces: false }, usize::from(*n)),
            [Action::EnterMode(Mode::Normal)] if self.is_visual() => {
                self.sync_file_mode();
                return KeyAction::Single(Action::VimletInput(Input::key(Key::Escape)));
            }
            [Action::IndentSelection(n)] if self.is_visual() => (
                Vim::Operate {
                    operator: Operator::Indent,
                    target: EditTarget::Selection,
                },
                usize::from(*n),
            ),
            [Action::UnindentSelection(n)] if self.is_visual() => (
                Vim::Operate {
                    operator: Operator::Outdent,
                    target: EditTarget::Selection,
                },
                usize::from(*n),
            ),
            [Action::TransformSelection(transform), Action::EnterMode(Mode::Normal)]
                if self.is_visual() =>
            {
                let op = match transform {
                    crate::editor::CaseTransform::Lower => Operator::Lowercase,
                    crate::editor::CaseTransform::Upper => Operator::Uppercase,
                    crate::editor::CaseTransform::Toggle => Operator::ToggleCase,
                };
                (
                    Vim::Operate {
                        operator: op,
                        target: EditTarget::Selection,
                    },
                    prefix,
                )
            }
            [Action::JoinLines(n), Action::EnterMode(Mode::Normal)] if self.is_visual() => {
                (Vim::Join { spaces: true }, usize::from(*n))
            }
            [Action::ChangeSelection] if self.is_visual() => (
                Vim::Operate {
                    operator: Operator::Change,
                    target: EditTarget::Selection,
                },
                prefix,
            ),
            [Action::Yank, Action::EnterMode(Mode::Normal)] if self.is_visual() => (
                Vim::Operate {
                    operator: Operator::Yank,
                    target: EditTarget::Selection,
                },
                prefix,
            ),
            [Action::Delete, Action::EnterMode(Mode::Normal)] if self.is_visual() => (
                Vim::Operate {
                    operator: Operator::Delete,
                    target: EditTarget::Selection,
                },
                prefix,
            ),
            _ => return mapping,
        };
        self.sync_file_mode();
        let mut input = Input::key(key);
        input.binding = Binding::Command(Command::new(action, count));
        KeyAction::Single(Action::VimletInput(input))
    }

    pub(in crate::editor) fn start_file_operator(
        &mut self,
        operator: Operator,
        count: u16,
        prefix: &str,
    ) {
        let version = self.file_version();
        if self
            .file_vim
            .document
            .is_some_and(|old| old.id != version.id)
        {
            self.file_vim
                .session
                .lifecycle(&mut self.file_vim.state, vimlet::Lifecycle::SwitchDocument);
        }
        self.file_vim
            .state
            .begin_operator(operator, usize::from(count));
        self.waiting_command = Some(prefix.to_owned());
    }

    pub(in crate::editor) fn file_prefix_input(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else { return None };
        if !self.file_vim.pending() {
            if self.pending_mark_action.is_some()
                || !matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
            {
                return None;
            }
            let KeyCode::Char(c) = key.code else {
                return None;
            };
            if self.is_normal()
                && Self::key_string_for_event(event)
                    .and_then(|key| self.config.keys.normal.get(&key))
                    .is_some_and(|binding| !matches!(binding, KeyAction::Nested(_)))
            {
                return None;
            }
            let operator = match c {
                'd' => Some(Operator::Delete),
                'c' => Some(Operator::Change),
                'y' => Some(Operator::Yank),
                '>' => Some(Operator::Indent),
                '<' => Some(Operator::Outdent),
                _ => None,
            };
            if let Some(operator) = operator.filter(|_| self.is_normal()) {
                let count = self.repeater.take().unwrap_or(0);
                self.start_file_operator(operator, count, &c.to_string());
                return Some(KeyAction::None);
            }
            if matches!(c, 'f' | 'F' | 't' | 'T' | 'r')
                || (self.is_visual() && matches!(c, 'i' | 'a'))
            {
                let count = self.repeater.take().unwrap_or(0);
                self.sync_file_mode();
                self.file_vim.state.set_count(usize::from(count));
                self.waiting_command = Some(c.to_string());
                return Some(KeyAction::Single(Action::VimletInput(Input::key(
                    Key::Char(c),
                ))));
            }
            return None;
        }
        let Some(key) = crate::keyboard::modal_key(*key) else {
            self.file_vim.cancel_pending();
            self.waiting_command = None;
            self.set_legacy_message(Some("invalid operator motion".into()));
            return Some(KeyAction::None);
        };
        let mut input = Input::key(key);
        if let Key::Char(c) = input.key {
            let prefix = self.file_vim.state.target_prefix();
            let target = match prefix {
                Some(prefix @ ('i' | 'a'))
                    if crate::editor::SyntaxObjectKind::for_text_object_key(c).is_some() =>
                {
                    Some(syntax_target(prefix as u8, c))
                }
                Some(prefix @ ('[' | ']'))
                    if crate::editor::structural_motion_kind(c).is_some() =>
                {
                    Some(syntax_target(prefix as u8, c))
                }
                Some('g') if c == 'c' => Some(syntax_target(b'g', c)),
                None if c == '%'
                    && !self.file_vim.state.has_motion_count()
                    && self.file_vim.state.binding_context()
                        == vimlet::BindingContext::OperatorPending =>
                {
                    Some(syntax_target(b'%', c))
                }
                _ => None,
            };
            if let Some(EditTarget::Host(request)) = target {
                input.binding = Binding::Command(Command::new(vimlet::Action::Target(request), 0));
            } else if self.file_vim.state.binding_context()
                == vimlet::BindingContext::OperatorPending
                && prefix.is_none()
            {
                let mapped =
                    Self::key_string_for_event(event).and_then(|k| self.config.keys.normal.get(&k));
                if let Some(motion) = mapped.and_then(file_motion) {
                    if c != 'G' && !c.is_ascii_digit() {
                        input.binding =
                            Binding::Command(Command::new(vimlet::Action::Move(motion), 0));
                    }
                }
            } else if matches!(prefix, Some('i' | 'a')) && c == 'q' {
                input.key = Key::Char('"');
            }
        }
        Some(KeyAction::Single(Action::VimletInput(input)))
    }
}
