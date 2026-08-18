//! Action-driven lessons for Red's optional, side-effect-free first-run tour.
//!
//! Lessons observe semantic editor actions rather than raw key presses. This keeps
//! custom keymaps working and lets real pickers remain interactive while the coach is
//! visible. Git and agent lessons explicitly use simulated, editor-owned previews.

use serde::{Deserialize, Serialize};

use crate::{buffer::BufferId, editor::Action, editor::Mode, window::WindowManagerSnapshot};

/// Increment this when persisted lesson identifiers or progression rules change.
pub const CURRICULUM_VERSION: u16 = 1;

/// Initial contents of the unnamed, never-saved practice buffer.
pub const PRACTICE_CONTENTS: &str = r#"// Welcome to Red. This practice buffer never touches your project.

fn total_price(prices: &[u32]) -> u32 {
    prices.iter().sum()
}

fn main() {
    let prices = [12, 8, 4];
    println!(\"Total: {}\", total_price(&prices));
}
"#;

/// Length and audience of a guided editor tour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorialTrack {
    /// The complete tour, including modal editing, Git, and theme customization.
    #[default]
    Guided,
    /// A short tour for people already comfortable with Vim.
    Quick,
}

impl TutorialTrack {
    /// User-facing duration for the selected curriculum.
    #[must_use]
    pub const fn duration(self) -> &'static str {
        match self {
            Self::Guided => "about 5 min",
            Self::Quick => "about 90 sec",
        }
    }

    /// Ordered lessons included in the selected curriculum.
    #[must_use]
    pub const fn lessons(self) -> &'static [TutorialLesson] {
        match self {
            Self::Guided => &GUIDED_LESSONS,
            Self::Quick => &QUICK_LESSONS,
        }
    }
}

/// Stable identifier for a hands-on lesson.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorialLesson {
    /// Enter Insert mode, change text, return to Normal mode, and undo.
    Editing,
    /// Discover effective commands and their configured shortcuts.
    Discovery,
    /// Open the file picker and project-search picker.
    Navigation,
    /// Trigger offline-safe, buffer-word completion.
    Completion,
    /// Inspect a simulated Git diff without touching the real repository.
    Git,
    /// Inspect and accept or reject a simulated agent proposal.
    Agent,
    /// Preview an installed theme and explicitly decide whether to keep it.
    Themes,
}

const GUIDED_LESSONS: [TutorialLesson; 7] = [
    TutorialLesson::Editing,
    TutorialLesson::Discovery,
    TutorialLesson::Navigation,
    TutorialLesson::Completion,
    TutorialLesson::Git,
    TutorialLesson::Agent,
    TutorialLesson::Themes,
];

const QUICK_LESSONS: [TutorialLesson; 3] = [
    TutorialLesson::Discovery,
    TutorialLesson::Navigation,
    TutorialLesson::Agent,
];

impl TutorialLesson {
    /// Short, readable title shown in the coach and progress indicator.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Editing => "Editing that feels familiar",
            Self::Discovery => "Discover every command",
            Self::Navigation => "Find files and search projects",
            Self::Completion => "Code intelligence, immediately",
            Self::Git => "Git without leaving Red",
            Self::Agent => "The agent asks permission",
            Self::Themes => "Make Red yours",
        }
    }

    /// The semantic action whose effective shortcut should be displayed.
    #[must_use]
    pub fn suggested_action(self, phase: u8) -> Option<Action> {
        match (self, phase) {
            (Self::Editing, 0) => Some(Action::EnterMode(Mode::Insert)),
            (Self::Editing, 2) => Some(Action::EnterMode(Mode::Normal)),
            (Self::Editing, 3) => Some(Action::Undo),
            (Self::Discovery, _) => Some(Action::CommandPalette),
            (Self::Navigation, 0) => Some(Action::FilePicker),
            (Self::Navigation, _) => Some(Action::PluginCommand("ProjectSearch".to_string())),
            (Self::Completion, 0) => Some(Action::EnterMode(Mode::Insert)),
            (Self::Completion, _) => Some(Action::RequestCompletion),
            (Self::Git, 0) => Some(Action::PluginCommand("GitDashboard".to_string())),
            (Self::Agent, 0) => Some(Action::PluginCommand("Agent".to_string())),
            (Self::Themes, 0) => Some(Action::PluginCommand("ThemeBrowser".to_string())),
            _ => None,
        }
    }

    /// Contextual instruction for the next real action in this lesson.
    #[must_use]
    pub fn instruction(self, phase: u8, shortcut: Option<&str>) -> String {
        let key = |fallback| shortcut.unwrap_or(fallback);
        match (self, phase) {
            (Self::Editing, 0) => format!("Press {} to enter Insert mode.", key("i")),
            (Self::Editing, 1) => "Type a few characters in the practice buffer.".to_string(),
            (Self::Editing, 2) => format!("Press {} to return to Normal mode.", key("Esc")),
            (Self::Editing, _) => format!("Press {} to undo your practice edit.", key("u")),
            (Self::Discovery, _) => {
                format!("Press {} to open the command palette.", key("Space ?"))
            }
            (Self::Navigation, 0) => {
                format!("Press {} to open the fuzzy file picker.", key("Ctrl-p"))
            }
            (Self::Navigation, _) => format!(
                "Close the picker, then press {} to search the project.",
                key("Space g")
            ),
            (Self::Completion, 0) => {
                format!("Press {} to enter Insert mode first.", key("i"))
            }
            (Self::Completion, _) => format!(
                "Press {} for offline-safe buffer completion.",
                key("Ctrl-Space")
            ),
            (Self::Git, 0) => format!("Press {} to inspect a safe sample diff.", key("Space G")),
            (Self::Git, _) => "Inspect the sample diff, then press Esc to continue.".to_string(),
            (Self::Agent, 0) => {
                format!("Press {} to inspect a simulated proposal.", key("Space A"))
            }
            (Self::Agent, _) => {
                "Press a to accept or r to reject the practice-only proposal.".to_string()
            }
            (Self::Themes, 0) => format!("Press {} to browse installed themes.", key("Space t")),
            (Self::Themes, _) => {
                "Preview a theme, then select one or close the browser.".to_string()
            }
        }
    }

    /// Short product explanation shown below the active instruction.
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::Editing => "Your normal Vim muscle memory works here.",
            Self::Discovery => "Search commands, descriptions, and your actual keymaps.",
            Self::Navigation => "Fuzzy previews and project search keep your hands on the keys.",
            Self::Completion => "Open buffers supply suggestions even without a language server.",
            Self::Git => "Real Git changes always remain under your control.",
            Self::Agent => "Nothing changes on disk until you review and accept it.",
            Self::Themes => "Bundled themes and plugins work without a config file.",
        }
    }
}

/// Persisted progress that can resume after Red restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorialProgress {
    /// Schema version of the lesson identifiers and action phases.
    #[serde(default = "curriculum_version")]
    pub curriculum_version: u16,
    /// Curriculum selected by the user.
    #[serde(default)]
    pub track: TutorialTrack,
    /// Zero-based index into the selected track.
    #[serde(default)]
    pub lesson_index: usize,
    /// Zero-based phase within the current lesson.
    #[serde(default)]
    pub phase: u8,
    /// Whether every lesson in the selected track has completed.
    #[serde(default)]
    pub completed: bool,
}

const fn curriculum_version() -> u16 {
    CURRICULUM_VERSION
}

impl TutorialProgress {
    /// Creates a new, unstarted track.
    #[must_use]
    pub const fn new(track: TutorialTrack) -> Self {
        Self {
            curriculum_version: CURRICULUM_VERSION,
            track,
            lesson_index: 0,
            phase: 0,
            completed: false,
        }
    }

    /// Returns the current lesson unless the selected track is complete.
    #[must_use]
    pub fn current_lesson(&self) -> Option<TutorialLesson> {
        (!self.completed)
            .then(|| self.track.lessons().get(self.lesson_index).copied())
            .flatten()
    }

    /// Older curricula restart safely instead of pointing at an unrelated lesson.
    #[must_use]
    pub fn normalized(self) -> Self {
        if self.curriculum_version != CURRICULUM_VERSION
            || self.lesson_index >= self.track.lessons().len() && !self.completed
        {
            Self::new(self.track)
        } else {
            self
        }
    }
}

/// Result of observing one semantic editor action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialObservation {
    /// The current lesson did not consume this action.
    Unchanged,
    /// Another action is needed inside the same lesson.
    Progressed,
    /// The next lesson is now active.
    Advanced,
    /// The selected track has finished.
    Completed,
}

/// Runtime-only ownership of the practice buffer and pre-tutorial window layout.
#[derive(Debug)]
pub struct TutorialController {
    progress: TutorialProgress,
    /// Stable identity of the unnamed practice buffer.
    pub practice_buffer_id: BufferId,
    /// Buffer selected before the tutorial opened.
    pub original_buffer_index: usize,
    /// Full pre-tutorial split tree and cursor/window state.
    pub original_window_layout: WindowManagerSnapshot,
}

impl TutorialController {
    /// Starts or resumes a curriculum inside an isolated practice buffer.
    #[must_use]
    pub fn new(
        progress: TutorialProgress,
        practice_buffer_id: BufferId,
        original_buffer_index: usize,
        original_window_layout: WindowManagerSnapshot,
    ) -> Self {
        Self {
            progress: progress.normalized(),
            practice_buffer_id,
            original_buffer_index,
            original_window_layout,
        }
    }

    /// Current progress for display and persistence.
    #[must_use]
    pub const fn progress(&self) -> &TutorialProgress {
        &self.progress
    }

    /// Current product lesson, if the track has not completed.
    #[must_use]
    pub fn lesson(&self) -> Option<TutorialLesson> {
        self.progress.current_lesson()
    }

    /// Advances explicitly, for the always-available skip command.
    pub fn advance(&mut self) -> TutorialObservation {
        if self.progress.completed {
            return TutorialObservation::Unchanged;
        }
        self.progress.lesson_index = self.progress.lesson_index.saturating_add(1);
        self.progress.phase = 0;
        if self.progress.lesson_index >= self.progress.track.lessons().len() {
            self.progress.completed = true;
            TutorialObservation::Completed
        } else {
            TutorialObservation::Advanced
        }
    }

    /// Observes editor actions without capturing keys or bypassing normal dispatch.
    pub fn observe(&mut self, action: &Action) -> TutorialObservation {
        let Some(lesson) = self.lesson() else {
            return TutorialObservation::Unchanged;
        };

        let accepted = match (lesson, self.progress.phase, action) {
            (TutorialLesson::Editing, 0, Action::EnterMode(Mode::Insert))
            | (TutorialLesson::Editing, 2, Action::EnterMode(Mode::Normal)) => true,
            (
                TutorialLesson::Editing,
                1,
                Action::InsertCharAtCursorPos(_)
                | Action::InsertString(_)
                | Action::InsertPastedText(_),
            ) => true,
            (TutorialLesson::Editing, 3, Action::Undo) => return self.advance(),
            (TutorialLesson::Discovery, _, Action::CommandPalette) => return self.advance(),
            (TutorialLesson::Navigation, 0, Action::FilePicker) => true,
            (TutorialLesson::Navigation, 1, Action::PluginCommand(name))
                if name == "ProjectSearch" =>
            {
                return self.advance();
            }
            (TutorialLesson::Completion, 0, Action::EnterMode(Mode::Insert)) => true,
            (
                TutorialLesson::Completion,
                _,
                Action::RequestCompletion | Action::RequestCompletionWithTrigger(_),
            ) => return self.advance(),
            (TutorialLesson::Git, 0, Action::PluginCommand(name)) if name == "GitDashboard" => true,
            (TutorialLesson::Git, 1, Action::DismissTutorialDemo) => return self.advance(),
            (TutorialLesson::Agent, 0, Action::PluginCommand(name))
                if matches!(name.as_str(), "Agent" | "AgentOpen") =>
            {
                true
            }
            (
                TutorialLesson::Agent,
                1,
                Action::AcceptTutorialProposal | Action::RejectTutorialProposal,
            ) => return self.advance(),
            (TutorialLesson::Themes, 0, Action::PluginCommand(name)) if name == "ThemeBrowser" => {
                true
            }
            (TutorialLesson::Themes, 1, Action::SetTheme(_) | Action::CloseDialog) => {
                return self.advance();
            }
            _ => false,
        };

        if accepted {
            self.progress.phase = self.progress.phase.saturating_add(1);
            TutorialObservation::Progressed
        } else {
            TutorialObservation::Unchanged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_track_teaches_reds_seven_highlights() {
        assert_eq!(TutorialTrack::Guided.lessons().len(), 7);
        assert!(TutorialTrack::Guided
            .lessons()
            .contains(&TutorialLesson::Git));
        assert!(TutorialTrack::Guided
            .lessons()
            .contains(&TutorialLesson::Agent));
    }

    #[test]
    fn quick_track_prioritizes_discovery_navigation_and_safe_agents() {
        assert_eq!(
            TutorialTrack::Quick.lessons(),
            &[
                TutorialLesson::Discovery,
                TutorialLesson::Navigation,
                TutorialLesson::Agent,
            ]
        );
    }

    #[test]
    fn stale_or_invalid_progress_restarts_the_same_track() {
        let stale = TutorialProgress {
            curriculum_version: 0,
            track: TutorialTrack::Quick,
            lesson_index: 100,
            phase: 3,
            completed: false,
        };

        assert_eq!(
            stale.normalized(),
            TutorialProgress::new(TutorialTrack::Quick)
        );
    }

    #[test]
    fn lessons_keep_real_fallback_shortcuts_readable() {
        assert!(TutorialLesson::Agent
            .instruction(/*phase*/ 0, /*shortcut*/ None)
            .contains("Space A"));
        assert!(TutorialLesson::Discovery
            .instruction(/*phase*/ 0, Some("F1"))
            .contains("F1"));
    }
}
