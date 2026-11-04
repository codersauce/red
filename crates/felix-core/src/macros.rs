//! Helix stores a macro as one ordinary register fragment, in key notation.
#[derive(Default)]
pub struct MacroRecorder {
    active: Option<(char, Vec<crate::MacroKey>)>,
}

impl MacroRecorder {
    pub fn register(&self) -> Option<char> {
        self.active.as_ref().map(|(name, _)| *name)
    }

    /// Call for physical key presses, including the key that stops recording.
    pub fn record(&mut self, key: impl FnOnce() -> crate::MacroKey) {
        if let Some((_, keys)) = &mut self.active {
            keys.push(key());
        }
    }

    /// Returns the register and its new contents when the recording finishes.
    pub fn toggle(&mut self, register: char) -> Option<(char, String)> {
        if let Some((register, mut keys)) = self.active.take() {
            keys.pop();
            Some((
                register,
                keys.into_iter().map(crate::MacroKey::notation).collect(),
            ))
        } else {
            self.active = Some((register, Vec::new()));
            None
        }
    }
}

struct Frame {
    register: char,
    keys: Vec<crate::MacroKey>,
    index: usize,
    remaining: usize,
}

/// Playback frames stay active while their last key is being dispatched, so an
/// indirect call back into the same register is detected even in tail position.
#[derive(Default)]
pub struct MacroReplay {
    frames: Vec<Frame>,
    emitted: usize,
}

impl MacroReplay {
    pub fn active(&self) -> bool {
        !self.frames.is_empty()
    }

    pub fn start(
        &mut self,
        register: char,
        values: Option<&[String]>,
        count: usize,
    ) -> Result<(), String> {
        if self.frames.iter().any(|frame| frame.register == register) {
            return Err(format!(
                "Cannot replay from register [{register}] because already replaying from same register"
            ));
        }
        let Some([notation]) = values else {
            return Err(format!("Register [{register}] empty"));
        };
        let keys = parse_keys(notation).map_err(|err| format!("Invalid macro: {err}"))?;
        if !self.active() {
            self.emitted = 0;
        }
        if !keys.is_empty() && count > 0 {
            self.frames.push(Frame {
                register,
                keys,
                index: 0,
                remaining: count,
            });
        }
        Ok(())
    }

    pub fn next_key(&mut self, limit: usize) -> Result<Option<crate::MacroKey>, String> {
        while let Some(frame) = self.frames.last_mut() {
            if frame.remaining == 0 {
                self.frames.pop();
                continue;
            }
            if self.emitted >= limit {
                self.frames.clear();
                return Err(format!("macro instruction limit ({limit}) reached"));
            }
            self.emitted += 1;
            let key = frame.keys[frame.index];
            frame.index += 1;
            if frame.index == frame.keys.len() {
                frame.index = 0;
                frame.remaining -= 1;
            }
            return Ok(Some(key));
        }
        Ok(None)
    }
}

fn parse_keys(mut notation: &str) -> Result<Vec<crate::MacroKey>, String> {
    let mut keys = Vec::new();
    while let Some(ch) = notation.chars().next() {
        let key = match ch {
            '>' => return Err("Unmatched '>'".into()),
            '<' => {
                let (token, rest) = notation[1..].split_once('>').ok_or("'>' expected")?;
                notation = rest;
                token
            }
            _ => {
                let (token, rest) = notation.split_at(ch.len_utf8());
                notation = rest;
                token
            }
        };
        keys.push(crate::MacroKey::parse(key)?);
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_macros_fail_before_yielding_their_first_key() {
        for notation in ["lr<", "lr>", "lr<F25>", "lr<C-C-x>", "lr<unknown>"] {
            let mut replay = MacroReplay::default();
            assert!(replay.start('a', Some(&[notation.into()]), 1).is_err());
            assert_eq!(replay.next_key(8), Ok(None));
        }
    }

    #[test]
    fn nested_counts_share_a_budget_and_the_next_invocation_starts_fresh() {
        let mut replay = MacroReplay::default();
        replay.start('a', Some(&["q".into()]), usize::MAX).unwrap();
        assert_eq!(replay.next_key(3).unwrap().unwrap().notation(), "q");
        replay.start('b', Some(&["l".into()]), 2).unwrap();
        for _ in 0..2 {
            assert_eq!(replay.next_key(3).unwrap().unwrap().notation(), "l");
        }
        assert!(replay.start('b', Some(&["l".into()]), 1).is_err());
        assert_eq!(
            replay.next_key(3).unwrap_err(),
            "macro instruction limit (3) reached"
        );
        assert!(!replay.active());
        replay.start('a', Some(&["x".into()]), 1).unwrap();
        assert_eq!(replay.next_key(1).unwrap().unwrap().notation(), "x");
        assert_eq!(replay.next_key(1), Ok(None));
    }

    #[test]
    fn canonical_register_notation_round_trips_special_keys() {
        for notation in [
            "é",
            "<lt>",
            "<gt>",
            "<space>",
            "<minus>",
            "<A-C-ret>",
            "<F24>",
            "<S-tab>",
            "<Meta-S-界>",
        ] {
            let keys = parse_keys(notation).unwrap();
            assert_eq!(keys.len(), 1);
            assert_eq!(keys[0].notation(), notation);
        }
        assert_eq!(parse_keys("<Win-S-r>").unwrap()[0].notation(), "<Meta-R>");
    }
}
