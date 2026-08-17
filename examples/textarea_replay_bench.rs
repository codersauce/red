//! Diagnostic replay timings for large embedded text areas. No timing assertions.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{editing::TextArea, editor::Mode};
use std::time::Instant;

fn keys(area: &mut TextArea, text: &str) {
    for ch in text.chars() {
        let code = if ch == '\u{1b}' {
            KeyCode::Esc
        } else {
            KeyCode::Char(ch)
        };
        area.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 80);
    }
}

fn main() {
    red::LOGGER.set(None).ok();
    for prefix in ["a".repeat(32_768), "λ".repeat(16_384)] {
        let mut area = TextArea::new(&prefix);
        area.set_mode(Mode::Normal);
        keys(&mut area, &format!("i{}\u{1b}", "x".repeat(128)));
        let before = area.text().len();
        let start = Instant::now();
        keys(&mut area, ".");
        let elapsed = start.elapsed();
        assert_eq!(area.text().len(), before + 128);
        println!(
            "ascii={} dot-128={:.3}ms",
            prefix.is_ascii(),
            elapsed.as_secs_f64() * 1000.0
        );
    }
}
