use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::event::{
    sys::unix::parse::{parse_event, BACKGROUND_REPORT_PREFIX},
    InternalEvent,
};

#[cfg(feature = "use-dev-tty")]
pub(crate) mod tty;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) mod mio;

#[cfg(feature = "use-dev-tty")]
pub(crate) use self::tty::UnixInternalEventSource;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) use self::mio::UnixInternalEventSource;

// A terminal reply, Escape, and Alt+] can all start with the same bytes.
// Wait briefly only while another byte could still distinguish them.
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(25);

struct Parser {
    buffer: Vec<u8>,
    internal_events: VecDeque<InternalEvent>,
    prefix_deadline: Option<Instant>,
    lone_escape: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            internal_events: VecDeque::with_capacity(128),
            prefix_deadline: None,
            lone_escape: false,
        }
    }
}

impl Parser {
    fn advance(&mut self, bytes: &[u8], more: bool) {
        // A lone Esc in the previous read was already a complete key unless
        // the next read continues the OSC introducer.
        if self.lone_escape && !bytes.starts_with(b"]") {
            self.emit_prefix_keys();
        }

        for &byte in bytes {
            let len = self.buffer.len();
            let diverges_from_report = (2..BACKGROUND_REPORT_PREFIX.len()).contains(&len)
                && self.buffer.starts_with(b"\x1b]")
                && byte != BACKGROUND_REPORT_PREFIX[len];
            if diverges_from_report || (self.buffer == b"\x1b" && byte == 0x1b) {
                self.emit_prefix_keys();
            }

            self.buffer.push(byte);
            if BACKGROUND_REPORT_PREFIX.starts_with(&self.buffer) {
                continue;
            }
            match parse_event(&self.buffer, true) {
                Ok(Some(event)) => self.internal_events.push_back(event),
                Ok(None) => continue,
                Err(_) => (),
            }
            self.buffer.clear();
        }

        self.lone_escape = !more && self.buffer == b"\x1b";
        self.prefix_deadline = (!self.buffer.is_empty()
            && self.buffer.len() < BACKGROUND_REPORT_PREFIX.len()
            && BACKGROUND_REPORT_PREFIX.starts_with(&self.buffer))
        .then(|| Instant::now() + ESCAPE_TIMEOUT);
    }

    // Replay an ambiguous prefix as the original Esc or Alt+] key, followed
    // by any digits already read. The mismatching byte starts the next event.
    fn emit_prefix_keys(&mut self) {
        let first_len = self.buffer.len().min(2);
        if let Ok(Some(key)) = parse_event(&self.buffer[..first_len], false) {
            self.internal_events.push_back(key);
        }
        for byte in &self.buffer[first_len..] {
            if let Ok(Some(key)) = parse_event(std::slice::from_ref(byte), false) {
                self.internal_events.push_back(key);
            }
        }
        self.buffer.clear();
        self.prefix_deadline = None;
        self.lone_escape = false;
    }

    fn poll_timeout(&self, timeout: Option<Duration>) -> Option<Duration> {
        match self.prefix_deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                Some(timeout.map_or(remaining, |timeout| timeout.min(remaining)))
            }
            None => timeout,
        }
    }

    fn next(&mut self) -> Option<InternalEvent> {
        if matches!(self.prefix_deadline, Some(deadline) if Instant::now() >= deadline) {
            self.emit_prefix_keys();
        }
        self.internal_events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> InternalEvent {
        InternalEvent::Event(Event::Key(code.into()))
    }

    #[test]
    fn red_discards_background_reports_split_at_any_byte() {
        let input = b"a\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b]11;rgb:00/00/00\x07\r";
        for split in 1..input.len() {
            let mut parser = Parser::default();
            parser.advance(&input[..split], false);
            parser.advance(&input[split..], false);
            assert_eq!(parser.next(), Some(key(KeyCode::Char('a'))));
            assert_eq!(parser.next(), Some(key(KeyCode::Enter)), "split {}", split);
            assert_eq!(parser.next(), None);
        }
    }

    #[test]
    fn red_replays_ambiguous_prefixes_as_keys() {
        let alt_bracket = InternalEvent::Event(Event::Key(KeyEvent::new(
            KeyCode::Char(']'),
            KeyModifiers::ALT,
        )));
        for prefix in [b"\x1b".as_slice(), b"\x1b]", b"\x1b]1", b"\x1b]11"] {
            for expired in [true, false] {
                let mut parser = Parser::default();
                parser.advance(prefix, false);
                assert!(parser.prefix_deadline.is_some());
                if expired {
                    parser.prefix_deadline = Some(Instant::now());
                } else {
                    parser.advance(b"x", false);
                }
                let first = if prefix.len() == 1 {
                    key(KeyCode::Esc)
                } else {
                    alt_bracket.clone()
                };
                assert_eq!(parser.next(), Some(first));
                for _ in 2..prefix.len() {
                    assert_eq!(parser.next(), Some(key(KeyCode::Char('1'))));
                }
                if !expired {
                    assert_eq!(parser.next(), Some(key(KeyCode::Char('x'))));
                }
                assert_eq!(parser.next(), None);
            }
        }
    }
}
