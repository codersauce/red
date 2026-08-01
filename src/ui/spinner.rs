//! Shared terminal spinner frames for editor-owned loading surfaces.

/// Delay between adjacent frames in the compact Braille loading animation.
pub(crate) const SPINNER_FRAME_INTERVAL_MS: u64 = 120;

/// One complete revolution of the compact Braille loading animation.
pub(crate) const SPINNER_FRAME_COUNT: usize = 10;

const SPINNER_FRAMES: [&str; SPINNER_FRAME_COUNT] =
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Returns the shared spinner glyph for the elapsed animation time.
pub(crate) fn spinner_frame(elapsed_ms: u64) -> &'static str {
    let index = (elapsed_ms / SPINNER_FRAME_INTERVAL_MS) as usize;
    SPINNER_FRAMES[index % SPINNER_FRAME_COUNT]
}

#[cfg(test)]
mod tests {
    use super::{spinner_frame, SPINNER_FRAME_COUNT, SPINNER_FRAME_INTERVAL_MS};

    #[test]
    fn braille_spinner_advances_and_wraps_at_the_shared_interval() {
        assert_eq!(spinner_frame(0), "⠋");
        assert_eq!(spinner_frame(SPINNER_FRAME_INTERVAL_MS - 1), "⠋");
        assert_eq!(spinner_frame(SPINNER_FRAME_INTERVAL_MS), "⠙");
        assert_eq!(
            spinner_frame(SPINNER_FRAME_INTERVAL_MS * SPINNER_FRAME_COUNT as u64),
            "⠋"
        );
    }
}
