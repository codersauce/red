//! ANSI styling and output helpers for plain-terminal CLI output (not the TUI).
//!
//! Shared by onboarding and the utility subcommands so every pre-editor
//! message follows the same rules: color only on a TTY and only when the
//! `NO_COLOR` convention is not set, alignment computed on unstyled text, and
//! a closed stdout pipe ending the process quietly instead of panicking.
//!
//! Plain (uncolored) output is a stable interface: release CI and the updater
//! match `--self-check` lines verbatim, so styling must only ever add ANSI
//! codes around existing text, never change the text itself.

use std::io::{self, IsTerminal, Write};

pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const CYAN: &str = "\x1b[36m";
pub const BOLD_RED: &str = "\x1b[1;31m";
pub const BOLD_GREEN: &str = "\x1b[1;32m";
pub const BOLD_YELLOW: &str = "\x1b[1;33m";
pub const BOLD_CYAN: &str = "\x1b[1;36m";
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

/// Whether stderr should receive ANSI styling right now.
pub fn stderr_color_enabled() -> bool {
    color_enabled(
        std::env::var_os("NO_COLOR").is_some(),
        io::stderr().is_terminal(),
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

/// Prefix a success message with a green check mark on a color terminal.
/// Plain output is the message unchanged.
pub fn success(message: &str, use_color: bool) -> String {
    if use_color {
        format!("{GREEN}✓{RESET} {message}")
    } else {
        message.to_string()
    }
}

/// Write one line to stdout without panicking when the reader has gone away
/// (e.g. `red plugin catalog | head`). A closed pipe exits with 141, the
/// status a process killed by SIGPIPE reports, so pipelines behave as they
/// would with any other Unix tool; other write failures exit with an error.
pub fn print_line(line: &str) {
    write_line(&mut io::stdout().lock(), line);
}

/// Like [`print_line`], but without a trailing newline beyond what `text`
/// already contains.
pub fn print_text(text: &str) {
    let mut out = io::stdout().lock();
    handle_write_result(out.write_all(text.as_bytes()).and_then(|()| out.flush()));
}

fn write_line(out: &mut impl Write, line: &str) {
    handle_write_result(writeln!(out, "{line}"));
}

fn handle_write_result(result: io::Result<()>) {
    if let Err(error) = result {
        if error.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(141);
        }
        let _ = writeln!(io::stderr(), "red: failed to write to stdout: {error}");
        std::process::exit(1);
    }
}

/// Maps a cell's text to the ANSI code it should be painted with, if any.
pub type CellStyle = fn(&str) -> Option<&'static str>;

pub fn style_plain(_cell: &str) -> Option<&'static str> {
    None
}

pub fn style_bold(_cell: &str) -> Option<&'static str> {
    Some(BOLD)
}

pub fn style_dim(_cell: &str) -> Option<&'static str> {
    Some(DIM)
}

/// Render rows as space-aligned columns, sizing each column to its widest
/// cell. Widths are measured on the plain text and ANSI codes are applied
/// afterward, so styling never skews the columns. Each line starts with
/// `indent` and has no trailing whitespace.
pub fn render_table<const N: usize>(
    indent: &str,
    header: Option<[&str; N]>,
    rows: &[[String; N]],
    styles: [CellStyle; N],
    use_color: bool,
) -> Vec<String> {
    let mut widths: [usize; N] = header.map_or([0; N], |header| header.map(display_width));
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(display_width(cell));
        }
    }
    let render_row = |cells: [&str; N], codes: [Option<&'static str>; N]| {
        let mut line = indent.to_string();
        for (index, ((cell, width), code)) in cells.iter().zip(widths).zip(codes).enumerate() {
            if index > 0 {
                line.push_str("  ");
            }
            match code {
                Some(code) if !cell.is_empty() => line.push_str(&paint(code, cell, use_color)),
                _ => line.push_str(cell),
            }
            if index + 1 < N {
                line.extend(std::iter::repeat_n(' ', width - display_width(cell)));
            }
        }
        line.truncate(line.trim_end().len());
        line
    };
    let mut lines = Vec::with_capacity(rows.len() + 1);
    if let Some(header) = header {
        lines.push(render_row(header, [Some(DIM); N]));
    }
    for row in rows {
        lines.push(render_row(
            std::array::from_fn(|index| row[index].as_str()),
            std::array::from_fn(|index| styles[index](&row[index])),
        ));
    }
    lines
}

/// Print a table with a dimmed uppercase header to stdout. See [`render_table`].
pub fn print_table<const N: usize>(
    header: [&str; N],
    rows: &[[String; N]],
    styles: [CellStyle; N],
) {
    let mut out = io::stdout().lock();
    for line in render_table("", Some(header), rows, styles, stdout_color_enabled()) {
        write_line(&mut out, &line);
    }
}

fn display_width(text: &str) -> usize {
    text.chars().count()
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

    #[test]
    fn success_marks_only_when_colored() {
        assert_eq!(success("config ok", false), "config ok");
        assert!(success("config ok", true).ends_with(" config ok"));
    }

    fn rows() -> Vec<[String; 2]> {
        vec![
            ["a".to_string(), "x".to_string()],
            ["longer".to_string(), "y".to_string()],
        ]
    }

    #[test]
    fn render_table_aligns_plain_output() {
        let lines = render_table(
            "  ",
            Some(["NAME", "SOURCE"]),
            &rows(),
            [style_bold, style_dim],
            false,
        );
        assert_eq!(lines, ["  NAME    SOURCE", "  a       x", "  longer  y"]);
    }

    #[test]
    fn render_table_styling_does_not_change_visible_text() {
        let plain = render_table("", None, &rows(), [style_bold, style_dim], false);
        let colored = render_table("", None, &rows(), [style_bold, style_dim], true);
        let strip = |line: &String| {
            [BOLD, DIM, RESET]
                .iter()
                .fold(line.clone(), |line, code| line.replace(code, ""))
        };
        assert_eq!(plain, colored.iter().map(strip).collect::<Vec<_>>());
        assert!(colored[0].contains(BOLD));
    }

    #[test]
    fn render_table_drops_trailing_whitespace() {
        let rows = vec![
            ["a".to_string(), String::new()],
            ["longer".to_string(), "x".to_string()],
        ];
        let plain = render_table("", None, &rows, [style_plain, style_dim], false);
        assert_eq!(plain[0], "a");
        let colored = render_table("", None, &rows, [style_plain, style_dim], true);
        assert_eq!(colored[0], "a");
    }
}
