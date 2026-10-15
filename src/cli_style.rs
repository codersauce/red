//! ANSI styling for plain-terminal CLI output (not the TUI).
//!
//! Shared by onboarding and the `red plugin` subcommands so every pre-editor
//! message follows the same rules: color only on a TTY and only when the
//! `NO_COLOR` convention is not set.

use std::io::{self, IsTerminal};

pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const CYAN: &str = "\x1b[36m";
pub const RESET: &str = "\x1b[0m";

/// Decide whether to emit ANSI color: only on a TTY and only when the
/// `NO_COLOR` convention is not set.
pub fn color_enabled(no_color_set: bool, is_tty: bool) -> bool {
    is_tty && !no_color_set
}

/// Whether stdout should receive ANSI styling right now.
pub fn stdout_color_enabled() -> bool {
    color_enabled(
        std::env::var_os("NO_COLOR").is_some(),
        io::stdout().is_terminal(),
    )
}

/// Wrap `text` in an ANSI `code` when `use_color`, otherwise return it plain.
pub fn paint(code: &str, text: &str, use_color: bool) -> String {
    if use_color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_enabled_only_on_tty_without_no_color() {
        assert!(color_enabled(false, true));
        assert!(!color_enabled(true, true)); // NO_COLOR set disables color
        assert!(!color_enabled(false, false)); // not a tty disables color
        assert!(!color_enabled(true, false));
    }

    #[test]
    fn paint_wraps_only_when_colored() {
        assert_eq!(paint(GREEN, "ok", true), format!("{GREEN}ok{RESET}"));
        assert_eq!(paint(GREEN, "ok", false), "ok");
    }
}
