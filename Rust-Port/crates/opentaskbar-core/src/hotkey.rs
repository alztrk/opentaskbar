//! Validated global hotkey bindings (C# `HotkeyBinding.cs` parity).
//!
//! `MOD_WIN` is intentionally rejected: a lost Win key-up across a lock/secure-desktop
//! transition can leave Windows believing Win is still held down.

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_NOREPEAT: u32 = 0x4000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub modifiers: u32,
    pub virtual_key: u32,
    pub display: String,
}

fn key_token_vk(token: &str) -> Option<u32> {
    let upper = token.to_ascii_uppercase();
    if upper.len() == 1 {
        let byte = upper.as_bytes()[0];
        if byte.is_ascii_uppercase() {
            return Some(byte as u32);
        }
        if byte.is_ascii_digit() {
            return Some(byte as u32);
        }
    }
    if upper.len() == 2
        && upper.starts_with('F')
        && let Ok(number) = upper[1..].parse::<u32>()
        && (1..=24).contains(&number)
    {
        return Some(0x6F + number);
    }
    Some(match upper.as_str() {
        "SPACE" | "SPACEBAR" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "BACKSPACE" => 0x08,
        "DELETE" | "DEL" => 0x2E,
        "INSERT" | "INS" => 0x2D,
        "HOME" => 0x24,
        "END" => 0x23,
        "PGUP" | "PAGEUP" => 0x21,
        "PGDN" | "PAGEDOWN" => 0x22,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "PLUS" | "+" => 0xBB,
        "MINUS" | "-" => 0xBD,
        "ESC" | "ESCAPE" => 0x1B,
        "PRTSC" | "SNAPSHOT" => 0x2C,
        _ => return None,
    })
}

fn is_disallowed(vk: u32) -> bool {
    matches!(
        vk,
        0x1B | 0x2C | 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0 | 0xA1 | 0xA2 | 0xA3 | 0xA4 | 0xA5
    )
}

/// Parses the persisted C# `HotkeyChoice` names and custom strings like `"Ctrl+Shift+L"`.
pub fn parse(text: &str) -> Option<HotkeyBinding> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Disabled") {
        return None;
    }

    // Canonical legacy choices (MOD_WIN variants resolve to nothing, like C#).
    let canonical = match trimmed.to_ascii_lowercase().as_str() {
        "altspace" | "alt+space" => Some((MOD_ALT, 0x20)),
        "ctrlspace" | "ctrl+space" => Some((MOD_CONTROL, 0x20)),
        "ctrlaltspace" | "ctrl+alt+space" => Some((MOD_CONTROL | MOD_ALT, 0x20)),
        "ctrlaltd" | "ctrl+alt+d" => Some((MOD_CONTROL | MOD_ALT, 0x44)),
        "win altd" | "win+alt+d" | "winaltd" => return None,
        _ => None,
    };
    if let Some((modifiers, virtual_key)) = canonical {
        return Some(HotkeyBinding {
            modifiers,
            virtual_key,
            display: format_display(modifiers, virtual_key),
        });
    }

    let tokens: Vec<&str> = trimmed
        .split('+')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.len() < 2 {
        return None;
    }
    let mut modifiers = 0u32;
    let mut key: Option<u32> = None;
    for token in tokens {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => {
                if modifiers & MOD_CONTROL != 0 {
                    return None;
                }
                modifiers |= MOD_CONTROL;
            }
            "alt" => {
                if modifiers & MOD_ALT != 0 {
                    return None;
                }
                modifiers |= MOD_ALT;
            }
            "shift" => {
                if modifiers & MOD_SHIFT != 0 {
                    return None;
                }
                modifiers |= MOD_SHIFT;
            }
            "win" | "windows" | "cmd" => return None,
            _ => {
                if key.is_some() {
                    return None;
                }
                key = key_token_vk(token);
                key?;
            }
        }
    }
    let virtual_key = key?;
    if modifiers & (MOD_ALT | MOD_CONTROL) == 0 || is_disallowed(virtual_key) {
        return None;
    }
    Some(HotkeyBinding {
        modifiers,
        virtual_key,
        display: format_display(modifiers, virtual_key),
    })
}

fn format_display(modifiers: u32, vk: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if modifiers & MOD_CONTROL != 0 {
        parts.push("Ctrl".to_owned());
    }
    if modifiers & MOD_ALT != 0 {
        parts.push("Alt".to_owned());
    }
    if modifiers & MOD_SHIFT != 0 {
        parts.push("Shift".to_owned());
    }
    parts.push(match vk {
        0x20 => String::from("Space"),
        value if (0x41..=0x5A).contains(&value) || (0x30..=0x39).contains(&value) => {
            char::from_u32(value).unwrap_or('?').to_string()
        }
        value if (0x70..=0x87).contains(&value) => format!("F{}", value - 0x6F),
        _ => format!("VK_{vk:02X}"),
    });
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_choices() {
        assert_eq!(parse("AltSpace").unwrap().display, "Alt+Space");
        assert_eq!(parse("CtrlAltD").unwrap().display, "Ctrl+Alt+D");
        assert_eq!(parse("Disabled"), None);
        assert_eq!(parse("WinAltD"), None);
    }

    #[test]
    fn parses_custom_combinations() {
        let binding = parse("Ctrl+Shift+L").unwrap();
        assert_eq!(binding.modifiers, MOD_CONTROL | MOD_SHIFT);
        assert_eq!(binding.virtual_key, 0x4C);
        assert_eq!(binding.display, "Ctrl+Shift+L");
    }

    #[test]
    fn rejects_invalid_combinations() {
        assert!(parse("Win+D").is_none());
        assert!(parse("Shift+F5").is_none());
        assert!(parse("Ctrl+A+B").is_none());
        assert!(parse("Ctrl+Ctrl+K").is_none());
        assert!(parse("K").is_none());
        assert!(parse("Ctrl+Escape").is_none());
    }
}
