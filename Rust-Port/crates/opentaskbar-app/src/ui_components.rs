//! Reusable iOS + shadcn/ui GPUI design system components for OpenTaskBar.
//!
//! Features:
//! - Semantic Light/Dark tokens
//! - Strict `min_w_0()` / `w_full()` / `shrink_0()` layout discipline (zero horizontal overflow)
//! - Native Windows 11 `Segoe Fluent Icons` glyphs
//! - Grouped inset `section_card`, concise `setting_row`, `ios_switch`, `segmented_track`,
//!   `stepper`, `button`, `badge`, `sidebar_nav_item`, and `scrollbar_overlay`.

use gpui::{AnyElement, Div, SharedString, div, prelude::*, px, rgba};

/// Semantic iOS + shadcn/ui color palette for GPUI surfaces.
#[derive(Clone, Copy)]
pub struct UiTheme {
    pub canvas_bg: u32,
    pub sidebar_bg: u32,
    pub surface: u32,
    pub surface_muted: u32,
    pub surface_hover: u32,
    pub dock_preview_bg: u32,
    pub border: u32,
    pub separator: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub text_muted: u32,
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
        if is_dark {
            Self {
                canvas_bg: 0x09090bff,
                sidebar_bg: 0x111114ff,
                surface: 0x18181bff,
                surface_muted: 0x222227ff,
                surface_hover: 0x292930ff,
                dock_preview_bg: 0x11141dff,
                border: 0xffffff16,
                separator: 0xffffff10,
                text_primary: 0xfafafaff,
                text_secondary: 0xa1a1aaff,
                text_muted: 0x71717aff,
                accent: 0x0a84ffff,
                accent_soft: 0x0a84ff26,
                accent_fg: 0xffffffff,
                switch_off: 0x39393dff,
                switch_on: 0x30d158ff,
                switch_thumb: 0xffffffff,
                destructive: 0xff453aff,
                destructive_soft: 0xff453a20,
                scrollbar_thumb: 0xffffff38,
            }
        } else {
            Self {
                canvas_bg: 0xf2f2f7ff,
                sidebar_bg: 0xf9f9fbff,
                surface: 0xffffffff,
                surface_muted: 0xefeff4ff,
                surface_hover: 0xe5e5eaff,
                dock_preview_bg: 0x1c202bff,
                border: 0x00000014,
                separator: 0x0000000e,
                text_primary: 0x09090bff,
                text_secondary: 0x52525bff,
                text_muted: 0x71717aff,
                accent: 0x007affff,
                accent_soft: 0x007aff1c,
                accent_fg: 0xffffffff,
                switch_off: 0xd1d1d6ff,
                switch_on: 0x34c759ff,
                switch_thumb: 0xffffffff,
                destructive: 0xff3b30ff,
                destructive_soft: 0xff3b3018,
                scrollbar_thumb: 0x00000038,
            }
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
        .rounded(px(12.0))
        .bg(rgba(theme.surface))
        .border_1()
        .border_color(rgba(theme.border))
        .overflow_hidden();

    let count = rows.len();
    for (idx, row) in rows.into_iter().enumerate() {
        card = card.child(row);
        if idx + 1 < count {
            card = card.child(div().h(px(1.0)).mx(px(14.0)).bg(rgba(theme.separator)));
        }
    }

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .px(px(4.0))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgba(theme.text_secondary))
                .child(title.into()),
        )
        .child(card)
}

/// Concise two-column setting row with strict `min_w_0` + `shrink_0` anti-overflow protection.
pub fn setting_row(
    theme: &UiTheme,
    icon: Option<&'static str>,
    title: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    let mut left = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(10.0))
        .pr(px(12.0))
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

    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.0))
        .py(px(10.0))
        .child(left)
        .child(div().flex_shrink_0().child(control))
}

/// Tactile iOS-style switch pill (`42x24`).
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
    let thumb_left = if checked { px(20.0) } else { px(2.0) };

    div()
        .id(id)
        .w(px(42.0))
        .h(px(24.0))
        .rounded(px(12.0))
        .bg(rgba(track_bg))
        .cursor_pointer()
        .flex()
        .items_center()
        .on_click(on_click)
        .child(
            div()
                .w(px(20.0))
                .h(px(20.0))
                .rounded(px(10.0))
                .bg(rgba(theme.switch_thumb))
                .ml(thumb_left),
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
        .text_size(px(10.5))
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
    let (bg, hover_bg, fg, border) = match variant {
        ButtonVariant::Primary => (theme.accent, theme.accent, theme.accent_fg, theme.accent),
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
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(7.0))
        .bg(rgba(bg))
        .border_1()
        .border_color(rgba(border))
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgba(fg))
        .cursor_pointer()
        .flex_shrink_0()
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
        .rounded(px(8.0))
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
        .py(px(4.0))
        .rounded(px(6.0))
        .bg(rgba(bg))
        .text_size(px(11.5))
        .font_weight(if active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        })
        .text_color(rgba(fg))
        .cursor_pointer()
        .hover(move |s| s.bg(rgba(hover_bg)))
        .on_click(on_click)
        .child(label.into())
        .into_any_element()
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
                .w(px(24.0))
                .h(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
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
                .w(px(24.0))
                .h(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
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
) -> impl IntoElement {
    let bg = if active {
        theme.accent_soft
    } else {
        0x00000000
    };
    let fg = if active {
        theme.accent
    } else {
        theme.text_primary
    };
    let icon_col = if active {
        theme.accent
    } else {
        theme.text_secondary
    };
    let hover = if active {
        theme.accent_soft
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
        .py(px(7.5))
        .rounded(px(8.0))
        .bg(rgba(bg))
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
                        .text_size(px(12.5))
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
