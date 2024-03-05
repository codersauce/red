use std::{cmp::Ordering, mem::discriminant};

use serde::{Deserialize, Serialize};

use crate::{buffer::SharedBuffer, config::KeyAction};

use super::Mode;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum Action {
    Quit(bool),
    Save,
    EnterMode(Mode),
    ToggleWrap,

    Undo,
    UndoMultiple(Vec<Action>),

    FindNext,
    FindPrevious,

    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    MoveToLineEnd,
    MoveToLineStart,
    MoveLineToViewportCenter,
    MoveLineToViewportBottom,
    MoveToBottom,
    MoveToTop,
    MoveTo(usize, usize),
    MoveToNextWord,
    MoveToPreviousWord,

    PageDown,
    PageUp,
    ScrollUp,
    ScrollDown,
    Click(usize, usize),

    DeletePreviousChar,
    DeleteCharAtCursorPos,
    DeleteCurrentLine,
    DeleteLineAt(usize),
    DeleteCharAt(usize, usize),
    DeleteWord,

    InsertNewLine,
    InsertCharAtCursorPos(char),
    InsertLineAt(usize, Option<String>),
    InsertLineBelowCursor,
    InsertLineAtCursor,
    InsertTab,

    ReplaceLineAt(usize, String),

    GoToLine(usize),
    GoToDefinition,

    DumpBuffer,
    Command(String),
    PluginCommand(String),
    SetCursor(usize, usize),
    SetWaitingKeyAction(Box<KeyAction>),
    OpenBuffer(String),
    OpenFile(String),

    NextBuffer,
    PreviousBuffer,
    FilePicker,
    ShowDialog,
    CloseDialog,
    RefreshDiagnostics,
    Hover,
    Print(String),

    OpenPicker(Option<String>, Vec<String>, Option<i32>),
    Picked(String, Option<i32>),
    Suspend,
    IncreaseLeft,
    DecreaseLeft,
}

#[derive(Debug)]
pub enum ActionEffect {
    None,
    Message(String),
    Error(String),
    RedrawCursor,
    RedrawLine,
    RedrawWindow,
    RedrawAll,
    NewBuffer(SharedBuffer),
    Actions(Vec<Action>),
    Quit,
}

impl Eq for ActionEffect {}

impl PartialEq for ActionEffect {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ActionEffect::NewBuffer(a), ActionEffect::NewBuffer(b)) => {
                a.lock_read().unwrap().name() == b.lock_read().unwrap().name()
            }
            (self_val, other_val) => {
                std::mem::discriminant(self_val) == std::mem::discriminant(other_val)
            }
        }
    }
}

impl PartialOrd for ActionEffect {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ActionEffect {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (ActionEffect::NewBuffer(a), ActionEffect::NewBuffer(b)) => a
                .lock_read()
                .unwrap()
                .name()
                .cmp(&b.lock_read().unwrap().name()),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl ActionEffect {
    pub fn is_quit(&self) -> bool {
        matches!(self, ActionEffect::Quit)
    }

    fn rank(&self) -> usize {
        match self {
            ActionEffect::None => 0,
            ActionEffect::Message(_) => 1,
            ActionEffect::Error(_) => 2,
            ActionEffect::RedrawCursor => 3,
            ActionEffect::RedrawLine => 4,
            ActionEffect::RedrawWindow => 5,
            ActionEffect::RedrawAll => 6,
            ActionEffect::NewBuffer(_) => 7,
            ActionEffect::Actions(_) => 8,
            ActionEffect::Quit => 9,
        }
    }
}

#[allow(unused)]
pub enum GoToLinePosition {
    Top,
    Center,
    Bottom,
}
