//! Pure policy logic for Dynamic Island widget placement, ordering, visibility, and sizing.
//!
//! Mirrors C# `Services/App/DynamicIslandPolicy.cs`.

use std::collections::{HashMap, HashSet};

use crate::settings::{DYNAMIC_ISLAND_ALL_DISPLAYS, DynamicIslandVisibilityMode};

/// Returns whether `widget_id` is assigned to `device_name` (or `*` for all displays).
pub fn is_assigned_to_display(
    placements: &HashMap<String, String>,
    widget_id: &str,
    device_name: &str,
) -> bool {
    let Some((_, target)) = placements
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(widget_id))
    else {
        return false;
    };
    target.eq_ignore_ascii_case(DYNAMIC_ISLAND_ALL_DISPLAYS)
        || target.eq_ignore_ascii_case(device_name)
}

/// Returns whether `widget_id` is assigned to at least one connected display.
pub fn is_assigned_to_any_display<S: AsRef<str>>(
    placements: &HashMap<String, String>,
    widget_id: &str,
    connected_device_names: &[S],
) -> bool {
    connected_device_names
        .iter()
        .any(|name| is_assigned_to_display(placements, widget_id, name.as_ref()))
}

/// Filters `selected_widgets` down to those assigned to `device_name` (or `*`).
pub fn widgets_for_display<S: AsRef<str>>(
    selected_widgets: &[S],
    placements: &HashMap<String, String>,
    device_name: &str,
) -> Vec<String> {
    selected_widgets
        .iter()
        .filter(|id| is_assigned_to_display(placements, id.as_ref(), device_name))
        .map(|id| id.as_ref().to_owned())
        .collect()
}

/// Orders `available_widgets` using `display_order`, then `widget_order`, then `default_order`.
/// Any available widgets missing from those lists are appended in their `available_widgets` order.
pub fn order_widgets(
    available_widgets: &[String],
    display_order: Option<&[String]>,
    widget_order: Option<&[String]>,
    default_order: &[&str],
) -> Vec<String> {
    let available: HashSet<String> = available_widgets
        .iter()
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(available_widgets.len());

    let mut push_if_available = |candidate: &str| {
        let lower = candidate.to_ascii_lowercase();
        if available.contains(&lower) && seen.insert(lower) {
            // Preserve the canonical casing from `available_widgets` when present.
            if let Some(canon) = available_widgets
                .iter()
                .find(|w| w.eq_ignore_ascii_case(candidate))
            {
                out.push(canon.clone());
            } else {
                out.push(candidate.to_owned());
            }
        }
    };

    if let Some(order) = display_order {
        for item in order {
            push_if_available(item);
        }
    }
    if let Some(order) = widget_order {
        for item in order {
            push_if_available(item);
        }
    }
    for item in default_order {
        push_if_available(item);
    }
    for item in available_widgets {
        push_if_available(item);
    }
    out
}

/// Moves `widget_id` one step earlier (`offset == -1`) or later (`offset == 1`) in `order`.
pub fn move_widget(order: &[String], widget_id: &str, offset: i32) -> Vec<String> {
    if offset != -1 && offset != 1 {
        return order.to_vec();
    }
    let Some(index) = order
        .iter()
        .position(|id| id.eq_ignore_ascii_case(widget_id))
    else {
        return order.to_vec();
    };
    let target = index as isize + offset as isize;
    if target < 0 || target >= order.len() as isize {
        return order.to_vec();
    }
    let mut moved = order.to_vec();
    moved.swap(index, target as usize);
    moved
}

/// Evaluates whether the Dynamic Island should be visible on its monitor.
pub fn should_show(
    visibility: DynamicIslandVisibilityMode,
    desktop_visible: bool,
    hide_in_fullscreen: bool,
    fullscreen: bool,
) -> bool {
    if hide_in_fullscreen && fullscreen {
        return false;
    }
    match visibility {
        DynamicIslandVisibilityMode::Always => true,
        DynamicIslandVisibilityMode::DesktopOnly => desktop_visible,
    }
}

/// Clamps preferred island width to `[requested_min, requested_max]` and monitor width minus insets.
pub fn clamp_window_width(
    preferred: f32,
    monitor_width: f32,
    requested_min: f32,
    requested_max: f32,
    edge_inset: f32,
) -> f32 {
    if !monitor_width.is_finite() || monitor_width <= 0.0 {
        return preferred.clamp(240.0, 960.0);
    }
    let available = (monitor_width - edge_inset.max(0.0) * 2.0).max(240.0);
    let maximum = requested_max.max(240.0).min(available);
    let minimum = requested_min.max(240.0).min(maximum);
    let value = if preferred.is_finite() {
        preferred
    } else {
        minimum
    };
    value.clamp(minimum, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_single_and_all_displays() {
        let displays = ["DISPLAY1", "DISPLAY2"];
        let mut placements = HashMap::new();
        placements.insert("Media".to_owned(), "DISPLAY1".to_owned());
        placements.insert("Clock".to_owned(), DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned());

        assert!(is_assigned_to_display(&placements, "media", "display1"));
        assert!(!is_assigned_to_display(&placements, "Media", "DISPLAY2"));
        assert!(is_assigned_to_display(&placements, "Clock", "DISPLAY1"));
        assert!(is_assigned_to_display(&placements, "Clock", "DISPLAY2"));

        assert!(is_assigned_to_any_display(&placements, "Clock", &displays));
        assert!(!is_assigned_to_any_display::<&str>(
            &placements,
            "Clock",
            &[]
        ));

        let d2 = widgets_for_display(&["Media", "Clock"], &placements, "DISPLAY2");
        assert_eq!(d2, vec!["Clock"]);
    }

    #[test]
    fn visibility_and_fullscreen_suppression() {
        assert!(should_show(
            DynamicIslandVisibilityMode::Always,
            false,
            true,
            false
        ));
        assert!(!should_show(
            DynamicIslandVisibilityMode::DesktopOnly,
            false,
            true,
            false
        ));
        assert!(should_show(
            DynamicIslandVisibilityMode::DesktopOnly,
            true,
            true,
            false
        ));
        assert!(!should_show(
            DynamicIslandVisibilityMode::Always,
            true,
            true,
            true
        ));
        assert!(should_show(
            DynamicIslandVisibilityMode::Always,
            true,
            false,
            true
        ));
    }

    #[test]
    fn ordering_and_moving_widgets() {
        let available = vec!["Media".to_owned(), "Clock".to_owned(), "Weather".to_owned()];
        let ordered = order_widgets(
            &available,
            Some(&["Media".to_owned()]),
            Some(&["Clock".to_owned()]),
            &["Weather", "Stopwatch"],
        );
        assert_eq!(ordered, vec!["Media", "Clock", "Weather"]);

        let moved_left = move_widget(&available, "Clock", -1);
        assert_eq!(moved_left, vec!["Clock", "Media", "Weather"]);

        let moved_right = move_widget(&available, "Clock", 1);
        assert_eq!(moved_right, vec!["Media", "Weather", "Clock"]);

        let single = vec!["Media".to_owned()];
        assert_eq!(move_widget(&single, "Media", -1), vec!["Media"]);
    }

    #[test]
    fn clamp_width_respects_monitor_and_bounds() {
        assert_eq!(clamp_window_width(800.0, 420.0, 430.0, 960.0, 16.0), 388.0);
        assert_eq!(
            clamp_window_width(1200.0, 1920.0, 360.0, 960.0, 16.0),
            960.0
        );
    }
}
