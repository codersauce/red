//! Terminal keyboard negotiation shared by the editor, attachments, and diagnostics.
//!
//! Crossterm remains the only input reader. Protocol selection improves what the
//! terminal sends; it cannot recover modifiers swallowed by an emulator or an OS.

use std::{
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicU8, Ordering},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;

/// Diagnostic override; normal editor sessions use automatic negotiation.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum KeyboardPreference {
    #[default]
    Auto,
    Legacy,
    Kitty,
    Xterm,
}

/// The keyboard mode actually requested from the active terminal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyboardProtocol {
    #[default]
    Legacy = 0,
    NativeWindows = 1,
    Kitty = 2,
    XtermRequested = 3,
}

// A process has one terminal-input owner. This also lets the panic hook restore
// the active mode before leaving the alternate screen, without a double pop.
static ACTIVE_PROTOCOL: AtomicU8 = AtomicU8::new(0);

/// Recognizes the macOS and Windows word-backspace shortcuts on either platform.
/// Terminal protocols and remote sessions can report either modifier.
pub(crate) fn is_word_backspace(key: KeyEvent) -> bool {
    key.code == KeyCode::Backspace
        && key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
}

impl KeyboardProtocol {
    /// Negotiate before starting the event loop; capability queries share its reader.
    pub fn start(output: &mut impl Write, preference: KeyboardPreference) -> io::Result<Self> {
        #[cfg(windows)]
        let protocol = {
            let _ = preference;
            Self::NativeWindows
        };
        #[cfg(not(windows))]
        let protocol = match preference {
            KeyboardPreference::Legacy => Self::Legacy,
            KeyboardPreference::Kitty => Self::Kitty,
            KeyboardPreference::Xterm => Self::XtermRequested,
            KeyboardPreference::Auto => {
                let term = std::env::var("TERM").unwrap_or_default();
                if term.is_empty() || term == "dumb" {
                    Self::Legacy
                } else if terminal::supports_keyboard_enhancement().unwrap_or(false) {
                    Self::Kitty
                } else if xterm_candidate(&term) {
                    Self::XtermRequested
                } else {
                    Self::Legacy
                }
            }
        };
        if let Err(error) = protocol.write_start(output) {
            let _ = protocol.write_stop(output);
            return Err(error);
        }
        ACTIVE_PROTOCOL.store(protocol as u8, Ordering::SeqCst);
        Ok(protocol)
    }

    /// Restore only the mode this owner enabled. Repeated cleanup is harmless.
    pub fn stop(self, output: &mut impl Write) -> io::Result<()> {
        if ACTIVE_PROTOCOL
            .compare_exchange(self as u8, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.write_stop(output)?;
        }
        Ok(())
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::NativeWindows => "native Windows console",
            Self::Kitty => "Kitty disambiguation",
            Self::XtermRequested => "xterm modifyOtherKeys requested (support unconfirmed)",
        }
    }

    fn write_start(self, output: &mut impl Write) -> io::Result<()> {
        output.write_all(match self {
            Self::Kitty => b"\x1b[>1u",
            Self::XtermRequested => b"\x1b[>4;2m",
            Self::Legacy | Self::NativeWindows => b"",
        })?;
        output.flush()
    }

    fn write_stop(self, output: &mut impl Write) -> io::Result<()> {
        output.write_all(match self {
            Self::Kitty => b"\x1b[<1u",
            // Omitted value restores the terminal's configured initial value.
            Self::XtermRequested => b"\x1b[>4m",
            Self::Legacy | Self::NativeWindows => b"",
        })?;
        output.flush()
    }
}

#[cfg(any(not(windows), test))]
fn xterm_candidate(term: &str) -> bool {
    ["xterm", "screen", "tmux", "rxvt"].iter().any(|prefix| {
        term == *prefix
            || term
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('-'))
    })
}

/// Best-effort panic cleanup, performed before leaving the alternate screen.
pub fn restore_after_panic(output: &mut impl Write) {
    let protocol = match ACTIVE_PROTOCOL.swap(0, Ordering::SeqCst) {
        2 => KeyboardProtocol::Kitty,
        3 => KeyboardProtocol::XtermRequested,
        _ => return,
    };
    let _ = protocol.write_stop(output);
}

struct DiagnosticTerminal {
    protocol: KeyboardProtocol,
    restore_raw_mode: bool,
}

impl Drop for DiagnosticTerminal {
    fn drop(&mut self) {
        let _ = self.protocol.stop(&mut io::stdout());
        if self.restore_raw_mode {
            let _ = terminal::disable_raw_mode();
        }
    }
}

/// Inspect decoded keys in an isolated, explicitly invoked diagnostic session.
/// Ordinary characters and paste contents are deliberately not displayed or logged.
pub fn inspect_keys(preference: KeyboardPreference, count: Option<usize>) -> anyhow::Result<()> {
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "key inspection requires a terminal"
    );
    let restore_raw_mode = !terminal::is_raw_mode_enabled()?;
    terminal::enable_raw_mode()?;
    let mut guard = DiagnosticTerminal {
        protocol: KeyboardProtocol::Legacy,
        restore_raw_mode,
    };
    let mut output = io::stdout();
    guard.protocol = KeyboardProtocol::start(&mut output, preference)?;
    writeln!(
        output,
        "Keyboard protocol: {}\r",
        guard.protocol.description()
    )?;
    writeln!(
        output,
        "Press Enter, Backspace, or their modified shortcuts; Esc/Ctrl+C exits. Text is not recorded.\r"
    )?;
    output.flush()?;
    let mut seen = 0;
    while count.is_none_or(|limit| seen < limit) {
        if let Event::Key(key) = event::read()? {
            writeln!(output, "{}\r", describe_key(key))?;
            output.flush()?;
            seen += 1;
            if key.kind != KeyEventKind::Release
                && (key.code == KeyCode::Esc
                    || (matches!(key.code, KeyCode::Char('c' | 'C'))
                        && key.modifiers.contains(KeyModifiers::CONTROL)))
            {
                break;
            }
        }
    }
    Ok(())
}

fn describe_key(key: KeyEvent) -> String {
    let code = match key.code {
        KeyCode::Enter => "Enter",
        KeyCode::Backspace => "Backspace",
        KeyCode::Char('\r') => "CR",
        KeyCode::Char('\n') => "LF",
        KeyCode::Char('j' | 'J') if key.modifiers.contains(KeyModifiers::CONTROL) => "Ctrl+J",
        KeyCode::Esc => "Escape",
        _ => "other key (hidden)",
    };
    format!(
        "code={code} modifiers={:?} kind={:?}",
        key.modifiers, key.kind
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_backspace_recognizes_both_platform_shortcuts() {
        for modifiers in [
            KeyModifiers::ALT,
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            assert!(is_word_backspace(KeyEvent::new(
                KeyCode::Backspace,
                modifiers
            )));
        }
        for (code, modifiers) in [
            (KeyCode::Backspace, KeyModifiers::NONE),
            (KeyCode::Backspace, KeyModifiers::SHIFT),
            (KeyCode::Delete, KeyModifiers::CONTROL),
            (KeyCode::Char('h'), KeyModifiers::CONTROL),
        ] {
            assert!(!is_word_backspace(KeyEvent::new(code, modifiers)));
        }
        assert!(
            describe_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT))
                .contains("code=Backspace modifiers=KeyModifiers(ALT)")
        );
    }

    #[test]
    fn protocol_commands_are_balanced_and_minimal() {
        for (protocol, expected) in [
            (KeyboardProtocol::Legacy, &b""[..]),
            (KeyboardProtocol::NativeWindows, &b""[..]),
            (KeyboardProtocol::Kitty, &b"\x1b[>1u\x1b[<1u"[..]),
            (KeyboardProtocol::XtermRequested, &b"\x1b[>4;2m\x1b[>4m"[..]),
        ] {
            let mut output = Vec::new();
            protocol.write_start(&mut output).unwrap();
            protocol.write_stop(&mut output).unwrap();
            assert_eq!(output, expected);
        }
    }

    #[test]
    fn legacy_fallback_is_limited_to_xterm_compatible_names() {
        for term in [
            "xterm",
            "xterm-256color",
            "screen-256color",
            "tmux-256color",
            "rxvt-unicode",
        ] {
            assert!(xterm_candidate(term), "{term}");
        }
        for term in ["dumb", "linux", "vt100", "", "xterminal"] {
            assert!(!xterm_candidate(term), "{term}");
        }
    }

    #[test]
    fn diagnostic_hides_ordinary_text() {
        assert!(
            describe_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
                .contains("other key (hidden)")
        );
        assert!(
            describe_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))
                .contains("code=Enter modifiers=KeyModifiers(ALT)")
        );
    }
}
