//! Small, exact fixtures for modal editing lessons.

pub(crate) const OBJECT_CONTENTS: &str = "let title = \"Old scoreboard\";\nlet score = 42;\n";
pub(crate) const OBJECT_EMPTY: &str = "let title = \"\";\nlet score = 42;\n";
pub(crate) const OBJECT_RESULT: &str = "let title = \"Scoreboard\";\nlet score = 42;\n";

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
    fn object_lesson_requires_preserved_quotes_and_finished_insert() {
        let lesson = Lesson::ChangeATextObject;
        let mut step = lesson.first_step();
        let change = Action::ChangeTextRange(crate::undo::TextRange::new(
            crate::undo::TextPosition::new(0, 13),
            crate::undo::TextPosition::new(0, 27),
        ));
        assert!(!step.observe(
            lesson,
            &Action::MoveRight,
            Mode::Normal,
            OBJECT_CONTENTS,
            (12, 0)
        ));
        assert!(step.observe(
            lesson,
            &Action::MoveRight,
            Mode::Normal,
            OBJECT_CONTENTS,
            (13, 0)
        ));
        assert!(!step.observe(lesson, &change, Mode::Insert, "let title = ;\n", (13, 0)));
        assert!(step.observe(lesson, &change, Mode::Insert, OBJECT_EMPTY, (13, 0)));
        assert!(!step.observe(
            lesson,
            &Action::InsertString("wrong".into()),
            Mode::Insert,
            OBJECT_EMPTY,
            (13, 0)
        ));
        assert!(step.observe(
            lesson,
            &Action::InsertString("Scoreboard".into()),
            Mode::Insert,
            OBJECT_RESULT,
            (23, 0)
        ));
        assert!(!step.observe(
            lesson,
            &Action::Refresh,
            Mode::Insert,
            OBJECT_RESULT,
            (23, 0)
        ));
        assert!(step.observe(
            lesson,
            &Action::EnterMode(Mode::Normal),
            Mode::Normal,
            OBJECT_RESULT,
            (22, 0)
        ));
        assert_eq!(step, PracticeStep::Complete);
    }

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
