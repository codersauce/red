//! Curriculum metadata and editor-native Learn Red exercises.

use crate::editor::{Action, Mode};

pub(crate) const FIRST_LESSON_ID: &str = "essentials.find-your-footing.v1";
pub(crate) const PRACTICE_CONTENTS: &str = "// This practice buffer never touches your project.\n\nfn add_score(score: u32, points: u32) -> u32 {\n    score + points\n}\n";
pub(crate) const EDIT_CONTENTS: &str =
    "let score = 41;;\n\n// Remove the extra semicolon on the first line.\n";
pub(crate) const EDIT_RESULT: &str =
    "let score = 41;\n\n// Remove the extra semicolon on the first line.\n";
pub(crate) const COMMAND_CONTENTS: &str = "// This deliberately long line makes wrapping visible: use the command palette to change how it is displayed, without changing a single character of the practice text.\n\nlet score = 42;\n";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lesson {
    #[default]
    FindYourFooting,
    EditWithConfidence,
    FindACommand,
}

impl Lesson {
    pub const AVAILABLE: [Self; 3] = [
        Self::FindYourFooting,
        Self::EditWithConfidence,
        Self::FindACommand,
    ];

    pub fn from_id(id: &str) -> Option<Self> {
        Self::AVAILABLE.into_iter().find(|lesson| lesson.id() == id)
    }

    pub fn from_number(number: usize) -> Option<Self> {
        number
            .checked_sub(1)
            .and_then(|index| Self::AVAILABLE.get(index))
            .copied()
    }

    pub const fn index(self) -> usize {
        match self {
            Self::FindYourFooting => 0,
            Self::EditWithConfidence => 1,
            Self::FindACommand => 2,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::FindYourFooting => FIRST_LESSON_ID,
            Self::EditWithConfidence => "essentials.edit-with-confidence.v1",
            Self::FindACommand => "essentials.find-a-command.v1",
        }
    }

    pub fn title(self) -> &'static str {
        TRACKS[0].lessons[self.index()]
    }

    pub const fn contents(self) -> &'static str {
        match self {
            Self::FindYourFooting => PRACTICE_CONTENTS,
            Self::EditWithConfidence => EDIT_CONTENTS,
            Self::FindACommand => COMMAND_CONTENTS,
        }
    }

    pub const fn first_step(self) -> PracticeStep {
        match self {
            Self::FindYourFooting => PracticeStep::Insert,
            Self::EditWithConfidence => PracticeStep::EditMove,
            Self::FindACommand => PracticeStep::CommandOpen,
        }
    }

    pub fn next(self) -> Option<Self> {
        Self::AVAILABLE.get(self.index() + 1).copied()
    }

    pub const fn checkpoints(self) -> &'static [&'static str] {
        match self {
            Self::FindYourFooting => &[
                "Enter Insert mode",
                "Change the practice text",
                "Return to Normal mode",
                "Undo back to the original",
            ],
            Self::EditWithConfidence => &[
                "Find the extra semicolon",
                "Delete one character",
                "Undo the change",
                "Redo to keep the fix",
            ],
            Self::FindACommand => &[
                "Open the command palette",
                "Turn line wrapping off",
                "Open keyboard shortcuts",
                "Return to the practice buffer",
            ],
        }
    }
}

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
    EditMove,
    EditDelete,
    EditUndo,
    EditRedo,
    CommandOpen,
    CommandRun,
    CommandHelp,
    CommandReturn,
    Complete,
}

impl PracticeStep {
    pub fn suggested_action(self) -> Option<Action> {
        match self {
            Self::Insert => Some(Action::EnterMode(Mode::Insert)),
            Self::Normal => Some(Action::EnterMode(Mode::Normal)),
            Self::Undo | Self::EditUndo => Some(Action::Undo),
            Self::EditMove => Some(Action::MoveToLineEnd),
            Self::EditDelete => Some(Action::DeleteCharAtCursorPos),
            Self::EditRedo => Some(Action::Redo),
            Self::CommandOpen => Some(Action::CommandPalette),
            Self::CommandHelp => Some(Action::KeyboardShortcuts),
            Self::Type | Self::CommandRun | Self::CommandReturn | Self::Complete => None,
        }
    }

    pub fn instruction(self, lesson: Lesson, shortcut: Option<&str>) -> String {
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
            Self::EditMove => format!(
                "Press {} to reach the extra semicolon at the end of the first line.",
                shortcut.unwrap_or("$")
            ),
            Self::EditDelete => format!(
                "Press {} to delete the extra semicolon. Leave the other one in place.",
                shortcut.unwrap_or("x")
            ),
            Self::EditUndo => format!(
                "Press {} to bring the extra semicolon back. You can always undo a change.",
                shortcut.unwrap_or("u")
            ),
            Self::EditRedo => format!(
                "Press {} to redo the change and keep the fix.",
                shortcut.unwrap_or("Ctrl-r")
            ),
            Self::CommandOpen => format!("Open the command palette with {} (or :commands). Search for Toggle line wrapping, then press Enter.", shortcut.unwrap_or("Alt-x")),
            Self::CommandRun => "Search for Toggle line wrapping and press Enter. The long line should stop wrapping. If you closed the picker, reopen :commands.".into(),
            Self::CommandHelp => format!("The text is unchanged. Press {} to explore the shortcuts available here.", shortcut.unwrap_or("F1")),
            Self::CommandReturn => "Close keyboard shortcuts with Esc to return to the practice buffer.".into(),
            Self::Complete => match lesson {
                Lesson::FindYourFooting => "Your original text is restored. Nicely done.",
                Lesson::EditWithConfidence => {
                    "The extra semicolon is gone. You can edit, undo, and redo with confidence."
                }
                Lesson::FindACommand => "You found a command and its shortcuts. The practice text is unchanged, and your original view will return when you leave.",
            }
            .into(),
        }
    }

    pub const fn completed_steps(self) -> usize {
        match self {
            Self::Insert | Self::EditMove | Self::CommandOpen => 0,
            Self::Type | Self::EditDelete | Self::CommandRun => 1,
            Self::Normal | Self::EditUndo | Self::CommandHelp => 2,
            Self::Undo | Self::EditRedo | Self::CommandReturn => 3,
            Self::Complete => 4,
        }
    }

    /// Progress follows successful editor effects, not just pressed keys.
    pub fn observe(
        &mut self,
        lesson: Lesson,
        action: &Action,
        mode: Mode,
        contents: &str,
        cursor: (usize, usize),
    ) -> bool {
        let original_text = contents == lesson.contents();
        let next = match (*self, action) {
            (Self::Insert, _) if mode == Mode::Insert => Self::Type,
            (Self::Type, _) if !original_text => Self::Normal,
            (Self::Normal, Action::EnterMode(Mode::Normal)) if mode == Mode::Normal => Self::Undo,
            (Self::Undo, Action::Undo) if original_text => Self::Complete,
            (Self::EditMove, _)
                if original_text
                    && mode == Mode::Normal
                    && cursor == ("let score = 41;;".len() - 1, 0) =>
            {
                Self::EditDelete
            }
            (Self::EditDelete, Action::DeleteCharAtCursorPos) if contents == EDIT_RESULT => {
                Self::EditUndo
            }
            (Self::EditUndo, Action::Undo) if original_text => Self::EditRedo,
            (Self::EditRedo, Action::Redo) if contents == EDIT_RESULT => Self::Complete,
            _ => return false,
        };
        *self = next;
        true
    }

    /// Observes UI state that can change through either a key or an action.
    pub fn observe_view(&mut self, action: &Action, view: PracticeView) -> bool {
        let next = match (*self, action) {
            (Self::CommandOpen, Action::CommandPalette) if view.command_palette_open => {
                Self::CommandRun
            }
            (Self::CommandRun, Action::ToggleWrap) if !view.wrapping => Self::CommandHelp,
            (Self::CommandHelp, _) if view.shortcuts_open => Self::CommandReturn,
            (Self::CommandReturn, _) if !view.shortcuts_open => Self::Complete,
            _ => return false,
        };
        *self = next;
        true
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PracticeView {
    pub command_palette_open: bool,
    pub wrapping: bool,
    pub shortcuts_open: bool,
}

/// Scratch lessons permit only edits to their isolated practice buffer.
pub(crate) fn practice_action_allowed(lesson: Lesson, action: &Action) -> bool {
    (lesson == Lesson::FindACommand
        && matches!(
            action,
            Action::CommandPalette | Action::KeyboardShortcuts | Action::ToggleWrap
        ))
        || matches!(
            action,
            Action::OpenLearn
                | Action::StartLearnLesson
                | Action::StartLearnLessonAt(_)
                | Action::ExitLearnLesson
                | Action::RestartLearnLesson
                | Action::FinishLearnLesson
                | Action::Quit(_)
                | Action::Command(_)
                | Action::Print(_)
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

    fn observe_first(step: &mut PracticeStep, action: &Action, mode: Mode, original: bool) -> bool {
        step.observe(
            Lesson::FindYourFooting,
            action,
            mode,
            if original {
                PRACTICE_CONTENTS
            } else {
                "changed"
            },
            (0, 0),
        )
    }

    #[test]
    fn lesson_requires_a_real_edit_and_restoring_undo() {
        let mut step = PracticeStep::default();
        assert!(!observe_first(&mut step, &Action::Undo, Mode::Normal, true));
        assert!(observe_first(
            &mut step,
            &Action::EnterMode(Mode::Insert),
            Mode::Insert,
            true
        ));
        assert!(!observe_first(
            &mut step,
            &Action::Refresh,
            Mode::Insert,
            true
        ));
        assert!(observe_first(
            &mut step,
            &Action::InsertCharAtCursorPos('x'),
            Mode::Insert,
            false
        ));
        assert!(observe_first(
            &mut step,
            &Action::EnterMode(Mode::Normal),
            Mode::Normal,
            false
        ));
        assert!(!observe_first(
            &mut step,
            &Action::Undo,
            Mode::Normal,
            false
        ));
        assert!(observe_first(&mut step, &Action::Undo, Mode::Normal, true));
        assert_eq!(step, PracticeStep::Complete);
    }

    #[test]
    fn edit_lesson_requires_the_target_edit_undo_and_redo() {
        let lesson = Lesson::EditWithConfidence;
        let mut step = lesson.first_step();
        assert!(!step.observe(
            lesson,
            &Action::MoveToLineEnd,
            Mode::Normal,
            EDIT_CONTENTS,
            (0, 0)
        ));
        assert!(step.observe(
            lesson,
            &Action::MoveToLineEnd,
            Mode::Normal,
            EDIT_CONTENTS,
            (15, 0)
        ));
        assert!(!step.observe(
            lesson,
            &Action::DeleteCharAtCursorPos,
            Mode::Normal,
            "wrong",
            (14, 0)
        ));
        assert!(step.observe(
            lesson,
            &Action::DeleteCharAtCursorPos,
            Mode::Normal,
            EDIT_RESULT,
            (14, 0)
        ));
        assert!(!step.observe(lesson, &Action::Undo, Mode::Normal, EDIT_RESULT, (14, 0)));
        assert!(step.observe(lesson, &Action::Undo, Mode::Normal, EDIT_CONTENTS, (15, 0)));
        assert!(!step.observe(
            lesson,
            &Action::DeleteCharAtCursorPos,
            Mode::Normal,
            EDIT_RESULT,
            (14, 0)
        ));
        assert!(step.observe(lesson, &Action::Redo, Mode::Normal, EDIT_RESULT, (14, 0)));
        assert_eq!(step, PracticeStep::Complete);
    }

    #[test]
    fn first_checkpoint_never_runs_workspace_actions() {
        for action in [
            Action::Save,
            Action::SaveAs("outside.rs".into()),
            Action::OpenFile("outside.rs".into()),
            Action::NextBuffer,
            Action::PluginCommand("Agent".into()),
            Action::InlineAssist,
        ] {
            assert!(!practice_action_allowed(Lesson::FindYourFooting, &action));
        }
        assert!(practice_action_allowed(
            Lesson::FindYourFooting,
            &Action::Command("tutorial quit".into())
        ));
    }

    #[test]
    fn command_lesson_requires_visible_ui_effects() {
        let mut step = Lesson::FindACommand.first_step();
        let mut view = PracticeView {
            command_palette_open: false,
            wrapping: true,
            shortcuts_open: false,
        };
        assert!(!step.observe_view(&Action::CommandPalette, view));
        view.command_palette_open = true;
        assert!(step.observe_view(&Action::CommandPalette, view));
        assert!(!step.observe_view(&Action::ToggleWrap, view));
        view.wrapping = false;
        assert!(step.observe_view(&Action::ToggleWrap, view));
        assert!(!step.observe_view(&Action::Refresh, view));
        view.shortcuts_open = true;
        assert!(step.observe_view(&Action::Refresh, view));
        assert!(!step.observe_view(&Action::Refresh, view));
        view.shortcuts_open = false;
        assert!(step.observe_view(&Action::Refresh, view));
        assert_eq!(step, PracticeStep::Complete);
        assert!(practice_action_allowed(
            Lesson::FindACommand,
            &Action::CommandPalette
        ));
        assert!(!practice_action_allowed(
            Lesson::EditWithConfidence,
            &Action::ToggleWrap
        ));
        assert!(!practice_action_allowed(
            Lesson::FindACommand,
            &Action::Save
        ));
    }
}
