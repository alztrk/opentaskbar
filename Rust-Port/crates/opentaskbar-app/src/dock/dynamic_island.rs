//! Dynamic Island expanded-details state machine (C# `DynamicIslandWindow.xaml.cs`,
//! `DynamicIslandPolicy`, `TimedWidgetTile.DetailsRequested`).
//!
//! Pure/headless policy plus a small state object the Win32 island host can drive.
//! The Rust port currently renders the island inside the dock visual tree; the page
//! width and input pre-process rules below are shared with that host.
//!
//! The details provider API is consumed by WS-C widgets once those land; until then
//! the state machine and its tests live here.
#![allow(dead_code)]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::layout::{DETAILS_PANEL_WIDTH_DIPS, ISLAND_ANIM_MS};

/// C# `DynamicIslandVisibilityMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandVisibility {
    Never,
    Always,
    DesktopOnly,
}

/// A single line of the 246px details panel (`WidgetChipDetails` / `GetDetails()`).
#[derive(Clone, Debug, PartialEq)]
pub struct DetailLine {
    pub label: String,
    pub value: String,
    pub accent: bool,
}

impl DetailLine {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            accent: false,
        }
    }

    pub fn accent(mut self) -> Self {
        self.accent = true;
        self
    }
}

/// Per-widget details provider result (C# `GetDetails()` on TimedWidgetTile / Clock / etc.).
#[derive(Clone, Debug, PartialEq)]
pub struct WidgetDetails {
    pub widget_id: String,
    pub title: String,
    pub lines: Vec<DetailLine>,
}

impl WidgetDetails {
    pub fn new(widget_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            widget_id: widget_id.into(),
            title: title.into(),
            lines: Vec::new(),
        }
    }

    pub fn line(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.lines.push(DetailLine::new(label, value));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Global input pre-process outcome. `Consumed` means the dock must not forward
/// the key to the focused widget or the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDisposition {
    PassThrough,
    Consumed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandInput {
    Escape,
    Left,
    Right,
    Other,
}

/// Island page specification: collapsed chip width and optional expanded width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageWidth {
    pub collapsed: f32,
    pub expanded: f32,
}

const STANDARD_PAGE: PageWidth = PageWidth {
    collapsed: 168.0,
    expanded: 168.0 + DETAILS_PANEL_WIDTH_DIPS,
};
const TIMED_PAGE: PageWidth = PageWidth {
    collapsed: 196.0,
    expanded: 196.0 + DETAILS_PANEL_WIDTH_DIPS,
};
const MEDIA_PAGE: PageWidth = PageWidth {
    collapsed: 314.0,
    expanded: 560.0,
};

/// `WidthForCurrentPage`: expanded pages grow by the 246px details panel (media uses
/// its own 560px island width, matching C# MediaWidget).
pub fn width_for_page(widget_id: &str, expanded: bool) -> f32 {
    let page = page_width(widget_id);
    if expanded {
        page.expanded
    } else {
        page.collapsed
    }
}

pub fn page_width(widget_id: &str) -> PageWidth {
    match widget_id.to_ascii_lowercase().as_str() {
        "media" => MEDIA_PAGE,
        "focus" | "stopwatch" | "countdown" => TIMED_PAGE,
        _ => STANDARD_PAGE,
    }
}

/// C# `DynamicIslandPolicy.ClampWindowWidth`.
pub fn clamp_window_width(
    preferred: f32,
    monitor_width: f32,
    requested_minimum: f32,
    requested_maximum: f32,
    edge_inset: f32,
) -> f32 {
    if !monitor_width.is_finite() || monitor_width <= 0.0 {
        return if preferred.is_finite() {
            preferred.clamp(240.0, 960.0)
        } else {
            240.0
        };
    }
    let available = (monitor_width - edge_inset.max(0.0) * 2.0).max(240.0);
    let maximum = requested_maximum.max(240.0).min(available);
    let minimum = requested_minimum.max(240.0).min(maximum);
    if preferred.is_finite() {
        preferred.clamp(minimum, maximum)
    } else {
        minimum
    }
}

pub fn should_show(
    visibility: IslandVisibility,
    desktop_visible: bool,
    hide_in_fullscreen: bool,
    fullscreen: bool,
) -> bool {
    !(hide_in_fullscreen && fullscreen)
        && match visibility {
            IslandVisibility::Always => true,
            IslandVisibility::DesktopOnly => desktop_visible,
            IslandVisibility::Never => false,
        }
}

/// C# `DynamicIslandPolicy.OrderWidgets(... timedOrder)` — the Rust signature omitted
/// `timedOrder`; this helper restores it.
pub fn order_widgets(
    available: &[String],
    display_order: &[String],
    widget_order: &[String],
    default_order: &[String],
    timed_order: &[String],
) -> Vec<String> {
    let available: HashSet<String> = available.iter().map(|w| w.to_ascii_lowercase()).collect();
    let mut ordered: Vec<String> = Vec::new();
    for candidate in display_order
        .iter()
        .chain(widget_order)
        .chain(default_order)
        .chain(timed_order)
    {
        let key = candidate.to_ascii_lowercase();
        if available.contains(&key) && !ordered.iter().any(|w| w.eq_ignore_ascii_case(candidate)) {
            ordered.push(candidate.clone());
        }
    }
    ordered
}

pub fn move_widget(order: &[String], widget_id: &str, offset: isize) -> Vec<String> {
    if offset != -1 && offset != 1 {
        return order.to_vec();
    }
    let Some(index) = order.iter().position(|w| w.eq_ignore_ascii_case(widget_id)) else {
        return order.to_vec();
    };
    let target = index as isize + offset;
    if target < 0 || target >= order.len() as isize {
        return order.to_vec();
    }
    let mut moved = order.to_vec();
    moved.swap(index, target as usize);
    moved
}

/// Mutable island state. `SetExpanded` starts a 180ms eased width transition;
/// `pre_process_input` consumes navigation keys while expanded.
pub struct IslandState {
    pub visible: bool,
    pub page_index: usize,
    pub expanded: bool,
    pub page_ids: Vec<String>,
    pub details: Option<WidgetDetails>,
    animation_started: Option<Instant>,
    details_requested: bool,
}

impl IslandState {
    pub fn new(page_ids: Vec<String>) -> Self {
        Self {
            visible: true,
            page_index: 0,
            expanded: false,
            page_ids,
            details: None,
            animation_started: None,
            details_requested: false,
        }
    }

    pub fn current_widget(&self) -> Option<&str> {
        self.page_ids.get(self.page_index).map(String::as_str)
    }

    /// C# `SetExpanded()`.
    pub fn set_expanded(&mut self, expanded: bool) -> bool {
        if self.expanded == expanded {
            return false;
        }
        self.expanded = expanded;
        self.animation_started = Some(Instant::now());
        if !expanded {
            self.details = None;
            self.details_requested = false;
        }
        true
    }

    pub fn animation_progress(&self, now: Instant) -> f32 {
        let Some(started) = self.animation_started else {
            return 1.0;
        };
        (now.saturating_duration_since(started).as_millis() as f32 / ISLAND_ANIM_MS as f32)
            .clamp(0.0, 1.0)
    }

    pub fn animation_done(&self, now: Instant) -> bool {
        self.animation_started.is_some_and(|started| {
            now.saturating_duration_since(started) >= Duration::from_millis(ISLAND_ANIM_MS)
        })
    }

    pub fn current_width(&self, now: Instant) -> f32 {
        let id = self.current_widget().unwrap_or("");
        let collapsed = width_for_page(id, false);
        if !self.expanded {
            return collapsed;
        }
        let expanded = width_for_page(id, true);
        let t = super::layout::cubic_ease_out(self.animation_progress(now));
        collapsed + (expanded - collapsed) * t
    }

    pub fn go_page(&mut self, direction: isize) -> bool {
        if self.page_ids.is_empty() {
            return false;
        }
        let len = self.page_ids.len() as isize;
        let next = (self.page_index as isize + direction).rem_euclid(len) as usize;
        if next == self.page_index {
            return false;
        }
        self.page_index = next;
        self.details = None;
        self.details_requested = false;
        true
    }

    /// Global input pre-process: Escape collapses, arrows switch pages while expanded.
    pub fn pre_process_input(&mut self, input: IslandInput) -> InputDisposition {
        if !self.visible {
            return InputDisposition::PassThrough;
        }
        match input {
            IslandInput::Escape if self.expanded => {
                self.set_expanded(false);
                InputDisposition::Consumed
            }
            IslandInput::Left if self.expanded && self.page_ids.len() > 1 => {
                self.go_page(-1);
                InputDisposition::Consumed
            }
            IslandInput::Right if self.expanded && self.page_ids.len() > 1 => {
                self.go_page(1);
                InputDisposition::Consumed
            }
            _ => InputDisposition::PassThrough,
        }
    }

    /// `TimedWidgetTile.DetailsRequested` hook: expand and ask the provider once per page.
    pub fn request_details(
        &mut self,
        provider: impl FnOnce(&str) -> Option<WidgetDetails>,
    ) -> bool {
        let Some(id) = self.current_widget().map(str::to_owned) else {
            return false;
        };
        self.set_expanded(true);
        if self.details_requested {
            return false;
        }
        self.details_requested = true;
        self.details = provider(&id).filter(|d| !d.is_empty());
        true
    }

    pub fn reset_transient(&mut self) {
        self.expanded = false;
        self.details = None;
        self.details_requested = false;
        self.animation_started = None;
        self.page_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages() -> Vec<String> {
        ["clock", "focus", "media"].map(str::to_owned).to_vec()
    }

    #[test]
    fn details_panel_adds_246_dips() {
        assert_eq!(width_for_page("clock", false), 168.0);
        assert_eq!(
            width_for_page("clock", true),
            168.0 + DETAILS_PANEL_WIDTH_DIPS
        );
        assert_eq!(width_for_page("media", true), 560.0);
        assert_eq!(
            page_width("focus").expanded - page_width("focus").collapsed,
            DETAILS_PANEL_WIDTH_DIPS
        );
    }

    #[test]
    fn clamp_window_width_respects_monitor_and_bounds() {
        assert_eq!(clamp_window_width(500.0, 1920.0, 240.0, 960.0, 16.0), 500.0);
        assert_eq!(
            clamp_window_width(2000.0, 1024.0, 240.0, 960.0, 16.0),
            960.0
        );
        assert_eq!(clamp_window_width(f32::NAN, 0.0, 240.0, 960.0, 16.0), 240.0);
    }

    #[test]
    fn visibility_modes() {
        assert!(should_show(IslandVisibility::Always, false, true, false));
        assert!(!should_show(IslandVisibility::Always, true, true, true));
        assert!(should_show(
            IslandVisibility::DesktopOnly,
            true,
            false,
            true
        ));
        assert!(!should_show(IslandVisibility::Never, true, false, false));
    }

    #[test]
    fn order_widgets_includes_timed_order_and_deduplicates() {
        let available = ["clock", "focus", "media"].map(str::to_owned).to_vec();
        let order = order_widgets(
            &available,
            &["media".into()],
            &["clock".into(), "hidden".into()],
            &["focus".into(), "clock".into()],
            &["focus".into()],
        );
        assert_eq!(order, ["media", "clock", "focus"]);
    }

    #[test]
    fn move_widget_only_swaps_one_step() {
        let order = ["a", "b", "c"].map(str::to_owned).to_vec();
        assert_eq!(move_widget(&order, "b", -1), ["b", "a", "c"]);
        assert_eq!(move_widget(&order, "a", -1), order);
        assert_eq!(move_widget(&order, "a", 2), order);
    }

    #[test]
    fn set_expanded_starts_180ms_animation() {
        let mut island = IslandState::new(pages());
        let now = Instant::now();
        assert!(!island.expanded);
        assert!(island.set_expanded(true));
        assert!(!island.set_expanded(true));
        assert!(island.current_width(now) < 168.0 + DETAILS_PANEL_WIDTH_DIPS);
        assert!(island.animation_progress(now + Duration::from_millis(90)) <= 0.51);
        assert!(island.animation_done(now + Duration::from_millis(181)));
        assert!(
            island.current_width(now + Duration::from_millis(200))
                >= 168.0 + DETAILS_PANEL_WIDTH_DIPS - 0.01
        );
    }

    #[test]
    fn pre_process_consumes_escape_and_arrows_only_when_expanded() {
        let mut island = IslandState::new(pages());
        assert_eq!(
            island.pre_process_input(IslandInput::Escape),
            InputDisposition::PassThrough
        );
        assert_eq!(
            island.pre_process_input(IslandInput::Right),
            InputDisposition::PassThrough
        );
        island.set_expanded(true);
        assert_eq!(
            island.pre_process_input(IslandInput::Right),
            InputDisposition::Consumed
        );
        assert_eq!(island.current_widget(), Some("focus"));
        assert_eq!(
            island.pre_process_input(IslandInput::Left),
            InputDisposition::Consumed
        );
        assert_eq!(island.current_widget(), Some("clock"));
        assert_eq!(
            island.pre_process_input(IslandInput::Escape),
            InputDisposition::Consumed
        );
        assert!(!island.expanded);
    }

    #[test]
    fn details_requested_fires_once_and_populates_panel() {
        let mut island = IslandState::new(pages());
        assert!(island.request_details(|id| {
            Some(
                WidgetDetails::new(id, "Date and time")
                    .line("Timezone", "Europe/Istanbul")
                    .line("Format", "24h"),
            )
        }));
        assert!(island.expanded);
        assert_eq!(island.details.as_ref().unwrap().lines.len(), 2);
        // A second request on the same page must not fire the provider again.
        assert!(!island.request_details(|_| panic!("provider must not be called twice")));
    }

    #[test]
    fn session_switch_resets_transient_island_ui() {
        let mut island = IslandState::new(pages());
        island.set_expanded(true);
        island.go_page(1);
        island.reset_transient();
        assert!(!island.expanded);
        assert_eq!(island.page_index, 0);
        assert!(island.details.is_none());
    }
}
