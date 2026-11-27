//! Text entry helpers for in-app UI surfaces.
//!
//! GPUI 0.2 does not ship a ready-made `Input` element. `TextInput` delegates layout and
//! painting to the wrapped element (usually a stateful `Div`), then registers the owning
//! entity as the active IME/text handler for the frame. Character input, IME composition
//! and paste are handled by the platform; the entity supplies the buffer via
//! `EntityInputHandler`.
//!
//! Settings uses `Div::on_key_down` plus the pure [`apply_keystroke`] / [`clamp_numeric`]
//! helpers so keystroke handling stays testable and consistent with the native C2
//! `TextInput` flyout block.

use gpui::{
    App, Bounds, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window,
};

pub(crate) struct TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    inner: D,
    focus: FocusHandle,
    view: Entity<T>,
}

impl<T, D> TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    pub(crate) fn new(inner: D, focus: FocusHandle, view: Entity<T>) -> Self {
        Self { inner, focus, view }
    }
}

impl<T, D> IntoElement for TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<T, D> Element for TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    type RequestLayoutState = D::RequestLayoutState;
    type PrepaintState = D::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        self.inner.id()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.inner.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.inner.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.inner
            .prepaint(id, inspector_id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
    }
}

/// Shared capacity for both in-app text entry engines (GPUI Settings and the C2 flyout).
pub const MAX_TEXT_CHARS: usize = 64;

/// Single source of truth for numeric character acceptance. A sign is only legal
/// at the start and a decimal separator only once, so `"--5"` / `"1.2.3"` can
/// never become an unparsable committed value.
#[must_use]
pub fn is_numeric_char(ch: char, buffer: &str) -> bool {
    match ch {
        '0'..='9' => true,
        '-' => buffer.is_empty(),
        '.' => !buffer.is_empty() && !buffer.ends_with('-') && !buffer.contains('.'),
        _ => false,
    }
}

/// Appends one printable character, enforcing the shared character limit.
/// Returns `true` when the buffer changed.
pub fn push_char(buffer: &mut String, ch: char, numeric: bool, max_chars: usize) -> bool {
    if ch.is_control() || buffer.chars().count() >= max_chars {
        return false;
    }
    if numeric && !is_numeric_char(ch, buffer) {
        return false;
    }
    buffer.push(ch);
    true
}

/// Clamps a numeric buffer to `min..=max`, normalizing integral values (`5.0` → `5`).
///
/// A reversed or non-finite bound is normalized instead of panicking: custom
/// widget manifests may contain `min > max`, and `f64::clamp` panics in that case.
pub fn normalize_numeric(buffer: &mut String, min: f64, max: f64) {
    let Ok(value) = buffer.parse::<f64>() else {
        return;
    };
    let value = if min.is_finite() && max.is_finite() {
        let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
        value.clamp(lo, hi)
    } else {
        value
    };
    *buffer = if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    };
}

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
    push_char(buffer, ch, numeric, MAX_TEXT_CHARS)
}

/// Clamps a numeric buffer to `min..=max`, normalizing integral values (`5.0` → `5`).
pub fn clamp_numeric(buffer: &mut String, min: f64, max: f64) {
    normalize_numeric(buffer, min, max);
}

#[cfg(test)]
mod tests {
    use super::{MAX_TEXT_CHARS, apply_keystroke, clamp_numeric, normalize_numeric, push_char};

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

    #[test]
    fn reversed_and_non_finite_bounds_never_panic() {
        let mut buf = "50".to_owned();
        normalize_numeric(&mut buf, 10.0, 1.0);
        assert_eq!(
            buf, "10",
            "reversed bounds are normalized, not passed to clamp"
        );

        let mut buf = "50".to_owned();
        normalize_numeric(&mut buf, f64::NAN, 10.0);
        assert_eq!(buf, "50", "a non-finite bound leaves the value untouched");

        let mut buf = "3.5".to_owned();
        normalize_numeric(&mut buf, 0.0, 1.0);
        assert_eq!(buf, "1");
    }

    #[test]
    fn both_engines_share_the_unicode_character_limit() {
        let mut buf = "é".repeat(MAX_TEXT_CHARS - 1);
        assert!(push_char(&mut buf, 'ğ', false, MAX_TEXT_CHARS));
        assert_eq!(buf.chars().count(), MAX_TEXT_CHARS);
        assert!(!push_char(&mut buf, 'x', false, MAX_TEXT_CHARS));
        assert_eq!(buf.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn numeric_helpers_reject_duplicate_sign_and_separator() {
        let mut buf = String::new();
        assert!(push_char(&mut buf, '-', true, MAX_TEXT_CHARS));
        assert!(!push_char(&mut buf, '-', true, MAX_TEXT_CHARS));
        assert!(!push_char(&mut buf, '.', true, MAX_TEXT_CHARS));
        assert!(push_char(&mut buf, '5', true, MAX_TEXT_CHARS));
        assert!(push_char(&mut buf, '.', true, MAX_TEXT_CHARS));
        assert!(!push_char(&mut buf, '.', true, MAX_TEXT_CHARS));
        assert!(push_char(&mut buf, '5', true, MAX_TEXT_CHARS));
        assert_eq!(buf, "-5.5");
        normalize_numeric(&mut buf, 0.0, 10.0);
        assert_eq!(buf, "0");
    }
}
