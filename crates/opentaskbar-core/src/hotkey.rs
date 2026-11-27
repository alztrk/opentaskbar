//! Global hotkey parsing/validation compatible with the `HotkeyBinding`.
//!
//! The Win key is deliberately never accepted: a lost key-up across a lock or secure-desktop
//! transition can leave Windows believing Win is still held down.

use serde::{Deserialize, Deserializer, Serialize};

/// Raw Win32 modifier flags used by `RegisterHotKey` (kept for the port's WIP call sites).
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_NOREPEAT: u32 = 0x4000;

/// reference persisted values that explicitly mean "no binding" in the WIP settings UI.
pub fn is_disabled_hotkey(text: &str) -> bool {
    let value = text.trim();
    value.is_empty()
        || value.eq_ignore_ascii_case("Disabled")
        || value.eq_ignore_ascii_case("Backspace")
        || value.eq_ignore_ascii_case("Esc")
        || value.eq_ignore_ascii_case("Escape")
}

/// Modifier bits used by Win32 `RegisterHotKey` and by the `HotkeyBinding`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const ALT: Self = Self(0x1);
    pub const CONTROL: Self = Self(0x2);
    pub const SHIFT: Self = Self(0x4);
    /// Accepted as an input flag only so it can be rejected with a specific message.
    pub const WIN: Self = Self(0x8);

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The preset choices persisted by reference as `LauncherHotkey` / `DockHotkey`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotkeyChoice {
    #[default]
    Disabled,
    AltSpace,
    CtrlSpace,
    CtrlAltSpace,
    CtrlAltD,
    WinAltD,
}

/// A validated global hotkey combination (modifiers + Win32 virtual key + canonical display text).
///
/// `modifiers`/`virtual_key` use the WIP `u32` widths so the Win32 call sites can pass them straight
/// to `RegisterHotKey`; `display` is the historic field name kept alongside the `display_text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HotkeyBinding {
    pub modifiers: u32,
    pub virtual_key: u32,
    pub display_text: String,
    pub display: String,
}

impl<'de> Deserialize<'de> for HotkeyBinding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Fields {
            #[serde(default)]
            modifiers: u32,
            #[serde(default)]
            virtual_key: u32,
            #[serde(default)]
            display_text: String,
            #[serde(default)]
            display: String,
        }
        let fields = Fields::deserialize(deserializer)?;
        let display_text = if fields.display_text.is_empty() {
            format_display_text(fields.modifiers, fields.virtual_key)
        } else {
            fields.display_text
        };
        let display = if fields.display.is_empty() {
            display_text.clone()
        } else {
            fields.display
        };
        Ok(Self {
            modifiers: fields.modifiers,
            virtual_key: fields.virtual_key,
            display_text,
            display,
        })
    }
}

/// Raw modifier + virtual-key input, as produced by a keyboard capture control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyEvent {
    pub modifiers: Modifiers,
    pub virtual_key: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    WinNotAllowed,
    ModifierRequired,
    DisallowedKey,
    InvalidVirtualKey,
}

impl HotkeyError {
    /// Localization key reported by the reference hotkey capture UI.
    pub const fn error_key(self) -> &'static str {
        match self {
            Self::WinNotAllowed => "HotkeyWinNotAllowed",
            Self::ModifierRequired => "HotkeyModifierRequired",
            Self::DisallowedKey => "HotkeyDisallowedKey",
            Self::InvalidVirtualKey => "HotkeyInvalidKey",
        }
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.error_key())
    }
}

impl std::error::Error for HotkeyError {}

const VK_ESCAPE: u32 = 0x1B;
const VK_SNAPSHOT: u32 = 0x2C;
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_LSHIFT: u32 = 0xA0;
const VK_RMENU: u32 = 0xA5;

impl HotkeyBinding {
    /// Port of `HotkeyBinding.TryParse(string?)`.
    ///
    /// Also accepts the WIP canonical no-`+` names (`AltSpace`, `CtrlAltD`, ...) so settings
    /// written by either build resolve to the same binding.
    pub fn try_parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if trimmed.is_empty() || is_disabled_hotkey(trimmed) {
            return None;
        }

        let canonical = match trimmed.to_ascii_lowercase().as_str() {
            "altspace" | "alt+space" => Some(HotkeyChoice::AltSpace),
            "ctrlspace" | "ctrl+space" => Some(HotkeyChoice::CtrlSpace),
            "ctrlaltspace" | "ctrl+alt+space" => Some(HotkeyChoice::CtrlAltSpace),
            "ctrlaltd" | "ctrl+alt+d" => Some(HotkeyChoice::CtrlAltD),
            "win altd" | "win+alt+d" | "winaltd" => Some(HotkeyChoice::WinAltD),
            _ => None,
        };
        if let Some(choice) = canonical {
            return Self::from_choice(choice);
        }

        let tokens = tokenize(trimmed)?;
        if tokens.len() < 2 {
            return None;
        }

        let mut modifiers = Modifiers::default();
        let mut key_token: Option<&str> = None;
        for token in &tokens {
            if token.eq_ignore_ascii_case("Ctrl") || token.eq_ignore_ascii_case("Control") {
                if modifiers.contains(Modifiers::CONTROL) {
                    return None;
                }
                modifiers = modifiers.union(Modifiers::CONTROL);
            } else if token.eq_ignore_ascii_case("Alt") {
                if modifiers.contains(Modifiers::ALT) {
                    return None;
                }
                modifiers = modifiers.union(Modifiers::ALT);
            } else if token.eq_ignore_ascii_case("Shift") {
                if modifiers.contains(Modifiers::SHIFT) {
                    return None;
                }
                modifiers = modifiers.union(Modifiers::SHIFT);
            } else if is_win_token(token) {
                return None;
            } else if key_token.is_some() {
                // Reject duplicate non-modifier keys, e.g. "Ctrl+A+B".
                return None;
            } else {
                key_token = Some(*token);
            }
        }

        let key_token = key_token?;
        if !modifiers.contains(Modifiers::ALT) && !modifiers.contains(Modifiers::CONTROL) {
            return None;
        }
        let vk = parse_key_token(key_token)?;
        if !is_valid_virtual_key(vk) {
            return None;
        }
        let modifiers = modifiers.0 as u32;
        let text = format_display_text(modifiers, vk);
        Some(Self {
            modifiers,
            virtual_key: vk,
            display_text: text.clone(),
            display: text,
        })
    }

    /// Historic WIP alias for `try_parse` used by the settings UI and dock process.
    pub fn parse(text: &str) -> Option<Self> {
        Self::try_parse(text)
    }

    /// Port of `HotkeyBinding.TryFromKeyEvent`.
    pub fn try_from_key_event(modifiers: Modifiers, virtual_key: u16) -> Result<Self, HotkeyError> {
        let _ = Self::try_from(KeyEvent {
            modifiers,
            virtual_key,
        })?;
        let modifiers = modifiers.0 as u32;
        let virtual_key = virtual_key as u32;
        let text = format_display_text(modifiers, virtual_key);
        Ok(Self {
            modifiers,
            virtual_key,
            display_text: text.clone(),
            display: text,
        })
    }

    pub fn from_choice(choice: HotkeyChoice) -> Option<Self> {
        let (modifiers, vk, text) = match choice {
            HotkeyChoice::AltSpace => (Modifiers::ALT.0, 0x20, "Alt+Space"),
            HotkeyChoice::CtrlSpace => (Modifiers::CONTROL.0, 0x20, "Ctrl+Space"),
            HotkeyChoice::CtrlAltSpace => (
                Modifiers::CONTROL.0 | Modifiers::ALT.0,
                0x20,
                "Ctrl+Alt+Space",
            ),
            HotkeyChoice::CtrlAltD => (Modifiers::CONTROL.0 | Modifiers::ALT.0, 0x44, "Ctrl+Alt+D"),
            HotkeyChoice::Disabled | HotkeyChoice::WinAltD => return None,
        };
        Some(Self {
            modifiers: modifiers as u32,
            virtual_key: vk as u32,
            display_text: text.to_owned(),
            display: text.to_owned(),
        })
    }

    /// A custom string wins over the legacy choice; "Disabled" disables the binding.
    pub fn resolve(choice: HotkeyChoice, custom: Option<&str>) -> Option<Self> {
        match custom {
            Some(text) if text.trim().eq_ignore_ascii_case("Disabled") => None,
            Some(text) if !text.trim().is_empty() => Self::try_parse(text),
            _ => Self::from_choice(choice),
        }
    }

    /// Null, "Disabled" and any parseable combination are all valid persisted values.
    pub fn is_valid_custom_string(text: Option<&str>) -> bool {
        match text {
            None => true,
            Some(value) => {
                value.trim().eq_ignore_ascii_case("Disabled") || Self::try_parse(value).is_some()
            }
        }
    }

    /// Two bindings conflict when modifiers and virtual key match (display text is irrelevant).
    pub fn equals_binding(a: Option<&Self>, b: Option<&Self>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => a.modifiers == b.modifiers && a.virtual_key == b.virtual_key,
            _ => false,
        }
    }
}

impl TryFrom<KeyEvent> for HotkeyBinding {
    type Error = HotkeyError;

    fn try_from(event: KeyEvent) -> Result<Self, Self::Error> {
        let KeyEvent {
            modifiers,
            virtual_key: vk,
        } = event;
        if modifiers.contains(Modifiers::WIN) {
            return Err(HotkeyError::WinNotAllowed);
        }
        if !modifiers.contains(Modifiers::ALT) && !modifiers.contains(Modifiers::CONTROL) {
            return Err(HotkeyError::ModifierRequired);
        }
        if !is_valid_virtual_key(vk as u32) {
            return Err(HotkeyError::DisallowedKey);
        }
        let modifiers = modifiers.0 as u32;
        let text = format_display_text(modifiers, vk as u32);
        Ok(Self {
            modifiers,
            virtual_key: vk as u32,
            display_text: text.clone(),
            display: text,
        })
    }
}

/// Historic WIP free-function alias (`hotkey::parse`) kept alongside `HotkeyBinding::try_parse`.
pub fn parse(text: &str) -> Option<HotkeyBinding> {
    HotkeyBinding::try_parse(text)
}

fn tokenize(trimmed: &str) -> Option<Vec<&str>> {
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let len = trimmed.len();
    while i < len {
        while i < len && trimmed.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            return None;
        }
        // A trailing '+' is the plus key itself, e.g. "Ctrl++".
        if i == len - 1 && trimmed.as_bytes()[i] == b'+' {
            tokens.push("+");
            break;
        }
        let rest = &trimmed[i..];
        match rest.find('+') {
            None => {
                let part = rest.trim();
                if part.is_empty() {
                    return None;
                }
                tokens.push(part);
                break;
            }
            Some(offset) => {
                let token = rest[..offset].trim();
                if token.is_empty() {
                    return None;
                }
                tokens.push(token);
                i += offset + 1;
                if i >= len {
                    return None;
                }
            }
        }
    }
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

fn is_win_token(token: &str) -> bool {
    token.eq_ignore_ascii_case("Win")
        || token.eq_ignore_ascii_case("Windows")
        || token.eq_ignore_ascii_case("LWin")
        || token.eq_ignore_ascii_case("RWin")
}

fn is_valid_virtual_key(vk: u32) -> bool {
    vk > 0
        && vk <= 0xFE
        && !matches!(
            vk,
            VK_ESCAPE | VK_SNAPSHOT | VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN
        )
        && !(VK_LSHIFT..=VK_RMENU).contains(&vk)
}

/// Parses the wide token set used by reference, including punctuation, PageUp/Prior and Back.
pub fn parse_key_token(token: &str) -> Option<u32> {
    if token.is_empty() {
        return None;
    }
    if token.len() == 1 {
        let ch = token.as_bytes()[0].to_ascii_uppercase();
        return match ch {
            b'A'..=b'Z' => Some(0x41 + (ch - b'A') as u32),
            b'0'..=b'9' => Some(0x30 + (ch - b'0') as u32),
            b'+' => Some(0xBB),
            b'-' => Some(0xBD),
            b',' => Some(0xBC),
            b'.' => Some(0xBE),
            b'/' => Some(0xBF),
            b';' => Some(0xBA),
            b'\'' => Some(0xDE),
            b'[' => Some(0xDB),
            b']' => Some(0xDD),
            b'\\' => Some(0xDC),
            b'`' => Some(0xC0),
            _ => None,
        };
    }

    let lower = token.to_ascii_lowercase();
    let vk = match lower.as_str() {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "backspace" | "back" => 0x08,
        "pageup" | "prior" => 0x21,
        "pagedown" | "next" => 0x22,
        "home" => 0x24,
        "end" => 0x23,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "plus" | "oemplus" => 0xBB,
        "minus" | "oemminus" => 0xBD,
        "comma" | "oemcomma" => 0xBC,
        "period" | "oemperiod" => 0xBE,
        "oemquestion" => 0xBF,
        "oemsemicolon" => 0xBA,
        "oemquotes" => 0xDE,
        "oemopenbrackets" => 0xDB,
        "oemclosebrackets" => 0xDD,
        "oempipe" => 0xDC,
        "oemtilde" => 0xC0,
        "multiply" => 0x6A,
        "add" => 0x6B,
        "subtract" => 0x6D,
        "decimal" => 0x6E,
        "divide" => 0x6F,
        "capslock" => 0x14,
        "pause" | "break" => 0x13,
        _ => {
            if let Some(number) = lower.strip_prefix('f')
                && let Ok(n) = number.parse::<u32>()
                && (1..=24).contains(&n)
            {
                return Some(0x6F + n);
            }
            if let Some(number) = lower.strip_prefix("numpad")
                && let Ok(n) = number.parse::<u32>()
                && n <= 9
            {
                return Some(0x60 + n);
            }
            // Raw numeric strings would otherwise look like enum values to the reference Enum.TryParse.
            return None;
        }
    };
    Some(vk)
}

/// Canonical display text: modifiers in Ctrl/Alt/Shift order, then the key name.
pub fn format_display_text(modifiers: u32, vk: u32) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(4);
    if modifiers & 0x2 != 0 {
        parts.push("Ctrl".to_owned());
    }
    if modifiers & 0x1 != 0 {
        parts.push("Alt".to_owned());
    }
    if modifiers & 0x4 != 0 {
        parts.push("Shift".to_owned());
    }
    parts.push(format_key_name(vk));
    parts.join("+")
}

fn format_key_name(vk: u32) -> String {
    match vk {
        0x41..=0x5A => ((b'A' + (vk as u8 - 0x41)) as char).to_string(),
        0x30..=0x39 => ((b'0' + (vk as u8 - 0x30)) as char).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x60..=0x69 => format!("NumPad{}", vk - 0x60),
        0x20 => "Space".to_owned(),
        0x0D => "Enter".to_owned(),
        0x09 => "Tab".to_owned(),
        0x08 => "Backspace".to_owned(),
        0x24 => "Home".to_owned(),
        0x23 => "End".to_owned(),
        0x21 => "PageUp".to_owned(),
        0x22 => "PageDown".to_owned(),
        0x25 => "Left".to_owned(),
        0x26 => "Up".to_owned(),
        0x27 => "Right".to_owned(),
        0x28 => "Down".to_owned(),
        0x2D => "Insert".to_owned(),
        0x2E => "Delete".to_owned(),
        0xBB => "+".to_owned(),
        0xBD => "-".to_owned(),
        0xBC => ",".to_owned(),
        0xBE => ".".to_owned(),
        0xBF => "/".to_owned(),
        0xBA => ";".to_owned(),
        0xDE => "'".to_owned(),
        0xDB => "[".to_owned(),
        0xDD => "]".to_owned(),
        0xDC => "\\".to_owned(),
        0xC0 => "`".to_owned(),
        _ => format!("Vk{vk}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wip_parse_api_still_works() {
        assert_eq!(parse("AltSpace").unwrap().display, "Alt+Space");
        assert_eq!(parse("CtrlAltD").unwrap().display, "Ctrl+Alt+D");
        assert_eq!(parse("Disabled"), None);
        assert_eq!(parse("WinAltD"), None);

        let binding = parse("Ctrl+Shift+L").unwrap();
        assert_eq!(binding.modifiers, MOD_CONTROL | MOD_SHIFT);
        assert_eq!(binding.virtual_key, 0x4C);
        assert_eq!(binding.display, "Ctrl+Shift+L");
        assert_eq!(binding.display, binding.display_text);

        assert!(parse("Win+D").is_none());
        assert!(parse("Shift+F5").is_none());
        assert!(parse("Ctrl+A+B").is_none());
        assert!(parse("Ctrl+Ctrl+K").is_none());
        assert!(parse("K").is_none());
        assert!(parse("Ctrl+Escape").is_none());
        assert!(parse("Backspace").is_none());
    }

    #[test]
    fn parses_wide_token_set() {
        for (text, vk) in [
            ("Ctrl+,", 0xBC),
            ("Ctrl+.", 0xBE),
            ("Ctrl+/", 0xBF),
            ("Ctrl+;", 0xBA),
            ("Ctrl+'", 0xDE),
            ("Ctrl+[", 0xDB),
            ("Ctrl+]", 0xDD),
            ("Ctrl+\\", 0xDC),
            ("Ctrl+`", 0xC0),
            ("Alt+PageUp", 0x21),
            ("Alt+Prior", 0x21),
            ("Ctrl+Back", 0x08),
            ("Ctrl+Backspace", 0x08),
            ("Ctrl++", 0xBB),
            ("Ctrl+-", 0xBD),
            ("Ctrl+F5", 0x74),
            ("Ctrl+NumPad3", 0x63),
        ] {
            let parsed = HotkeyBinding::try_parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(parsed.virtual_key, vk, "{text}");
        }
    }

    #[test]
    fn rejects_invalid_combinations() {
        assert!(HotkeyBinding::try_parse("A").is_none());
        assert!(HotkeyBinding::try_parse("Win+Alt+A").is_none());
        assert!(HotkeyBinding::try_parse("Ctrl+Escape").is_none());
        assert!(HotkeyBinding::try_parse("Ctrl+PrintScreen").is_none());
        assert!(HotkeyBinding::try_parse("Ctrl+A+B").is_none());
        assert!(HotkeyBinding::try_parse("Ctrl+123").is_none());
        assert!(HotkeyBinding::try_parse("Disabled").is_none());
        assert!(HotkeyBinding::try_parse("Ctrl+").is_none());
    }

    #[test]
    fn key_event_and_resolution_match_reference() {
        let event = KeyEvent {
            modifiers: Modifiers::CONTROL.union(Modifiers::ALT),
            virtual_key: 0x44,
        };
        let parsed = HotkeyBinding::try_from(event).unwrap();
        assert_eq!(parsed.display_text, "Ctrl+Alt+D");
        assert_eq!(
            HotkeyBinding::try_from_key_event(Modifiers::WIN, 0x41).unwrap_err(),
            HotkeyError::WinNotAllowed
        );
        assert_eq!(
            HotkeyBinding::try_from_key_event(Modifiers::default(), 0x41).unwrap_err(),
            HotkeyError::ModifierRequired
        );

        let resolver = HotkeyBinding::resolve(HotkeyChoice::AltSpace, Some("Ctrl+Alt+D"));
        assert_eq!(resolver.as_ref(), Some(&parsed));
        assert!(HotkeyBinding::resolve(HotkeyChoice::AltSpace, Some("Disabled")).is_none());
        assert!(HotkeyBinding::is_valid_custom_string(None));
        assert!(HotkeyBinding::is_valid_custom_string(Some("Disabled")));
        assert!(!HotkeyBinding::is_valid_custom_string(Some("Ctrl")));
        assert!(HotkeyBinding::equals_binding(
            resolver.as_ref(),
            HotkeyBinding::from_choice(HotkeyChoice::CtrlAltD).as_ref()
        ));
        assert!(!HotkeyBinding::equals_binding(None, None));
    }
}
