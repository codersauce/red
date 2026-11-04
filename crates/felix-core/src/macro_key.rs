//! Key notation shared by register recording and playback, independent of a terminal backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroCode {
    Char(char),
    Function(u8),
    Named(NamedKey),
}

macro_rules! named_keys {
    ($($key:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum NamedKey { $($key),+ }
        impl NamedKey {
            pub fn name(self) -> &'static str {
                match self { $(Self::$key => $name),+ }
            }
            fn parse(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$key)),+, _ => None }
            }
        }
    };
}
named_keys! {
    Enter => "ret",
    Tab => "tab",
    Esc => "esc",
    Backspace => "backspace",
    Delete => "del",
    Insert => "ins",
    Left => "left",
    Right => "right",
    Up => "up",
    Down => "down",
    Home => "home",
    End => "end",
    PageUp => "pageup",
    PageDown => "pagedown",
    Null => "null",
    CapsLock => "capslock",
    ScrollLock => "scrolllock",
    NumLock => "numlock",
    PrintScreen => "printscreen",
    Pause => "pause",
    Menu => "menu",
    KeypadBegin => "keypadbegin",
    MediaPlay => "play",
    MediaPause => "pausemedia",
    MediaPlayPause => "playpause",
    MediaReverse => "reverse",
    MediaStop => "stop",
    MediaFastForward => "fastforward",
    MediaRewind => "rewind",
    MediaTrackNext => "tracknext",
    MediaTrackPrevious => "trackprevious",
    MediaRecord => "record",
    MediaLowerVolume => "lowervolume",
    MediaRaiseVolume => "raisevolume",
    MediaMuteVolume => "mutevolume",
    LeftShift => "leftshift",
    LeftControl => "leftcontrol",
    LeftAlt => "leftalt",
    LeftSuper => "leftsuper",
    LeftHyper => "lefthyper",
    LeftMeta => "leftmeta",
    RightShift => "rightshift",
    RightControl => "rightcontrol",
    RightAlt => "rightalt",
    RightSuper => "rightsuper",
    RightHyper => "righthyper",
    RightMeta => "rightmeta",
    IsoLevel3Shift => "isolevel3shift",
    IsoLevel5Shift => "isolevel5shift",
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacroKey {
    pub code: MacroCode,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub meta: bool,
}

impl MacroKey {
    pub fn plain(code: MacroCode) -> Self {
        Self {
            code,
            shift: false,
            alt: false,
            control: false,
            meta: false,
        }
    }

    pub fn notation(self) -> String {
        let name = match self.code {
            MacroCode::Char(' ') => "space".into(),
            MacroCode::Char('-') => "minus".into(),
            MacroCode::Char('<') => "lt".into(),
            MacroCode::Char('>') => "gt".into(),
            MacroCode::Char(ch) => ch.to_string(),
            MacroCode::Function(n) => format!("F{n}"),
            MacroCode::Named(code) => code.name().to_owned(),
        };
        if !self.meta && !self.shift && !self.alt && !self.control && name.chars().count() == 1 {
            return name;
        }
        let mut token = String::from("<");
        for (set, prefix) in [
            (self.meta, "Meta-"),
            (self.shift, "S-"),
            (self.alt, "A-"),
            (self.control, "C-"),
        ] {
            if set {
                token.push_str(prefix);
            }
        }
        token.push_str(&name);
        token.push('>');
        token
    }

    pub fn parse(token: &str) -> Result<Self, String> {
        if token == "-" {
            return Ok(Self::plain(MacroCode::Char('-')));
        }
        let mut parts = token.rsplit('-');
        let name = parts.next().unwrap_or_default();
        let mut chars = name.chars();
        let code = match name {
            "space" => MacroCode::Char(' '),
            "minus" => MacroCode::Char('-'),
            "lt" => MacroCode::Char('<'),
            "gt" => MacroCode::Char('>'),
            _ if name.chars().count() == 1 => MacroCode::Char(chars.next().unwrap()),
            _ if name.starts_with('F') => {
                let n = name[1..].parse::<u8>().map_err(|e| e.to_string())?;
                if !(1..=24).contains(&n) {
                    return Err(format!("Invalid function key '{n}'"));
                }
                MacroCode::Function(n)
            }
            "" if token.ends_with("--") => {
                return Err(format!(
                    "Key '-' cannot be used with modifiers, use '{}-minus' instead",
                    token.trim_end_matches('-')
                ));
            }
            _ => MacroCode::Named(
                NamedKey::parse(name).ok_or_else(|| format!("Invalid key code '{name}'"))?,
            ),
        };
        let mut key = Self::plain(code);
        for part in parts.rev() {
            let set = match part {
                "S" => &mut key.shift,
                "A" => &mut key.alt,
                "C" => &mut key.control,
                "Meta" | "Cmd" | "Win" => &mut key.meta,
                _ => return Err(format!("Invalid key modifier '{part}-'")),
            };
            if *set {
                return Err(format!("Repeated key modifier '{part}-'"));
            }
            *set = true;
        }
        if let MacroCode::Char(ch) = key.code
            && ch.is_ascii_lowercase()
            && key.shift
        {
            key.code = MacroCode::Char(ch.to_ascii_uppercase());
            key.shift = false;
        }
        Ok(key)
    }
}
