use serde::{Deserialize, Serialize};

use crate::config::KeyAction;

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

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ActionEffect {
    None,
    RedrawCursor,
    RedrawLine,
    RedrawWindow,
    RedrawAll,
    Quit,
}

impl ActionEffect {
    pub fn is_quit(&self) -> bool {
        matches!(self, ActionEffect::Quit)
    }
}

#[allow(unused)]
pub enum GoToLinePosition {
    Top,
    Center,
    Bottom,
}
