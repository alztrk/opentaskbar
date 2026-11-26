//! C# `DockLayoutChecks` / `DockFisheyeLayout` / `DockMetrics` parity (pure
//! portion). WS-D owns the live dock, which is absent from this worktree's
//! baseline; these standalone tests pin the deterministic layout math.

/// Critically damped spring step used by the fisheye magnification.
fn step_spring(current: f32, velocity: f32, target: f32, dt: f32, omega: f32) -> (f32, f32) {
    let acceleration = omega * omega * (target - current) - 2.0 * omega * velocity;
    let velocity = velocity + acceleration * dt;
    let current = current + velocity * dt;
    (current, velocity)
}

/// Raised-cosine falloff: full scale at the pointer, 1.0 at `radius`.
fn raised_cosine_scale(distance: f32, radius: f32, max_scale: f32) -> f32 {
    if radius <= 0.0 || distance >= radius {
        return 1.0;
    }
    let t = distance / radius;
    let weight = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
    1.0 + (max_scale - 1.0) * weight
}

/// Expanded icons must not overlap their neighbours.
fn compute_no_overlap_max_scale(slot_width: f32, icon_width: f32, max_scale: f32) -> f32 {
    if icon_width <= 0.0 {
        return max_scale;
    }
    max_scale.min(slot_width / icon_width).max(1.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GapUnit {
    Pixels,
    Percent,
}

fn resolve_gap(unit: GapUnit, pixels: i32, percent: i32, screen_width: f32) -> f32 {
    match unit {
        GapUnit::Pixels => pixels as f32,
        GapUnit::Percent => screen_width * (percent as f32 / 100.0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeightMode {
    Auto,
    Pixels,
    Percent,
    Fill,
}

fn resolve_height(mode: HeightMode, pixels: i32, percent: i32, screen: f32, natural: f32) -> f32 {
    match mode {
        HeightMode::Auto => natural,
        HeightMode::Pixels => pixels as f32,
        HeightMode::Percent => screen * (percent as f32 / 100.0),
        HeightMode::Fill => screen,
    }
}

/// Reordering has hysteresis: only move an item once the pointer passes the
/// midpoint of the neighbouring slot.
fn insertion_index_with_hysteresis(length: usize, from: usize, to: usize, threshold: f32) -> usize {
    if length == 0 || from >= length {
        return 0;
    }
    let drift = to as f32 - from as f32;
    if drift.abs() < threshold {
        from
    } else if drift > 0.0 {
        (from + 1).min(length - 1)
    } else {
        from.saturating_sub(1)
    }
}

/// C# `DockItemOrdering.ApplyPinnedGroups`: shell buttons lead, tray/bin trail.
fn apply_pinned_groups(items: &[&str]) -> Vec<String> {
    const FIRST: &[&str] = &["start", "search", "taskview"];
    const LAST: &[&str] = &["tray", "bin"];
    let mut head = Vec::new();
    let mut middle = Vec::new();
    let mut tail = Vec::new();
    for group in [FIRST, LAST] {
        for pinned in group {
            if let Some(pos) = items.iter().position(|item| item == pinned) {
                let value = items[pos].to_owned();
                if group == FIRST {
                    head.push(value);
                } else {
                    tail.push(value);
                }
            }
        }
    }
    for item in items {
        if !FIRST.contains(item) && !LAST.contains(item) {
            middle.push((*item).to_owned());
        }
    }
    head.extend(middle);
    head.extend(tail);
    head
}

#[test]
fn spring_converges_without_overshoot() {
    let (mut current, mut velocity) = (1.0f32, 0.0f32);
    for _ in 0..240 {
        (current, velocity) = step_spring(current, velocity, 1.6, 1.0 / 60.0, 12.0);
    }
    assert!((current - 1.6).abs() < 0.01, "current={current}");
    assert!(velocity.abs() < 0.05, "velocity={velocity}");
    assert!(
        current <= 1.61,
        "critically damped spring must not overshoot"
    );
}

#[test]
fn raised_cosine_is_symmetric_and_bounded() {
    assert!((raised_cosine_scale(0.0, 100.0, 1.6) - 1.6).abs() < f32::EPSILON);
    let half = raised_cosine_scale(50.0, 100.0, 1.6);
    assert!((half - 1.3).abs() < 0.001, "half={half}");
    assert_eq!(raised_cosine_scale(100.0, 100.0, 1.6), 1.0);
    assert_eq!(raised_cosine_scale(200.0, 100.0, 1.6), 1.0);
    assert_eq!(raised_cosine_scale(0.0, 0.0, 1.6), 1.0);
}

#[test]
fn overlap_guard_caps_the_expansion() {
    assert_eq!(compute_no_overlap_max_scale(100.0, 50.0, 1.6), 1.6);
    assert_eq!(compute_no_overlap_max_scale(60.0, 50.0, 1.6), 1.2);
    assert_eq!(compute_no_overlap_max_scale(40.0, 50.0, 1.6), 1.0);
    assert_eq!(compute_no_overlap_max_scale(100.0, 0.0, 1.6), 1.6);
}

#[test]
fn gap_units_resolve_to_dips() {
    assert_eq!(resolve_gap(GapUnit::Pixels, 6, 0, 1920.0), 6.0);
    assert_eq!(resolve_gap(GapUnit::Percent, 0, 5, 1920.0), 96.0);
}

#[test]
fn height_modes_resolve_to_dips() {
    assert_eq!(resolve_height(HeightMode::Auto, 48, 7, 1080.0, 58.0), 58.0);
    assert_eq!(
        resolve_height(HeightMode::Pixels, 72, 7, 1080.0, 58.0),
        72.0
    );
    assert_eq!(
        resolve_height(HeightMode::Percent, 72, 10, 1080.0, 58.0),
        108.0
    );
    assert_eq!(
        resolve_height(HeightMode::Fill, 72, 10, 1080.0, 58.0),
        1080.0
    );
}

#[test]
fn reorder_requires_the_pointer_to_cross_the_threshold() {
    assert_eq!(insertion_index_with_hysteresis(5, 2, 2, 0.5), 2);
    assert_eq!(insertion_index_with_hysteresis(5, 2, 3, 0.5), 3);
    assert_eq!(insertion_index_with_hysteresis(5, 2, 1, 0.5), 1);
    assert_eq!(insertion_index_with_hysteresis(0, 0, 1, 0.5), 0);
}

#[test]
fn pinned_shell_groups_are_placed_at_the_edges() {
    let order = apply_pinned_groups(&[
        "app:b", "bin", "search", "app:a", "tray", "start", "taskview",
    ]);
    assert_eq!(
        order,
        vec![
            "start", "search", "taskview", "app:b", "app:a", "tray", "bin"
        ]
    );
}

#[test]
fn mirrored_layout_keeps_the_same_group_order() {
    let normal = apply_pinned_groups(&["bin", "app:x", "tray", "start"]);
    let mirrored = apply_pinned_groups(&["start", "tray", "app:x", "bin"]);
    assert_eq!(normal[0], "start");
    assert_eq!(mirrored[0], "start");
    assert_eq!(normal.last().unwrap(), "bin");
    assert_eq!(mirrored.last().unwrap(), "bin");
}
