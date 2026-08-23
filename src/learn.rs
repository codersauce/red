//! Curriculum metadata and the first editor-native Learn Red exercise.

use crate::editor::{Action, Mode};

pub(crate) const FIRST_LESSON_ID: &str = "essentials.find-your-footing.v1";
pub(crate) const PRACTICE_CONTENTS: &str = "// This practice buffer never touches your project.\n\nfn add_score(score: u32, points: u32) -> u32 {\n    score + points\n}\n";

pub(crate) struct Track {
    pub id: &'static str,
    pub title: &'static str,
    pub category: &'static str,
    pub duration: &'static str,
    pub description: &'static str,
    pub outcome: &'static str,
    pub lessons: &'static [&'static str],
}

pub(crate) const TRACKS: [Track; 6] = [
    Track {
        id: "essentials",
        title: "Essentials",
        category: "Start here",
        duration: "~4 min",
        description: "Get comfortable, make a change, and know how to get back.",
        outcome: "Make your first edit without losing your place.",
        lessons: &[
            "Find your footing",
            "Edit with confidence",
            "Find a command",
            "Save a practice file",
        ],
    },
    Track {
        id: "ai",
        title: "Build with AI",
        category: "AI coding",
        duration: "~12 min",
        description:
            "Start with a question. Make a focused change. Understand exactly what it did.",
        outcome: "Use AI to fix a small bug and review the result.",
        lessons: &[
            "Understand selected code",
            "Make a focused change",
            "Choose what to keep",
            "Continue in Agent",
            "Review what changed",
        ],
    },
    Track {
        id: "ship",
        title: "Fix & ship",
        category: "LSP + Git",
        duration: "~15 min",
        description: "Follow a defect from the first diagnostic to a clean local commit.",
        outcome: "Diagnose, repair, and commit one small change.",
        lessons: &[
            "Read the diagnostic",
            "Follow the symbol",
            "Repair the code",
            "Stage the right hunk",
            "Make a local commit",
        ],
    },
    Track {
        id: "navigation",
        title: "Find your way",
        category: "Navigation",
        duration: "~8 min",
        description: "Move around a codebase without losing the thread.",
        outcome: "Find the right code and return to where you started.",
        lessons: &[
            "Open a file by name",
            "Search the project",
            "Follow symbols",
            "Arrange your workspace",
        ],
    },
    Track {
        id: "editing",
        title: "Edit with precision",
        category: "Modal editing",
        duration: "~10 min",
        description: "Build a small vocabulary of edits you can combine.",
        outcome: "Make a precise edit, repeat it, and recover confidently.",
        lessons: &[
            "Move with intent",
            "Change a text object",
            "Repeat and recover",
            "Find and replace",
        ],
    },
    Track {
        id: "custom",
        title: "Make Red yours",
        category: "Setup + workflow",
        duration: "~7 min",
        description: "Tune the parts you use every day, one decision at a time.",
        outcome: "Leave with a setup you understand and can change.",
        lessons: &[
            "Choose a theme",
            "Discover your keymap",
            "Check language support",
            "Keep your place",
        ],
    },
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PracticeStep {
    #[default]
    Insert,
    Type,
    Normal,
    Undo,
    Complete,
}

impl PracticeStep {
    pub fn suggested_action(self) -> Option<Action> {
        match self {
            Self::Insert => Some(Action::EnterMode(Mode::Insert)),
            Self::Normal => Some(Action::EnterMode(Mode::Normal)),
            Self::Undo => Some(Action::Undo),
            Self::Type | Self::Complete => None,
        }
    }

    pub fn instruction(self, shortcut: Option<&str>) -> String {
        match self {
            Self::Insert => format!("Press {} to enter Insert mode.", shortcut.unwrap_or("i")),
            Self::Type => "Type a few characters in the practice buffer.".into(),
            Self::Normal => format!(
                "Press {} to return to Normal mode.",
                shortcut.unwrap_or("Esc")
            ),
            Self::Undo => format!(
                "Press {} to undo your practice edit.",
                shortcut.unwrap_or("u")
            ),
            Self::Complete => "Your original text is restored. Nicely done.".into(),
        }
    }

    pub const fn completed_steps(self) -> usize {
        match self {
            Self::Insert => 0,
            Self::Type => 1,
            Self::Normal => 2,
            Self::Undo => 3,
            Self::Complete => 4,
        }
    }

    /// Progress follows successful editor effects, not just pressed keys.
    pub fn observe(&mut self, action: &Action, mode: Mode, original_text: bool) -> bool {
        let next = match (*self, action) {
            (Self::Insert, _) if mode == Mode::Insert => Self::Type,
            (Self::Type, _) if !original_text => Self::Normal,
            (Self::Normal, Action::EnterMode(Mode::Normal)) if mode == Mode::Normal => Self::Undo,
            (Self::Undo, Action::Undo) if original_text => Self::Complete,
            _ => return false,
        };
        *self = next;
        true
    }
}

/// This first checkpoint permits only edits to its isolated scratch buffer.
pub(crate) fn practice_action_allowed(action: &Action) -> bool {
    matches!(
        action,
        Action::OpenLearn
            | Action::StartLearnLesson
            | Action::ExitLearnLesson
            | Action::RestartLearnLesson
            | Action::FinishLearnLesson
            | Action::Quit(_)
            | Action::Command(_)
            | Action::Print(_)
            | Action::PrintWarning(_)
            | Action::EnterMode(Mode::Normal | Mode::Insert | Mode::Command)
            | Action::SetWaitingKey(_)
            | Action::Refresh
            | Action::CloseDialog
            | Action::InsertCharAtCursorPos(_)
            | Action::InsertString(_)
            | Action::InsertPastedText(_)
            | Action::InsertNewLine
            | Action::InsertTab
            | Action::InsertLineBelowCursor
            | Action::InsertLineAtCursor
            | Action::DeletePreviousChar
            | Action::DeleteCharAtCursorPos
            | Action::Undo
            | Action::Redo
            | Action::MoveUp
            | Action::MoveDown
            | Action::MoveLeft
            | Action::MoveRight
            | Action::MoveToLineEnd
            | Action::MoveToLineStart
            | Action::MoveToFirstLineChar
            | Action::MoveToNextWord
            | Action::MoveToPreviousWord
            | Action::MoveToNextWordEnd
            | Action::MoveToTop
            | Action::MoveToBottom
            | Action::GoToLine(_)
            | Action::PageDown
            | Action::PageUp
            | Action::HalfPageDown(_)
            | Action::HalfPageUp(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lesson_requires_a_real_edit_and_restoring_undo() {
        let mut step = PracticeStep::default();
        assert!(!step.observe(&Action::Undo, Mode::Normal, true));
        assert!(step.observe(&Action::EnterMode(Mode::Insert), Mode::Insert, true));
        assert!(!step.observe(&Action::Refresh, Mode::Insert, true));
        assert!(step.observe(&Action::InsertCharAtCursorPos('x'), Mode::Insert, false));
        assert!(step.observe(&Action::EnterMode(Mode::Normal), Mode::Normal, false));
        assert!(!step.observe(&Action::Undo, Mode::Normal, false));
        assert!(step.observe(&Action::Undo, Mode::Normal, true));
        assert_eq!(step, PracticeStep::Complete);
    }

    #[test]
    fn first_checkpoint_never_runs_workspace_actions() {
        for action in [
            Action::Save,
            Action::SaveAs("outside.rs".into()),
            Action::OpenFile("outside.rs".into()),
            Action::NextBuffer,
            Action::AlternateBuffer,
            Action::PluginCommand("Agent".into()),
            Action::InlineAssist,
        ] {
            assert!(!practice_action_allowed(&action));
        }
        assert!(practice_action_allowed(&Action::Command(
            "tutorial quit".into()
        )));
    }
}
