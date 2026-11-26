//! Small pure helpers for in-app text entry (search box, hotkey capture, numeric fields).
//!
//! GPUI 0.2 has no bundled text field; Settings uses `Div::on_key_down` plus these helpers so
//! keystroke handling stays testable and consistent with the native C2 `TextInput` flyout block.

/// Applies a GPUI keystroke to a text buffer. Returns `true` when the buffer changed.
///
/// Only single printable characters are accepted; modifier-only and navigation keys are ignored.
pub fn apply_keystroke(
    buffer: &mut String,
    key: &str,
    key_char: Option<&str>,
    numeric: bool,
) -> bool {
    match key {
        "backspace" => {
            let changed = buffer.pop().is_some();
            return changed;
        }
        "delete" => {
            if buffer.is_empty() {
                return false;
            }
            buffer.clear();
            return true;
        }
        "escape" | "enter" | "tab" | "shift" | "control" | "alt" | "windows" | "capslock"
        | "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown" => {
            return false;
        }
        _ => {}
    }
    let ch = key_char
        .filter(|s| !s.is_empty())
        .unwrap_or(key)
        .chars()
        .next();
    let Some(ch) = ch else {
        return false;
    };
    if ch.is_control() || buffer.chars().count() >= 64 {
        return false;
    }
    if numeric && !ch.is_ascii_digit() && ch != '.' && ch != '-' {
        return false;
    }
    buffer.push(ch);
    true
}

/// Clamps a numeric buffer to `min..=max`, normalizing integral values (`5.0` → `5`).
pub fn clamp_numeric(buffer: &mut String, min: f64, max: f64) {
    let Ok(mut value) = buffer.parse::<f64>() else {
        return;
    };
    value = value.clamp(min, max);
    *buffer = if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    };
}

#[cfg(test)]
mod tests {
    use super::{apply_keystroke, clamp_numeric};

    #[test]
    fn keystrokes_build_and_erase_text() {
        let mut buf = String::new();
        for c in ["w", "e", "a", "t", "h", "e", "r"] {
            assert!(apply_keystroke(&mut buf, c, None, false));
        }
        assert_eq!(buf, "weather");
        assert!(apply_keystroke(&mut buf, "backspace", None, false));
        assert_eq!(buf, "weathe");
        assert!(!apply_keystroke(&mut buf, "shift", None, false));
        assert!(!apply_keystroke(&mut buf, "enter", None, false));
    }

    #[test]
    fn numeric_fields_reject_letters_and_clamp() {
        let mut buf = String::new();
        assert!(apply_keystroke(&mut buf, "1", None, true));
        assert!(apply_keystroke(&mut buf, "4", None, true));
        assert!(!apply_keystroke(&mut buf, "x", None, true));
        assert_eq!(buf, "14");
        clamp_numeric(&mut buf, 1.0, 10.0);
        assert_eq!(buf, "10");
    }
}
