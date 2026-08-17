//! Connected files shared by the navigation exercises.

pub(crate) const GUIDE: &str = "# Scoreboard practice\n\nThis small project belongs to Learn Red.\n\n- src/score.hk contains the scoring function.\n- tests/score.hk describes its expected result.\n- src/main.hk shows the call site.\n\nFind the two score.hk files, then return here.\n";
pub(crate) const SCORE: &str = "// Scoring implementation\nfn add_score(score: i32, points: i32) -> i32 {\n    score + points\n}\n";
pub(crate) const TESTS: &str = "// Scoring expectations\n// add_score(40, 2) should return 42.\n// A zero-point round should preserve the current score.\n";
pub(crate) const MAIN: &str =
    "// Scoreboard entry point\nfn main() {\n    let score = add_score(40, 2);\n}\n";
pub(crate) const FILES: &[(&str, &str)] = &[
    ("README.md", GUIDE),
    ("src/score.hk", SCORE),
    ("tests/score.hk", TESTS),
    ("src/main.hk", MAIN),
];
