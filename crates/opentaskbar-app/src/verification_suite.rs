//! Exhaustive Settings-Matrix, End-to-End Wiring & Frame-Time/FPS Verification Suite.
//!
//! Covers:
//! 1. Full `DockSettings` round-trip & validation matrix (`save_to` -> `try_load_from`).
//! 2. Concurrent save/read contention stress on Windows (`save_to` vs `try_load_from`).
//! 3. End-to-end live theme transformation across all 6 built-in themes, Windows Default,
//!    and a freshly created custom theme across Dark & Light modes, `SpatialGlassConfig`,
//!    `UiTheme`, and `preview_swatches`.
//! 4. All built-in catalog widgets + `custom:sample` across chip specs, island detail specs,
//!    canonical placement/ordering/moving, Dynamic Island Strip packing, and viewport popover
//!    placement sweeps.
//! 5. Settings search catalog integrity (no duplicates, valid target rows, localized strings,
//!    zero unformatted `{0}` placeholders).
//! 6. Real microsecond frame-time/performance benchmark guard for 240 FPS hot paths.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use opentaskbar_core::dynamic_island as island_policy;
use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::{
    AiUsageRing, AppTheme, AutoHideMode, CATALOG_WIDGET_COUNT, ClockDateFormat, ClockDateLayout,
    ClockDateStyle, DYNAMIC_ISLAND_ALL_DISPLAYS, DockContentAlignment, DockEdge, DockGapUnit,
    DockHeightMode, DockPosition, DockSettings, DockSize, DockWidthMode,
    DynamicIslandClosedDisplay, DynamicIslandIdleMode, DynamicIslandLayout, DynamicIslandStyle,
    DynamicIslandVisibilityMode, FisheyeIntensity, MiddleClickAction, SecondaryAppScope,
    StartButtonAction, StartButtonPlacement, SurfaceBackground, TemperatureUnit, WidgetDensityMode,
    WidgetFlyoutTrigger, canonical_widget_setting_id, save_to, try_load_from,
};
use serde_json::{Value, json};

use crate::design;
use crate::design::spatial_glass::SpatialGlassConfig;
use crate::design::theme::{self, ThemeManager};
use crate::design::tokens::TokenMap;
use crate::design::widget_pack::WidgetPackManifest;
use crate::dock;
use crate::dock::dynamic_island::{
    self, group_content_width, island_detail_layout, island_header_layout,
    island_width_for_content, pack_strip_pages, strip_max_width_dips,
};
use crate::platform::theme_packs::{
    self, ThemePack, ThemePackManifest, preview_swatches, save_custom_theme, widgets_override,
};
use crate::ui::components::{UiTheme, compute_popover_placement};
use crate::ui::settings_search;
use crate::widgets::{self, WidgetRuntime, registry};
use opentaskbar_core::dynamic_island::geometry::{COMPACT_HEIGHT_DIPS, EXPANDED_HEIGHT_DIPS};

const BUILTIN_THEMES: [&str; 6] = [
    "catppuccin",
    "gruvbox",
    "nord",
    "prism",
    "solarized",
    "tokyo-night",
];

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "opentaskbar-verify-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp verification dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn builtin_catalog_ids() -> Vec<&'static str> {
    registry::BUILTINS
        .iter()
        .filter(|w| w.catalog)
        .map(|w| w.id)
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Exhaustive Settings Round-Trip & Validation Matrix
// ---------------------------------------------------------------------------

#[test]
fn settings_matrix_round_trips_every_field_without_error() {
    let tmp = TempDirGuard::new("settings-matrix");
    let path = tmp.path().join("settings.json");

    let all_widgets = builtin_catalog_ids();
    assert_eq!(
        all_widgets.len(),
        CATALOG_WIDGET_COUNT,
        "expected exactly {CATALOG_WIDGET_COUNT} catalog widgets"
    );

    let surface_modes = [
        SurfaceBackground::Glass,
        SurfaceBackground::Solid,
        SurfaceBackground::None,
        SurfaceBackground::TrueBlack,
    ];
    let theme_ids = [
        "",
        "catppuccin",
        "gruvbox",
        "nord",
        "prism",
        "solarized",
        "tokyo-night",
        "Özel-Tema_İstanbul-2026",
    ];
    let border_colors = [None, Some("#38BDF8"), Some("#CC1E1E2E"), Some("#FF00AA55")];
    let island_styles = [DynamicIslandStyle::Notch, DynamicIslandStyle::Floating];
    let island_layouts = [DynamicIslandLayout::Paged, DynamicIslandLayout::Strip];
    let shrink_biases = [-10, 0, 10];
    let countdown_mins = [1u32, 15, 180];
    let ai_rings = [
        AiUsageRing::Highest,
        AiUsageRing::Session,
        AiUsageRing::Weekly,
    ];
    let anim_fps_opts = [None, Some(30u32), Some(60), Some(120)];
    let anim_fps_caps = [0i32, 30, 60, 120];
    let oled_thresholds = [0i32, 10, 20, 30, 50];
    let densities = [
        WidgetDensityMode::Auto,
        WidgetDensityMode::Compact,
        WidgetDensityMode::Full,
    ];

    // Sweep combinations so every enum variant, boundary, theme ID, border hex,
    // OLED care toggle, widget density, and all built-in widgets (enabled/disabled/ordered)
    // are exercised through `save_to` -> `try_load_from`.
    for i in 0..16 {
        let mut s = DockSettings {
            position: match i % 3 {
                0 => DockPosition::Left,
                1 => DockPosition::Center,
                _ => DockPosition::Right,
            },
            edge: match i % 3 {
                0 => DockEdge::Bottom,
                1 => DockEdge::Left,
                _ => DockEdge::Right,
            },
            size: match i % 3 {
                0 => DockSize::Compact,
                1 => DockSize::Medium,
                _ => DockSize::Large,
            },
            theme: match i % 3 {
                0 => AppTheme::Dark,
                1 => AppTheme::Light,
                _ => AppTheme::System,
            },
            auto_hide: match i % 3 {
                0 => AutoHideMode::Disabled,
                1 => AutoHideMode::Smart,
                _ => AutoHideMode::Always,
            },
            // All 4 SurfaceBackground modes rotated across all 5 surfaces.
            dock_background: surface_modes[i % surface_modes.len()],
            menu_background: surface_modes[(i + 1) % surface_modes.len()],
            widget_background: surface_modes[(i + 2) % surface_modes.len()],
            settings_background: surface_modes[(i + 3) % surface_modes.len()],
            dynamic_island_background: surface_modes[(i + 1) % surface_modes.len()],
            dock_opacity: 0.25 + (i % 4) as f64 * 0.25,
            menu_opacity: 0.4 + (i % 3) as f64 * 0.25,
            widget_opacity: 0.35 + (i % 3) as f64 * 0.3,
            settings_opacity: 0.65 + (i % 4) as f64 * 0.1,
            surface_opacity: 0.5 + (i % 2) as f64 * 0.4,
            dynamic_island_opacity: 0.3 + (i % 4) as f64 * 0.2,
            ..DockSettings::default()
        };

        // #RRGGBB and #AARRGGBB border colors + valid thicknesses.
        let bc_a = border_colors[i % border_colors.len()].map(str::to_owned);
        let bc_b = border_colors[(i + 1) % border_colors.len()].map(str::to_owned);
        let bc_c = border_colors[(i + 2) % border_colors.len()].map(str::to_owned);
        let bc_d = border_colors[(i + 3) % border_colors.len()].map(str::to_owned);
        s.dock_border_color = bc_a.clone();
        s.menu_border_color = bc_b.clone();
        s.widget_border_color = bc_c.clone();
        s.settings_border_color = bc_d.clone();
        s.dock_border_thickness = (i % 5) as f64 * 0.5;
        s.menu_border_thickness = 1.0 + (i % 3) as f64;
        s.widget_border_thickness = 0.5 + (i % 4) as f64 * 0.5;
        s.settings_border_thickness = 1.0 + (i % 2) as f64;

        // Theme packs: Windows Default, all 6 built-in themes, and Turkish/Unicode custom ID.
        s.theme_pack = theme_ids[i % theme_ids.len()].to_owned();
        s.theme_application_icons = i % 2 == 0;

        // Dynamic Island style, layout, focus widgets, and background color.
        let style = island_styles[i % island_styles.len()];
        let layout = island_layouts[(i / 2) % island_layouts.len()];
        let show_focus_widgets = i % 2 == 0;
        s.set_dynamic_island_style(style);
        s.set_dynamic_island_layout(layout);
        s.set_dynamic_island_show_focus_widgets(show_focus_widgets);
        s.dynamic_island_visibility = if i % 2 == 0 {
            DynamicIslandVisibilityMode::Always
        } else {
            DynamicIslandVisibilityMode::DesktopOnly
        };
        s.dynamic_island_enable = i % 3 != 0;
        s.dynamic_island_auto_hide = i % 2 == 1;
        s.dynamic_island_hide_in_fullscreen = i % 2 == 0;
        s.dynamic_island_use_theme = i % 2 == 0;
        s.dynamic_island_background_color = if i % 2 == 0 {
            Some("#1A2B3C".to_owned())
        } else {
            None
        };

        // Shrink priority bias (-10, 0, 10) and countdown default minutes (1, 15, 180).
        let bias = shrink_biases[i % shrink_biases.len()];
        let countdown = countdown_mins[i % countdown_mins.len()];
        s.set_shrink_priority_bias(bias);
        s.set_countdown_default_minutes(countdown);

        // AI Usage Ring + per-provider overrides.
        let ring = ai_rings[i % ai_rings.len()];
        s.ai_usage_ring = ring;
        let claude_override = ai_rings[(i + 1) % ai_rings.len()];
        let cursor_override = ai_rings[(i + 2) % ai_rings.len()];
        s.extra.insert(
            "AiUsageRingOverrides".to_owned(),
            json!({
                "AiUsageClaude": format!("{claude_override:?}"),
                "AiUsageCursor": format!("{cursor_override:?}"),
            }),
        );

        // Animation FPS + cap + motion settings.
        s.animations_enabled = i % 4 != 0;
        s.animation_fps = anim_fps_opts[i % anim_fps_opts.len()];
        s.animation_fps_cap = anim_fps_caps[i % anim_fps_caps.len()];
        s.magnification_enabled = i % 2 == 0;
        s.magnification_expands_dock = i % 2 == 1;
        s.fisheye_intensity = match i % 3 {
            0 => FisheyeIntensity::Low,
            1 => FisheyeIntensity::Normal,
            _ => FisheyeIntensity::Strong,
        };
        s.middle_click_action = match i % 3 {
            0 => MiddleClickAction::NewWindow,
            1 => MiddleClickAction::CloseWindow,
            _ => MiddleClickAction::Nothing,
        };

        // OLED Display Care toggles & ranges.
        s.oled_reduce_glow_enabled = i % 2 == 1;
        s.burn_in_shift_enabled = i % 3 == 1;
        s.burn_in_shift_minutes = 5 + ((i as i32 * 17) % 116);
        s.oled_auto_trigger_enabled = i % 2 == 0;
        s.oled_low_battery_threshold = oled_thresholds[i % oled_thresholds.len()];
        s.oled_auto_on_night_light = i % 3 == 2;
        s.ambient_mode_enabled = i % 2 == 1;

        // Extra feature toggles: WindowPickerEnabled, WeatherCompactMode, StorageCompactMode.
        let picker_enabled = i % 2;
        let weather_compact = i % 2 == 1;
        let storage_compact = i % 3 == 0;
        s.extra
            .insert("WindowPickerEnabled".to_owned(), json!(picker_enabled));
        s.extra
            .insert("WeatherCompactMode".to_owned(), json!(weather_compact));
        s.extra
            .insert("StorageCompactMode".to_owned(), json!(storage_compact));

        // Additional layout/clock/weather/display fields.
        s.dock_corner_radius = Some((i as i32 * 3) % 49);
        s.dock_bottom_gap_pixels = (i as i32 * 5) % 40;
        s.dock_icon_padding = (i as i32) % 13;
        s.dock_scale = 0.75 + (i % 4) as f64 * 0.25;
        s.dock_width_mode = match i % 4 {
            0 => DockWidthMode::Auto,
            1 => DockWidthMode::Pixels,
            2 => DockWidthMode::Percent,
            _ => DockWidthMode::Fill,
        };
        s.dock_height_mode = match i % 4 {
            0 => DockHeightMode::Auto,
            1 => DockHeightMode::Pixels,
            2 => DockHeightMode::Percent,
            _ => DockHeightMode::Fill,
        };
        s.dock_top_gap_unit = if i % 2 == 0 {
            DockGapUnit::Pixels
        } else {
            DockGapUnit::Percent
        };
        s.dock_bottom_gap_unit = if i % 2 == 1 {
            DockGapUnit::Pixels
        } else {
            DockGapUnit::Percent
        };
        s.content_alignment = match i % 3 {
            0 => DockContentAlignment::Left,
            1 => DockContentAlignment::Center,
            _ => DockContentAlignment::Right,
        };
        s.start_action = if i % 2 == 0 {
            StartButtonAction::WindowsStart
        } else {
            StartButtonAction::Launcher
        };
        s.start_placement = match i % 4 {
            0 => StartButtonPlacement::Start,
            1 => StartButtonPlacement::End,
            2 => StartButtonPlacement::Hidden,
            _ => StartButtonPlacement::InWidgets,
        };
        s.secondary_apps = if i % 2 == 0 {
            SecondaryAppScope::CurrentMonitorOnly
        } else {
            SecondaryAppScope::AllMonitors
        };
        s.clock_24_hour = i % 2 == 0;
        s.clock_show_seconds = i % 2 == 1;
        s.date_style = match i % 4 {
            0 => ClockDateStyle::DayAndShortDate,
            1 => ClockDateStyle::ShortDateOnly,
            2 => ClockDateStyle::NumericDate,
            _ => ClockDateStyle::Hidden,
        };
        s.date_format = match i % 6 {
            0 => ClockDateFormat::System,
            1 => ClockDateFormat::DdMmYyyy,
            2 => ClockDateFormat::DdMmYy,
            3 => ClockDateFormat::MmDdYyyy,
            4 => ClockDateFormat::YyyyMmDd,
            _ => ClockDateFormat::DdMmYyyyDots,
        };
        s.clock_date_layout = if i % 2 == 0 {
            ClockDateLayout::Beside
        } else {
            ClockDateLayout::Below
        };
        s.weather_unit = if i % 2 == 0 {
            TemperatureUnit::Celsius
        } else {
            TemperatureUnit::Fahrenheit
        };
        s.weather_city = "İstanbul".to_owned();
        s.global_widget_density = densities[i % densities.len()];
        s.widget_flyout_trigger = if i % 2 == 0 {
            WidgetFlyoutTrigger::Click
        } else {
            WidgetFlyoutTrigger::Hover
        };
        s.glass_blur_sigma = (i % 10) as f64 * 3.5;
        s.glass_saturation = 0.8 + (i % 5) as f64 * 0.25;
        s.glass_edge_glow = (i % 5) as f64 * 0.25;
        s.glass_pointer_light = i % 2 == 0;

        // N4 Dynamic Island smart behavior & scale.
        let idle_modes = [
            DynamicIslandIdleMode::Full,
            DynamicIslandIdleMode::Minimal,
            DynamicIslandIdleMode::Closed,
        ];
        let closed_displays = [
            DynamicIslandClosedDisplay::MinimalLine,
            DynamicIslandClosedDisplay::Clock,
            DynamicIslandClosedDisplay::Music,
            DynamicIslandClosedDisplay::AiUsage,
            DynamicIslandClosedDisplay::Hidden,
        ];
        let idle_mode = idle_modes[i % idle_modes.len()];
        let closed_display = closed_displays[i % closed_displays.len()];
        let hover_expand = i % 2 == 1;
        let hover_delay_ms = 80 + ((i as u32 * 95) % 920);
        let auto_close_secs = (i as u32 * 9) % 121;
        let follow_mouse = i % 3 == 1;
        let island_scale = 0.80 + (i % 12) as f32 * 0.05;
        s.set_dynamic_island_idle_mode(idle_mode);
        s.set_dynamic_island_closed_display(closed_display);
        s.set_dynamic_island_hover_expand(hover_expand);
        s.set_dynamic_island_hover_delay_ms(hover_delay_ms);
        s.set_dynamic_island_auto_close_secs(auto_close_secs);
        s.set_dynamic_island_follow_mouse(follow_mouse);
        s.set_dynamic_island_scale(island_scale);

        // N1 System HUDs + O5 Windows Notifications.
        let hud_volume = i % 2 == 0;
        let hud_brightness = i % 3 != 0;
        let hud_capslock = i % 2 == 1;
        let hud_kbd_lang = i % 4 != 0;
        let hud_power = i % 3 == 0;
        let hud_network = i % 2 == 0;
        let hud_bluetooth = i % 5 != 0;
        let hud_duration_ms = 600 + ((i as u32 * 400) % 5401);
        let notif_enabled = i % 2 == 0;
        let notif_duration_ms = 1500 + ((i as u32 * 1000) % 10_501);
        let notif_respect_focus = i % 3 != 1;
        let notif_muted = if i % 2 == 0 {
            vec!["Teams".to_owned(), "Slack".to_owned()]
        } else {
            Vec::new()
        };
        s.set_hud_volume_enabled(hud_volume);
        s.set_hud_brightness_enabled(hud_brightness);
        s.set_hud_capslock_enabled(hud_capslock);
        s.set_hud_keyboard_lang_enabled(hud_kbd_lang);
        s.set_hud_power_enabled(hud_power);
        s.set_hud_network_enabled(hud_network);
        s.set_hud_bluetooth_enabled(hud_bluetooth);
        s.set_hud_duration_ms(hud_duration_ms);
        s.set_island_notifications_enabled(notif_enabled);
        s.set_island_notifications_duration_ms(notif_duration_ms);
        s.set_island_notifications_respect_focus(notif_respect_focus);
        s.set_island_notifications_muted_apps(notif_muted.clone());

        // N2 File Shelf + N3 Clipboard History + N5 Smart Tools + N6 Media + N7 Agents + N8 Todo.
        let shelf_enabled = i % 2 == 0;
        let shelf_max_items = 4 + (i * 3) % 29;
        let clip_enabled = i % 2 == 1;
        let clip_max_items = 5 + (i * 4) % 46;
        let clip_ignore_sensitive = i % 3 != 0;
        let ocr_enabled = i % 2 == 0;
        let quick_shot_enabled = i % 3 != 2;
        let media_waveform = i % 2 == 0;
        let media_lyrics = i % 2 == 1;
        let media_lyrics_pill = i % 3 != 1;
        let agent_monitor = i % 2 == 1;
        let agent_approval = i % 2 == 0;
        let todo_reminders = i % 3 != 0;
        let ical_url = if i % 2 == 0 {
            format!("https://calendar.example.test/feed-{i}.ics")
        } else {
            String::new()
        };
        let ical_poll_mins = 5 + ((i as u32 * 10) % 116);
        s.set_shelf_enabled(shelf_enabled);
        s.set_shelf_max_items(shelf_max_items);
        s.set_clipboard_history_enabled(clip_enabled);
        s.set_clipboard_max_items(clip_max_items);
        s.set_clipboard_ignore_sensitive(clip_ignore_sensitive);
        s.set_smart_ocr_enabled(ocr_enabled);
        s.set_quick_screenshot_enabled(quick_shot_enabled);
        s.set_media_waveform_enabled(media_waveform);
        s.set_media_lyrics_enabled(media_lyrics);
        s.set_media_lyrics_pill_enabled(media_lyrics_pill);
        s.set_agent_monitor_enabled(agent_monitor);
        s.set_agent_approval_banners(agent_approval);
        s.set_todo_reminders_enabled(todo_reminders);
        s.set_ical_feed_url(ical_url.clone());
        s.set_ical_poll_minutes(ical_poll_mins);

        // N9 Privacy, Quick Power, extra hotkeys + Wireless multi-brand protocols.
        let privacy_dots = i % 2 == 0;
        let quick_power = i % 3 != 0;
        let hk_island = if i % 2 == 0 {
            "Ctrl+Alt+I".to_owned()
        } else {
            String::new()
        };
        let hk_ocr = if i % 2 == 0 {
            "Ctrl+Shift+O".to_owned()
        } else {
            String::new()
        };
        let hk_shot = if i % 2 == 0 {
            "Ctrl+Shift+S".to_owned()
        } else {
            String::new()
        };
        let hk_clip = if i % 2 == 0 {
            "Ctrl+Shift+V".to_owned()
        } else {
            String::new()
        };
        let wireless_multi_brand = i % 2 == 0;
        let wireless_disabled_vendors = if i % 3 == 0 {
            vec!["Corsair".to_owned(), "Razer".to_owned()]
        } else {
            Vec::new()
        };
        s.set_privacy_indicators_enabled(privacy_dots);
        s.set_quick_power_actions_enabled(quick_power);
        s.set_island_toggle_hotkey(hk_island.clone());
        s.set_ocr_hotkey(hk_ocr.clone());
        s.set_screenshot_hotkey(hk_shot.clone());
        s.set_clipboard_hotkey(hk_clip.clone());
        s.set_wireless_multi_brand_enabled(wireless_multi_brand);
        s.set_wireless_disabled_vendors(wireless_disabled_vendors.clone());

        // Exercise ALL catalog widgets: full enable on even passes, selective toggle-off on odd passes,
        // custom ordering, density overrides, and Dynamic Island placements.
        let mut ordered_widgets: Vec<String> = all_widgets
            .iter()
            .map(|id| canonical_widget_setting_id(id))
            .collect();
        let rot = i % ordered_widgets.len();
        ordered_widgets.rotate_left(rot);
        s.widget_order = ordered_widgets.clone();

        let mut density_map = BTreeMap::new();
        for (w_idx, w_id) in all_widgets.iter().enumerate() {
            let enable = i % 2 == 0 || w_idx % 2 == 0;
            s.set_widget_enabled(w_id, enable);
            let canonical = canonical_widget_setting_id(w_id);
            density_map.insert(canonical.clone(), densities[(i + w_idx) % densities.len()]);
            if enable && w_idx < 6 {
                s.dynamic_island_placements
                    .insert(canonical, DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned());
            }
        }
        if i % 2 == 1 {
            // Also verify turning a previously enabled widget off keeps validation clean.
            s.set_widget_enabled("clock", false);
            s.set_widget_enabled("stats", false);
        }
        s.widget_density_overrides = density_map.clone();
        s.dynamic_island_widget_orders.insert(
            DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned(),
            ordered_widgets.iter().take(6).cloned().collect(),
        );

        let validation_errors = s.validate();
        assert!(
            validation_errors.is_empty(),
            "iteration {i} failed pre-save validation: {validation_errors:?}"
        );

        save_to(&path, &s).unwrap_or_else(|e| {
            panic!("iteration {i} save_to failed (theme={}): {e}", s.theme_pack)
        });
        let loaded = try_load_from(&path)
            .unwrap_or_else(|e| panic!("iteration {i} try_load_from failed: {e}"));
        assert!(
            loaded.validate().is_empty(),
            "iteration {i} loaded settings failed validate: {:?}",
            loaded.validate()
        );

        // Verify exact fidelity on every exercised field.
        assert_eq!(loaded.position, s.position);
        assert_eq!(loaded.edge, s.edge);
        assert_eq!(loaded.size, s.size);
        assert_eq!(loaded.theme, s.theme);
        assert_eq!(loaded.auto_hide, s.auto_hide);
        assert_eq!(loaded.dock_background, s.dock_background);
        assert_eq!(loaded.menu_background, s.menu_background);
        assert_eq!(loaded.widget_background, s.widget_background);
        assert_eq!(loaded.settings_background, s.settings_background);
        assert_eq!(
            loaded.dynamic_island_background,
            s.dynamic_island_background
        );
        assert_eq!(loaded.theme_pack, s.theme_pack);
        assert_eq!(loaded.theme_application_icons, s.theme_application_icons);
        assert_eq!(loaded.dock_border_color, bc_a);
        assert_eq!(loaded.menu_border_color, bc_b);
        assert_eq!(loaded.widget_border_color, bc_c);
        assert_eq!(loaded.settings_border_color, bc_d);
        assert_eq!(loaded.dynamic_island_style(), style);
        assert_eq!(loaded.dynamic_island_layout(), layout);
        assert_eq!(
            loaded.dynamic_island_show_focus_widgets(),
            show_focus_widgets
        );
        assert_eq!(
            loaded.dynamic_island_background_color,
            s.dynamic_island_background_color
        );
        assert_eq!(
            loaded.dynamic_island_background_hex(),
            s.dynamic_island_background_color.as_deref()
        );
        assert_eq!(loaded.shrink_priority_bias(), bias);
        assert_eq!(loaded.countdown_default_minutes(), countdown);
        assert_eq!(loaded.ai_usage_ring, ring);
        assert_eq!(
            widgets::resolved_ai_usage_ring(&loaded, "aiusage_claude"),
            claude_override
        );
        assert_eq!(
            widgets::resolved_ai_usage_ring(&loaded, "aiusage_cursor"),
            cursor_override
        );
        assert_eq!(
            widgets::resolved_ai_usage_ring(&loaded, "aiusage_codex"),
            ring
        );
        assert_eq!(loaded.animation_fps, s.animation_fps);
        assert_eq!(loaded.animation_fps_cap, s.animation_fps_cap);
        assert_eq!(loaded.oled_reduce_glow_enabled, s.oled_reduce_glow_enabled);
        assert_eq!(loaded.burn_in_shift_enabled, s.burn_in_shift_enabled);
        assert_eq!(loaded.burn_in_shift_minutes, s.burn_in_shift_minutes);
        assert_eq!(
            loaded.oled_auto_trigger_enabled,
            s.oled_auto_trigger_enabled
        );
        assert_eq!(
            loaded.oled_low_battery_threshold,
            s.oled_low_battery_threshold
        );
        assert_eq!(loaded.oled_auto_on_night_light, s.oled_auto_on_night_light);
        assert_eq!(loaded.ambient_mode_enabled, s.ambient_mode_enabled);
        assert_eq!(
            loaded
                .extra
                .get("WindowPickerEnabled")
                .and_then(Value::as_u64),
            Some(picker_enabled as u64)
        );
        assert_eq!(
            loaded
                .extra
                .get("WeatherCompactMode")
                .and_then(Value::as_bool),
            Some(weather_compact)
        );
        assert_eq!(
            loaded
                .extra
                .get("StorageCompactMode")
                .and_then(Value::as_bool),
            Some(storage_compact)
        );
        assert_eq!(loaded.dynamic_island_idle_mode(), idle_mode);
        assert_eq!(loaded.dynamic_island_closed_display(), closed_display);
        assert_eq!(loaded.dynamic_island_hover_expand(), hover_expand);
        assert_eq!(loaded.dynamic_island_hover_delay_ms(), hover_delay_ms);
        assert_eq!(loaded.dynamic_island_auto_close_secs(), auto_close_secs);
        assert_eq!(loaded.dynamic_island_follow_mouse(), follow_mouse);
        assert!(
            (loaded.dynamic_island_scale() - island_scale).abs() < 0.001,
            "dynamic_island_scale diverged: {} vs {}",
            loaded.dynamic_island_scale(),
            island_scale
        );
        assert_eq!(loaded.hud_volume_enabled(), hud_volume);
        assert_eq!(loaded.hud_brightness_enabled(), hud_brightness);
        assert_eq!(loaded.hud_capslock_enabled(), hud_capslock);
        assert_eq!(loaded.hud_keyboard_lang_enabled(), hud_kbd_lang);
        assert_eq!(loaded.hud_power_enabled(), hud_power);
        assert_eq!(loaded.hud_network_enabled(), hud_network);
        assert_eq!(loaded.hud_bluetooth_enabled(), hud_bluetooth);
        assert_eq!(loaded.hud_duration_ms(), hud_duration_ms);
        assert_eq!(loaded.island_notifications_enabled(), notif_enabled);
        assert_eq!(loaded.island_notifications_duration_ms(), notif_duration_ms);
        assert_eq!(
            loaded.island_notifications_respect_focus(),
            notif_respect_focus
        );
        assert_eq!(loaded.island_notifications_muted_apps(), notif_muted);
        assert_eq!(loaded.shelf_enabled(), shelf_enabled);
        assert_eq!(loaded.shelf_max_items(), shelf_max_items);
        assert_eq!(loaded.clipboard_history_enabled(), clip_enabled);
        assert_eq!(loaded.clipboard_max_items(), clip_max_items);
        assert_eq!(loaded.clipboard_ignore_sensitive(), clip_ignore_sensitive);
        assert_eq!(loaded.smart_ocr_enabled(), ocr_enabled);
        assert_eq!(loaded.quick_screenshot_enabled(), quick_shot_enabled);
        assert_eq!(loaded.media_waveform_enabled(), media_waveform);
        assert_eq!(loaded.media_lyrics_enabled(), media_lyrics);
        assert_eq!(loaded.media_lyrics_pill_enabled(), media_lyrics_pill);
        assert_eq!(loaded.agent_monitor_enabled(), agent_monitor);
        assert_eq!(loaded.agent_approval_banners(), agent_approval);
        assert_eq!(loaded.todo_reminders_enabled(), todo_reminders);
        assert_eq!(loaded.ical_feed_url(), ical_url);
        assert_eq!(loaded.ical_poll_minutes(), ical_poll_mins);
        assert_eq!(loaded.privacy_indicators_enabled(), privacy_dots);
        assert_eq!(loaded.quick_power_actions_enabled(), quick_power);
        assert_eq!(loaded.island_toggle_hotkey(), hk_island);
        assert_eq!(loaded.ocr_hotkey(), hk_ocr);
        assert_eq!(loaded.screenshot_hotkey(), hk_shot);
        assert_eq!(loaded.clipboard_hotkey(), hk_clip);
        assert!(
            loaded.detect_hotkey_conflicts().is_empty(),
            "non-colliding hotkeys must not report conflicts: {:?}",
            loaded.detect_hotkey_conflicts()
        );
        assert_eq!(loaded.wireless_multi_brand_enabled(), wireless_multi_brand);
        assert_eq!(
            loaded.wireless_disabled_vendors(),
            wireless_disabled_vendors
        );
        assert_eq!(loaded.widget_density_overrides, density_map);
        assert_eq!(loaded.widget_order, ordered_widgets);
        assert_eq!(loaded.enabled_widgets.len(), s.enabled_widgets.len());
        for w_id in &all_widgets {
            assert_eq!(
                loaded.is_widget_enabled(w_id),
                s.is_widget_enabled(w_id),
                "widget {w_id} enabled state diverged on iteration {i}"
            );
        }
    }

    // Verify real-time multi-hotkey conflict detection catches colliding shortcuts.
    let mut conflict_settings = DockSettings::default();
    conflict_settings.set_ocr_hotkey("Alt+Space".to_owned());
    conflict_settings.set_clipboard_hotkey("Ctrl+Shift+V".to_owned());
    conflict_settings.set_screenshot_hotkey("Ctrl+Shift+V".to_owned());
    let conflicts = conflict_settings.detect_hotkey_conflicts();
    assert!(
        conflicts
            .iter()
            .any(|(a, b)| a == "LauncherHotkey" && b == "OcrHotkey"),
        "expected LauncherHotkey/OcrHotkey conflict, got {conflicts:?}"
    );
    assert!(
        conflicts
            .iter()
            .any(|(a, b)| a == "ScreenshotHotkey" && b == "ClipboardHotkey"),
        "expected ScreenshotHotkey/ClipboardHotkey conflict, got {conflicts:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Concurrent Save/Read Contention Stress
// ---------------------------------------------------------------------------

#[test]
fn concurrent_save_and_read_never_fails() {
    let tmp = TempDirGuard::new("concurrent-io");
    let path = tmp.path().join("settings.json");

    // Seed initial valid file before starting contention.
    let initial = DockSettings::default();
    save_to(&path, &initial).expect("initial seed save_to must succeed");

    let stop = Arc::new(AtomicBool::new(false));
    let read_ok = Arc::new(AtomicUsize::new(0));
    let read_errors = Arc::new(AtomicUsize::new(0));
    let reader_path = path.clone();
    let reader_stop = Arc::clone(&stop);
    let reader_ok = Arc::clone(&read_ok);
    let reader_errs = Arc::clone(&read_errors);

    let reader = thread::spawn(move || {
        let mut first_error: Option<String> = None;
        while !reader_stop.load(Ordering::Acquire) {
            match try_load_from(&reader_path) {
                Ok(loaded) => {
                    if !loaded.validate().is_empty() {
                        reader_errs.fetch_add(1, Ordering::Relaxed);
                        if first_error.is_none() {
                            first_error =
                                Some(format!("loaded invalid settings: {:?}", loaded.validate()));
                        }
                    } else {
                        reader_ok.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(err) => {
                    reader_errs.fetch_add(1, Ordering::Relaxed);
                    if first_error.is_none() {
                        first_error = Some(format!("{err:?}"));
                    }
                }
            }
        }
        first_error
    });

    let modes = [
        SurfaceBackground::Glass,
        SurfaceBackground::Solid,
        SurfaceBackground::None,
        SurfaceBackground::TrueBlack,
    ];
    let mut write_errors = Vec::new();
    for step in 0..80usize {
        let mut s = DockSettings {
            dock_background: modes[step % modes.len()],
            theme_pack: BUILTIN_THEMES[step % BUILTIN_THEMES.len()].to_owned(),
            dock_opacity: 0.2 + (step % 8) as f64 * 0.1,
            ..DockSettings::default()
        };
        s.set_shrink_priority_bias((step as i32 % 21) - 10);
        s.set_countdown_default_minutes(1 + (step as u32 % 180));
        s.set_widget_enabled("clock", step % 2 == 0);
        s.set_widget_enabled("stats", step % 3 == 0);
        s.set_widget_enabled("weather", step % 4 == 0);
        if let Err(err) = save_to(&path, &s) {
            write_errors.push(format!("step {step}: {err}"));
        }
    }

    stop.store(true, Ordering::Release);
    let first_read_error = reader.join().expect("reader thread must not panic");

    assert!(
        write_errors.is_empty(),
        "expected 0 write errors during 80 rapid concurrent saves, got: {write_errors:?}"
    );
    assert_eq!(
        read_errors.load(Ordering::Relaxed),
        0,
        "expected 0 corrupted/failed reads during concurrent saves (first error: {first_read_error:?})"
    );
    assert!(
        read_ok.load(Ordering::Relaxed) > 0,
        "reader thread must complete at least one valid read"
    );
}

// ---------------------------------------------------------------------------
// 3. End-to-End Live Theme Transformation Matrix
// ---------------------------------------------------------------------------

#[test]
fn all_six_builtin_and_custom_themes_transform_all_surfaces() {
    let _guard = theme::theme_test_guard();
    let _ = theme_packs::install_bundled();

    let tmp = TempDirGuard::new("custom-theme-e2e");
    let custom_root = tmp.path().join("Themes");

    // Create a freshly authored custom theme in a temp dir with distinct Dark & Light tokens.
    let custom_manifest = ThemePackManifest {
        name: Some("Aurora Verify Pack".to_owned()),
        author: Some("Verification Suite".to_owned()),
        description: Some("End-to-end live theme transformation test pack".to_owned()),
        preserve_original_icons: true,
    };
    let mut custom_widgets = WidgetPackManifest {
        name: Some("Aurora Verify Widgets".to_owned()),
        ..WidgetPackManifest::default()
    };
    custom_widgets
        .dark
        .insert("DockMaterial".to_owned(), Value::from("#FF1B1026"));
    custom_widgets
        .dark
        .insert("DockBorderMaterial".to_owned(), Value::from("#FFEA4C89"));
    custom_widgets
        .dark
        .insert("AccentBrush".to_owned(), Value::from("#FFFF5C8A"));
    custom_widgets
        .dark
        .insert("SettingsBackground".to_owned(), Value::from("#FF140B1D"));
    custom_widgets
        .dark
        .insert("SettingsControl".to_owned(), Value::from("#FF261636"));
    custom_widgets
        .dark
        .insert("TextPrimary".to_owned(), Value::from("#FFFCEEF5"));

    custom_widgets
        .light
        .insert("DockMaterial".to_owned(), Value::from("#FFFBEBF2"));
    custom_widgets
        .light
        .insert("DockBorderMaterial".to_owned(), Value::from("#FFD63384"));
    custom_widgets
        .light
        .insert("AccentBrush".to_owned(), Value::from("#FFC2185B"));
    custom_widgets
        .light
        .insert("SettingsBackground".to_owned(), Value::from("#FFFFF5F9"));
    custom_widgets
        .light
        .insert("SettingsControl".to_owned(), Value::from("#FFF7DCE8"));
    custom_widgets
        .light
        .insert("TextPrimary".to_owned(), Value::from("#FF2A0F1E"));

    let custom_pack = save_custom_theme(
        &custom_root,
        Some("Aurora-Verify-Özel"),
        &custom_manifest,
        &custom_widgets,
    )
    .expect("save_custom_theme in temp dir must succeed");
    assert_eq!(custom_pack.id, "Aurora-Verify-Özel");

    for (app_theme, is_dark) in [(AppTheme::Dark, true), (AppTheme::Light, false)] {
        // Baseline: Windows Default (empty theme_pack).
        let default_settings = DockSettings {
            theme: app_theme,
            theme_pack: String::new(),
            dock_background: SurfaceBackground::Glass,
            settings_background: SurfaceBackground::Glass,
            ..DockSettings::default()
        };
        theme::sync_from_settings(&default_settings);
        let stock_palette = design::active_palette(is_dark);
        assert!(
            !theme::has_active_theme_customization(),
            "Windows Default must not report active theme customization"
        );
        assert_eq!(
            theme::theme_surface_tint("DockMaterial", "DockBorderMaterial"),
            (None, None),
            "Windows Default must keep neutral Clear Crystal tint"
        );

        let stock_solid = UiTheme::for_settings_background(is_dark, SurfaceBackground::Solid);
        let stock_glass = UiTheme::for_settings_background(is_dark, SurfaceBackground::Glass);

        let mut seen_signatures = HashSet::new();

        // Verify ALL 6 built-in themes through `sync_from_settings`.
        for id in BUILTIN_THEMES {
            let themed_settings = DockSettings {
                theme: app_theme,
                theme_pack: id.to_owned(),
                dock_background: SurfaceBackground::Glass,
                settings_background: SurfaceBackground::Glass,
                ..DockSettings::default()
            };
            theme::sync_from_settings(&themed_settings);

            assert!(
                theme::has_active_theme_customization(),
                "{id} ({app_theme:?}) must activate theme customization"
            );

            let pal = design::active_palette(is_dark);
            assert!(
                pal.accent != stock_palette.accent
                    || pal.dock_pill != stock_palette.dock_pill
                    || pal.canvas != stock_palette.canvas,
                "{id} ({app_theme:?}) must visibly transform active_palette vs Windows Default"
            );
            let sig = (pal.accent, pal.dock_pill, pal.canvas, pal.surface);
            assert!(
                seen_signatures.insert(sig),
                "built-in theme {id} ({app_theme:?}) collided with another theme's palette: {sig:?}"
            );

            let (tint, rim) = theme::theme_surface_tint("DockMaterial", "DockBorderMaterial");
            assert!(
                tint.is_some() && rim.is_some(),
                "{id} ({app_theme:?}) must supply both glass tint and rim, got ({tint:?}, {rim:?})"
            );

            // SpatialGlassConfig: carries tint in Glass, disables shadows when top_flush = true,
            // and stays strictly disabled in TrueBlack.
            let dock_glass = SpatialGlassConfig::from_settings(
                &themed_settings,
                SurfaceBackground::Glass,
                0.9,
                is_dark,
                false,
            )
            .with_theme_tint(tint, rim);
            assert!(dock_glass.enabled);
            assert_eq!(dock_glass.tint_rgb, tint);
            assert_eq!(dock_glass.rim_rgb, rim);
            assert!(
                dock_glass.shadows_visible(),
                "dock glass (top_flush=false) must enable drop shadows"
            );

            let island_glass = SpatialGlassConfig::from_settings(
                &themed_settings,
                SurfaceBackground::Glass,
                0.9,
                is_dark,
                true,
            )
            .with_theme_tint(tint, rim);
            assert!(island_glass.enabled);
            assert!(island_glass.top_flush);
            assert_eq!(island_glass.tint_rgb, tint);
            assert_eq!(island_glass.rim_rgb, rim);
            assert!(
                !island_glass.shadows_visible(),
                "top_flush=true glass must disable drop shadows"
            );

            let true_black_glass = SpatialGlassConfig::from_settings(
                &themed_settings,
                SurfaceBackground::TrueBlack,
                0.9,
                is_dark,
                false,
            )
            .with_theme_tint(tint, rim);
            assert!(!true_black_glass.enabled);
            assert_eq!(true_black_glass.tint_rgb, None);
            assert_eq!(true_black_glass.rim_rgb, None);
            assert!(!true_black_glass.shadows_visible());

            // UiTheme reflects active theme in Solid & Glass while keeping TrueBlack strictly pure black.
            let ui_solid = UiTheme::for_settings_background(is_dark, SurfaceBackground::Solid);
            let ui_glass = UiTheme::for_settings_background(is_dark, SurfaceBackground::Glass);
            let ui_black = UiTheme::for_settings_background(is_dark, SurfaceBackground::TrueBlack);
            assert!(
                ui_solid.canvas_bg != stock_solid.canvas_bg
                    || ui_solid.accent != stock_solid.accent
                    || ui_solid.surface != stock_solid.surface,
                "{id} ({app_theme:?}) Solid UiTheme must reflect active theme"
            );
            assert!(
                ui_glass.surface != stock_glass.surface
                    || ui_glass.accent != stock_glass.accent
                    || ui_glass.border != stock_glass.border,
                "{id} ({app_theme:?}) Glass UiTheme must reflect active theme"
            );
            assert_eq!(
                ui_black.canvas_bg, 0x0000_00ff,
                "{id} TrueBlack canvas must remain strictly 0x0000_00ff"
            );
            assert_eq!(
                ui_black.sidebar_bg, 0x0000_00ff,
                "{id} TrueBlack sidebar must remain strictly 0x0000_00ff"
            );

            // Preview swatches from the bundled pack on disk.
            let pack = ThemePack::load(&Path::new(crate::resource!("themes")).join(id))
                .unwrap_or_else(|e| panic!("load built-in theme {id}: {e}"));
            let sw = preview_swatches(&pack, app_theme);
            assert!(sw.surface.a > 0 && sw.accent.a > 0 && sw.text.a > 0 && sw.border.a > 0);
        }
        assert_eq!(
            seen_signatures.len(),
            6,
            "all 6 built-in themes in {app_theme:?} must produce distinct palettes"
        );

        // Verify the freshly saved Custom Theme from temp dir.
        let reloaded_custom =
            ThemePack::load(&custom_pack.directory).expect("reload custom theme from temp dir");
        let custom_tokens = widgets_override(&reloaded_custom, app_theme);
        assert!(
            !custom_tokens.is_empty(),
            "custom theme must resolve widget tokens for {app_theme:?}"
        );
        ThemeManager::apply(app_theme, Some(&default_settings));
        theme::apply_pack_tokens_to_palette(&custom_tokens);
        theme::apply_token_overrides(TokenMap::new());

        let custom_pal = design::active_palette(is_dark);
        assert_ne!(
            custom_pal.accent, stock_palette.accent,
            "custom theme ({app_theme:?}) must override accent"
        );
        assert_ne!(
            custom_pal.dock_pill, stock_palette.dock_pill,
            "custom theme ({app_theme:?}) must override dock_pill"
        );
        assert_ne!(
            custom_pal.canvas, stock_palette.canvas,
            "custom theme ({app_theme:?}) must override canvas"
        );
        let (c_tint, c_rim) = theme::theme_surface_tint("DockMaterial", "DockBorderMaterial");
        assert!(c_tint.is_some() && c_rim.is_some());
        let custom_swatches = preview_swatches(&reloaded_custom, app_theme);
        assert!(
            custom_swatches.surface.a > 0
                && custom_swatches.card.a > 0
                && custom_swatches.accent.a > 0
                && custom_swatches.text.a > 0
                && custom_swatches.border.a > 0
        );
        let ui_black = UiTheme::for_settings_background(is_dark, SurfaceBackground::TrueBlack);
        assert_eq!(ui_black.canvas_bg, 0x0000_00ff);
    }

    // Restore clean Windows Default theme state so later tests start from stock defaults.
    theme::sync_from_settings(&DockSettings::default());
}

// ---------------------------------------------------------------------------
// 4. All Built-in Catalog Widgets + Dynamic Island + Viewport Popover Matrix
// ---------------------------------------------------------------------------

#[test]
fn all_22_widgets_island_and_popovers_are_clean_and_finite() {
    let builtin_ids = builtin_catalog_ids();
    assert_eq!(
        builtin_ids.len(),
        CATALOG_WIDGET_COUNT,
        "expected {CATALOG_WIDGET_COUNT} built-in catalog widgets"
    );

    let mut all_ids: Vec<&str> = builtin_ids.clone();
    all_ids.push("custom:sample");

    // 4a. Canonical placement matching via lowercase/catalog ID & PascalCase setting ID.
    let mut placements = HashMap::new();
    for &id in &all_ids {
        let canonical = canonical_widget_setting_id(id);
        assert!(
            island_policy::same_widget_id(id, &canonical),
            "{id} must match canonical {canonical}"
        );
        assert!(
            island_policy::same_widget_id(&canonical, id),
            "{canonical} must match catalog {id}"
        );
        placements.insert(canonical, DYNAMIC_ISLAND_ALL_DISPLAYS.to_owned());
    }
    for &id in &all_ids {
        assert!(
            island_policy::is_assigned_to_display(&placements, id, "DISPLAY1"),
            "{id} must match placement via catalog ID"
        );
        let canonical = canonical_widget_setting_id(id);
        assert!(
            island_policy::is_assigned_to_display(&placements, &canonical, "DISPLAY1"),
            "{canonical} must match placement via canonical ID"
        );
    }
    let available: Vec<String> = all_ids.iter().map(|s| (*s).to_owned()).collect();
    let reverse_canonical: Vec<String> = all_ids
        .iter()
        .rev()
        .map(|s| canonical_widget_setting_id(s))
        .collect();
    let ordered = island_policy::order_widgets(&available, Some(&reverse_canonical), None, &[]);
    assert_eq!(ordered.len(), all_ids.len());
    assert!(island_policy::same_widget_id(
        &ordered[0],
        all_ids[all_ids.len() - 1]
    ));

    let moved = island_policy::move_widget(&ordered, "SystemStats", 1);
    assert_eq!(moved.len(), all_ids.len());
    let moved_back = island_policy::move_widget(&moved, "stats", -1);
    assert_eq!(moved_back.len(), all_ids.len());

    // 4b. WidgetRuntime::chip_spec across all built-in widgets (+ custom:sample) × Auto/Compact/Full.
    let mut runtime = WidgetRuntime::default();
    let densities = [
        WidgetDensityMode::Auto,
        WidgetDensityMode::Compact,
        WidgetDensityMode::Full,
    ];
    for density in densities {
        let mut settings = DockSettings {
            global_widget_density: density,
            ..DockSettings::default()
        };
        for &id in &all_ids {
            settings
                .widget_density_overrides
                .insert(canonical_widget_setting_id(id), density);
            for is_dark in [true, false] {
                let _spec = runtime.chip_spec(id, &settings, is_dark);
                let _pascal_spec =
                    runtime.chip_spec(&canonical_widget_setting_id(id), &settings, is_dark);
            }
            let width = runtime.widget_width_dips_with_settings(id, &settings);
            assert!(
                width.is_finite() && width > 0.0,
                "widget {id} ({density:?}) returned invalid width {width}"
            );
            let pascal_width = runtime
                .widget_width_dips_with_settings(&canonical_widget_setting_id(id), &settings);
            assert!(
                pascal_width.is_finite() && pascal_width > 0.0,
                "widget {id} PascalCase ({density:?}) returned invalid width {pascal_width}"
            );
            assert!(
                (width - pascal_width).abs() < f32::EPSILON,
                "widget {id} width ({width}) must match PascalCase width ({pascal_width})"
            );
        }
    }

    // 4c. WidgetRuntime::island_detail_spec across all built-in widgets + custom:sample.
    let default_settings = DockSettings::default();
    for &id in &all_ids {
        let detail = runtime.island_detail_spec(id, &default_settings);
        assert!(
            !detail.title.trim().is_empty(),
            "island_detail_spec({id}) must have non-empty title"
        );
        if !id.starts_with("custom:") {
            assert!(
                !detail.title.starts_with("aiusage_"),
                "island_detail_spec({id}) leaked raw AI key in title: {}",
                detail.title
            );
            assert_ne!(
                detail.title, "stats",
                "island_detail_spec({id}) leaked raw 'stats' key"
            );
            assert_ne!(
                detail.title, "keyboard",
                "island_detail_spec({id}) leaked raw 'keyboard' key"
            );
        }
        assert!(
            !detail.headline.trim().is_empty(),
            "island_detail_spec({id}) must have non-empty headline"
        );
        for row in &detail.rows {
            if let Some(p) = row.progress {
                assert!(
                    p.is_finite() && (0.0..=1.0).contains(&p),
                    "island_detail_spec({id}) row '{}' progress out of bounds: {p}",
                    row.label
                );
            }
        }
    }

    // 4d. Dynamic Island Strip layout packing across 1, 2, 5, 10, CATALOG_WIDGET_COUNT widgets ×
    //     screen widths 1024..=3840 × DPIs 1.0..=2.0.
    let all_widths: Vec<f32> = builtin_ids
        .iter()
        .map(|id| runtime.widget_width_dips_with_settings(id, &default_settings))
        .collect();
    for count in [1usize, 2, 5, 10, CATALOG_WIDGET_COUNT] {
        let slice = &all_widths[..count];
        for screen_px in [1024.0_f32, 1366.0, 1920.0, 2560.0, 3840.0] {
            for dpi in [1.0_f32, 1.25, 1.5, 2.0] {
                let monitor_dips = screen_px / dpi;
                let max_strip_w = strip_max_width_dips(monitor_dips);
                assert!(
                    max_strip_w.is_finite() && max_strip_w > 0.0,
                    "strip_max_width_dips invalid for {screen_px}@{dpi}"
                );
                let side_overhead =
                    dynamic_island::island_side_cluster_dips(count > 1, true) * 2.0 + 40.0;
                let content_budget = (max_strip_w - side_overhead).max(160.0);
                let gap = 8.0_f32;
                let pages = pack_strip_pages(slice, content_budget, gap);
                assert!(!pages.is_empty(), "pack_strip_pages returned empty");

                let mut covered = Vec::with_capacity(count);
                for (page_idx, page) in pages.iter().enumerate() {
                    assert!(!page.is_empty(), "empty strip page at index {page_idx}");
                    let content_w = group_content_width(slice, page, gap);
                    assert!(content_w.is_finite() && content_w > 0.0);
                    if page.len() > 1 {
                        assert!(
                            content_w <= content_budget + 0.05,
                            "multi-widget strip page exceeded budget ({content_w} > {content_budget}) at {screen_px}@{dpi} count={count}"
                        );
                    }
                    let pill_w = island_width_for_content(content_w, pages.len() > 1, true);
                    assert!(pill_w.is_finite() && pill_w > 0.0);

                    let hdr = island_header_layout(
                        pill_w,
                        COMPACT_HEIGHT_DIPS,
                        pages.len(),
                        page_idx,
                        true,
                    );
                    assert!(hdr.content_left.is_finite() && hdr.content_right.is_finite());
                    assert!(hdr.content_right > hdr.content_left);
                    assert!(hdr.chip_h.is_finite() && hdr.chip_h > 0.0);

                    let det = island_detail_layout(
                        pill_w,
                        COMPACT_HEIGHT_DIPS,
                        EXPANDED_HEIGHT_DIPS,
                        3,
                        3,
                    );
                    assert!(det.panel.2 > det.panel.0 && det.panel.3 > det.panel.1);
                    assert_eq!(det.rows.len(), 3);
                    assert_eq!(det.buttons.len(), 3);

                    covered.extend_from_slice(page);
                }
                let expected: Vec<usize> = (0..count).collect();
                assert_eq!(
                    covered, expected,
                    "strip packing must cover every widget 0..{count} in order without loss"
                );
            }
        }
    }

    // 4e. Viewport Popover Placement Sweep across trigger_top 40..=700 (step 10)
    //     on viewports 740px, 620px, 520px.
    let gap = 4.0_f32;
    let margin = 8.0_f32;
    let top_clearance = 36.0_f32;
    let trigger_h = 30.0_f32;
    let vp_w = 900.0_f32;
    for vp_h in [740.0_f32, 620.0, 520.0] {
        for (menu_w, menu_h) in [(180.0_f32, 160.0_f32), (240.0, 220.0), (260.0, 240.0)] {
            for trigger_right in [80.0_f32, 450.0, 880.0, 960.0] {
                let mut trigger_top = 40.0_f32;
                while trigger_top <= 700.0 {
                    let (x, y, flipped_up) = compute_popover_placement(
                        trigger_top,
                        trigger_right,
                        trigger_h,
                        menu_w,
                        menu_h,
                        vp_w,
                        vp_h,
                        gap,
                        margin,
                        top_clearance,
                    );
                    assert!(x.is_finite() && y.is_finite());
                    // Never enters the 36px titlebar (+ margin).
                    assert!(
                        y >= top_clearance + margin - 0.01,
                        "popover entered titlebar: y={y} < {} (vp_h={vp_h}, trigger_top={trigger_top})",
                        top_clearance + margin
                    );
                    // Never clips below viewport bottom when menu fits inside viewport.
                    if top_clearance + margin + menu_h + margin <= vp_h {
                        assert!(
                            y + menu_h <= vp_h - margin + 0.01,
                            "popover clipped below viewport: y={y}, menu_h={menu_h}, vp_h={vp_h}, trigger_top={trigger_top}"
                        );
                    }
                    // Never overlaps trigger when flipped up and space above fits the menu.
                    let space_above = (trigger_top - top_clearance - margin).max(0.0);
                    if flipped_up && space_above >= menu_h + gap {
                        assert!(
                            y + menu_h <= trigger_top - gap + 0.01,
                            "flipped-up popover overlapped trigger: y={y}, menu_h={menu_h}, trigger_top={trigger_top}"
                        );
                    }
                    // Never exits left/right viewport bounds.
                    assert!(
                        x >= margin - 0.01,
                        "popover exited left bound: x={x}, vp_w={vp_w}, trigger_right={trigger_right}"
                    );
                    assert!(
                        x + menu_w <= vp_w - margin + 0.01,
                        "popover exited right bound: x={x}, menu_w={menu_w}, vp_w={vp_w}, trigger_right={trigger_right}"
                    );
                    trigger_top += 10.0;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Search Catalog Integrity Check
// ---------------------------------------------------------------------------

#[test]
fn every_search_entry_maps_to_real_target_row() {
    let settings_src = include_str!("ui/settings.rs");
    let mut rendered_targets: HashSet<String> = HashSet::new();

    for marker in [
        "self.target_row(",
        "self.target_row_desc(",
        "self.mark_target(",
    ] {
        for (offset, _) in settings_src.match_indices(marker) {
            let after = &settings_src[offset + marker.len()..];
            if let Some(first_arg) = after.split(',').next()
                && let Some(id) = first_arg
                    .trim()
                    .strip_prefix('"')
                    .and_then(|s| s.split('"').next())
            {
                rendered_targets.insert(id.to_owned());
            }
        }
    }
    // Helper calls `self.surface_row(t, cx, elem_id, "SurfaceX", ...)` and
    // `self.surface_opacity_row(t, cx, "SurfaceOpacityLabel:X", ...)` delegate to `self.target_row`.
    for (marker, arg_index) in [
        ("self.surface_row(", 3usize),
        ("self.surface_opacity_row(", 2usize),
    ] {
        for (offset, _) in settings_src.match_indices(marker) {
            let after = &settings_src[offset + marker.len()..];
            if let Some(arg) = after.split(',').nth(arg_index)
                && let Some(id) = arg
                    .trim()
                    .strip_prefix('"')
                    .and_then(|s| s.split('"').next())
            {
                rendered_targets.insert(id.to_owned());
            }
        }
    }
    // Widget catalog cards are rendered dynamically with `widget-{id}`.
    for w in widgets::catalog_widgets() {
        rendered_targets.insert(format!("widget-{}", w.id));
    }

    let prev_lang = loc::language();
    for lang in ["en-US", "tr-TR"] {
        assert!(loc::set_language(lang), "must switch to {lang}");
        let entries = settings_search::catalog();
        assert!(!entries.is_empty(), "search catalog must not be empty");

        let mut seen_row_ids = HashSet::new();
        for entry in &entries {
            assert!(
                !entry.row_id.trim().is_empty(),
                "search entry has empty row_id"
            );
            assert!(
                seen_row_ids.insert(entry.row_id.clone()),
                "duplicate search catalog row_id '{}' in locale {lang}",
                entry.row_id
            );
            assert!(
                rendered_targets.contains(&entry.row_id),
                "search catalog row_id '{}' does not match any rendered target row in ui/settings.rs",
                entry.row_id
            );
            assert!(
                !entry.title.trim().is_empty(),
                "empty title for row_id '{}' in {lang}",
                entry.row_id
            );
            assert!(
                !entry.section.trim().is_empty(),
                "empty section for row_id '{}' in {lang}",
                entry.row_id
            );
            assert!(
                !entry.keywords.trim().is_empty(),
                "empty keywords for row_id '{}' in {lang}",
                entry.row_id
            );
            assert!(
                !entry.title.contains("{0}") && !entry.title.contains("{1}"),
                "unformatted placeholder in title '{}' for '{}' ({lang})",
                entry.title,
                entry.row_id
            );
            assert!(
                !entry.section.contains("{0}") && !entry.section.contains("{1}"),
                "unformatted placeholder in section '{}' for '{}' ({lang})",
                entry.section,
                entry.row_id
            );
        }
    }
    let _ = loc::set_language(prev_lang);
}

// ---------------------------------------------------------------------------
// 6. Real Frame-Time / Performance Benchmark Guard (240 FPS = 4,167 µs budget)
// ---------------------------------------------------------------------------

#[test]
fn hot_paths_stay_well_within_240fps_frame_budget() {
    let _guard = theme::theme_test_guard();
    let _ = theme_packs::install_bundled();

    let mut settings = DockSettings {
        theme: AppTheme::Dark,
        theme_pack: "catppuccin".to_owned(),
        dock_background: SurfaceBackground::Glass,
        settings_background: SurfaceBackground::Glass,
        animation_fps: Some(120),
        animation_fps_cap: 60,
        ..DockSettings::default()
    };
    settings.set_shrink_priority_bias(2);
    theme::sync_from_settings(&settings);

    let builtin_ids = builtin_catalog_ids();
    assert_eq!(builtin_ids.len(), CATALOG_WIDGET_COUNT);
    let mut runtime = WidgetRuntime::default();
    let search_entries = settings_search::catalog();
    let shrink_inputs: Vec<(&'static str, f32)> = builtin_ids
        .iter()
        .map(|&id| (id, runtime.widget_width_dips_with_settings(id, &settings)))
        .collect();

    // Warm up one-time lazy caches (drives, AI auth probe cache, audio detail cache, i18n).
    runtime.poll_snapshot.refresh();
    for &id in &builtin_ids {
        let _ = runtime.chip_spec(id, &settings, true);
        let _ = runtime.island_detail_spec(id, &settings);
    }

    const ITERS: u32 = 200;

    // 6a. Theme resolution + surface tint + GPUI UiTheme.
    let t0 = Instant::now();
    for _ in 0..ITERS {
        let pal = design::active_palette(true);
        let (tint, rim) = theme::theme_surface_tint("DockMaterial", "DockBorderMaterial");
        let ui = UiTheme::for_settings_background(true, SurfaceBackground::Glass);
        std::hint::black_box((pal.accent, tint, rim, ui.surface));
    }
    let theme_avg_us = t0.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(ITERS);

    let builtin_count = builtin_ids.len();

    // 6b. WidgetRuntime::chip_spec across all built-in widgets.
    let t1 = Instant::now();
    for _ in 0..ITERS {
        for &id in &builtin_ids {
            let spec = runtime.chip_spec(id, &settings, true);
            std::hint::black_box(spec);
        }
    }
    let chips_avg_us = t1.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(ITERS);

    // 6c. WidgetRuntime::island_detail_spec across all built-in widgets.
    let t2 = Instant::now();
    for _ in 0..ITERS {
        for &id in &builtin_ids {
            let spec = runtime.island_detail_spec(id, &settings);
            std::hint::black_box(spec);
        }
    }
    let island_detail_avg_us = t2.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(ITERS);

    // 6d. Popover placement + settings_search queries.
    let t3 = Instant::now();
    for i in 0..ITERS {
        let top = 40.0 + ((i % 66) as f32) * 10.0;
        let placed =
            compute_popover_placement(top, 820.0, 30.0, 220.0, 200.0, 900.0, 740.0, 4.0, 8.0, 36.0);
        let hits = settings_search::search(&search_entries, "dynamic island");
        std::hint::black_box((placed, hits.len()));
    }
    let popover_search_avg_us = t3.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(ITERS);

    // 6e. Dock background + target FPS + ShrinkPriority compaction math.
    let t4 = Instant::now();
    for _ in 0..ITERS {
        let bg = dock::effective_dock_background(&settings, false, true);
        let fps_live = dock::compute_target_fps(&settings, "");
        let fps_hz = dock::target_fps_for_hz(&settings, 240);
        let shrunk = dock::widget_shrink_factors(&shrink_inputs, 140.0, 2);
        std::hint::black_box((bg, fps_live, fps_hz, shrunk.len()));
    }
    let dock_math_avg_us = t4.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(ITERS);

    eprintln!(
        "[PERF 240FPS GUARD ({ITERS} iters)] theme={theme_avg_us:.1}us | chip_spec({builtin_count})={chips_avg_us:.1}us | island_detail({builtin_count})={island_detail_avg_us:.1}us | popover+search={popover_search_avg_us:.1}us | dock_bg+fps+shrink={dock_math_avg_us:.1}us"
    );

    // Restore clean Windows Default theme state.
    theme::sync_from_settings(&DockSettings::default());

    // Generous debug-build CI-safe upper bounds (< 5 ms = 5,000 us per full pass) that
    // reliably catch accidental O(N^2), disk I/O, or blocking COM regressions.
    assert!(
        theme_avg_us < 5_000.0,
        "theme hot path regressed: {theme_avg_us:.1} us/iter"
    );
    assert!(
        chips_avg_us < 5_000.0,
        "chip_spec({builtin_count}) hot path regressed: {chips_avg_us:.1} us/iter"
    );
    assert!(
        island_detail_avg_us < 5_000.0,
        "island_detail_spec({builtin_count}) hot path regressed: {island_detail_avg_us:.1} us/iter"
    );
    assert!(
        popover_search_avg_us < 5_000.0,
        "popover+search hot path regressed: {popover_search_avg_us:.1} us/iter"
    );
    assert!(
        dock_math_avg_us < 5_000.0,
        "dock bg+fps+shrink hot path regressed: {dock_math_avg_us:.1} us/iter"
    );
}

// ---------------------------------------------------------------------------
// 7. Low-RAM & Lazy-Init Bounded Memory Guard
// ---------------------------------------------------------------------------

#[test]
fn disabled_features_stay_lazy_and_memory_bounded() {
    use crate::platform::hud_monitor::HudMonitor;
    use crate::platform::notification_listener::{
        MAX_BODY_CHARS, MAX_NOTIFICATIONS, NotificationMirror,
    };
    use crate::widgets::clipboard::{ClipboardStore, MAX_ENTRY_BYTES};
    use crate::widgets::lyrics::{LyricsCache, MAX_CACHED_TRACKS, SyncedLyrics};
    use crate::widgets::shelf::FileShelfState;

    // 7a. Default WidgetRuntime must keep all opt-in lazy subsystems unallocated.
    let runtime = WidgetRuntime::default();
    assert_eq!(
        runtime.lazy_subsystems_allocated(),
        0,
        "expected 0 lazy subsystems allocated by default"
    );

    // 7b. NotificationMirror ring buffer stays <= 12 after 100 alerts and enforces text bounds.
    let mut notif_settings = DockSettings::default();
    notif_settings.set_island_notifications_enabled(true);
    let mut mirror = NotificationMirror::new();
    assert_eq!(MAX_NOTIFICATIONS, 12);
    assert_eq!(MAX_BODY_CHARS, 200);
    for i in 0..100usize {
        let long_text = format!("alert-{i}-{}", "x".repeat(320));
        let _ = mirror.push_shell_alert(
            format!("App{i}"),
            long_text,
            (i + 1) as isize,
            &notif_settings,
            false,
        );
    }
    assert!(
        mirror.recent().len() <= 12,
        "NotificationMirror exceeded 12 entries: {}",
        mirror.recent().len()
    );
    assert!(!mirror.recent().is_empty());
    for entry in mirror.recent() {
        assert!(
            entry.body.chars().count() <= 200,
            "NotificationMirror body exceeded 200 chars: {}",
            entry.body.chars().count()
        );
        assert!(
            entry.title.chars().count() <= 120,
            "NotificationMirror title exceeded 120 chars: {}",
            entry.title.chars().count()
        );
    }

    // 7c. ClipboardStore stays <= clipboard_max_items after 100 entries and caps > 16 KB payloads.
    let default_settings = DockSettings::default();
    let clip_cap = default_settings.clipboard_max_items();
    let mut clip = ClipboardStore::new();
    for i in 0..100usize {
        assert!(clip.ingest_text(
            &format!("clip-entry-{i}"),
            clip_cap,
            1_700_000_000 + i as u64
        ));
    }
    assert!(
        clip.entries().len() <= clip_cap,
        "ClipboardStore exceeded clipboard_max_items ({clip_cap}): {}",
        clip.entries().len()
    );
    let oversized = "A".repeat(MAX_ENTRY_BYTES * 2 + 512);
    assert!(clip.ingest_text(&oversized, clip_cap, 1_700_001_000));
    assert!(
        clip.entries()[0].text.len() <= MAX_ENTRY_BYTES,
        "ClipboardStore failed to truncate > 16 KB payload: {} bytes",
        clip.entries()[0].text.len()
    );

    // 7d. FileShelfState stays <= shelf_max_items after adding 100 items.
    let shelf_cap = default_settings.shelf_max_items();
    let mut shelf = FileShelfState::default();
    for i in 0..100usize {
        assert!(shelf.add_text(&format!("shelf-snippet-{i}"), shelf_cap));
    }
    assert!(
        shelf.count() <= shelf_cap,
        "FileShelfState exceeded shelf_max_items ({shelf_cap}): {}",
        shelf.count()
    );

    // 7e. LyricsCache stays <= 8 tracks after inserting 30 tracks.
    let mut lyrics = LyricsCache::new();
    assert_eq!(MAX_CACHED_TRACKS, 8);
    for i in 0..30usize {
        lyrics.insert(SyncedLyrics::from_lrc(
            format!("artist-{i}::track-{i}"),
            "[00:01.00] First line\n[00:05.00] Second line",
        ));
    }
    assert!(
        lyrics.len() <= 8,
        "LyricsCache exceeded 8 tracks: {}",
        lyrics.len()
    );

    // 7f. HudMonitor::poll returns None immediately when all HUDs are disabled.
    let mut hud_disabled = DockSettings::default();
    hud_disabled.set_hud_volume_enabled(false);
    hud_disabled.set_hud_brightness_enabled(false);
    hud_disabled.set_hud_capslock_enabled(false);
    hud_disabled.set_hud_keyboard_lang_enabled(false);
    hud_disabled.set_hud_power_enabled(false);
    hud_disabled.set_hud_network_enabled(false);
    hud_disabled.set_hud_bluetooth_enabled(false);
    let mut hud = HudMonitor::new();
    assert!(
        hud.poll(&hud_disabled, Some((42, false)), &[]).is_none(),
        "HudMonitor::poll must return None when disabled"
    );
    assert!(
        hud.poll(
            &hud_disabled,
            Some((90, true)),
            &[("Headset".to_owned(), 75)],
        )
        .is_none(),
        "HudMonitor::poll must stay None on subsequent changes when disabled"
    );
}
