//! Keyboard shortcut model: parsing, canonical formatting and validation.
//!
//! Keys are stored as Windows virtual-key codes so the same value can be
//! registered with `RegisterHotKey`, polled with `GetAsyncKeyState` and sent
//! with `SendInput`. Parsing is platform independent and unit tested.

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod win;

use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Modifiers {
    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.win
    }

    /// Bit flags as used by `RegisterHotKey` (MOD_ALT=1, MOD_CONTROL=2,
    /// MOD_SHIFT=4, MOD_WIN=8).
    pub fn register_flags(&self) -> u32 {
        (self.alt as u32) | (self.ctrl as u32) << 1 | (self.shift as u32) << 2 | (self.win as u32) << 3
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub mods: Modifiers,
    /// Windows virtual-key code of the main (non-modifier) key.
    pub vk: u16,
}

/// Virtual-key codes used by SwiftVoice itself.
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const RETURN: u16 = 0x0D;
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const ESCAPE: u16 = 0x1B;
    pub const SPACE: u16 = 0x20;
    pub const INSERT: u16 = 0x2D;
    pub const LWIN: u16 = 0x5B;
    pub const RWIN: u16 = 0x5C;
    pub const V: u16 = 0x56;
    pub const LSHIFT: u16 = 0xA0;
    pub const RSHIFT: u16 = 0xA1;
    pub const LCONTROL: u16 = 0xA2;
    pub const RCONTROL: u16 = 0xA3;
    pub const LMENU: u16 = 0xA4;
    pub const RMENU: u16 = 0xA5;
}

/// (canonical name, aliases, virtual-key code) for every named key.
const NAMED_KEYS: &[(&str, &[&str], u16)] = &[
    ("Space", &["spacebar"], 0x20),
    ("Enter", &["return"], 0x0D),
    ("Tab", &[], 0x09),
    ("Escape", &["esc"], 0x1B),
    ("Backspace", &["back"], 0x08),
    ("Insert", &["ins"], 0x2D),
    ("Delete", &["del"], 0x2E),
    ("Home", &[], 0x24),
    ("End", &[], 0x23),
    ("PageUp", &["pgup", "prior"], 0x21),
    ("PageDown", &["pgdn", "next"], 0x22),
    ("Left", &["arrowleft"], 0x25),
    ("Up", &["arrowup"], 0x26),
    ("Right", &["arrowright"], 0x27),
    ("Down", &["arrowdown"], 0x28),
    ("Pause", &["break"], 0x13),
    ("ScrollLock", &["scroll"], 0x91),
    ("CapsLock", &["caps"], 0x14),
    ("PrintScreen", &["prtsc", "printscr", "snapshot"], 0x2C),
    ("NumLock", &[], 0x90),
    ("Apps", &["menukey", "contextmenu"], 0x5D),
    ("LAlt", &["leftalt", "leftmenu"], 0xA4),
    ("RAlt", &["rightalt", "rightmenu"], 0xA5),
    ("NumpadMultiply", &["multiply"], 0x6A),
    ("NumpadAdd", &["add"], 0x6B),
    ("NumpadSubtract", &["subtract"], 0x6D),
    ("NumpadDecimal", &["decimal"], 0x6E),
    ("NumpadDivide", &["divide"], 0x6F),
    (";", &["semicolon"], 0xBA),
    ("=", &["equals", "plus"], 0xBB),
    (",", &["comma"], 0xBC),
    ("-", &["minus"], 0xBD),
    (".", &["period"], 0xBE),
    ("/", &["slash"], 0xBF),
    ("`", &["backquote", "grave", "backtick"], 0xC0),
    ("[", &["bracketleft"], 0xDB),
    ("\\", &["backslash"], 0xDC),
    ("]", &["bracketright"], 0xDD),
    ("'", &["quote", "apostrophe"], 0xDE),
];

fn key_from_name(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0];
        if c.is_ascii_lowercase() {
            return Some(u16::from(c.to_ascii_uppercase()));
        }
        if c.is_ascii_digit() {
            return Some(u16::from(c));
        }
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
        if (1..=24).contains(&n) && !lower[1..].starts_with('0') {
            return Some(0x70 + n - 1);
        }
    }
    if let Some(n) = lower.strip_prefix("numpad").or_else(|| lower.strip_prefix("num")).and_then(|d| d.parse::<u16>().ok()) {
        if n <= 9 {
            return Some(0x60 + n);
        }
    }
    NAMED_KEYS
        .iter()
        .find(|(canon, aliases, _)| canon.eq_ignore_ascii_case(&lower) || aliases.contains(&lower.as_str()))
        .map(|(_, _, vk)| *vk)
}

/// Canonical display name of a virtual-key code.
pub fn key_name(vk: u16) -> String {
    match vk {
        0x41..=0x5A | 0x30..=0x39 => (vk as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        _ => NAMED_KEYS
            .iter()
            .find(|(_, _, code)| *code == vk)
            .map(|(name, _, _)| name.to_string())
            .unwrap_or_else(|| format!("VK{vk:#04X}")),
    }
}

/// True for Ctrl/Alt/Shift/Win keys (left, right or generic).
pub fn is_modifier_vk(code: u16) -> bool {
    matches!(
        code,
        vk::SHIFT
            | vk::CONTROL
            | vk::MENU
            | vk::LWIN
            | vk::RWIN
            | vk::LSHIFT
            | vk::RSHIFT
            | vk::LCONTROL
            | vk::RCONTROL
            | vk::LMENU
            | vk::RMENU
    )
}

impl Hotkey {
    pub fn new(mods: Modifiers, vk: u16) -> Hotkey {
        Hotkey { mods, vk }
    }

    /// Parse strings such as `"Ctrl+Alt+Space"`, `"win+shift+f9"`, `"F13"`.
    pub fn parse(text: &str) -> Result<Hotkey, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty shortcut".into());
        }
        let mut mods = Modifiers::default();
        let mut key: Option<u16> = None;
        for raw in text.split('+') {
            let part = raw.trim();
            if part.is_empty() {
                return Err(format!("\"{text}\" has an empty part (use \"Plus\" or \"=\" for the + key)"));
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "strg" => set_once(&mut mods.ctrl, part)?,
                "alt" | "option" => set_once(&mut mods.alt, part)?,
                "shift" => set_once(&mut mods.shift, part)?,
                "win" | "windows" | "super" | "meta" | "cmd" => set_once(&mut mods.win, part)?,
                _ => {
                    let code = key_from_name(part).ok_or_else(|| format!("unknown key \"{part}\""))?;
                    if key.replace(code).is_some() {
                        return Err(format!("\"{text}\" has more than one non-modifier key"));
                    }
                }
            }
        }
        let vk = key.ok_or_else(|| format!("\"{text}\" needs a key besides the modifiers"))?;
        Ok(Hotkey { mods, vk })
    }

    /// Rules for shortcuts registered system-wide. A bare letter, digit,
    /// Space or Enter would stop that key from working in every app, so
    /// those need at least one modifier. `allow_bare_escape` is used for the
    /// cancel shortcut, which is only registered while dictating.
    pub fn check_global(&self, allow_bare_escape: bool) -> Result<(), String> {
        if self.mods.any() {
            return Ok(());
        }
        let ok_bare =
            matches!(self.vk, 0x70..=0x87 | 0x13 | 0x91 | 0x2C | vk::LMENU | vk::RMENU) || (allow_bare_escape && self.vk == vk::ESCAPE);
        if ok_bare {
            Ok(())
        } else {
            Err(format!(
                "\"{self}\" without Ctrl, Alt, Shift or Win would block that key in every app; add a modifier or use F13-F24, Pause, Scroll Lock or RAlt"
            ))
        }
    }

    /// True for keys that Windows accepts in `RegisterHotKey` but never
    /// fires; the registry serves these with a low-level keyboard hook.
    pub fn needs_keyboard_hook(&self) -> bool {
        matches!(self.vk, vk::LMENU | vk::RMENU)
    }
}

fn set_once(flag: &mut bool, part: &str) -> Result<(), String> {
    if *flag {
        return Err(format!("modifier \"{part}\" appears twice"));
    }
    *flag = true;
    Ok(())
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if self.mods.ctrl {
            parts.push("Ctrl".into());
        }
        if self.mods.alt {
            parts.push("Alt".into());
        }
        if self.mods.shift {
            parts.push("Shift".into());
        }
        if self.mods.win {
            parts.push("Win".into());
        }
        parts.push(key_name(self.vk));
        f.write_str(&parts.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults() {
        let hk = Hotkey::parse("Ctrl+Alt+Space").unwrap();
        assert!(hk.mods.ctrl && hk.mods.alt && !hk.mods.shift && !hk.mods.win);
        assert_eq!(hk.vk, 0x20);
        assert_eq!(hk.to_string(), "Ctrl+Alt+Space");
        assert_eq!(Hotkey::parse("ctrl + alt + v").unwrap().to_string(), "Ctrl+Alt+V");
    }

    #[test]
    fn canonical_order_and_aliases() {
        assert_eq!(Hotkey::parse("shift+win+control+f9").unwrap().to_string(), "Ctrl+Shift+Win+F9");
        assert_eq!(Hotkey::parse("Esc").unwrap().to_string(), "Escape");
        assert_eq!(Hotkey::parse("alt+pgdn").unwrap().to_string(), "Alt+PageDown");
        assert_eq!(Hotkey::parse("Shift+Ins").unwrap().to_string(), "Shift+Insert");
        assert_eq!(Hotkey::parse("ctrl+num7").unwrap().to_string(), "Ctrl+Numpad7");
        assert_eq!(Hotkey::parse("ctrl+plus").unwrap().to_string(), "Ctrl+=");
        assert_eq!(Hotkey::parse("F24").unwrap().vk, 0x87);
    }

    #[test]
    fn round_trips_every_named_key() {
        for (name, _, code) in NAMED_KEYS {
            let hk = Hotkey::parse(&format!("Ctrl+{name}")).unwrap();
            assert_eq!(hk.vk, *code, "{name}");
            assert_eq!(Hotkey::parse(&hk.to_string()).unwrap(), hk);
        }
        for code in (0x41..=0x5A).chain(0x30..=0x39).chain(0x70..=0x87).chain(0x60..=0x69) {
            let hk = Hotkey::new(Modifiers { alt: true, ..Default::default() }, code);
            assert_eq!(Hotkey::parse(&hk.to_string()).unwrap(), hk);
        }
    }

    #[test]
    fn rejects_malformed_shortcuts() {
        assert!(Hotkey::parse("").is_err());
        assert!(Hotkey::parse("Ctrl+Alt").unwrap_err().contains("needs a key"));
        assert!(Hotkey::parse("Ctrl+A+B").unwrap_err().contains("more than one"));
        assert!(Hotkey::parse("Ctrl+Ctrl+A").unwrap_err().contains("twice"));
        assert!(Hotkey::parse("Ctrl++").unwrap_err().contains("empty part"));
        assert!(Hotkey::parse("Hyper+A").unwrap_err().contains("unknown key"));
        assert!(Hotkey::parse("F25").is_err());
        assert!(Hotkey::parse("F0").is_err());
        assert!(Hotkey::parse("F01").is_err());
    }

    #[test]
    fn global_rules_block_bare_typing_keys() {
        assert!(Hotkey::parse("Space").unwrap().check_global(false).is_err());
        assert!(Hotkey::parse("A").unwrap().check_global(false).is_err());
        assert!(Hotkey::parse("F13").unwrap().check_global(false).is_ok());
        assert!(Hotkey::parse("Pause").unwrap().check_global(false).is_ok());
        assert!(Hotkey::parse("Escape").unwrap().check_global(false).is_err());
        assert!(Hotkey::parse("Escape").unwrap().check_global(true).is_ok());
        assert!(Hotkey::parse("Win+H").unwrap().check_global(false).is_ok());
    }

    #[test]
    fn lone_alt_keys_are_valid_shortcuts() {
        let ralt = Hotkey::parse("RAlt").unwrap();
        assert_eq!(ralt.vk, vk::RMENU);
        assert_eq!(ralt.mods.register_flags(), 0);
        assert_eq!(ralt.to_string(), "RAlt");
        assert!(ralt.check_global(false).is_ok());
        assert!(ralt.needs_keyboard_hook());
        assert_eq!(Hotkey::parse("rightalt").unwrap(), ralt);
        assert_eq!(Hotkey::parse("LAlt").unwrap().vk, vk::LMENU);
        assert!(Hotkey::parse("Ctrl+RAlt").unwrap().needs_keyboard_hook());
        assert!(!Hotkey::parse("F13").unwrap().needs_keyboard_hook());
    }

    #[test]
    fn register_flags_match_win32_constants() {
        let hk = Hotkey::parse("Ctrl+Alt+Shift+Win+X").unwrap();
        assert_eq!(hk.mods.register_flags(), 1 | 2 | 4 | 8);
        assert_eq!(Hotkey::parse("Alt+X").unwrap().mods.register_flags(), 1);
        assert_eq!(Hotkey::parse("Ctrl+X").unwrap().mods.register_flags(), 2);
    }

    #[test]
    fn modifier_keys_are_detected() {
        for code in [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5] {
            assert!(is_modifier_vk(code));
        }
        assert!(!is_modifier_vk(0x20));
    }
}
