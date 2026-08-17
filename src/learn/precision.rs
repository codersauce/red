//! Small, exact fixtures for modal editing lessons.

pub(crate) const MOTION_CONTENTS: &str = "let score = unused 42;\nlet next = score + 3;\n";
pub(crate) const MOTION_RESULT: &str = "let score = 42;\nlet next = score + 3;\n";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::{Action, Mode},
        learn::{Lesson, PracticeStep},
    };

    #[test]
    fn motion_lesson_checks_position_and_exact_operator_result() {
        let lesson = Lesson::MoveWithIntent;
        let mut step = lesson.first_step();
        assert!(!step.observe(
            lesson,
            &Action::MoveToNextWord,
            Mode::Normal,
            MOTION_CONTENTS,
            (4, 0)
        ));
        assert!(step.observe(
            lesson,
            &Action::MoveToNextWord,
            Mode::Normal,
            MOTION_CONTENTS,
            (12, 0)
        ));
        assert!(!step.observe(
            lesson,
            &Action::DeleteWord,
            Mode::Normal,
            "wrong edit",
            (12, 0)
        ));
        assert!(step.observe(
            lesson,
            &Action::DeleteWord,
            Mode::Normal,
            MOTION_RESULT,
            (12, 0)
        ));
        assert!(!step.observe(lesson, &Action::Undo, Mode::Normal, MOTION_RESULT, (12, 0)));
        assert!(step.observe(
            lesson,
            &Action::Undo,
            Mode::Normal,
            MOTION_CONTENTS,
            (12, 0)
        ));
        assert!(step.observe(lesson, &Action::Redo, Mode::Normal, MOTION_RESULT, (12, 0)));
        assert_eq!(step, PracticeStep::Complete);
    }
}
