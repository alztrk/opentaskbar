//! `WidgetHoverCoordinator` — C# `WidgetHoverCoordinator.cs` parity.
//!
//! Owns hover-to-expand intent (350 ms), inline widget expansion, slider pointer capture,
//! cursor verification against the widget rectangle, and post-click suppression (300 ms).
//! WS-D calls the public API from the dock's `WM_MOUSEMOVE` / `WM_MOUSELEAVE` handlers.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{POINT, RECT};

/// C# `WidgetHoverCoordinator.HoverIntentDelayMs`.
pub const HOVER_INTENT_MS: u64 = 350;
/// C# `WidgetPanelHost` suppression window after a flyout is dismissed.
pub const SUPPRESS_MS: u64 = 300;

/// What changed after a coordinator tick. WS-D uses this to call `update_visuals()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverChange {
    None,
    /// Hover intent fired — expand this widget inline.
    Expand,
    /// Cursor left / capture ended — collapse this widget inline.
    Collapse,
}

/// Lifecycle phase of a hovered widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverPhase {
    Idle,
    /// Cursor is inside, 350 ms intent timer running.
    Pending,
    /// Intent fired; the widget renders its expanded inline content.
    Expanded,
    /// A flyout slider owns the pointer; hover-leave must not collapse.
    Captured,
    /// Suppressed after a click/dismiss; hover must be re-armed.
    Suppressed,
}

/// Pure, UI-thread-owned state machine. No locks (the dock is single-threaded).
pub struct WidgetHoverCoordinator {
    phase: HoverPhase,
    pending_id: Option<String>,
    pending_since: Option<Instant>,
    expanded_id: Option<String>,
    captured: Option<(String, u32)>,
    suppressed: Option<(String, Instant)>,
    last_cursor: POINT,
    /// Rectangle of the currently expanded widget (DIP-free physical pixels).
    expanded_rect: Option<RECT>,
}

impl Default for WidgetHoverCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetHoverCoordinator {
    pub fn new() -> Self {
        Self {
            phase: HoverPhase::Idle,
            pending_id: None,
            pending_since: None,
            expanded_id: None,
            captured: None,
            suppressed: None,
            last_cursor: POINT { x: 0, y: 0 },
            expanded_rect: None,
        }
    }

    pub fn phase(&self) -> HoverPhase {
        self.phase
    }

    pub fn expanded_id(&self) -> Option<&str> {
        self.expanded_id.as_deref()
    }

    pub fn is_captured(&self) -> bool {
        self.captured.is_some()
    }

    pub fn captured_slider(&self) -> Option<(&str, u32)> {
        self.captured
            .as_ref()
            .map(|(id, slider)| (id.as_str(), *slider))
    }

    /// Records the latest cursor position (physical pixels) for verification.
    pub fn set_cursor(&mut self, x: i32, y: i32) {
        self.last_cursor = POINT { x, y };
    }

    /// C# `WidgetHoverCoordinator.Enter`: arm the 350 ms intent timer.
    /// Returns `true` when the widget was already expanded (no visual change needed).
    pub fn on_enter(&mut self, id: &str, now: Instant) -> bool {
        if self.expanded_id.as_deref() == Some(id) {
            return true;
        }
        if self.is_suppressed(id, now) {
            self.phase = HoverPhase::Suppressed;
            return false;
        }
        if self.captured.is_some() {
            return false;
        }
        self.pending_id = Some(id.to_owned());
        self.pending_since = Some(now);
        if self.phase != HoverPhase::Captured {
            self.phase = HoverPhase::Pending;
        }
        false
    }

    /// C# `WidgetHoverCoordinator.Leave`: cancel pending intent. An expanded widget is kept
    /// expanded until `verify_cursor` proves the pointer left its rect (avoids flicker when the
    /// pointer crosses a gap between inline sub-buttons).
    pub fn on_leave(&mut self, id: &str, now: Instant) -> HoverChange {
        if self.pending_id.as_deref() == Some(id) {
            self.pending_id = None;
            self.pending_since = None;
            if self.phase == HoverPhase::Pending {
                self.phase = HoverPhase::Idle;
            }
        }
        if self.expanded_id.as_deref() == Some(id) && self.captured.is_none() {
            self.collapse(id, now)
        } else {
            HoverChange::None
        }
    }

    /// Drives the intent timer. Call once per UI frame (or on a 16 ms tick).
    pub fn tick(&mut self, now: Instant) -> HoverChange {
        if self.phase == HoverPhase::Pending
            && let (Some(id), Some(since)) = (self.pending_id.as_deref(), self.pending_since)
            && now.duration_since(since) >= Duration::from_millis(HOVER_INTENT_MS)
        {
            self.expanded_id = Some(id.to_owned());
            self.pending_id = None;
            self.pending_since = None;
            self.phase = HoverPhase::Expanded;
            return HoverChange::Expand;
        }
        HoverChange::None
    }

    /// C# `WidgetHoverCoordinator.VerifyCursor`: collapse if the cursor is no longer inside the
    /// expanded widget rectangle. Returns `true` when still inside.
    pub fn verify_cursor(&mut self, rect: RECT, cursor: POINT) -> bool {
        self.last_cursor = cursor;
        let inside = cursor.x >= rect.left
            && cursor.x < rect.right
            && cursor.y >= rect.top
            && cursor.y < rect.bottom;
        if !inside && self.captured.is_none() {
            if let Some(id) = self.expanded_id.take() {
                self.expanded_rect = None;
                self.phase = HoverPhase::Idle;
                let _ = id; // caller is told via `verify_cursor` returning false
            }
        } else if inside {
            self.expanded_rect = Some(rect);
        }
        inside
    }

    /// Stores the live rect of the expanded widget for subsequent `verify_cursor` calls that
    /// omit an explicit rect.
    pub fn set_expanded_rect(&mut self, rect: RECT) {
        self.expanded_rect = Some(rect);
    }

    /// Verifies against the stored rect (convenience for WS-D).
    pub fn verify_cursor_stored(&mut self, cursor: POINT) -> bool {
        match self.expanded_rect {
            Some(rect) => self.verify_cursor(rect, cursor),
            None => true,
        }
    }

    /// C# slider pointer capture: while a flyout/inline slider is dragged, hover-leave and cursor
    /// verification must not collapse the widget (otherwise the drag target disappears).
    pub fn begin_slider_capture(&mut self, id: &str, slider_id: u32) {
        self.captured = Some((id.to_owned(), slider_id));
        self.phase = HoverPhase::Captured;
    }

    pub fn end_slider_capture(&mut self, now: Instant) -> HoverChange {
        let was = self.captured.take();
        if let Some((id, _)) = was {
            self.phase = HoverPhase::Idle;
            if self.expanded_id.as_deref() == Some(&id) {
                return self.collapse(&id, now);
            }
        }
        HoverChange::None
    }

    /// C# `WidgetHoverCoordinator.Suppress`: ignore hover for this widget for a short window
    /// (called after a flyout is dismissed so the pointer landing back on the card doesn't
    /// immediately re-open it).
    pub fn suppress(&mut self, id: &str, now: Instant) {
        self.suppressed = Some((id.to_owned(), now));
        self.pending_id = None;
        self.pending_since = None;
        if self.expanded_id.as_deref() != Some(id) {
            self.phase = HoverPhase::Suppressed;
        }
    }

    pub fn is_suppressed(&self, id: &str, now: Instant) -> bool {
        self.suppressed.as_ref().is_some_and(|(sid, at)| {
            sid.eq_ignore_ascii_case(id)
                && now.duration_since(*at) < Duration::from_millis(SUPPRESS_MS)
        })
    }

    /// Clears suppression immediately (e.g. on an explicit user click).
    pub fn clear_suppression(&mut self) {
        self.suppressed = None;
        if self.phase == HoverPhase::Suppressed {
            self.phase = HoverPhase::Idle;
        }
    }

    fn collapse(&mut self, id: &str, _now: Instant) -> HoverChange {
        if self.expanded_id.as_deref() == Some(id) {
            self.expanded_id = None;
            self.expanded_rect = None;
            self.phase = HoverPhase::Idle;
            HoverChange::Collapse
        } else {
            HoverChange::None
        }
    }

    /// Full state reset (session switch / dock relocate / settings reload).
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_intent_fires_after_350ms() {
        let mut c = WidgetHoverCoordinator::new();
        let t0 = Instant::now();
        assert!(!c.on_enter("stats", t0));
        assert_eq!(c.tick(t0 + Duration::from_millis(349)), HoverChange::None);
        assert_eq!(c.tick(t0 + Duration::from_millis(350)), HoverChange::Expand);
        assert_eq!(c.expanded_id(), Some("stats"));
    }

    #[test]
    fn leave_before_intent_cancels() {
        let mut c = WidgetHoverCoordinator::new();
        let t0 = Instant::now();
        c.on_enter("media", t0);
        assert_eq!(
            c.on_leave("media", t0 + Duration::from_millis(100)),
            HoverChange::None
        );
        assert_eq!(c.tick(t0 + Duration::from_millis(500)), HoverChange::None);
        assert_eq!(c.expanded_id(), None);
    }

    #[test]
    fn slider_capture_blocks_collapse() {
        let mut c = WidgetHoverCoordinator::new();
        let t0 = Instant::now();
        c.on_enter("media", t0);
        c.tick(t0 + Duration::from_millis(400));
        c.begin_slider_capture("media", 2);
        assert!(c.is_captured());
        assert_eq!(
            c.on_leave("media", t0 + Duration::from_millis(500)),
            HoverChange::None
        );
        assert_eq!(c.expanded_id(), Some("media"));
        assert_eq!(
            c.end_slider_capture(t0 + Duration::from_millis(600)),
            HoverChange::Collapse
        );
    }

    #[test]
    fn cursor_verification_collapses_on_exit() {
        let mut c = WidgetHoverCoordinator::new();
        let t0 = Instant::now();
        c.on_enter("audio", t0);
        c.tick(t0 + Duration::from_millis(400));
        let rect = RECT {
            left: 10,
            top: 10,
            right: 100,
            bottom: 50,
        };
        assert!(c.verify_cursor(rect, POINT { x: 50, y: 30 }));
        assert!(!c.verify_cursor(rect, POINT { x: 200, y: 30 }));
        assert_eq!(c.expanded_id(), None);
    }

    #[test]
    fn suppression_blocks_hover_for_300ms() {
        let mut c = WidgetHoverCoordinator::new();
        let t0 = Instant::now();
        c.suppress("clock", t0);
        assert!(c.is_suppressed("clock", t0 + Duration::from_millis(299)));
        c.on_enter("clock", t0 + Duration::from_millis(100));
        assert_eq!(c.tick(t0 + Duration::from_millis(500)), HoverChange::None);
        assert!(!c.is_suppressed("clock", t0 + Duration::from_millis(301)));
        c.on_enter("clock", t0 + Duration::from_millis(400));
        assert_eq!(c.tick(t0 + Duration::from_millis(800)), HoverChange::Expand);
    }
}
