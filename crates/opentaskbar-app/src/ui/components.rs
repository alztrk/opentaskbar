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

/// Rows kept alive in a virtualized searchable dropdown at once. A 10 000-city
/// list costs ~12 row elements per keystroke instead of 10 000.
const SEARCH_VIEWPORT_ROWS: usize = 8;
const SEARCH_OVERSCAN_ROWS: usize = 2;
const SEARCH_ROW_H_PX: f32 = 30.0;
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

impl UiTheme {
    pub fn new(is_dark: bool) -> Self {
        let p = design::palette(is_dark);
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

    /// Settings window theme: opaque in Solid/None modes; in Glass mode lets the EXACT same
    /// unified Clear Crystal `SpatialGlassHost` shine cleanly across the whole window (zero
    /// opaque titlebar/sidebar slabs) with crisp glass cards and high-contrast ink.
    pub fn for_settings(is_dark: bool, glass_mode: bool) -> Self {
        let mut t = Self::new(is_dark);
        if !glass_mode {
            return t;
        }
        if is_dark {
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

/// Crisp native Windows 11 Segoe Fluent Icons glyph.
pub fn icon_glyph(glyph: &'static str, size_px: f32, color: u32) -> Div {
    div()
        .font_family("Segoe Fluent Icons")
        .text_size(px(size_px))
        .text_color(rgba(color))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .child(glyph)
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
                .bg(rgba(theme.switch_thumb))
                .ml(thumb_left - px(1.0)),
        )
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
    #[allow(dead_code)]
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
        .cursor_pointer()
        .flex_shrink_0()
        .tab_index(0)
        .focus({
            let focus = theme.text_primary;
            move |style| style.border_color(rgba(focus))
        })
        .hover(move |s| s.bg(rgba(hover_bg)))
        .active(|s| s.opacity(0.84))
        .on_click(on_click)
        .child(label.into())
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
        .min_w(px(148.0))
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
            .max_h(px(240.0))
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

        for (idx, opt) in options.into_iter().enumerate() {
            let opt_bg = if opt.selected {
                theme.accent_soft
            } else {
                0x00000000
            };
            let opt_fg = if opt.selected {
                theme.accent
            } else {
                theme.text_primary
            };
            let opt_hover = if opt.selected {
                theme.accent_soft
            } else {
                theme.surface_hover
            };
            let cb = opt.on_select;
            let mut left = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .min_w_0()
                .overflow_hidden();
            if let Some(path) = opt.flag_path {
                left = left.child(
                    img(path)
                        .w(px(18.0))
                        .h(px(13.0))
                        .rounded(px(2.5))
                        .flex_shrink_0(),
                );
            } else if let Some(g) = opt.glyph {
                left = left.child(icon_glyph(g, 12.0, opt_fg));
            }
            left = left.child(
                div()
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(if opt.selected {
                        gpui::FontWeight::SEMIBOLD
                    } else {
                        gpui::FontWeight::MEDIUM
                    })
                    .text_color(rgba(opt_fg))
                    .overflow_hidden()
                    .child(opt.label),
            );

            menu_list = menu_list.child(
                div()
                    .id(SharedString::from(format!("dd-opt-{id}-{idx}")))
                    .w_full()
                    .min_h(px(30.0))
                    .px(px(9.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .bg(rgba(opt_bg))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgba(opt_hover)))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        cb(window, cx);
                    })
                    .child(left)
                    .when(opt.selected, |row| {
                        row.child(icon_glyph("\u{E73E}", 11.0, accent))
                    }),
            );
        }

        let popover = deferred(
            div()
                .occlude()
                .absolute()
                .top(px(36.0))
                .right_0()
                .child(menu_list),
        )
        .with_priority(1);
        wrap = wrap.child(popover);
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
            .max_h(px(240.0))
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

        for (idx, opt) in options.into_iter().enumerate() {
            let opt_bg = if opt.selected {
                theme.accent_soft
            } else {
                0x00000000
            };
            let opt_fg = if opt.selected {
                theme.accent
            } else {
                theme.text_primary
            };
            let opt_hover = if opt.selected {
                theme.accent_soft
            } else {
                theme.surface_hover
            };
            let cb = opt.on_select;
            let mut left = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .min_w_0()
                .overflow_hidden();
            if let Some(g) = opt.glyph {
                left = left.child(icon_glyph(g, 12.0, opt_fg));
            }
            left = left.child(
                div()
                    .text_size(px(fonts::scaled_size(12.0)))
                    .font_weight(if opt.selected {
                        gpui::FontWeight::SEMIBOLD
                    } else {
                        gpui::FontWeight::MEDIUM
                    })
                    .text_color(rgba(opt_fg))
                    .overflow_hidden()
                    .child(opt.label),
            );

            menu_list = menu_list.child(
                div()
                    .id(SharedString::from(format!("icon-act-opt-{id}-{idx}")))
                    .w_full()
                    .min_h(px(30.0))
                    .px(px(9.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .bg(rgba(opt_bg))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgba(opt_hover)))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        cb(window, cx);
                    })
                    .child(left),
            );
        }

        let popover = deferred(
            div()
                .occlude()
                .absolute()
                .top(px(34.0))
                .right_0()
                .child(menu_list),
        )
        .with_priority(1);
        wrap = wrap.child(popover);
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
        if len == 0 {
            self.active = 0;
            return;
        }
        let current = self.active.min(len - 1) as isize;
        let next = current + delta;
        self.active = if next < 0 {
            0
        } else if next >= len as isize {
            len - 1
        } else {
            next as usize
        };
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

/// Callbacks supplied by the owning entity for a [`searchable_dropdown`].
pub struct SearchableDropdownHandlers {
    pub on_toggle: DropdownSelectHandler,
    /// All keys except Enter/Escape (which the component itself resolves against the
    /// filtered list) are forwarded here so the owner can update its text buffer.
    pub on_key_down: SearchableDropdownKeyHandler,
    pub on_close: DropdownSelectHandler,
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
            .is_none_or(|at| now.duration_since(at) >= self.interval)
    }
}

/// UI-thread state for one virtualized picker. Keyed by element id so the city
/// and country pickers never share a scroll offset or debounce window.
#[derive(Default)]
struct VirtualList {
    debounce: SearchDebounce,
    cached: Vec<usize>,
    cached_query: String,
    cached_options: usize,
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
#[allow(clippy::too_many_arguments)]
fn searchable_row(
    id: &str,
    theme: &UiTheme,
    opt_index: usize,
    opt: &DropdownOption,
    is_active: bool,
    accent: u32,
    close: DropdownSelectHandler,
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
        .id(SharedString::from(format!("sdd-opt-{id}-{opt_index}")))
        .w_full()
        .min_h(px(SEARCH_ROW_H_PX))
        .px(px(9.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .bg(rgba(opt_bg))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .cursor_pointer()
        .hover(move |s| s.bg(rgba(opt_hover)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            cb(window, cx);
            close(window, cx);
        })
        .child(left)
        .when(opt.selected, |row| {
            row.child(icon_glyph("\u{E73E}", 11.0, accent))
        })
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
        .min_w(px(148.0))
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
        let (filtered, range) = SEARCH_LISTS.with(|lists| {
            let mut lists = lists.borrow_mut();
            let slot = lists.entry(scroll_id.clone()).or_default();
            let changed = slot.debounce.observe(&state.query, now);
            if changed {
                slot.scroll_top = 0;
            }
            let settled = !changed && slot.debounce.settled(now);
            if !settled || slot.cached_query != state.query || slot.cached_options != options.len()
            {
                slot.cached = state.filtered_indices(options);
                slot.cached_query = state.query.clone();
                slot.cached_options = options.len();
            }
            slot.scroll_top =
                scroll_top_for_active(state.active, slot.scroll_top, SEARCH_VIEWPORT_ROWS)
                    .min(slot.cached.len().saturating_sub(1));
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
        let key_active = state.active;
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
                        let next_active = if event.keystroke.key == "down" {
                            (key_active + 1).min(len.saturating_sub(1))
                        } else {
                            key_active.saturating_sub(1)
                        };
                        SEARCH_LISTS.with(|lists| {
                            if let Some(slot) = lists.borrow_mut().get_mut(&key_scroll_id) {
                                slot.scroll_top = scroll_top_for_active(
                                    next_active,
                                    slot.scroll_top,
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
            .h(px(SEARCH_VIEWPORT_ROWS as f32 * SEARCH_ROW_H_PX + 36.0))
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
                    let max = slot.cached.len().saturating_sub(SEARCH_VIEWPORT_ROWS);
                    slot.scroll_top = (slot.scroll_top as isize + rows)
                        .clamp(0, max as isize)
                        .max(0) as usize;
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

        let popover = deferred(
            div()
                .occlude()
                .absolute()
                .top(px(36.0))
                .right_0()
                .child(menu_list),
        )
        .with_priority(1);
        wrap = wrap.child(popover);
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
                .min_w(px(46.0))
                .flex()
                .items_center()
                .justify_end()
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
        .flex()
        .items_center()
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
                .min_w(px(46.0))
                .flex()
                .items_center()
                .justify_center()
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
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .px(px(10.0))
        .min_h(px(38.0))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(rgba(if active { theme.accent_soft } else { 0 }))
        .bg(rgba(bg))
        .tab_index(0)
        .focus({
            let accent = theme.accent;
            move |style| style.border_color(rgba(accent))
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
    row
}

#[cfg(test)]
mod tests {
    use super::{
        DropdownOption, Duration, Instant, SearchDebounce, SearchableDropdownState, SharedString,
        filter_dropdown_options, fold_search_text, no_results_message, scroll_top_for_active,
        visible_row_range,
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
}
