//! Reusable iOS + shadcn/ui GPUI design system components for OpenTaskBar.
//!
//! Features:
//! - Semantic Light/Dark tokens
//! - Strict `min_w_0()` / `w_full()` / `shrink_0()` layout discipline (zero horizontal overflow)
//! - Native Windows 11 `Segoe Fluent Icons` glyphs
//! - Grouped inset `section_card`, concise `setting_row`, `ios_switch`, `segmented_track`,
//!   `stepper`, `button`, `badge`, `sidebar_nav_item`, and `scrollbar_overlay`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Div, FocusHandle, KeyDownEvent, MouseButton, ScrollAnchor, SharedString, deferred,
    div, img, prelude::*, px, rgba,
};

use opentaskbar_core::i18n as loc;

use crate::design::{self, fonts, gpui_rgba};

const PANEL_RADIUS: f32 = 14.0;
const CONTROL_RADIUS: f32 = 8.0;
const ROW_INSET: f32 = 16.0;
/// Shared width of right-column controls (dropdown triggers, steppers) so a card's
/// controls form one aligned vertical rail.
const CONTROL_RAIL_WIDTH: f32 = 164.0;
/// Fixed, right-aligned slider readout column; wide enough for "Otomatik" / "%132" so every
/// slider track in a card starts and ends at the same x.
const SLIDER_VALUE_WIDTH: f32 = 76.0;
// Stepper = 2 x 28px buttons + 46px readout + gaps/padding/border; it must fit the rail.
const _: () = assert!(CONTROL_RAIL_WIDTH >= 28.0 * 2.0 + 46.0 + 8.0 + 4.0 + 2.0);

/// Rows kept alive in a virtualized searchable dropdown at once. A 10 000-city
/// list costs ~10 row elements per keystroke instead of 10 000.
const SEARCH_VIEWPORT_ROWS: usize = 6;
const SEARCH_OVERSCAN_ROWS: usize = 2;
const SEARCH_ROW_H_PX: f32 = 30.0;
/// Every dropdown popover stays at most this tall so it always fits either above or
/// below its trigger inside the 740 px Settings window.
const POPOVER_MAX_H: f32 = 240.0;
const SEARCH_MENU_H: f32 = SEARCH_VIEWPORT_ROWS as f32 * SEARCH_ROW_H_PX + 36.0;
const _: () = assert!(SEARCH_MENU_H <= POPOVER_MAX_H);
/// While typing, re-filter every frame; once the query settles, reuse the cached
/// index list across repaints and arrow-key navigation.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);

/// Unified neutral Zinc/Carbon palette for GPUI surfaces (shared with Dock & Flyout).
#[derive(Clone, Copy)]
pub struct UiTheme {
    pub canvas_bg: u32,
    pub sidebar_bg: u32,
    pub surface: u32,
    pub surface_muted: u32,
    pub surface_hover: u32,
    pub popover_bg: u32,
    pub dock_preview_bg: u32,
    pub dock_preview_fg: u32,
    pub dock_preview_sub: u32,
    pub border: u32,
    pub border_width: f32,
    pub separator: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_soft: u32,
    pub accent_fg: u32,
    pub switch_off: u32,
    pub switch_on: u32,
    pub switch_thumb: u32,
    pub destructive: u32,
    pub destructive_soft: u32,
    pub destructive_hover: u32,
    pub scrollbar_thumb: u32,
}

/// Readable foreground (white or near-black) for text drawn on top of `bg`.
fn readable_on(bg: design::Rgb) -> u32 {
    let luma = 0.2126 * f32::from(bg.0) + 0.7152 * f32::from(bg.1) + 0.0722 * f32::from(bg.2);
    if luma > 160.0 {
        0x0f17_2aff
    } else {
        0xffff_ffff
    }
}

impl UiTheme {
    /// Opaque GPUI theme driven by the live (theme-pack / editor aware) palette.
    ///
    /// With the stock Windows Default theme the colours are byte-identical to the base
    /// palette; once a theme is customised, hairlines and the accent foreground follow
    /// the themed palette too.
    pub fn new(is_dark: bool) -> Self {
        let p = design::active_palette(is_dark);
        let themed = design::theme::has_active_theme_customization();
        let mut t = Self::from_palette(&p, is_dark);
        if themed {
            t.border = gpui_rgba(p.border_rgb, 0xff);
            t.separator = gpui_rgba(p.sep_rgb, 0xff);
            t.accent_fg = readable_on(p.accent);
        }
        t
    }

    fn from_palette(p: &design::Palette, is_dark: bool) -> Self {
        Self {
            canvas_bg: gpui_rgba(p.canvas, 0xff),
            sidebar_bg: gpui_rgba(p.sidebar, 0xff),
            surface: gpui_rgba(p.surface, 0xff),
            surface_muted: gpui_rgba(p.elevated, 0xff),
            surface_hover: gpui_rgba(p.hover, 0xff),
            popover_bg: gpui_rgba(p.surface, 0xff),
            dock_preview_bg: gpui_rgba(p.dock_pill, 0xf2),
            dock_preview_fg: gpui_rgba(p.text_primary, 0xff),
            dock_preview_sub: gpui_rgba(p.text_secondary, 0xff),
            border: if is_dark { 0xffffff16 } else { 0x00000014 },
            border_width: 1.0,
            separator: if is_dark { 0xffffff10 } else { 0x0000000e },
            text_primary: gpui_rgba(p.text_primary, 0xff),
            text_secondary: gpui_rgba(p.text_secondary, 0xff),
            accent: gpui_rgba(p.accent, 0xff),
            accent_hover: gpui_rgba(p.accent, 0xd8),
            accent_soft: gpui_rgba(p.accent, 0x22),
            accent_fg: 0xffffffff,
            switch_off: gpui_rgba(p.switch_off, 0xff),
            switch_on: gpui_rgba(p.accent, 0xff),
            switch_thumb: 0xffffffff,
            destructive: gpui_rgba(p.danger, 0xff),
            destructive_soft: gpui_rgba(p.danger, 0x20),
            destructive_hover: gpui_rgba(p.danger, 0x38),
            scrollbar_thumb: if is_dark { 0xffffff38 } else { 0x00000038 },
        }
    }

    /// Settings window theme: opaque in Solid mode, pure black in TrueBlack (OLED) mode,
    /// transparent canvas in None mode, and in Glass mode lets the unified Clear Crystal
    /// `SpatialGlassHost` shine cleanly across the whole window.
    pub fn for_settings(is_dark: bool, glass_mode: bool) -> Self {
        let bg = if glass_mode {
            opentaskbar_core::settings::SurfaceBackground::Glass
        } else {
            opentaskbar_core::settings::SurfaceBackground::Solid
        };
        Self::for_settings_background(is_dark, bg)
    }

    pub fn for_settings_background(
        is_dark: bool,
        background: opentaskbar_core::settings::SurfaceBackground,
    ) -> Self {
        use opentaskbar_core::settings::SurfaceBackground;
        match background {
            // Every surface/text/accent token follows the live palette.
            SurfaceBackground::Solid => Self::new(is_dark),
            // OLED care: the black canvas, sidebar and neutral text stay fixed; only the
            // accent family follows the themed palette (inherited from `new`).
            SurfaceBackground::TrueBlack => {
                let mut t = Self::new(true);
                t.canvas_bg = 0x0000_00ff;
                t.sidebar_bg = 0x0000_00ff;
                t.surface = 0x0c0c_0eff;
                t.surface_muted = 0x1414_18ff;
                t.surface_hover = 0x1e1e_24ff;
                t.popover_bg = 0x0a0a_0cff;
                t.border = 0xffff_ff18;
                t.separator = 0xffff_ff14;
                t.text_primary = 0xf4f4_f5ff;
                t.text_secondary = 0xa1a1_aaff;
                t.switch_off = 0x2a2a_32ff;
                t
            }
            SurfaceBackground::None => {
                let mut t = Self::new(is_dark);
                t.canvas_bg = 0x0000_0000;
                t.sidebar_bg = if design::theme::has_active_theme_customization() {
                    gpui_rgba(design::active_palette(is_dark).sidebar, 0x40)
                } else if is_dark {
                    0x0000_0028
                } else {
                    0xffff_ff28
                };
                t
            }
            SurfaceBackground::Glass => {
                let mut t = Self::new(is_dark);
                if design::theme::has_active_theme_customization() {
                    // Themed glass: translucent palette tints over the transparent canvas
                    // so the tinted `SpatialGlassHost` keeps shining through.
                    let p = design::active_palette(is_dark);
                    let (sidebar_a, surface_a, muted_a, hover_a, popover_a, line_a) = if is_dark {
                        (0x40, 0x66, 0x80, 0xa0, 0xf2, 0xcc)
                    } else {
                        (0x50, 0x80, 0x90, 0xb0, 0xf5, 0xdd)
                    };
                    t.canvas_bg = 0x0000_0000;
                    t.sidebar_bg = gpui_rgba(p.sidebar, sidebar_a);
                    t.surface = gpui_rgba(p.surface, surface_a);
                    t.surface_muted = gpui_rgba(p.elevated, muted_a);
                    t.surface_hover = gpui_rgba(p.hover, hover_a);
                    t.popover_bg = gpui_rgba(p.surface, popover_a);
                    t.border = gpui_rgba(p.border_rgb, line_a);
                    t.separator = gpui_rgba(p.sep_rgb, line_a);
                    t.text_primary = gpui_rgba(p.text_primary, 0xff);
                    t.text_secondary = gpui_rgba(p.text_secondary, 0xff);
                    t.switch_off = gpui_rgba(p.switch_off, muted_a);
                } else if is_dark {
                    t.canvas_bg = 0x0000_0000;
                    t.sidebar_bg = 0x0000_001e;
                    t.surface = 0xffff_ff0f;
                    t.surface_muted = 0xffff_ff18;
                    t.surface_hover = 0xffff_ff28;
                    t.popover_bg = 0x1216_20f2;
                    t.border = 0xffff_ff26;
                    t.separator = 0xffff_ff18;
                    t.text_primary = 0xffff_ffff;
                    t.text_secondary = 0xe2e8_f0dc;
                    t.switch_off = 0xffff_ff36;
                } else {
                    t.canvas_bg = 0x0000_0000;
                    t.sidebar_bg = 0xffff_ff28;
                    t.surface = 0xffff_ff4c;
                    t.surface_muted = 0x0000_0010;
                    t.surface_hover = 0x0000_001b;
                    t.popover_bg = 0xf8fa_fcf5;
                    t.border = 0x0000_001f;
                    t.separator = 0x0000_0015;
                    t.text_primary = 0x0f17_2aff;
                    t.text_secondary = 0x2632_45eb;
                    t.switch_off = 0x0000_002e;
                }
                t
            }
        }
    }
}

/// Crisp native Windows 11 Segoe Fluent Icons glyph.
pub fn icon_glyph(glyph: &'static str, size_px: f32, color: u32) -> Div {
    icon_glyph_owned(glyph, size_px, color)
}

/// Same as [`icon_glyph`] for dynamically built glyph strings (e.g. a glyph
/// code point read from a custom widget manifest).
pub fn icon_glyph_owned(glyph: impl Into<SharedString>, size_px: f32, color: u32) -> Div {
    div()
        .font_family("Segoe Fluent Icons")
        .text_size(px(size_px))
        .text_color(rgba(color))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .child(glyph.into())
}

/// iOS grouped inset section header + rounded container card with hairline dividers.
pub fn section_card(theme: &UiTheme, title: impl Into<SharedString>, rows: Vec<AnyElement>) -> Div {
    let mut card = div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(PANEL_RADIUS))
        .bg(rgba(theme.surface))
        .border(px(theme.border_width))
        .border_color(rgba(theme.border));

    let count = rows.len();
    for (idx, row) in rows.into_iter().enumerate() {
        card = card.child(row);
        if idx + 1 < count {
            card = card.child(div().h(px(1.0)).mx(px(ROW_INSET)).bg(rgba(theme.separator)));
        }
    }

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .px(px(4.0))
                .text_size(px(fonts::scaled_size(13.0)))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgba(theme.text_secondary))
                .child(title.into()),
        )
        .child(card)
}

/// Concise two-column setting row with strict `min_w_0` + `shrink_0` anti-overflow protection.
///
/// Thin wrapper for the ~100 rows that are not search targets: no element id, no highlight,
/// no scroll anchor. Searchable rows use [`setting_row_with_id`] instead.
pub fn setting_row(
    theme: &UiTheme,
    icon: Option<&'static str>,
    title: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    setting_row_inner(theme, false, icon, title, None, control)
}

/// Setting row with an inline subtitle description kept inside the same card cell so section
/// hairline dividers never split a control from its explanation.
pub fn setting_row_desc(
    theme: &UiTheme,
    icon: Option<&'static str>,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    setting_row_inner(theme, false, icon, title, Some(description.into()), control)
}

/// Searchable variant of [`setting_row`].
///
/// `id` is the stable id shared with the Settings search catalog. When `highlighted` is true
/// the row gets a tinted background plus a left accent bar so the jumped-to control is
/// obvious. `anchor` is a [`ScrollAnchor`] tied to the Settings scroll handle; attaching it
/// lets GPUI scroll to this row even though it is nested several levels inside a section card.
pub fn setting_row_with_id(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    anchor: Option<ScrollAnchor>,
    highlighted: bool,
    icon: Option<&'static str>,
    title: impl Into<SharedString>,
    control: impl IntoElement,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    // `anchor_scroll` lives on `StatefulInteractiveElement`, so the element id comes first.
    setting_row_inner(theme, highlighted, icon, title, None, control)
        .id(id)
        .anchor_scroll(anchor)
}

/// Searchable setting row with an inline subtitle description.
#[allow(clippy::too_many_arguments)]
pub fn setting_row_with_id_desc(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    anchor: Option<ScrollAnchor>,
    highlighted: bool,
    icon: Option<&'static str>,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    setting_row_inner(
        theme,
        highlighted,
        icon,
        title,
        Some(description.into()),
        control,
    )
    .id(id)
    .anchor_scroll(anchor)
}

/// Wraps a dependent control row to visually dim it when its parent toggle is turned off.
pub fn dim_when(disabled: bool, element: impl IntoElement) -> Div {
    let wrap = div().w_full();
    if disabled {
        wrap.opacity(0.46).child(element)
    } else {
        wrap.child(element)
    }
}

fn setting_row_inner(
    theme: &UiTheme,
    highlighted: bool,
    icon: Option<&'static str>,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
) -> Div {
    let mut left = div()
        .flex_1()
        .min_w(px(150.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .overflow_hidden();

    if let Some(g) = icon {
        left = left.child(icon_glyph(g, 14.0, theme.text_secondary));
    }

    let mut labels = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .overflow_hidden()
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_size(px(fonts::scaled_size(13.0)))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgba(theme.text_primary))
                .overflow_hidden()
                .child(title.into()),
        );
    if let Some(desc) = description {
        labels = labels.child(
            div()
                .w_full()
                .min_w_0()
                .text_size(px(fonts::scaled_size(11.0)))
                .text_color(rgba(theme.text_secondary))
                .child(desc),
        );
    }
    left = left.child(labels);

    let mut row = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .px(px(ROW_INSET))
        .py(px(12.0));

    if highlighted {
        row = row.bg(rgba(theme.accent_soft)).child(
            div()
                .w(px(3.0))
                .h(px(18.0))
                .rounded(px(2.0))
                .bg(rgba(theme.accent))
                .flex_shrink_0(),
        );
    }

    row.child(left).child(div().flex_shrink_0().child(control))
}

/// Crisp desktop toggle switch pill (`36x20`).
pub fn ios_switch(
    id: impl Into<gpui::ElementId>,
    theme: &UiTheme,
    checked: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let track_bg = if checked {
        theme.switch_on
    } else {
        theme.switch_off
    };
    let thumb_left = if checked { px(18.0) } else { px(2.0) };
    let thumb_bg = switch_thumb_color(theme, checked);
    // Checked switches already use `theme.accent` as their fill, so keyboard focus uses a
    // high-contrast outline (`text_primary`) to stay visible in both checked and unchecked states.
    let focus_border = if checked {
        theme.text_primary
    } else {
        theme.accent
    };

    div()
        .id(id)
        .w(px(36.0))
        .h(px(20.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(rgba(track_bg))
        .bg(rgba(track_bg))
        .tab_index(0)
        .focus(move |style| style.border_color(rgba(focus_border)))
        .hover(|style| style.opacity(0.92))
        .active(|style| style.opacity(0.82))
        .cursor_pointer()
        .flex()
        .items_center()
        .on_click(on_click)
        .child(
            div()
                .w(px(16.0))
                .h(px(16.0))
                .rounded(px(8.0))
                .bg(rgba(thumb_bg))
                .ml(thumb_left - px(1.0)),
        )
}

/// ON switches keep a crisp opaque thumb; OFF switches mute it (78% alpha) so the
/// unchecked state reads as resting instead of competing with checked toggles.
pub fn switch_thumb_color(theme: &UiTheme, checked: bool) -> u32 {
    if checked {
        theme.switch_thumb
    } else {
        (theme.switch_thumb & 0xffff_ff00) | 0xc8
    }
}

/// Compact badge pill.
pub fn badge(theme: &UiTheme, text: impl Into<SharedString>, accent: bool) -> Div {
    let (bg, fg) = if accent {
        (theme.accent_soft, theme.accent)
    } else {
        (theme.surface_muted, theme.text_secondary)
    };
    div()
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(999.0))
        .bg(rgba(bg))
        .text_size(px(fonts::scaled_size(11.0)))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgba(fg))
        .flex_shrink_0()
        .child(text.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Destructive,
}

pub fn button(
    id: impl Into<gpui::ElementId>,
    theme: &UiTheme,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    button_with_state(id, theme, label, variant, true, on_click)
}

/// Opacity applied to a disabled button; it also drops hover, focus and click handling.
pub const DISABLED_OPACITY: f32 = 0.45;

/// Same as [`button`], but `enabled == false` renders a dimmed, non-interactive button
/// (no pointer cursor, hover, keyboard focus or click handler).
pub fn button_with_state(
    id: impl Into<gpui::ElementId>,
    theme: &UiTheme,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let (bg, hover_bg, fg, border) = match variant {
        ButtonVariant::Primary => (
            theme.accent,
            theme.accent_hover,
            theme.accent_fg,
            theme.accent,
        ),
        ButtonVariant::Secondary => (
            theme.surface_muted,
            theme.surface_hover,
            theme.text_primary,
            theme.border,
        ),
        ButtonVariant::Destructive => (
            theme.destructive_soft,
            theme.destructive_hover,
            theme.destructive,
            theme.destructive,
        ),
    };

    div()
        .id(id)
        .px(px(12.0))
        .py(px(7.0))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgba(bg))
        .border_1()
        .border_color(rgba(border))
        .text_size(px(fonts::scaled_size(12.0)))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgba(fg))
        .flex_shrink_0()
        .when(!enabled, |b| b.opacity(DISABLED_OPACITY).cursor_default())
        .when(enabled, move |b| {
            let focus = theme.text_primary;
            b.cursor_pointer()
                .tab_index(0)
                .focus(move |style| style.border_color(rgba(focus)))
                .hover(move |s| s.bg(rgba(hover_bg)))
                .active(|s| s.opacity(0.84))
                .on_click(on_click)
        })
        .child(label.into())
}

/// Settings widget-card pill: glyph + label with the card click suppressed.
pub fn card_button(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    glyph: Option<&'static str>,
    label: impl Into<SharedString>,
    selected: bool,
    on_press: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    let bg = if selected {
        theme.accent_soft
    } else {
        theme.surface_muted
    };
    let border = if selected { theme.accent } else { theme.border };
    let fg = if selected {
        theme.accent
    } else {
        theme.text_secondary
    };
    let hover = if selected {
        theme.accent_soft
    } else {
        theme.surface_hover
    };
    let mut row = div()
        .id(id)
        .h(px(28.0))
        .px(px(9.0))
        .rounded(px(7.0))
        .bg(rgba(bg))
        .border_1()
        .border_color(rgba(border))
        .flex()
        .items_center()
        .gap(px(5.0))
        .cursor_pointer()
        .hover(move |s| s.bg(rgba(hover)))
        .on_mouse_down(MouseButton::Left, on_press);
    if let Some(g) = glyph {
        row = row.child(icon_glyph(g, 11.5, fg));
    }
    row.child(
        div()
            .whitespace_nowrap()
            .text_size(px(fonts::scaled_size(11.5)))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgba(fg))
            .child(label.into()),
    )
}

/// Text-only card action (Approve / Folder / Edit / Remove).
pub fn compact_card_button(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    label: impl Into<SharedString>,
    accent: bool,
    on_press: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    let (bg, fg) = if accent {
        (theme.accent_soft, theme.accent)
    } else {
        (theme.surface_muted, theme.text_secondary)
    };
    div()
        .id(id)
        .px(px(7.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .bg(rgba(bg))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_press)
        .child(
            div()
                .text_size(px(fonts::scaled_size(10.5)))
                .text_color(rgba(fg))
                .child(label.into()),
        )
}

/// Callback invoked when a dropdown option is selected.
pub type DropdownSelectHandler = std::rc::Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>;

/// One selectable entry inside [`dropdown`] and [`searchable_dropdown`].
#[derive(Clone)]
pub struct DropdownOption {
    pub label: SharedString,
    pub flag_path: Option<SharedString>,
    pub glyph: Option<&'static str>,
    pub selected: bool,
    pub on_select: DropdownSelectHandler,
}

/// One popover row shared by all three dropdown builders. `min_h`/`gap` are the
/// only visual differences between the standard, action-menu and searchable
/// variants; event plumbing (`stop_propagation`, select, optional close) is
/// identical and therefore lives here exactly once.
#[allow(clippy::too_many_arguments)]
fn dropdown_option_row(
    id: &str,
    theme: &UiTheme,
    opt_index: usize,
    opt: &DropdownOption,
    is_active: bool,
    accent: u32,
    min_h: f32,
    gap: f32,
    close: Option<DropdownSelectHandler>,
) -> gpui::Stateful<Div> {
    let opt_bg = if is_active {
        theme.accent_soft
    } else {
        0x00000000
    };
    let opt_fg = if is_active {
        theme.accent
    } else {
        theme.text_primary
    };
    let opt_hover = if is_active {
        theme.accent_soft
    } else {
        theme.surface_hover
    };
    let cb = opt.on_select.clone();
    let mut left = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .overflow_hidden();
    if let Some(path) = &opt.flag_path {
        left = left.child(
            img(path.clone())
                .w(px(18.0))
                .h(px(13.0))
                .rounded(px(2.5))
                .flex_shrink_0(),
        );
    } else if let Some(glyph) = opt.glyph {
        left = left.child(icon_glyph(glyph, 12.0, opt_fg));
    }
    left = left.child(
        div()
            .text_size(px(fonts::scaled_size(12.0)))
            .font_weight(if is_active {
                gpui::FontWeight::SEMIBOLD
            } else {
                gpui::FontWeight::MEDIUM
            })
            .text_color(rgba(opt_fg))
            .overflow_hidden()
            .child(opt.label.clone()),
    );

    div()
        .id(SharedString::from(format!("{id}-{opt_index}")))
        .w_full()
        .min_h(px(min_h))
        .px(px(9.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .bg(rgba(opt_bg))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(gap))
        .cursor_pointer()
        .hover(move |s| s.bg(rgba(opt_hover)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            cb(window, cx);
            if let Some(close) = &close {
                close(window, cx);
            }
        })
        .child(left)
        .when(opt.selected, |row| {
            row.child(icon_glyph("\u{E73E}", 11.0, accent))
        })
}

/// Height of the custom Settings titlebar a popover must never slide under.
const POPOVER_TOP_CLEARANCE: f32 = 36.0;
/// Minimum distance kept between a popover and every window edge.
const POPOVER_MARGIN: f32 = 8.0;
/// Gap between the trigger and the popover it opens.
const POPOVER_GAP: f32 = 4.0;

/// Pure placement geometry for a right-aligned popover anchored to a trigger.
///
/// Opens below the trigger when the menu fits there (or when below is still the roomier
/// side); otherwise flips above the trigger without overlapping it. The result is then
/// clamped inside the viewport, never above the titlebar (`top_clearance`) and never off
/// the left/right edges. Returns `(x, y, flipped_up)` in window coordinates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_popover_placement(
    trigger_top: f32,
    trigger_right: f32,
    trigger_h: f32,
    menu_w: f32,
    menu_h: f32,
    vp_w: f32,
    vp_h: f32,
    gap: f32,
    margin: f32,
    top_clearance: f32,
) -> (f32, f32, bool) {
    let trigger_bottom = trigger_top + trigger_h;
    let space_below = (vp_h - trigger_bottom - margin).max(0.0);
    let space_above = (trigger_top - top_clearance - margin).max(0.0);
    let min_y = top_clearance + margin;
    let max_y = (vp_h - margin - menu_h).max(min_y);

    let opens_below = space_below >= menu_h + gap || space_below >= space_above;
    let y = if opens_below {
        trigger_bottom + gap
    } else {
        (trigger_top - gap - menu_h).max(min_y)
    };
    let y = y.min(max_y).max(min_y);

    let max_x = (vp_w - menu_w - margin).max(margin);
    let x = (trigger_right - menu_w).min(max_x).max(margin);
    (x, y, !opens_below)
}

/// Viewport-aware popover body: right-aligns its child to the trigger and opens it
/// downward, or flips it above the trigger when the window bottom would clip it.
///
/// Rendered inside `deferred(..)`, whose replayed `prepaint` runs under the trigger's
/// absolute element offset, so `bounds` is the live (already scrolled) window-space rect
/// of the trigger container. The child is re-offset in `prepaint`, so painting, hitboxes,
/// mouse and scroll-wheel handling all follow the flipped position.
struct ViewportPopover {
    child: AnyElement,
    trigger_h: f32,
    gap: f32,
    margin: f32,
    top_clearance: f32,
}

impl ViewportPopover {
    fn new(child: AnyElement, trigger_h: f32) -> Self {
        Self {
            child,
            trigger_h,
            gap: POPOVER_GAP,
            margin: POPOVER_MARGIN,
            top_clearance: POPOVER_TOP_CLEARANCE,
        }
    }
}

impl gpui::Element for ViewportPopover {
    type RequestLayoutState = gpui::LayoutId;
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let child_layout_id = self.child.request_layout(window, cx);
        // Absolutely positioned against the trigger wrapper's top-right corner and sized to
        // its content: `bounds.right()` is the trigger's right edge and the child keeps its
        // own fixed width. The wrapper itself paints nothing.
        let mut style = gpui::Style {
            position: gpui::Position::Absolute,
            ..Default::default()
        };
        style.inset.top = px(0.0).into();
        style.inset.right = px(0.0).into();
        let layout_id = window.request_layout(style, [child_layout_id], cx);
        (layout_id, child_layout_id)
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        child_layout_id: &mut Self::RequestLayoutState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        let child_bounds = window.layout_bounds(*child_layout_id);
        let vp = window.viewport_size();
        let (x, y, _flipped_up) = compute_popover_placement(
            f32::from(bounds.origin.y),
            f32::from(bounds.right()),
            self.trigger_h,
            f32::from(child_bounds.size.width),
            f32::from(child_bounds.size.height),
            f32::from(vp.width),
            f32::from(vp.height),
            self.gap,
            self.margin,
            self.top_clearance,
        );
        let target = gpui::point(px(x.round()), px(y.round()));
        let child = &mut self.child;
        window.with_element_offset(target - child_bounds.origin, |window| {
            child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<gpui::Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        self.child.paint(window, cx);
    }
}

impl IntoElement for ViewportPopover {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Shared deferred popover shell for all three dropdown builders: viewport-aware placement
/// (flips above the trigger near the window bottom) and an occluding menu so clicks inside
/// it never reach the owner card.
fn dropdown_popover(menu: gpui::Stateful<Div>, trigger_h: f32) -> AnyElement {
    deferred(ViewportPopover::new(
        menu.occlude().into_any_element(),
        trigger_h,
    ))
    .with_priority(1)
    .into_any_element()
}

/// Custom desktop dropdown replacing `segmented_track` with optional country flag images
/// and a deferred floating popover menu that never clips inside cards or scroll views.
pub fn dropdown(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    open: bool,
    options: Vec<DropdownOption>,
    on_toggle: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> Div {
    dropdown_labeled(id, theme, open, None, options, on_toggle)
}

/// Same dropdown, but with an explicit trigger label (used by compact `...` action menus
/// where the trigger must not show the name of the first action).
pub fn dropdown_labeled(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    open: bool,
    trigger_label: Option<SharedString>,
    options: Vec<DropdownOption>,
    on_toggle: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> Div {
    let id: SharedString = id.into();
    let current_label = trigger_label.unwrap_or_else(|| {
        options
            .iter()
            .find(|o| o.selected)
            .or_else(|| options.first())
            .map(|o| o.label.clone())
            .unwrap_or_default()
    });
    let current_flag = options
        .iter()
        .find(|o| o.selected)
        .or_else(|| options.first())
        .and_then(|o| o.flag_path.clone());
    let current_glyph = options
        .iter()
        .find(|o| o.selected)
        .or_else(|| options.first())
        .and_then(|o| o.glyph);

    let trigger_bg = if open {
        theme.surface_hover
    } else {
        theme.surface_muted
    };
    let trigger_border = if open { theme.accent } else { theme.border };
    let hover_bg = theme.surface_hover;
    let accent = theme.accent;

    let mut trigger_left = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .overflow_hidden();
    if let Some(path) = current_flag {
        trigger_left = trigger_left.child(
            img(path)
                .w(px(18.0))
                .h(px(13.0))
                .rounded(px(2.5))
                .flex_shrink_0(),
        );
    } else if let Some(g) = current_glyph {
        trigger_left = trigger_left.child(icon_glyph(g, 12.0, theme.text_secondary));
    }
    trigger_left = trigger_left.child(
        div()
            .text_size(px(fonts::scaled_size(12.0)))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgba(theme.text_primary))
            .overflow_hidden()
            .child(current_label),
    );

    let trigger = div()
        .id(SharedString::from(format!("dd-trig-{id}")))
        .min_w(px(CONTROL_RAIL_WIDTH))
        .h(px(32.0))
        .px(px(11.0))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgba(trigger_bg))
        .border_1()
        .border_color(rgba(trigger_border))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .cursor_pointer()
        .tab_index(0)
        .focus(move |style| style.border_color(rgba(accent)))
        .hover(move |s| s.bg(rgba(hover_bg)))
        .active(|s| s.opacity(0.88))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_toggle(window, cx);
        })
        .child(trigger_left)
        .child(icon_glyph(
            if open { "\u{E70E}" } else { "\u{E70D}" },
            10.0,
            if open {
                theme.accent
            } else {
                theme.text_secondary
            },
        ));

    let mut wrap = div().relative().flex_shrink_0().child(trigger);

    if open {
        let mut menu_list = div()
            .id(SharedString::from(format!("dd-menu-{id}")))
            .w(px(216.0))
            .max_h(px(POPOVER_MAX_H))
            .overflow_y_scroll()
            .p(px(4.0))
            .rounded(px(10.0))
            .bg(rgba(theme.popover_bg))
            .border_1()
            .border_color(rgba(theme.border))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        for (idx, opt) in options.iter().enumerate() {
            menu_list = menu_list.child(dropdown_option_row(
                &format!("dd-opt-{id}"),
                theme,
                idx,
                opt,
                opt.selected,
                accent,
                30.0,
                10.0,
                None,
            ));
        }

        wrap = wrap.child(dropdown_popover(menu_list, 32.0));
    }

    wrap
}

/// Compact square vertical three-dots (`⋮`) action menu button with no extra prefix/chevron
/// glyphs on the trigger, opening a right-aligned popover with per-option Fluent icons.
pub fn icon_action_menu(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    open: bool,
    options: Vec<DropdownOption>,
    on_toggle: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> Div {
    let id: SharedString = id.into();
    let trigger_bg = if open {
        theme.surface_hover
    } else {
        theme.surface_muted
    };
    let trigger_border = if open { theme.accent } else { theme.border };
    let hover_bg = theme.surface_hover;
    let accent = theme.accent;
    let dot_color = if open {
        theme.accent
    } else {
        theme.text_secondary
    };

    let dot = move || {
        div()
            .w(px(3.0))
            .h(px(3.0))
            .rounded(px(999.0))
            .bg(rgba(dot_color))
    };

    let trigger = div()
        .id(SharedString::from(format!("icon-act-trig-{id}")))
        .w(px(30.0))
        .h(px(30.0))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgba(trigger_bg))
        .border_1()
        .border_color(rgba(trigger_border))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.5))
        .cursor_pointer()
        .tab_index(0)
        .focus(move |style| style.border_color(rgba(accent)))
        .hover(move |s| s.bg(rgba(hover_bg)))
        .active(|s| s.opacity(0.86))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_toggle(window, cx);
        })
        .child(dot())
        .child(dot())
        .child(dot());

    let mut wrap = div().relative().flex_shrink_0().child(trigger);

    if open {
        let mut menu_list = div()
            .id(SharedString::from(format!("icon-act-menu-{id}")))
            .w(px(196.0))
            .max_h(px(POPOVER_MAX_H))
            .overflow_y_scroll()
            .p(px(4.0))
            .rounded(px(10.0))
            .bg(rgba(theme.popover_bg))
            .border_1()
            .border_color(rgba(theme.border))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        for (idx, opt) in options.iter().enumerate() {
            menu_list = menu_list.child(dropdown_option_row(
                &format!("icon-act-opt-{id}"),
                theme,
                idx,
                opt,
                opt.selected,
                accent,
                30.0,
                8.0,
                None,
            ));
        }

        wrap = wrap.child(dropdown_popover(menu_list, 30.0));
    }

    wrap
}

/// Case fold used by the searchable pickers.
///
/// ASCII `I` folds to plain `i`, dotted `İ` (and the lowercase sequence `i` + combining dot)
/// folds to the dotted marker `İ`, and dotless `ı` stays `ı`. The three classes never mix:
/// `istanbul` finds `Istanbul`, `İstanbul` / `i̇stanbul` find `İstanbul`, `ıstanbul` finds
/// neither — the distinction Turkish users expect.
pub fn fold_search_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            'I' => out.push('i'),
            'İ' => out.push('İ'),
            'i' if chars.peek() == Some(&'\u{307}') => {
                chars.next();
                out.push('İ');
            }
            _ => out.extend(ch.to_lowercase()),
        }
    }
    out
}

/// Indices of options matching every whitespace-separated query token.
pub fn filter_dropdown_options(options: &[DropdownOption], query: &str) -> Vec<usize> {
    let tokens: Vec<String> = fold_search_text(query.trim())
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    options
        .iter()
        .enumerate()
        .filter(|(_, option)| {
            let haystack = fold_search_text(&option.label);
            tokens.iter().all(|token| haystack.contains(token))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Navigation policy shared by the popup menu (wrap) and searchable dropdowns (clamp).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMode {
    Wrap,
    Clamp,
}

/// Moves an index by `delta` over `len` rows according to `mode`.
#[must_use]
pub fn step_index(current: usize, len: usize, delta: isize, mode: StepMode) -> usize {
    if len == 0 {
        return 0;
    }
    let next = (current.min(len - 1) as isize).saturating_add(delta);
    match mode {
        StepMode::Wrap => next.rem_euclid(len as isize) as usize,
        StepMode::Clamp => next.clamp(0, len as isize - 1) as usize,
    }
}

/// Lightweight content signature: a same-length replacement list (locale
/// change, reloaded cities, changed selection) must invalidate the filter cache.
#[must_use]
fn options_fingerprint(options: &[DropdownOption]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for option in options {
        option.label.hash(&mut hash);
        option.flag_path.hash(&mut hash);
        option.glyph.hash(&mut hash);
        option.selected.hash(&mut hash);
    }
    hash.finish()
}

/// Single scroll bound for both the keyboard and wheel handlers.
#[must_use]
fn clamp_scroll_top(top: usize, len: usize, viewport: usize) -> usize {
    top.min(len.saturating_sub(viewport))
}

/// State of a [`searchable_dropdown`]. Kept outside the component so the owning entity can
/// update it from GPUI key events and share it with the filter logic in tests.
#[derive(Clone, Debug, Default)]
pub struct SearchableDropdownState {
    pub query: String,
    /// Row index inside the *filtered* list, not the original option index.
    pub active: usize,
}

impl SearchableDropdownState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.active = 0;
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.active = 0;
    }

    /// Clamps the highlighted row inside `len`, wrapping neither end.
    pub fn move_active(&mut self, delta: isize, len: usize) {
        self.active = step_index(self.active, len, delta, StepMode::Clamp);
    }

    pub fn filtered_indices(&self, options: &[DropdownOption]) -> Vec<usize> {
        filter_dropdown_options(options, &self.query)
    }

    /// Original option index selected by Enter, if any rows survive the filter.
    pub fn active_option(&self, options: &[DropdownOption]) -> Option<usize> {
        self.filtered_indices(options).get(self.active).copied()
    }
}

/// Key handler forwarded by [`searchable_dropdown`] for text edits and arrow navigation.
pub type SearchableDropdownKeyHandler =
    std::rc::Rc<dyn Fn(&KeyDownEvent, &mut gpui::Window, &mut gpui::App)>;

/// Reads the active row from the owner, so two arrow keys in one frame advance
/// two steps instead of both reusing the value captured at render time.
pub type SearchableDropdownActiveHandler = std::rc::Rc<dyn Fn(&mut gpui::App) -> usize>;

/// Callbacks supplied by the owning entity for a [`searchable_dropdown`].
pub struct SearchableDropdownHandlers {
    pub on_toggle: DropdownSelectHandler,
    /// All keys except Enter/Escape (which the component itself resolves against the
    /// filtered list) are forwarded here so the owner can update its text buffer.
    pub on_key_down: SearchableDropdownKeyHandler,
    pub on_close: DropdownSelectHandler,
    pub active: SearchableDropdownActiveHandler,
}

/// Half-open range of filtered rows that should be materialised by the
/// virtualized dropdown. Overscan is clamped at both ends so a short list never
/// produces indices outside `0..total`.
#[must_use]
pub fn visible_row_range(
    total: usize,
    scroll_top: usize,
    viewport: usize,
    overscan: usize,
) -> Range<usize> {
    if total == 0 || viewport == 0 {
        return 0..0;
    }
    let viewport = viewport.min(total);
    let start = scroll_top.min(total - viewport);
    let before = overscan.min(start);
    let after = overscan.min(total - start - viewport);
    (start - before)..(start + viewport + after)
}

/// Returns the scroll offset that keeps `active` inside the viewport, without
/// moving the window when the active row is already visible.
#[must_use]
pub fn scroll_top_for_active(active: usize, scroll_top: usize, viewport: usize) -> usize {
    if viewport == 0 {
        return 0;
    }
    if active < scroll_top {
        active
    } else if active >= scroll_top.saturating_add(viewport) {
        active + 1 - viewport
    } else {
        scroll_top
    }
}

/// Coalesces repeated filter passes while the user types. This never serves a
/// stale result: a query change always recomputes; once the query is settled the
/// cached index list is reused across repaints / arrow navigation instead of
/// re-scanning thousands of options on every frame.
#[derive(Clone, Debug)]
pub struct SearchDebounce {
    interval: Duration,
    last_query: String,
    last_change: Option<Instant>,
}

impl Default for SearchDebounce {
    fn default() -> Self {
        Self::new(SEARCH_DEBOUNCE)
    }
}

impl SearchDebounce {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_query: String::new(),
            last_change: None,
        }
    }

    /// Records a query observation. `true` means the query actually changed.
    pub fn observe(&mut self, query: &str, now: Instant) -> bool {
        if self.last_query == query {
            return false;
        }
        self.last_query = query.to_owned();
        self.last_change = Some(now);
        true
    }

    /// True once the most recent query has been stable for `interval`.
    #[must_use]
    pub fn settled(&self, now: Instant) -> bool {
        self.last_change
            .is_none_or(|at| now.saturating_duration_since(at) >= self.interval)
    }
}

/// UI-thread state for one virtualized picker. Keyed by element id so the city
/// and country pickers never share a scroll offset or debounce window.
#[derive(Default)]
struct VirtualList {
    debounce: SearchDebounce,
    cached: Vec<usize>,
    cached_query: String,
    cached_fingerprint: u64,
    scroll_top: usize,
}

thread_local! {
    static SEARCH_LISTS: RefCell<HashMap<SharedString, VirtualList>> =
        RefCell::new(HashMap::new());
}

/// Empty-state text comes from the shared `SearchNoResults` locale key (WS-2
/// ships all 14 translations). The legacy per-caller message is retained in the
/// signature for API compatibility but is deliberately not used.
fn no_results_message() -> SharedString {
    SharedString::from(loc::t("SearchNoResults"))
}

/// Single row builder, reused for every virtualized row (recycling): the element
/// tree only ever holds `viewport + 2 * overscan` instances of this shape.
fn searchable_row(
    id: &str,
    theme: &UiTheme,
    opt_index: usize,
    opt: &DropdownOption,
    is_active: bool,
    accent: u32,
    close: DropdownSelectHandler,
) -> gpui::Stateful<Div> {
    dropdown_option_row(
        &format!("sdd-opt-{id}"),
        theme,
        opt_index,
        opt,
        is_active,
        accent,
        SEARCH_ROW_H_PX,
        10.0,
        Some(close),
    )
}

/// Dropdown with a text field pinned above the option list. Filtering is pure
/// ([`filter_dropdown_options`]); keyboard navigation, Enter and Escape are handled here while
/// text edits are delegated to `handlers.on_key_down`. The plain [`dropdown`] is untouched.
#[allow(clippy::too_many_arguments)]
pub fn searchable_dropdown(
    id: impl Into<SharedString>,
    theme: &UiTheme,
    open: bool,
    state: &SearchableDropdownState,
    focus: &FocusHandle,
    options: &[DropdownOption],
    placeholder: impl Into<SharedString>,
    _empty_message: impl Into<SharedString>,
    handlers: SearchableDropdownHandlers,
) -> Div {
    let id: SharedString = id.into();
    let placeholder: SharedString = placeholder.into();
    let SearchableDropdownHandlers {
        on_toggle,
        on_key_down,
        on_close,
        active: read_active,
    } = handlers;

    let current = options
        .iter()
        .find(|option| option.selected)
        .or_else(|| options.first());
    let current_label = current
        .map(|option| option.label.clone())
        .unwrap_or_else(|| placeholder.clone());
    let current_flag = current.and_then(|option| option.flag_path.clone());
    let current_glyph = current.and_then(|option| option.glyph);

    let trigger_bg = if open {
        theme.surface_hover
    } else {
        theme.surface_muted
    };
    let trigger_border = if open { theme.accent } else { theme.border };
    let hover_bg = theme.surface_hover;
    let accent = theme.accent;

    let mut trigger_left = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .overflow_hidden();
    if let Some(path) = current_flag {
        trigger_left = trigger_left.child(
            img(path)
                .w(px(18.0))
                .h(px(13.0))
                .rounded(px(2.5))
                .flex_shrink_0(),
        );
    } else if let Some(glyph) = current_glyph {
        trigger_left = trigger_left.child(icon_glyph(glyph, 12.0, theme.text_secondary));
    }
    trigger_left = trigger_left.child(
        div()
            .text_size(px(fonts::scaled_size(12.0)))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgba(theme.text_primary))
            .overflow_hidden()
            .child(current_label),
    );

    let toggle = on_toggle.clone();
    let trigger = div()
        .id(SharedString::from(format!("sdd-trig-{id}")))
        .min_w(px(CONTROL_RAIL_WIDTH))
        .h(px(32.0))
        .px(px(11.0))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgba(trigger_bg))
        .border_1()
        .border_color(rgba(trigger_border))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .cursor_pointer()
        .tab_index(0)
        .focus(move |style| style.border_color(rgba(accent)))
        .hover(move |s| s.bg(rgba(hover_bg)))
        .active(|s| s.opacity(0.88))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            toggle(window, cx);
        })
        .child(trigger_left)
        .child(icon_glyph(
            if open { "\u{E70E}" } else { "\u{E70D}" },
            10.0,
            if open {
                theme.accent
            } else {
                theme.text_secondary
            },
        ));

    let mut wrap = div().relative().flex_shrink_0().child(trigger);

    if open {
        // Virtualized filter + debounce: recompute on every query change, then
        // reuse the cached index list once the query settles. Only the visible
        // slice below is ever turned into GPUI elements.
        let now = Instant::now();
        let scroll_id = id.clone();
        let fingerprint = options_fingerprint(options);
        let (filtered, range) = SEARCH_LISTS.with(|lists| {
            let mut lists = lists.borrow_mut();
            let slot = lists.entry(scroll_id.clone()).or_default();
            let changed = slot.debounce.observe(&state.query, now);
            if changed {
                slot.scroll_top = 0;
            }
            let settled = !changed && slot.debounce.settled(now);
            if !settled
                || slot.cached_query != state.query
                || slot.cached_fingerprint != fingerprint
            {
                slot.cached = state.filtered_indices(options);
                slot.cached_query = state.query.clone();
                slot.cached_fingerprint = fingerprint;
            }
            slot.scroll_top = clamp_scroll_top(
                scroll_top_for_active(state.active, slot.scroll_top, SEARCH_VIEWPORT_ROWS),
                slot.cached.len(),
                SEARCH_VIEWPORT_ROWS,
            );
            (
                slot.cached.clone(),
                visible_row_range(
                    slot.cached.len(),
                    slot.scroll_top,
                    SEARCH_VIEWPORT_ROWS,
                    SEARCH_OVERSCAN_ROWS,
                ),
            )
        });
        let active_option_index = filtered.get(state.active).copied();
        let active_handler = active_option_index
            .and_then(|index| options.get(index))
            .map(|option| option.on_select.clone());

        let input_close = on_close.clone();
        let wheel_id = id.clone();
        let key_scroll_id = id.clone();
        let key_handler =
            move |event: &KeyDownEvent, window: &mut gpui::Window, cx: &mut gpui::App| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        input_close(window, cx);
                        cx.stop_propagation();
                    }
                    "enter" => {
                        if let Some(handler) = &active_handler {
                            handler(window, cx);
                        }
                        input_close(window, cx);
                        cx.stop_propagation();
                    }
                    "up" | "down" => {
                        // The owning entity moves `active`; here we only keep the
                        // virtual window paged so the new active row stays visible.
                        on_key_down(event, window, cx);
                        let len = SEARCH_LISTS.with(|lists| {
                            lists
                                .borrow()
                                .get(&key_scroll_id)
                                .map_or(0, |slot| slot.cached.len())
                        });
                        let next_active = read_active(cx).min(len.saturating_sub(1));
                        SEARCH_LISTS.with(|lists| {
                            if let Some(slot) = lists.borrow_mut().get_mut(&key_scroll_id) {
                                slot.scroll_top = clamp_scroll_top(
                                    scroll_top_for_active(
                                        next_active,
                                        slot.scroll_top,
                                        SEARCH_VIEWPORT_ROWS,
                                    ),
                                    slot.cached.len(),
                                    SEARCH_VIEWPORT_ROWS,
                                );
                            }
                        });
                    }
                    _ => on_key_down(event, window, cx),
                }
            };

        let input_value = if state.query.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(fonts::scaled_size(12.0)))
                .text_color(rgba(theme.text_secondary))
                .child(placeholder)
        } else {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_size(px(fonts::scaled_size(12.0)))
                .text_color(rgba(theme.text_primary))
                .child(state.query.clone())
        };

        let menu_base = div()
            .w(px(240.0))
            // Fixed viewport: only the virtualized slice exists in the tree.
            .h(px(SEARCH_MENU_H))
            .overflow_hidden()
            .p(px(4.0))
            .rounded(px(10.0))
            .bg(rgba(theme.popover_bg))
            .border_1()
            .border_color(rgba(theme.border))
            .flex()
            .flex_col()
            .gap(px(2.0));

        // Wheel paging updates the virtual scroll offset instead of relying on
        // a native scroll container that would need every row to exist.
        let menu_base = menu_base.on_scroll_wheel(move |event, _window, cx| {
            let delta: f32 = event.delta.pixel_delta(px(SEARCH_ROW_H_PX)).y.into();
            if delta == 0.0 {
                return;
            }
            let rows = (delta / SEARCH_ROW_H_PX).round() as isize;
            SEARCH_LISTS.with(|lists| {
                if let Some(slot) = lists.borrow_mut().get_mut(&wheel_id) {
                    slot.scroll_top = clamp_scroll_top(
                        (slot.scroll_top as isize + rows).max(0) as usize,
                        slot.cached.len(),
                        SEARCH_VIEWPORT_ROWS,
                    );
                }
            });
            cx.refresh_windows();
        });

        let mut menu_list = menu_base
            .id(SharedString::from(format!("sdd-menu-{id}")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .id(SharedString::from(format!("sdd-input-{id}")))
                    .track_focus(focus)
                    .tab_index(0)
                    .h(px(28.0))
                    .mb(px(4.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .bg(rgba(theme.surface_muted))
                    .border_1()
                    .border_color(rgba(theme.border))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_text()
                    .on_key_down(key_handler)
                    .child(icon_glyph("\u{E721}", 11.0, theme.text_secondary))
                    .child(input_value),
            );

        if filtered.is_empty() {
            menu_list = menu_list.child(
                div()
                    .w_full()
                    .px(px(9.0))
                    .py(px(8.0))
                    .text_size(px(fonts::scaled_size(12.0)))
                    .text_color(rgba(theme.text_secondary))
                    .child(no_results_message()),
            );
        }

        // Recycle one row builder across the visible slice only. With thousands
        // of cities a keystroke now creates ~12 rows instead of thousands.
        for &opt_index in &filtered[range] {
            let Some(opt) = options.get(opt_index) else {
                continue;
            };
            let is_active = active_option_index == Some(opt_index);
            menu_list = menu_list.child(searchable_row(
                &id,
                theme,
                opt_index,
                opt,
                is_active,
                accent,
                on_close.clone(),
            ));
        }

        wrap = wrap.child(dropdown_popover(menu_list, 32.0));
    }

    wrap
}

#[allow(dead_code)]
pub fn segmented_track(theme: &UiTheme, items: Vec<AnyElement>) -> Div {
    let mut track = div()
        .flex()
        .items_center()
        .gap(px(2.0))
        .p(px(2.5))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgba(theme.surface_muted))
        .border_1()
        .border_color(rgba(theme.border))
        .flex_shrink_0();
    for item in items {
        track = track.child(item);
    }
    track
}

#[allow(dead_code)]
pub fn segmented_item(
    id: impl Into<gpui::ElementId>,
    theme: &UiTheme,
    label: impl Into<SharedString>,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let bg = if active { theme.surface } else { 0x00000000 };
    let fg = if active {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let hover_bg = if active {
        theme.surface
    } else {
        theme.surface_hover
    };
    div()
        .id(id)
        .px(px(9.0))
        .py(px(5.0))
        .rounded(px(CONTROL_RADIUS - 3.0))
        .border_1()
        .border_color(rgba(if active { theme.border } else { 0 }))
        .bg(rgba(bg))
        .text_size(px(fonts::scaled_size(12.0)))
        .font_weight(if active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        })
        .text_color(rgba(fg))
        .cursor_pointer()
        .tab_index(0)
        .focus({
            let accent = theme.accent;
            move |style| style.border_color(rgba(accent))
        })
        .hover(move |s| s.bg(rgba(hover_bg)))
        .active(|s| s.opacity(0.84))
        .on_click(on_click)
        .child(label.into())
        .into_any_element()
}

/// Interactive live slider supporting click, continuous drag, and keyboard arrow adjustments.
pub fn live_slider<F>(
    id_prefix: &'static str,
    theme: &UiTheme,
    ratio: f32,
    steps: usize,
    value_text: impl Into<SharedString>,
    make_handler: F,
) -> Div
where
    F: Fn(usize) -> std::rc::Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>,
{
    let clamped = ratio.clamp(0.0, 1.0);
    let track_w = 132.0_f32;
    let fill_w = (track_w * clamped).round().clamp(0.0, track_w);
    let thumb_x = (fill_w - 7.0).clamp(0.0, track_w - 14.0);
    let count = steps.max(2);
    let max_idx = count - 1;
    let cur_idx = ((clamped * max_idx as f32).round() as usize).min(max_idx);

    // Cap pointer hit-slices to 40 cells (~3.3px each on the 132px track) so high-step sliders
    // like 15..240 Hz do not build hundreds of DOM nodes while keyboard arrows still step by 1.
    let slices = count.min(40);
    let mut stops = div().absolute().inset_0().flex().items_center();
    for s in 0..slices {
        let mapped_idx = if slices == count {
            s
        } else {
            ((s as f32 / (slices - 1) as f32) * max_idx as f32).round() as usize
        }
        .min(max_idx);
        let cb_down = make_handler(mapped_idx);
        let cb_move = cb_down.clone();
        stops = stops.child(
            div()
                .id(SharedString::from(format!("{id_prefix}-{s}")))
                .flex_1()
                .h_full()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_, win, cx| cb_down(win, cx))
                .on_mouse_move(move |ev, win, cx| {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        cb_move(win, cx);
                    }
                }),
        );
    }

    let cb_dec = make_handler(cur_idx.saturating_sub(1));
    let cb_inc = make_handler((cur_idx + 1).min(max_idx));
    let cb_min = make_handler(0);
    let cb_max = make_handler(max_idx);

    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .flex_shrink_0()
        .child(
            div()
                .id(SharedString::from(format!("{id_prefix}-track")))
                .relative()
                .w(px(track_w))
                .h(px(22.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(rgba(0))
                .tab_index(0)
                .focus({
                    let accent = theme.accent;
                    move |style| style.border_color(rgba(accent))
                })
                .on_key_down(
                    move |ev: &KeyDownEvent, win, cx| match ev.keystroke.key.as_str() {
                        "left" | "down" => cb_dec(win, cx),
                        "right" | "up" => cb_inc(win, cx),
                        "home" => cb_min(win, cx),
                        "end" => cb_max(win, cx),
                        _ => {}
                    },
                )
                .flex()
                .items_center()
                .child(
                    div()
                        .w_full()
                        .h(px(5.0))
                        .rounded(px(3.0))
                        .bg(rgba(theme.switch_off))
                        .overflow_hidden()
                        .child(
                            div()
                                .w(px(fill_w))
                                .h_full()
                                .rounded(px(3.0))
                                .bg(rgba(theme.accent)),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(thumb_x))
                        .w(px(14.0))
                        .h(px(14.0))
                        .rounded(px(7.0))
                        .bg(rgba(theme.switch_thumb))
                        .border_1()
                        .border_color(rgba(theme.accent)),
                )
                .child(stops),
        )
        .child(
            div()
                .w(px(SLIDER_VALUE_WIDTH))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_end()
                .whitespace_nowrap()
                .text_size(px(fonts::scaled_size(12.0)))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgba(theme.text_primary))
                .child(value_text.into()),
        )
}

pub fn stepper(
    id_minus: impl Into<gpui::ElementId>,
    id_plus: impl Into<gpui::ElementId>,
    theme: &UiTheme,
    value_text: impl Into<SharedString>,
    on_minus: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_plus: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Div {
    let hover = theme.surface_hover;
    div()
        .min_w(px(CONTROL_RAIL_WIDTH))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(4.0))
        .p(px(2.0))
        .rounded(px(8.0))
        .bg(rgba(theme.surface_muted))
        .border_1()
        .border_color(rgba(theme.border))
        .flex_shrink_0()
        .child(
            div()
                .id(id_minus)
                .w(px(28.0))
                .h(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .border_1()
                .border_color(rgba(0))
                .tab_index(0)
                .focus({
                    let accent = theme.accent;
                    move |style| style.border_color(rgba(accent))
                })
                .text_size(px(fonts::scaled_size(13.0)))
                .text_color(rgba(theme.text_primary))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(hover)))
                .active(|s| s.opacity(0.82))
                .on_click(on_minus)
                .child("-"),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(46.0))
                .flex()
                .items_center()
                .justify_center()
                .whitespace_nowrap()
                .text_size(px(fonts::scaled_size(11.5)))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgba(theme.text_primary))
                .child(value_text.into()),
        )
        .child(
            div()
                .id(id_plus)
                .w(px(28.0))
                .h(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .border_1()
                .border_color(rgba(0))
                .tab_index(0)
                .focus({
                    let accent = theme.accent;
                    move |style| style.border_color(rgba(accent))
                })
                .text_size(px(fonts::scaled_size(13.0)))
                .text_color(rgba(theme.text_primary))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(hover)))
                .active(|s| s.opacity(0.82))
                .on_click(on_plus)
                .child("+"),
        )
}

pub fn sidebar_nav_item(
    id: impl Into<gpui::ElementId>,
    theme: &UiTheme,
    glyph: &'static str,
    label: impl Into<SharedString>,
    count_badge: Option<String>,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let bg = if active {
        theme.surface_muted
    } else {
        0x00000000
    };
    let fg = if active {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let icon_col = if active {
        theme.accent
    } else {
        theme.text_secondary
    };
    let hover = if active {
        theme.surface_muted
    } else {
        theme.surface_hover
    };

    let mut row = div()
        .id(id)
        .relative()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .px(px(10.0))
        .min_h(px(38.0))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(rgba(0))
        .bg(rgba(bg))
        .tab_index(0)
        .focus({
            // Subtle keyboard ring: a clicked (and therefore focused) active tab must not look
            // stuck in a bright focus state; the accent pill carries the selection instead.
            let ring = theme.accent_soft;
            move |style| style.border_color(rgba(ring))
        })
        .cursor_pointer()
        .hover(move |s| s.bg(rgba(hover)))
        .active(|s| s.opacity(0.86))
        .on_click(on_click)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(9.0))
                .min_w_0()
                .child(icon_glyph(glyph, 14.0, icon_col))
                .child(
                    div()
                        .text_size(px(fonts::scaled_size(13.0)))
                        .font_weight(if active {
                            gpui::FontWeight::SEMIBOLD
                        } else {
                            gpui::FontWeight::MEDIUM
                        })
                        .text_color(rgba(fg))
                        .child(label.into()),
                ),
        );

    if let Some(cnt) = count_badge {
        row = row.child(badge(theme, cnt, active));
    }
    if active {
        row = row.child(
            div()
                .absolute()
                .left(px(1.0))
                .top(px(11.0))
                .bottom(px(11.0))
                .w(px(3.0))
                .rounded(px(1.5))
                .bg(rgba(theme.accent)),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use super::{
        DropdownOption, Duration, Instant, SearchDebounce, SearchableDropdownState, SharedString,
        StepMode, clamp_scroll_top, compute_popover_placement, filter_dropdown_options,
        fold_search_text, no_results_message, options_fingerprint, scroll_top_for_active,
        step_index, visible_row_range,
    };

    fn options(labels: &[&str]) -> Vec<DropdownOption> {
        labels
            .iter()
            .map(|label| DropdownOption {
                label: SharedString::from((*label).to_owned()),
                flag_path: None,
                glyph: None,
                selected: false,
                on_select: std::rc::Rc::new(|_, _| {}),
            })
            .collect()
    }

    #[test]
    fn filter_is_case_insensitive_and_matches_every_token() {
        let opts = options(&["Ankara", "İstanbul", "İzmir", "Antalya"]);
        assert_eq!(filter_dropdown_options(&opts, ""), vec![0, 1, 2, 3]);
        assert_eq!(filter_dropdown_options(&opts, "ANKARA"), vec![0]);
        assert_eq!(filter_dropdown_options(&opts, "  an  "), vec![0, 1, 3]);
        assert_eq!(filter_dropdown_options(&opts, "an ar"), vec![0]);
        assert!(filter_dropdown_options(&opts, "istanbul").is_empty());
    }

    #[test]
    fn turkish_dotless_and_dotted_i_are_distinct() {
        let opts = options(&["İstanbul", "Istanbul", "Isparta", "Izmir"]);
        assert_eq!(fold_search_text("İ"), "İ");
        assert_eq!(fold_search_text("i\u{307}"), "İ");
        assert_eq!(fold_search_text("ı"), "ı");
        assert_eq!(filter_dropdown_options(&opts, "İstanbul"), vec![0]);
        assert_eq!(filter_dropdown_options(&opts, "i̇stanbul"), vec![0]);
        assert!(filter_dropdown_options(&opts, "ıstanbul").is_empty());
        assert_eq!(filter_dropdown_options(&opts, "istanbul"), vec![1]);
        assert!(filter_dropdown_options(&opts, "ısparta").is_empty());
    }

    #[test]
    fn virtual_slice_covers_viewport_with_overscan_clamped_at_both_ends() {
        assert_eq!(visible_row_range(0, 0, 8, 2), 0..0);
        assert_eq!(visible_row_range(1000, 0, 8, 2), 0..10);
        assert_eq!(visible_row_range(1000, 100, 8, 2), 98..110);
        assert_eq!(visible_row_range(1000, 999, 8, 2), 990..1000);
        assert_eq!(visible_row_range(5, 0, 8, 2), 0..5);
        assert_eq!(visible_row_range(1000, 10, 0, 2), 0..0);
        // Exhaustive invariants: bounded work regardless of list size or offset.
        for total in [1usize, 8, 9, 100, 5000] {
            for top in [0usize, 1, total / 2, total] {
                let range = visible_row_range(total, top, 8, 2);
                assert!(range.start <= range.end && range.end <= total);
                assert!(range.end - range.start <= 12);
            }
        }
    }

    #[test]
    fn virtual_scroll_tracks_active_row_without_jitter() {
        assert_eq!(scroll_top_for_active(3, 0, 8), 0);
        assert_eq!(scroll_top_for_active(8, 0, 8), 1);
        assert_eq!(scroll_top_for_active(2, 5, 8), 2);
        assert_eq!(scroll_top_for_active(5, 5, 8), 5);
        assert_eq!(scroll_top_for_active(0, 0, 0), 0);
    }

    #[test]
    fn search_debounce_settles_before_reusing_the_filter() {
        let now = Instant::now();
        let mut debounce = SearchDebounce::new(Duration::from_millis(120));
        assert!(debounce.observe("a", now));
        assert!(!debounce.observe("a", now + Duration::from_millis(10)));
        assert!(!debounce.settled(now + Duration::from_millis(10)));
        assert!(debounce.observe("an", now + Duration::from_millis(20)));
        assert!(!debounce.settled(now + Duration::from_millis(139)));
        assert!(debounce.settled(now + Duration::from_millis(140)));
        assert!(!debounce.observe("an", now + Duration::from_millis(200)));
    }

    #[test]
    fn no_results_uses_the_shared_search_no_results_key() {
        let message = no_results_message();
        assert_eq!(&message[..], super::loc::t("SearchNoResults"));
        assert!(!message.is_empty());
    }

    #[test]
    fn index_stepping_preserves_wrap_and_clamp_policies() {
        assert_eq!(step_index(0, 5, -1, StepMode::Wrap), 4);
        assert_eq!(step_index(4, 5, 1, StepMode::Wrap), 0);
        assert_eq!(step_index(0, 5, -1, StepMode::Clamp), 0);
        assert_eq!(step_index(4, 5, 1, StepMode::Clamp), 4);
        assert_eq!(step_index(2, 0, 1, StepMode::Wrap), 0);
    }

    #[test]
    fn option_fingerprint_detects_same_length_replacement() {
        let before = options(&["Ankara", "Izmir"]);
        let mut after = options(&["Ankara", "Izmir"]);
        assert_eq!(options_fingerprint(&before), options_fingerprint(&after));
        after[1].label = SharedString::from("İzmir".to_owned());
        assert_ne!(options_fingerprint(&before), options_fingerprint(&after));
        let mut selected = options(&["Ankara", "Izmir"]);
        selected[0].selected = true;
        assert_ne!(options_fingerprint(&before), options_fingerprint(&selected));
    }

    #[test]
    fn scroll_top_uses_one_bound_for_keyboard_and_wheel() {
        assert_eq!(clamp_scroll_top(50, 100, 8), 50);
        assert_eq!(clamp_scroll_top(120, 100, 8), 92);
        assert_eq!(clamp_scroll_top(3, 5, 8), 0);
        assert_eq!(clamp_scroll_top(0, 0, 8), 0);
    }

    #[test]
    fn dropdown_state_clamps_navigation_and_tracks_active_option() {
        let opts = options(&["Ankara", "Antalya", "İstanbul"]);
        let mut state = SearchableDropdownState::new();
        state.set_query("an".to_owned());
        assert_eq!(state.filtered_indices(&opts), vec![0, 1, 2]);
        assert_eq!(state.active_option(&opts), Some(0));
        state.move_active(1, 3);
        assert_eq!(state.active_option(&opts), Some(1));
        state.move_active(5, 3);
        assert_eq!(state.active, 2);
        state.move_active(-5, 3);
        assert_eq!(state.active, 0);
        state.move_active(-1, 0);
        assert_eq!(state.active, 0);
        state.set_query("missing".to_owned());
        assert_eq!(state.active_option(&opts), None);
        state.clear();
        assert_eq!(state.query, "");
        assert_eq!(state.active, 0);
    }

    #[test]
    fn switch_thumb_is_muted_only_when_off() {
        let theme = super::UiTheme::new(true);
        assert_eq!(super::switch_thumb_color(&theme, true), theme.switch_thumb);
        let off = super::switch_thumb_color(&theme, false);
        assert_ne!(off, theme.switch_thumb);
        assert_eq!(off & 0xffff_ff00, theme.switch_thumb & 0xffff_ff00);
        assert!((off & 0xff) < (theme.switch_thumb & 0xff));
    }

    #[test]
    fn settings_theme_distinguishes_glass_solid_none_and_true_black() {
        use super::UiTheme;
        use opentaskbar_core::settings::SurfaceBackground;
        // `UiTheme` reads the live palette; serialize with the theme tests.
        let _guard = crate::design::theme::theme_test_guard();
        let glass = UiTheme::for_settings_background(true, SurfaceBackground::Glass);
        let solid = UiTheme::for_settings_background(false, SurfaceBackground::Solid);
        let none = UiTheme::for_settings_background(true, SurfaceBackground::None);
        let black = UiTheme::for_settings_background(false, SurfaceBackground::TrueBlack);
        assert_eq!(glass.canvas_bg, 0x0000_0000);
        assert_ne!(solid.canvas_bg & 0xff, 0);
        assert_eq!(none.canvas_bg, 0x0000_0000);
        assert_eq!(black.canvas_bg, 0x0000_00ff);
        assert_eq!(black.sidebar_bg, 0x0000_00ff);
        assert_eq!(black.text_primary, 0xf4f4_f5ff);
    }

    #[test]
    fn settings_theme_follows_applied_pack_tokens() {
        use super::UiTheme;
        use crate::design::theme::{
            ThemeManager, apply_pack_tokens_to_palette, apply_token_overrides,
            has_active_theme_customization, theme_test_guard,
        };
        use crate::design::tokens::{Rgba, TokenMap, TokenValue};
        use opentaskbar_core::settings::{AppTheme, SurfaceBackground};

        let _guard = theme_test_guard();
        ThemeManager::apply(AppTheme::Dark, None);
        apply_token_overrides(TokenMap::new());
        apply_pack_tokens_to_palette(TokenMap::new());
        let stock_solid = UiTheme::for_settings_background(true, SurfaceBackground::Solid);
        let stock_glass = UiTheme::for_settings_background(true, SurfaceBackground::Glass);

        let mut pack = TokenMap::new();
        pack.insert(
            "AccentBrush".to_owned(),
            TokenValue::Solid(Rgba::from_rgb(0x12, 0x34, 0x56)),
        );
        pack.insert(
            "SettingsBackground".to_owned(),
            TokenValue::Solid(Rgba::from_rgb(0x20, 0x30, 0x40)),
        );
        apply_pack_tokens_to_palette(&pack);
        assert!(has_active_theme_customization());

        let solid = UiTheme::for_settings_background(true, SurfaceBackground::Solid);
        let glass = UiTheme::for_settings_background(true, SurfaceBackground::Glass);
        let black = UiTheme::for_settings_background(true, SurfaceBackground::TrueBlack);

        assert_eq!(solid.accent, 0x1234_56ff);
        assert_ne!(solid.accent, stock_solid.accent);
        assert_eq!(solid.canvas_bg, 0x2030_40ff);
        assert_ne!(solid.canvas_bg, stock_solid.canvas_bg);
        assert_eq!(glass.accent, 0x1234_56ff);
        assert_ne!(glass.accent, stock_glass.accent);
        assert_eq!(glass.canvas_bg, 0x0000_0000);
        assert_eq!(black.canvas_bg, 0x0000_00ff);
        assert_eq!(black.sidebar_bg, 0x0000_00ff);
        assert_eq!(black.text_primary, 0xf4f4_f5ff);
        assert_eq!(black.accent, 0x1234_56ff);

        apply_pack_tokens_to_palette(TokenMap::new());
        apply_token_overrides(TokenMap::new());
    }

    #[test]
    fn popover_opens_downward_when_there_is_room_below() {
        // Trigger near the top of the 740 px Settings window, 240 px menu.
        let (x, y, flipped) = compute_popover_placement(
            120.0, 900.0, 32.0, 216.0, 240.0, 960.0, 740.0, 4.0, 8.0, 36.0,
        );
        assert!(!flipped);
        assert_eq!(y, 120.0 + 32.0 + 4.0);
        assert_eq!(x, 900.0 - 216.0);
        assert!(y + 240.0 <= 740.0 - 8.0);
    }

    #[test]
    fn popover_flips_upward_near_the_bottom_without_covering_the_trigger() {
        // "Tüm ekranlar" style trigger at the bottom of a 740 px window.
        let trigger_top = 680.0;
        let (x, y, flipped) = compute_popover_placement(
            trigger_top,
            900.0,
            32.0,
            216.0,
            240.0,
            960.0,
            740.0,
            4.0,
            8.0,
            36.0,
        );
        assert!(flipped);
        assert_eq!(y, trigger_top - 4.0 - 240.0);
        // Bottom edge of the menu stays above the trigger (no overlap).
        assert!(y + 240.0 <= trigger_top);
        assert!(y >= 36.0 + 8.0);
        assert_eq!(x, 900.0 - 216.0);

        // Same behaviour for the 216 px searchable menu under a 30 px trigger.
        let (_, y, flipped) = compute_popover_placement(
            650.0, 900.0, 30.0, 196.0, 216.0, 960.0, 740.0, 4.0, 8.0, 36.0,
        );
        assert!(flipped);
        assert!(y + 216.0 <= 650.0);
    }

    #[test]
    fn popover_stays_inside_a_small_viewport() {
        let (vp_w, vp_h) = (540.0, 540.0);
        let (gap, margin, clearance) = (4.0, 8.0, 36.0);
        for trigger_top in [40.0_f32, 120.0, 250.0, 380.0, 500.0] {
            for trigger_right in [60.0_f32, 200.0, 540.0, 700.0] {
                let (x, y, _) = compute_popover_placement(
                    trigger_top,
                    trigger_right,
                    32.0,
                    240.0,
                    240.0,
                    vp_w,
                    vp_h,
                    gap,
                    margin,
                    clearance,
                );
                assert!(y >= clearance + margin, "under titlebar at {trigger_top}");
                assert!(
                    y + 240.0 <= vp_h - margin,
                    "clipped bottom at {trigger_top}"
                );
                assert!(x >= margin, "off left edge at {trigger_right}");
                assert!(
                    x + 240.0 <= vp_w - margin,
                    "off right edge at {trigger_right}"
                );
            }
        }
        // A menu taller than the whole viewport pins to the titlebar instead of escaping it.
        let (_, y, _) = compute_popover_placement(
            300.0, 400.0, 32.0, 240.0, 900.0, vp_w, vp_h, gap, margin, clearance,
        );
        assert_eq!(y, clearance + margin);
    }
}
