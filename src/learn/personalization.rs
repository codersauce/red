//! Practice documents for settings and workflow discovery.

pub(crate) const RECOVERY_CONTENTS: &str =
    "status: TODO\n\nThis owned file stays unchanged on disk.\n";
pub(crate) const RECOVERY_RESULT: &str =
    "status: DONE\n\nThis owned file stays unchanged on disk.\n";

pub(crate) const KEYMAP_CONTENTS: &str = "# This is an owned practice config, not your real config. This deliberately long comment makes the effect of toggling line wrapping visible while you try the temporary binding.\n[keys.normal]\n\"F6\" = \"MoveRight\"\n";
