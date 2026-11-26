//! Reusable iOS + shadcn/ui GPUI design system components for OpenTaskBar.
//!
//! Features:
//! - Semantic Light/Dark tokens
//! - Strict `min_w_0()` / `w_full()` / `shrink_0()` layout discipline (zero horizontal overflow)
//! - Native Windows 11 `Segoe Fluent Icons` glyphs
//! - Grouped inset `section_card`, concise `setting_row`, `ios_switch`, `segmented_track`,
//!   `stepper`, `button`, `badge`, `sidebar_nav_item`, and `scrollbar_overlay`.

use gpui::{
    AnyElement, Div, MouseButton, ScrollAnchor, SharedString, div, prelude::*, px, rgba,
};

use crate::design::{self, gpui_rgba};

const PANEL_RADIUS: f32 = 14.0;
const CONTROL_RADIUS: f32 = 8.0;
const ROW_INSET: f32 = 16.0;

/// Unified neutral Zinc/Carbon palette for GPUI surfaces (shared with Dock & Flyout).
#[derive(Clone, Copy)]
pub struct UiTheme {
    pub canvas_bg: u32,
    pub sidebar_bg: u32,
    pub surface: u32,
    pub surface_muted: u32,
    pub surface_hover: u32,
    pub dock_preview_bg: u32,
    pub dock_preview_fg: u32,
    pub dock_preview_sub: u32,
    pub border: u32,
    pub border_width: f32,
    pub separator: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub accent: u32,
    pub accent_soft: u32,
    pub accent_fg: u32,
    pub switch_off: u32,
    pub switch_on: u32,
    pub switch_thumb: u32,
    pub destructive: u32,
    pub destructive_soft: u32,
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
            dock_preview_bg: gpui_rgba(p.dock_pill, 0xf2),
            dock_preview_fg: gpui_rgba(p.text_primary, 0xff),
            dock_preview_sub: gpui_rgba(p.text_secondary, 0xff),
            border: if is_dark { 0xffffff16 } else { 0x00000014 },
            border_width: 1.0,
            separator: if is_dark { 0xffffff10 } else { 0x0000000e },
            text_primary: gpui_rgba(p.text_primary, 0xff),
            text_secondary: gpui_rgba(p.text_secondary, 0xff),
            accent: gpui_rgba(p.accent, 0xff),
            accent_soft: gpui_rgba(p.accent, 0x22),
            accent_fg: 0xffffffff,
            switch_off: gpui_rgba(p.switch_off, 0xff),
            switch_on: gpui_rgba(p.accent, 0xff),
            switch_thumb: 0xffffffff,
            destructive: gpui_rgba(p.danger, 0xff),
            destructive_soft: gpui_rgba(p.danger, 0x22),
            scrollbar_thumb: if is_dark { 0xffffff38 } else { 0x00000038 },
        }
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
        .border_color(rgba(theme.border))
        .overflow_hidden();

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
                .text_size(px(13.0))
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
    setting_row_inner(theme, false, icon, title, control)
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
    setting_row_inner(theme, highlighted, icon, title, control)
        .id(id)
        .anchor_scroll(anchor)
}

fn setting_row_inner(
    theme: &UiTheme,
    highlighted: bool,
    icon: Option<&'static str>,
    title: impl Into<SharedString>,
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

    left = left.child(
        div()
            .flex_1()
            .min_w_0()
            .text_size(px(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgba(theme.text_primary))
            .overflow_hidden()
            .child(title.into()),
    );

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
        row = row
            .bg(rgba(theme.accent_soft))
            .child(
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

    div()
        .id(id)
        .w(px(36.0))
        .h(px(20.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(rgba(track_bg))
        .bg(rgba(track_bg))
        .tab_index(0)
        .focus({
            let accent = theme.accent;
            move |style| style.border_color(rgba(accent))
        })
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
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgba(fg))
        .flex_shrink_0()
        .child(text.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
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
        ButtonVariant::Secondary => (
            theme.surface_muted,
            theme.surface_hover,
            theme.text_primary,
            theme.border,
        ),
        ButtonVariant::Destructive => (
            theme.destructive_soft,
            theme.destructive_soft,
            theme.destructive,
            theme.destructive_soft,
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
        .text_size(px(12.0))
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
        .on_click(on_click)
        .child(label.into())
}

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
        .text_size(px(12.0))
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
        .on_click(on_click)
        .child(label.into())
        .into_any_element()
}

/// Interactive live slider supporting both click and continuous left-button pointer drag.
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

    let mut stops = div().absolute().inset_0().flex().items_center();
    for i in 0..count {
        let cb_down = make_handler(i);
        let cb_move = cb_down.clone();
        stops = stops.child(
            div()
                .id(SharedString::from(format!("{id_prefix}-{i}")))
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

    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .flex_shrink_0()
        .child(
            div()
                .relative()
                .w(px(track_w))
                .h(px(22.0))
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
                .text_size(px(12.0))
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
                .text_size(px(13.0))
                .text_color(rgba(theme.text_primary))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(hover)))
                .on_click(on_minus)
                .child("-"),
        )
        .child(
            div()
                .min_w(px(46.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.5))
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
                .text_size(px(13.0))
                .text_color(rgba(theme.text_primary))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(hover)))
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
                        .text_size(px(13.0))
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
