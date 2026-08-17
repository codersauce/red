//! Curriculum metadata and editor-native Learn Red exercises.

use crate::editor::{Action, Mode};

mod git;
mod workspace;
pub(crate) use workspace::PracticeWorkspace;

pub(crate) const FIRST_LESSON_ID: &str = "essentials.find-your-footing.v1";
pub(crate) const PRACTICE_CONTENTS: &str = "// This practice buffer never touches your project.\n\nfn add_score(score: u32, points: u32) -> u32 {\n    score + points\n}\n";
pub(crate) const EDIT_CONTENTS: &str =
    "let score = 41;;\n\n// Remove the extra semicolon on the first line.\n";
pub(crate) const EDIT_RESULT: &str =
    "let score = 41;\n\n// Remove the extra semicolon on the first line.\n";
pub(crate) const COMMAND_CONTENTS: &str = "// This deliberately long line makes wrapping visible: use the command palette to change how it is displayed, without changing a single character of the practice text.\n\nlet score = 42;\n";
pub(crate) const SAVE_CONTENTS: &str = "Today I learned:\n";
pub(crate) const AI_LINE: &str =
    "fn add_score(score: u32, points: u32) -> u32 { score - points }\n";
pub(crate) const AI_CONTENTS: &str = "fn add_score(score: u32, points: u32) -> u32 { score - points }\n\n// Offline practice: no prompt or code is sent to an AI service.\n";
pub(crate) const AI_FIXED_LINE: &str =
    "fn add_score(score: u32, points: u32) -> u32 { score + points }\n";
pub(crate) const AI_FIXED_CONTENTS: &str = "fn add_score(score: u32, points: u32) -> u32 { score + points }\n\n// Offline practice: no prompt or code is sent to an AI service.\n";
pub(crate) const AI_BONUS_LINE: &str =
    "fn add_score(score: u32, points: u32) -> u32 { score + points + 1 }\n";
pub(crate) const AI_BONUS_CONTENTS: &str = "fn add_score(score: u32, points: u32) -> u32 { score + points + 1 }\n\n// Offline practice: no prompt or code is sent to an AI service.\n";
pub(crate) const AGENT_EXAMPLE: &str = "// Practice usage example\nlet expected_score = 39;\n";
pub(crate) const AGENT_EXAMPLE_FIXED: &str =
    "// Practice usage example: add_score(40, 2)\nlet expected_score = 42;\n";
pub(crate) const HUSK_DIAGNOSTIC: &str = "expected `;` after let binding";
pub(crate) const HUSK_CONTENTS: &str = "fn add_score(score: i32, points: i32) -> i32 {\n    score + points\n}\n\nfn main() {\n    let score = add_score(40, 2)\n    let next = 45;\n}\n";
pub(crate) const HUSK_SYMBOL_CONTENTS: &str = "fn add_score(score: i32, points: i32) -> i32 {\n    score + points\n}\n\nfn main() {\n    let score = add_score(40, 2);\n    let next = add_score(score, 3);\n}\n";
pub(crate) const LEARN_GIT_WORKSPACE: &str = "learn-git";
pub(crate) const LEARN_AGENT_PANEL: &str = "learn-recorded-agent";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lesson {
    #[default]
    FindYourFooting,
    EditWithConfidence,
    FindACommand,
    SaveAPracticeFile,
    UnderstandSelectedCode,
    MakeAFocusedChange,
    ChooseWhatToKeep,
    ContinueInAgent,
    ReviewWhatChanged,
    ReadTheDiagnostic,
    FollowTheSymbol,
}

impl Lesson {
    pub const AVAILABLE: [Self; 11] = [
        Self::FindYourFooting,
        Self::EditWithConfidence,
        Self::FindACommand,
        Self::SaveAPracticeFile,
        Self::UnderstandSelectedCode,
        Self::MakeAFocusedChange,
        Self::ChooseWhatToKeep,
        Self::ContinueInAgent,
        Self::ReviewWhatChanged,
        Self::ReadTheDiagnostic,
        Self::FollowTheSymbol,
    ];

    pub fn from_id(id: &str) -> Option<Self> {
        Self::AVAILABLE.into_iter().find(|lesson| lesson.id() == id)
    }

    pub fn from_number(number: usize) -> Option<Self> {
        Self::from_track_number(0, number)
    }

    pub fn for_track(track: usize) -> impl Iterator<Item = Self> + Clone {
        Self::AVAILABLE
            .into_iter()
            .filter(move |lesson| lesson.track_index() == track)
    }

    pub fn from_track_number(track: usize, number: usize) -> Option<Self> {
        number
            .checked_sub(1)
            .and_then(|index| Self::for_track(track).nth(index))
    }

    pub const fn index(self) -> usize {
        match self {
            Self::FindYourFooting => 0,
            Self::EditWithConfidence => 1,
            Self::FindACommand => 2,
            Self::SaveAPracticeFile => 3,
            Self::UnderstandSelectedCode => 4,
            Self::MakeAFocusedChange => 5,
            Self::ChooseWhatToKeep => 6,
            Self::ContinueInAgent => 7,
            Self::ReviewWhatChanged => 8,
            Self::ReadTheDiagnostic => 9,
            Self::FollowTheSymbol => 10,
        }
    }

    pub const fn track_index(self) -> usize {
        match self {
            Self::UnderstandSelectedCode
            | Self::MakeAFocusedChange
            | Self::ChooseWhatToKeep
            | Self::ContinueInAgent
            | Self::ReviewWhatChanged => 1,
            Self::ReadTheDiagnostic | Self::FollowTheSymbol => 2,
            _ => 0,
        }
    }

    pub fn lesson_index(self) -> usize {
        Self::for_track(self.track_index())
            .position(|lesson| lesson == self)
            .unwrap_or(0)
    }

    pub const fn is_lsp_practice(self) -> bool {
        matches!(self, Self::ReadTheDiagnostic | Self::FollowTheSymbol)
    }

    pub const fn is_ai_practice(self) -> bool {
        matches!(
            self,
            Self::UnderstandSelectedCode
                | Self::MakeAFocusedChange
                | Self::ChooseWhatToKeep
                | Self::ContinueInAgent
        )
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::FindYourFooting => FIRST_LESSON_ID,
            Self::EditWithConfidence => "essentials.edit-with-confidence.v1",
            Self::FindACommand => "essentials.find-a-command.v1",
            Self::SaveAPracticeFile => "essentials.save-a-practice-file.v1",
            Self::UnderstandSelectedCode => "ai.understand-selected-code.v1",
            Self::MakeAFocusedChange => "ai.make-a-focused-change.v1",
            Self::ChooseWhatToKeep => "ai.choose-what-to-keep.v1",
            Self::ContinueInAgent => "ai.continue-in-agent.v1",
            Self::ReviewWhatChanged => "ai.review-what-changed.v1",
            Self::ReadTheDiagnostic => "ship.read-the-diagnostic.v1",
            Self::FollowTheSymbol => "ship.follow-the-symbol.v1",
        }
    }

    pub fn title(self) -> &'static str {
        TRACKS[self.track_index()].lessons[self.lesson_index()]
    }

    pub const fn contents(self) -> &'static str {
        match self {
            Self::FindYourFooting => PRACTICE_CONTENTS,
            Self::EditWithConfidence => EDIT_CONTENTS,
            Self::FindACommand => COMMAND_CONTENTS,
            Self::SaveAPracticeFile => SAVE_CONTENTS,
            Self::UnderstandSelectedCode
            | Self::MakeAFocusedChange
            | Self::ChooseWhatToKeep
            | Self::ContinueInAgent => AI_CONTENTS,
            Self::ReviewWhatChanged => AI_FIXED_CONTENTS,
            Self::ReadTheDiagnostic => HUSK_CONTENTS,
            Self::FollowTheSymbol => HUSK_SYMBOL_CONTENTS,
        }
    }

    pub const fn first_step(self) -> PracticeStep {
        match self {
            Self::FindYourFooting => PracticeStep::Insert,
            Self::EditWithConfidence => PracticeStep::EditMove,
            Self::FindACommand => PracticeStep::CommandOpen,
            Self::SaveAPracticeFile => PracticeStep::SaveEdit,
            Self::UnderstandSelectedCode => PracticeStep::AiSelect,
            Self::MakeAFocusedChange => PracticeStep::AiChangeSelect,
            Self::ChooseWhatToKeep => PracticeStep::AiChoiceSelect,
            Self::ContinueInAgent => PracticeStep::AgentSelect,
            Self::ReviewWhatChanged => PracticeStep::GitOpen,
            Self::ReadTheDiagnostic => PracticeStep::DiagnosticOpen,
            Self::FollowTheSymbol => PracticeStep::SymbolDefinition,
        }
    }

    pub fn next(self) -> Option<Self> {
        Self::for_track(self.track_index()).nth(self.lesson_index() + 1)
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
            Self::SaveAPracticeFile => &[
                "Change the practice note",
                "Write the file to disk",
                "Make an unsaved change",
                "Save the latest version",
            ],
            Self::UnderstandSelectedCode => &[
                "Select the function",
                "Open inline assist",
                "Ask for an explanation",
                "Read the inline comment",
            ],
            Self::MakeAFocusedChange => &[
                "Select only the function",
                "Open a bounded inline prompt",
                "Request the focused fix",
                "Keep the unsaved edit",
            ],
            Self::ChooseWhatToKeep => &[
                "Inspect a practice suggestion",
                "Undo the unwanted change",
                "Request and refine a suggestion",
                "Keep only the corrected result",
            ],
            Self::FollowTheSymbol => &[
                "Go to the symbol definition",
                "Find its references",
                "Open the second call",
                "Return with jump history",
            ],
            Self::ReadTheDiagnostic => &[
                "Open the diagnostics picker",
                "Jump to the reported problem",
                "Read the line diagnostic",
                "Return to the source",
            ],
            Self::ReviewWhatChanged => &[
                "Open the practice Git review",
                "Inspect the score.rs change",
                "Inspect the example.rs change",
                "Return to code",
            ],
            Self::ContinueInAgent => &[
                "Make an unsaved inline fix",
                "Continue the task in Agent",
                "Submit the recorded follow-up",
                "Inspect the saved practice files",
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
    SaveEdit,
    SaveWrite,
    SaveEditAgain,
    SaveWriteAgain,
    AiSelect,
    AiOpen,
    AiSubmit,
    AiRead,
    AiChangeSelect,
    AiChangeOpen,
    AiChangeSubmit,
    AiChangeKeep,
    AiChoiceSelect,
    AiChoiceRequest,
    AiChoiceUndo,
    AiChoiceAgainSelect,
    AiChoiceAgain,
    AiChoiceRefine,
    AiChoiceKeep,
    AgentSelect,
    AgentInlineOpen,
    AgentInlineSubmit,
    AgentEscalate,
    AgentPrompt,
    AgentInspect,
    GitOpen,
    GitScore,
    GitExample,
    GitReturn,
    DiagnosticOpen,
    DiagnosticJump,
    DiagnosticRead,
    DiagnosticReturn,
    SymbolDefinition,
    SymbolReferences,
    SymbolChoose,
    SymbolReturn,
    Complete,
}

impl PracticeStep {
    pub fn suggested_action(self) -> Option<Action> {
        match self {
            Self::SymbolDefinition => Some(Action::GoToDefinition),
            Self::SymbolReferences => Some(Action::PluginCommand("LspReferences".into())),
            Self::SymbolChoose => None,
            Self::SymbolReturn => Some(Action::JumpBack),
            Self::DiagnosticOpen => Some(Action::OpenDiagnosticsPicker),
            Self::DiagnosticRead => Some(Action::ShowLineDiagnostics),
            Self::DiagnosticJump | Self::DiagnosticReturn => None,
            Self::GitOpen => Some(Action::PluginCommand("GitDashboard".into())),
            Self::GitScore | Self::GitExample | Self::GitReturn => None,
            Self::Insert => Some(Action::EnterMode(Mode::Insert)),
            Self::Normal => Some(Action::EnterMode(Mode::Normal)),
            Self::Undo | Self::EditUndo => Some(Action::Undo),
            Self::EditMove => Some(Action::MoveToLineEnd),
            Self::EditDelete => Some(Action::DeleteCharAtCursorPos),
            Self::EditRedo => Some(Action::Redo),
            Self::CommandOpen => Some(Action::CommandPalette),
            Self::CommandHelp => Some(Action::KeyboardShortcuts),
            Self::SaveWrite | Self::SaveWriteAgain => Some(Action::Save),
            Self::SaveEdit | Self::SaveEditAgain => Some(Action::EnterMode(Mode::Insert)),
            Self::AiSelect
            | Self::AiChangeSelect
            | Self::AiChoiceSelect
            | Self::AiChoiceAgainSelect
            | Self::AgentSelect => Some(Action::EnterMode(Mode::VisualLine)),
            Self::AiOpen
            | Self::AiChangeOpen
            | Self::AiChoiceRequest
            | Self::AiChoiceAgain
            | Self::AgentInlineOpen => Some(Action::InlineAssist),
            Self::AiRead => Some(Action::ShowInlineComment),
            Self::AiChangeKeep | Self::AiChoiceKeep => Some(Action::KeepInlineAssist),
            Self::AiChoiceUndo => Some(Action::UndoInlineAssist),
            Self::AiChoiceRefine => Some(Action::RefineInlineAssist),
            Self::AgentEscalate => Some(Action::EscalateInlineAssist),
            Self::AgentInlineSubmit | Self::AgentPrompt | Self::AgentInspect => None,
            Self::AiSubmit | Self::AiChangeSubmit => None,
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
            Self::SaveEdit => format!("Press {} and add a word to the note. This is a disposable practice.txt, outside your project.", shortcut.unwrap_or("i")),
            Self::SaveWrite => "Return to Normal mode with Esc, then use :w to write practice.txt. Watch the unsaved marker disappear.".into(),
            Self::SaveEditAgain => "The first version is on disk. Enter Insert mode and change the note again. Notice the unsaved marker return.".into(),
            Self::SaveWriteAgain => "Return to Normal mode and use :w again. The file on disk should now match your latest text.".into(),
            Self::AiSelect => format!("Offline practice. Press {} on the first line to select the function. No code or prompt will leave Red.", shortcut.unwrap_or("V")),
            Self::AiOpen => format!("Press {} to open inline assist for the selected function.", shortcut.unwrap_or("Space i")),
            Self::AiSubmit => "Ask what this function does, then press Enter. This lesson supplies a labeled recorded response.".into(),
            Self::AiRead => format!("Press Enter to keep the explanation, then {} to read its inline comment. The source text has not changed.", shortcut.unwrap_or("Space v")),
            Self::AiChangeSelect => format!("The function subtracts points. Press {} on the first line to select only that function. This is recorded, offline practice.", shortcut.unwrap_or("V")),
            Self::AiChangeOpen => format!("Press {} to request a change limited to your selection.", shortcut.unwrap_or("Space i")),
            Self::AiChangeSubmit => "Ask to add points instead of subtracting them, then press Enter. The recorded result will make a real, unsaved buffer edit.".into(),
            Self::AiChangeKeep => "Inspect the + in the function. Press Enter to keep the edit. Keep closes inline assist; it does not save the file. If you undid it, select the line and request it again.".into(),
            Self::AiChoiceSelect => format!("Press {} on the first function. This recorded practice intentionally returns an imperfect suggestion so you can try undo and refine.", shortcut.unwrap_or("V")),
            Self::AiChoiceRequest => format!("Open inline assist with {}, ask to add points, and press Enter. Inspect the suggested code before keeping it.", shortcut.unwrap_or("Space i")),
            Self::AiChoiceUndo => "The suggestion adds an unwanted + 1. Press u in the result controls to undo it. If you already kept it, normal editor undo works too.".into(),
            Self::AiChoiceAgainSelect => format!("The original is restored. Press {} to select the first function again.", shortcut.unwrap_or("V")),
            Self::AiChoiceAgain => format!("Open inline assist with {}, ask to add points again, and submit. This time you will refine the suggestion.", shortcut.unwrap_or("Space i")),
            Self::AiChoiceRefine => "Press r in the result controls. Ask to remove the extra bonus point, then press Enter. Refinement stays scoped to the same function.".into(),
            Self::AiChoiceKeep => "Check that the function is now score + points, with no extra + 1. Press Enter to keep that result. It is still unsaved.".into(),
            Self::AgentSelect => format!("This lesson owns two disposable files. Press {} on the first function to select it. No prompt will leave Red.", shortcut.unwrap_or("V")),
            Self::AgentInlineOpen => format!("Open inline assist with {}. Ask to add points instead of subtracting them.", shortcut.unwrap_or("Space i")),
            Self::AgentInlineSubmit => "Ask to add points, then submit. The inline fix will be unsaved; next you will continue the task in Agent.".into(),
            Self::AgentEscalate => "The inline fix is unsaved. Press A in its result controls to continue in the recorded Agent workspace.".into(),
            Self::AgentPrompt => "Ask Agent to save the fix and update the usage example. Press Enter to submit. This recorded turn uses real editor file tools, not a live model.".into(),
            Self::AgentInspect => "The recorded Agent saved both files. Choose Back to code (or q), then use :bn to open example.rs and inspect the saved value 42.".into(),
            Self::GitOpen => format!("The recorded task changed two saved files. Open Git review with {} (or :GitDashboard). This repository is disposable and has no remote.", shortcut.unwrap_or("Space g g")),
            Self::GitScore => "Select score.rs, press Tab to focus its diff, then move with j/k onto a red or green changed line. Check that subtraction became addition. Reopen with :GitDashboard if needed.".into(),
            Self::GitExample => "Tab back to Files, select example.rs, then Tab into its diff. Move onto a changed line and inspect the expected value 42. Reopen with :GitDashboard if needed.".into(),
            Self::GitReturn => "Both changes are reviewed. Press q or Esc to return to code. Nothing has been staged or committed.".into(),
            Self::DiagnosticOpen => format!("The bundled Husk server checks this disposable file offline. When the error marker appears, press {} to open Diagnostics. If the picker is empty, close it and try again in a moment.", shortcut.unwrap_or("Space d")),
            Self::DiagnosticJump => "Choose the missing-semicolon error and press Enter. The editor will take you to the reported location. Reopen Diagnostics if you closed the picker.".into(),
            Self::DiagnosticRead => format!("Press {} to read the diagnostic on this line. The parser points at the next token; the incomplete statement is just above it.", shortcut.unwrap_or("D")),
            Self::DiagnosticReturn => "Read the message and diagnostic code, then press Esc to return to the source. You will repair the defect in a later lesson.".into(),
            Self::SymbolDefinition => format!("The cursor starts on the first add_score call. Press {} to follow it to its definition. This uses the real bundled Husk server. If you moved, use :6, then ^3w.", shortcut.unwrap_or("g d")),
            Self::SymbolReferences => format!("You are at the function definition. Press {} (or :LspReferences) to find the places that call it.", shortcut.unwrap_or("Space k")),
            Self::SymbolChoose => "Choose the second call, let next = add_score(score, 3), and press Enter. If you closed the picker, find references again from the definition.".into(),
            Self::SymbolReturn => format!("Press {} to return to the definition, then press it again to return to the first call. The jump list remembers where you came from.", shortcut.unwrap_or("Ctrl-o")),
            Self::Complete => match lesson {
                Lesson::FindYourFooting => "Your original text is restored. Nicely done.",
                Lesson::EditWithConfidence => {
                    "The extra semicolon is gone. You can edit, undo, and redo with confidence."
                }
                Lesson::FindACommand => "You found a command and its shortcuts. The practice text is unchanged, and your original view will return when you leave.",
                Lesson::SaveAPracticeFile => "Your latest text is saved. Essentials complete! This disposable file is removed when you leave; your own work is untouched.",
                Lesson::UnderstandSelectedCode => "You explained selected code without editing it. Recorded practice complete; real inline assist sends your selected context only when you submit a prompt.",
                Lesson::MakeAFocusedChange => "The fix is kept in the buffer, still unsaved. Inline edits use normal undo history; keeping one is not the same as writing a file.",
                Lesson::ChooseWhatToKeep => "You rejected an unwanted change, refined a suggestion, and kept the corrected result. The final edit is unsaved and remains undoable.",
                Lesson::FollowTheSymbol => "You followed a real definition, inspected its references, and returned through the jump list. No source was changed.",
                Lesson::ReadTheDiagnostic => "You found a real language-server error and read it at its source. The file is unchanged. Your original language servers and diagnostics return when you leave.",
                Lesson::ReviewWhatChanged => "You reviewed the real diff for both files. Build with AI complete! These changes are still unstaged; your own repository was never touched.",
                Lesson::ContinueInAgent => "Both practice files are saved. Unlike Keep in inline assist, Agent file-edit tools can write to disk. Your real workspace and Agent draft are untouched.",
            }
            .into(),
        }
    }

    pub const fn completed_steps(self) -> usize {
        match self {
            Self::Insert
            | Self::EditMove
            | Self::CommandOpen
            | Self::SaveEdit
            | Self::AiSelect
            | Self::AiChangeSelect
            | Self::AiChoiceSelect
            | Self::AiChoiceRequest
            | Self::AgentSelect
            | Self::AgentInlineOpen
            | Self::AgentInlineSubmit
            | Self::GitOpen
            | Self::DiagnosticOpen
            | Self::SymbolDefinition => 0,
            Self::Type
            | Self::EditDelete
            | Self::CommandRun
            | Self::SaveWrite
            | Self::AiOpen
            | Self::AiChangeOpen
            | Self::AiChoiceUndo
            | Self::AgentEscalate
            | Self::GitScore
            | Self::DiagnosticJump
            | Self::SymbolReferences => 1,
            Self::Normal
            | Self::EditUndo
            | Self::CommandHelp
            | Self::SaveEditAgain
            | Self::AiSubmit
            | Self::AiChangeSubmit
            | Self::AiChoiceAgainSelect
            | Self::AiChoiceAgain
            | Self::AiChoiceRefine
            | Self::AgentPrompt
            | Self::GitExample
            | Self::DiagnosticRead
            | Self::SymbolChoose => 2,
            Self::Undo
            | Self::EditRedo
            | Self::CommandReturn
            | Self::SaveWriteAgain
            | Self::AiRead
            | Self::AiChangeKeep
            | Self::AiChoiceKeep
            | Self::AgentInspect
            | Self::GitReturn
            | Self::DiagnosticReturn
            | Self::SymbolReturn => 3,
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
            (Self::SaveEdit, _) if !original_text => Self::SaveWrite,
            (Self::AiSelect, _) if original_text && mode == Mode::VisualLine && cursor.1 == 0 => {
                Self::AiOpen
            }
            (Self::AiChangeSelect, _)
                if original_text && mode == Mode::VisualLine && cursor.1 == 0 =>
            {
                Self::AiChangeOpen
            }
            (Self::AiChoiceSelect, _)
                if original_text && mode == Mode::VisualLine && cursor.1 == 0 =>
            {
                Self::AiChoiceRequest
            }
            (Self::AiChoiceAgainSelect, _)
                if original_text && mode == Mode::VisualLine && cursor.1 == 0 =>
            {
                Self::AiChoiceAgain
            }
            (Self::AgentSelect, _)
                if original_text && mode == Mode::VisualLine && cursor.1 == 0 =>
            {
                Self::AgentInlineOpen
            }
            _ => return false,
        };
        *self = next;
        true
    }

    /// Observes UI state that can change through either a key or an action.
    pub fn observe_view(&mut self, action: &Action, view: PracticeView) -> bool {
        let next = match (*self, action) {
            (Self::SymbolDefinition, Action::OpenLocation(_, _))
                if view.symbol_definition_received && view.symbol_at_definition =>
            {
                Self::SymbolReferences
            }
            (Self::SymbolReferences, Action::ShowDialog)
                if view.symbol_references_received && view.references_picker_open =>
            {
                Self::SymbolChoose
            }
            (Self::SymbolChoose, Action::OpenLocation(_, _))
                if view.symbol_references_received && view.symbol_at_second_call =>
            {
                Self::SymbolReturn
            }
            (Self::SymbolReturn, Action::JumpBack)
                if view.symbol_at_first_call && view.original_text =>
            {
                Self::Complete
            }

            (
                Self::DiagnosticOpen,
                Action::OpenDiagnosticsPicker | Action::OpenErrorDiagnosticsPicker,
            ) if view.diagnostic_present && view.diagnostics_picker_open => Self::DiagnosticJump,
            (Self::DiagnosticJump, Action::OpenLocation(_, _)) if view.diagnostic_under_cursor => {
                Self::DiagnosticRead
            }
            (Self::DiagnosticRead, Action::ShowLineDiagnostics)
                if view.diagnostic_under_cursor && view.diagnostic_popup_open =>
            {
                Self::DiagnosticReturn
            }
            (Self::DiagnosticReturn, Action::CloseDialog)
                if !view.diagnostic_popup_open && view.diagnostic_present && view.original_text =>
            {
                Self::Complete
            }
            (Self::CommandOpen, Action::CommandPalette) if view.command_palette_open => {
                Self::CommandRun
            }
            (Self::CommandRun, Action::ToggleWrap) if !view.wrapping => Self::CommandHelp,
            (Self::CommandHelp, _) if view.shortcuts_open => Self::CommandReturn,
            (Self::CommandReturn, _) if !view.shortcuts_open => Self::Complete,
            (Self::SaveWrite, Action::Save) if view.file_matches_buffer => Self::SaveEditAgain,
            (Self::SaveEditAgain, _) if view.dirty => Self::SaveWriteAgain,
            (Self::SaveWriteAgain, Action::Save) if view.file_matches_buffer => Self::Complete,
            (Self::AiOpen, Action::InlineAssist) if view.inline_target_selected => Self::AiSubmit,
            (Self::AiSubmit, Action::SubmitInlineAssist(_)) if view.inline_explanation_received => {
                Self::AiRead
            }
            (Self::AiRead, Action::ShowInlineComment) if view.inline_comment_open => Self::Complete,
            (Self::AiChangeOpen, Action::InlineAssist) if view.inline_target_selected => {
                Self::AiChangeSubmit
            }
            (Self::AiChangeSubmit, Action::SubmitInlineAssist(_))
                if view.inline_edit_applied && view.fixed_text && view.dirty =>
            {
                Self::AiChangeKeep
            }
            (Self::AiChangeKeep, Action::KeepInlineAssist)
                if view.inline_closed && view.fixed_text && view.dirty =>
            {
                Self::Complete
            }
            (Self::AiChoiceRequest, Action::SubmitInlineAssist(_))
                if view.inline_edit_applied && view.bonus_text =>
            {
                Self::AiChoiceUndo
            }
            (Self::AiChoiceUndo, Action::UndoInlineAssist | Action::Undo)
                if view.inline_closed && view.original_text && !view.dirty =>
            {
                Self::AiChoiceAgainSelect
            }
            (Self::AiChoiceAgain, Action::SubmitInlineAssist(_))
                if view.inline_edit_applied && view.bonus_text =>
            {
                Self::AiChoiceRefine
            }
            (Self::AiChoiceRefine, Action::SubmitInlineAssist(_))
                if view.inline_edit_applied && view.fixed_text =>
            {
                Self::AiChoiceKeep
            }
            (Self::AiChoiceKeep, Action::KeepInlineAssist)
                if view.inline_closed && view.fixed_text && view.dirty =>
            {
                Self::Complete
            }
            (Self::AgentInlineOpen, Action::InlineAssist) if view.inline_target_selected => {
                Self::AgentInlineSubmit
            }
            (Self::AgentInlineSubmit, Action::SubmitInlineAssist(_))
                if view.inline_edit_applied && view.fixed_text && view.dirty =>
            {
                Self::AgentEscalate
            }
            (Self::AgentEscalate, Action::EscalateInlineAssist) if view.agent_pane_open => {
                Self::AgentPrompt
            }
            (Self::AgentPrompt, _) if view.agent_files_saved => Self::AgentInspect,
            (Self::AgentInspect, _)
                if !view.agent_pane_open
                    && view.agent_files_saved
                    && view.file_matches_buffer
                    && view.agent_example_visible =>
            {
                Self::Complete
            }
            _ => return false,
        };
        *self = next;
        true
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PracticeView {
    pub command_palette_open: bool,
    pub wrapping: bool,
    pub shortcuts_open: bool,
    pub file_matches_buffer: bool,
    pub dirty: bool,
    pub inline_target_selected: bool,
    pub inline_explanation_received: bool,
    pub inline_comment_open: bool,
    pub inline_edit_applied: bool,
    pub inline_closed: bool,
    pub fixed_text: bool,
    pub bonus_text: bool,
    pub original_text: bool,
    pub agent_pane_open: bool,
    pub agent_files_saved: bool,
    pub agent_example_visible: bool,
    pub diagnostic_present: bool,
    pub diagnostics_picker_open: bool,
    pub diagnostic_under_cursor: bool,
    pub diagnostic_popup_open: bool,
    pub symbol_definition_received: bool,
    pub symbol_references_received: bool,
    pub references_picker_open: bool,
    pub symbol_at_definition: bool,
    pub symbol_at_first_call: bool,
    pub symbol_at_second_call: bool,
}

/// Scratch lessons permit only edits to their isolated practice buffer.
pub(crate) fn practice_action_allowed(lesson: Lesson, action: &Action) -> bool {
    (lesson == Lesson::FollowTheSymbol
        && (matches!(
            action,
            Action::GoToDefinition | Action::JumpBack | Action::JumpForward
        ) || matches!(action, Action::PluginCommand(name) if name == "LspReferences")))
        || (lesson.is_lsp_practice()
            && matches!(
                action,
                Action::OpenDiagnosticsPicker
                    | Action::OpenErrorDiagnosticsPicker
                    | Action::ShowLineDiagnostics
                    | Action::NextDiagnostic
                    | Action::PreviousDiagnostic
                    | Action::RefreshDiagnostics
                    | Action::OpenLocation(_, _)
                    | Action::MoveTo(_, _)
                    | Action::ShowDialog
                    | Action::CommandPalette
                    | Action::KeyboardShortcuts
            ))
        || (lesson == Lesson::ReviewWhatChanged
            && (matches!(action, Action::PluginCommand(name) if name == "GitDashboard")
                || matches!(action, Action::NotifyPlugins(method, _) if method == &format!("workspace:event:{LEARN_GIT_WORKSPACE}"))))
        || (lesson == Lesson::ContinueInAgent
            && (matches!(
                action,
                Action::EscalateInlineAssist
                    | Action::OpenTextPanelTurnActions
                    | Action::NextBuffer
                    | Action::PreviousBuffer
            ) || matches!(action, Action::CopyTextPanelTurn { panel_id, .. } | Action::ReuseTextPanelPrompt { panel_id, .. } if panel_id == LEARN_AGENT_PANEL)
                || matches!(action, Action::PluginCommand(name) if matches!(name.as_str(), "Agent" | "AgentOpen" | "AgentToggle"))
                || matches!(action, Action::NotifyPlugins(method, _) if method == &format!("panel:event:{LEARN_AGENT_PANEL}"))))
        || (lesson.is_ai_practice()
            && matches!(
                action,
                Action::EnterMode(Mode::Visual | Mode::VisualLine)
                    | Action::InlineAssist
                    | Action::SubmitInlineAssist(_)
                    | Action::CancelInlineAssist
                    | Action::CancelInlineAssistRefine
                    | Action::KeepInlineAssist
                    | Action::UndoInlineAssist
                    | Action::RefineInlineAssist
                    | Action::ShowInlineComment
                    | Action::DismissInlineComment
                    | Action::NextInlineComment
                    | Action::PreviousInlineComment
            ))
        || (lesson == Lesson::SaveAPracticeFile && matches!(action, Action::Save))
        || (lesson == Lesson::FindACommand
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

    #[test]
    fn symbol_lesson_requires_lsp_results_and_return_to_the_original_call() {
        let mut step = Lesson::FollowTheSymbol.first_step();
        let mut view = PracticeView {
            original_text: true,
            ..PracticeView::default()
        };
        let location = Action::OpenLocation(
            crate::plugin::PluginLocation {
                path: "main.hk".into(),
                line: 0,
                column: 3,
                column_encoding: crate::plugin::LocationColumnEncoding::Utf16,
            },
            crate::plugin::OpenLocationTarget::Current,
        );
        view.symbol_at_definition = true;
        assert!(!step.observe_view(&location, view));
        view.symbol_definition_received = true;
        assert!(step.observe_view(&location, view));
        view.references_picker_open = true;
        assert!(!step.observe_view(&Action::ShowDialog, view));
        view.symbol_references_received = true;
        assert!(step.observe_view(&Action::ShowDialog, view));
        assert!(!step.observe_view(&location, view));
        view.symbol_at_second_call = true;
        assert!(step.observe_view(&location, view));
        assert!(!step.observe_view(&Action::JumpBack, view));
        view.symbol_at_first_call = true;
        assert!(step.observe_view(&Action::JumpBack, view));
        assert_eq!(step, PracticeStep::Complete);
    }

    #[test]
    fn diagnostic_lesson_requires_real_picker_location_and_popup_effects() {
        let mut step = Lesson::ReadTheDiagnostic.first_step();
        let mut view = PracticeView {
            original_text: true,
            ..PracticeView::default()
        };
        let open = Action::OpenDiagnosticsPicker;
        assert!(!step.observe_view(&open, view));
        view.diagnostic_present = true;
        assert!(!step.observe_view(&open, view));
        view.diagnostics_picker_open = true;
        assert!(step.observe_view(&open, view));
        let jump = Action::OpenLocation(
            crate::plugin::PluginLocation {
                path: "main.hk".into(),
                line: 6,
                column: 4,
                column_encoding: crate::plugin::LocationColumnEncoding::Utf16,
            },
            crate::plugin::OpenLocationTarget::Current,
        );
        assert!(!step.observe_view(&jump, view));
        view.diagnostic_under_cursor = true;
        assert!(step.observe_view(&jump, view));
        assert!(!step.observe_view(&Action::ShowLineDiagnostics, view));
        view.diagnostic_popup_open = true;
        assert!(step.observe_view(&Action::ShowLineDiagnostics, view));
        assert!(!step.observe_view(&Action::CloseDialog, view));
        view.diagnostic_popup_open = false;
        assert!(step.observe_view(&Action::CloseDialog, view));
        assert_eq!(step, PracticeStep::Complete);
    }

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
            file_matches_buffer: false,
            dirty: false,
            inline_target_selected: false,
            inline_explanation_received: false,
            inline_comment_open: false,
            inline_edit_applied: false,
            inline_closed: true,
            fixed_text: false,
            bonus_text: false,
            original_text: true,
            agent_pane_open: false,
            agent_files_saved: false,
            agent_example_visible: false,
            diagnostic_present: false,
            diagnostics_picker_open: false,
            diagnostic_under_cursor: false,
            diagnostic_popup_open: false,
            ..PracticeView::default()
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
