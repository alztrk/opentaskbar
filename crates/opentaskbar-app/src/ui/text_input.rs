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
