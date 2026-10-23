//! Hotkey strings from the config (`"Ctrl+Shift+G"`, `"Mouse4"`) as typed values.
//!
//! Pure parsing, no Win32 calls: the platform thread maps the result to
//! `RegisterHotKey` (keyboard) or `GetAsyncKeyState` polling (mouse, PTT).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Virtual-key codes used by the parser (values from `winuser.h`).
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;
pub const VK_XBUTTON1: u16 = 0x05;
pub const VK_XBUTTON2: u16 = 0x06;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

/// Extra mouse buttons (the only ones a global shortcut may use).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// "Mouse4" (back).
    X1,
    /// "Mouse5" (forward).
    X2,
}

impl MouseButton {
    pub fn vk(self) -> u16 {
        match self {
            MouseButton::X1 => VK_XBUTTON1,
            MouseButton::X2 => VK_XBUTTON2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyKey {
    /// Windows virtual-key code.
    Vk(u16),
    Mouse(MouseButton),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    pub key: HotkeyKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseHotkeyError {
    #[error("empty hotkey")]
    Empty,
    #[error("empty part in hotkey {0:?} (stray '+')")]
    EmptyPart(String),
    #[error("unknown key {0:?}")]
    UnknownKey(String),
    #[error("{0:?} is not a modifier (Ctrl, Alt, Shift, Win); the key must come last")]
    NotAModifier(String),
    #[error("modifier {0:?} given twice")]
    DuplicateModifier(String),
    #[error("hotkey {0:?} has only modifiers; a key is required")]
    ModifierOnly(String),
}

const NAMED_KEYS: &[(&str, u16)] = &[
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Esc", 0x1B),
    ("Backspace", 0x08),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("NumpadMultiply", 0x6A),
    ("NumpadAdd", 0x6B),
    ("NumpadSubtract", 0x6D),
    ("NumpadDecimal", 0x6E),
    ("NumpadDivide", 0x6F),
];

const ALIASES: &[(&str, u16)] = &[
    ("Escape", 0x1B),
    ("Return", 0x0D),
    ("Ins", 0x2D),
    ("Del", 0x2E),
    ("PgUp", 0x21),
    ("PgDn", 0x22),
];

fn parse_modifier(token: &str, m: &mut Modifiers) -> Option<Result<(), ParseHotkeyError>> {
    let slot = match token.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => &mut m.ctrl,
        "alt" => &mut m.alt,
        "shift" => &mut m.shift,
        "win" | "windows" | "super" | "meta" => &mut m.win,
        _ => return None,
    };
    if *slot {
        return Some(Err(ParseHotkeyError::DuplicateModifier(token.to_string())));
    }
    *slot = true;
    Some(Ok(()))
}

fn parse_key(token: &str) -> Option<HotkeyKey> {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "mouse4" => return Some(HotkeyKey::Mouse(MouseButton::X1)),
        "mouse5" => return Some(HotkeyKey::Mouse(MouseButton::X2)),
        _ => {}
    }
    let bytes = lower.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
        return Some(HotkeyKey::Vk(u16::from(bytes[0].to_ascii_uppercase())));
    }
    if bytes.len() == 1 && bytes[0].is_ascii_digit() {
        return Some(HotkeyKey::Vk(u16::from(bytes[0])));
    }
    // F1..F24
    if let Some(n) = lower.strip_prefix('f')
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
        && let Ok(n) = n.parse::<u16>()
        && (1..=24).contains(&n)
    {
        return Some(HotkeyKey::Vk(0x70 + n - 1));
    }
    // Numpad0..Numpad9
    if let Some(n) = lower.strip_prefix("numpad")
        && n.len() == 1
        && n.as_bytes()[0].is_ascii_digit()
    {
        return Some(HotkeyKey::Vk(0x60 + u16::from(n.as_bytes()[0] - b'0')));
    }
    NAMED_KEYS
        .iter()
        .chain(ALIASES)
        .find(|(name, _)| name.eq_ignore_ascii_case(token))
        .map(|&(_, vk)| HotkeyKey::Vk(vk))
}

impl FromStr for Hotkey {
    type Err = ParseHotkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseHotkeyError::Empty);
        }
        let tokens: Vec<&str> = s.split('+').map(str::trim).collect();
        if tokens.iter().any(|t| t.is_empty()) {
            return Err(ParseHotkeyError::EmptyPart(s.to_string()));
        }
        let (last, mods) = tokens.split_last().expect("non-empty");
        let mut modifiers = Modifiers::default();
        for t in mods {
            match parse_modifier(t, &mut modifiers) {
                Some(r) => r?,
                None => return Err(ParseHotkeyError::NotAModifier((*t).to_string())),
            }
        }
        match parse_key(last) {
            Some(key) => Ok(Hotkey { modifiers, key }),
            None => {
                // A modifier name in last position means "no key".
                if parse_modifier(last, &mut Modifiers::default()).is_some() {
                    Err(ParseHotkeyError::ModifierOnly(s.to_string()))
                } else {
                    Err(ParseHotkeyError::UnknownKey((*last).to_string()))
                }
            }
        }
    }
}

impl Hotkey {
    /// Whether the combination is held right now, given a key-state probe
    /// (`GetAsyncKeyState` in production). Requested modifiers must be down;
    /// extra modifiers are tolerated (a PTT key must not break when the user
    /// also holds Shift in a game).
    pub fn is_down(&self, down: impl Fn(u16) -> bool) -> bool {
        let m = self.modifiers;
        let key = match self.key {
            HotkeyKey::Vk(vk) => vk,
            HotkeyKey::Mouse(b) => b.vk(),
        };
        (!m.ctrl || down(VK_CONTROL))
            && (!m.alt || down(VK_MENU))
            && (!m.shift || down(VK_SHIFT))
            && (!m.win || down(VK_LWIN) || down(VK_RWIN))
            && down(key)
    }
}

fn key_name(vk: u16) -> String {
    match vk {
        0x41..=0x5A | 0x30..=0x39 => (vk as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        _ => NAMED_KEYS
            .iter()
            .find(|&&(_, v)| v == vk)
            .map(|&(n, _)| n.to_string())
            .unwrap_or_else(|| format!("VK{vk:#04X}")),
    }
}

/// Canonical text (`Ctrl+Shift+G`), also what the tray menu shows.
impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, name) in [(m.ctrl, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift"), (m.win, "Win")] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            HotkeyKey::Vk(vk) => f.write_str(&key_name(vk)),
            HotkeyKey::Mouse(MouseButton::X1) => f.write_str("Mouse4"),
            HotkeyKey::Mouse(MouseButton::X2) => f.write_str("Mouse5"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(vk: u16, ctrl: bool, alt: bool, shift: bool, win: bool) -> Hotkey {
        Hotkey {
            modifiers: Modifiers { ctrl, alt, shift, win },
            key: HotkeyKey::Vk(vk),
        }
    }

    #[test]
    fn parses_default_hotkeys() {
        assert_eq!("Ctrl+Shift+G".parse(), Ok(key(b'G'.into(), true, false, true, false)));
        assert_eq!("Ctrl+Shift+M".parse(), Ok(key(b'M'.into(), true, false, true, false)));
        assert_eq!("Ctrl+Shift+D".parse(), Ok(key(b'D'.into(), true, false, true, false)));
        assert_eq!(
            "Mouse4".parse(),
            Ok(Hotkey { modifiers: Modifiers::default(), key: HotkeyKey::Mouse(MouseButton::X1) })
        );
        assert_eq!(
            "Mouse5".parse::<Hotkey>().unwrap().key,
            HotkeyKey::Mouse(MouseButton::X2)
        );
    }

    #[test]
    fn parses_numpad_and_function_keys() {
        assert_eq!("Ctrl+Numpad1".parse(), Ok(key(0x61, true, false, false, false)));
        assert_eq!("numpad0".parse(), Ok(key(0x60, false, false, false, false)));
        assert_eq!("Alt+F13".parse(), Ok(key(0x7C, false, true, false, false)));
        assert_eq!("F1".parse(), Ok(key(0x70, false, false, false, false)));
        assert_eq!("Ctrl+Alt+Shift+F24".parse(), Ok(key(0x87, true, true, true, false)));
        assert_eq!("Win+Space".parse(), Ok(key(0x20, false, false, false, true)));
        assert_eq!("Ctrl+5".parse(), Ok(key(b'5'.into(), true, false, false, false)));
        assert_eq!("Ctrl+Escape".parse(), Ok(key(0x1B, true, false, false, false)));
    }

    #[test]
    fn is_case_insensitive_and_tolerates_spaces() {
        let a: Hotkey = "ctrl+SHIFT+g".parse().unwrap();
        let b: Hotkey = " Ctrl + Shift + G ".parse().unwrap();
        assert_eq!(a, b);
        assert_eq!(a, "Ctrl+Shift+G".parse().unwrap());
        assert_eq!("MOUSE4".parse::<Hotkey>(), "Mouse4".parse());
        assert_eq!("alt+f13".parse::<Hotkey>(), "Alt+F13".parse());
        // modifier order does not matter
        assert_eq!("Shift+Ctrl+G".parse::<Hotkey>(), "Ctrl+Shift+G".parse());
    }

    #[test]
    fn mouse_button_accepts_modifiers() {
        let h: Hotkey = "Ctrl+Mouse5".parse().unwrap();
        assert!(h.modifiers.ctrl);
        assert_eq!(h.key, HotkeyKey::Mouse(MouseButton::X2));
    }

    #[test]
    fn rejects_invalid_strings_with_clear_errors() {
        assert_eq!("".parse::<Hotkey>(), Err(ParseHotkeyError::Empty));
        assert_eq!("   ".parse::<Hotkey>(), Err(ParseHotkeyError::Empty));
        assert!(matches!("Ctrl++G".parse::<Hotkey>(), Err(ParseHotkeyError::EmptyPart(_))));
        assert!(matches!("Ctrl+".parse::<Hotkey>(), Err(ParseHotkeyError::EmptyPart(_))));
        assert_eq!(
            "Ctrl+Banana".parse::<Hotkey>(),
            Err(ParseHotkeyError::UnknownKey("Banana".into()))
        );
        assert_eq!(
            "Mouse3".parse::<Hotkey>(),
            Err(ParseHotkeyError::UnknownKey("Mouse3".into()))
        );
        assert_eq!("F25".parse::<Hotkey>(), Err(ParseHotkeyError::UnknownKey("F25".into())));
        assert_eq!("F0".parse::<Hotkey>(), Err(ParseHotkeyError::UnknownKey("F0".into())));
        assert_eq!("Numpad10".parse::<Hotkey>(), Err(ParseHotkeyError::UnknownKey("Numpad10".into())));
        assert!(matches!("Ctrl+Shift".parse::<Hotkey>(), Err(ParseHotkeyError::ModifierOnly(_))));
        assert!(matches!("Ctrl".parse::<Hotkey>(), Err(ParseHotkeyError::ModifierOnly(_))));
        assert_eq!(
            "Ctrl+Ctrl+G".parse::<Hotkey>(),
            Err(ParseHotkeyError::DuplicateModifier("Ctrl".into()))
        );
        assert_eq!(
            "G+Ctrl".parse::<Hotkey>(),
            Err(ParseHotkeyError::NotAModifier("G".into()))
        );
        assert_eq!(
            "Ctrl+A+B".parse::<Hotkey>(),
            Err(ParseHotkeyError::NotAModifier("A".into()))
        );
        // the message names the offender
        let msg = "Ctrl+Banana".parse::<Hotkey>().unwrap_err().to_string();
        assert!(msg.contains("Banana"), "{msg}");
    }

    #[test]
    fn is_down_requires_key_and_modifiers() {
        let h: Hotkey = "Ctrl+Shift+G".parse().unwrap();
        let held = |keys: &'static [u16]| move |vk: u16| keys.contains(&vk);
        assert!(h.is_down(held(&[VK_CONTROL, VK_SHIFT, b'G' as u16])));
        assert!(h.is_down(held(&[VK_CONTROL, VK_SHIFT, VK_MENU, b'G' as u16]))); // extra Alt ok
        assert!(!h.is_down(held(&[VK_CONTROL, b'G' as u16]))); // Shift missing
        assert!(!h.is_down(held(&[VK_CONTROL, VK_SHIFT]))); // key missing
        let m: Hotkey = "Mouse4".parse().unwrap();
        assert!(m.is_down(held(&[VK_XBUTTON1])));
        assert!(!m.is_down(held(&[VK_XBUTTON2])));
        let w: Hotkey = "Win+F1".parse().unwrap();
        assert!(w.is_down(held(&[VK_RWIN, 0x70])));
        assert!(!w.is_down(held(&[0x70])));
    }

    #[test]
    fn display_roundtrips() {
        for s in [
            "Ctrl+Shift+G",
            "Ctrl+Numpad1",
            "Alt+F13",
            "Mouse4",
            "Mouse5",
            "Ctrl+Alt+Shift+Win+Space",
            "Ctrl+PageUp",
            "5",
            "Ctrl+NumpadAdd",
        ] {
            let h: Hotkey = s.parse().unwrap();
            assert_eq!(h.to_string(), s);
            assert_eq!(h.to_string().parse::<Hotkey>(), Ok(h));
        }
        // aliases normalise
        assert_eq!("ctrl+esc".parse::<Hotkey>().unwrap().to_string(), "Ctrl+Esc");
    }
}
