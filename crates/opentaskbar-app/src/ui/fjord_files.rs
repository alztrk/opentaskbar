//! GPUI Fjord Files window (`Views/Windows/FjordFilesWindow.xaml.cs`).
//!
//! This PC + favourites + drives, asynchronous navigation with back/forward/up
//! and breadcrumbs, culture-aware filtering, the reference keyboard shortcuts,
//! clipboard/reveal actions and a cancellable preview pane.
//!
//! NOTE: `ui/mod.rs` registration and the `main.rs` `--files` dispatch belong to
//! the orchestrator (`fjord::activation::handle_cli` is the entry point).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    AnyElement, App, Application, Bounds, Context, FocusHandle, KeyDownEvent, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgba, size,
};
use opentaskbar_core::i18n as loc;

use crate::design::fonts;
use crate::fjord::{files, preview};
use crate::platform::win32;
use crate::ui::components::{ButtonVariant, UiTheme, button, icon_glyph};
use crate::ui::text_input::apply_keystroke;

actions!(
    fjord_files,
    [
        FocusLocation,
        FocusSearch,
        GoBack,
        GoForward,
        GoUp,
        TogglePreview,
        RefreshFolder,
        OpenSelection,
        QuickLook
    ]
);

const WIN_W: f32 = 1020.0;
const WIN_H: f32 = 690.0;
const ROW_H: f32 = 30.0;

/// Opens a file-manager window for `initial` (or This PC).
pub fn run_files_window(initial: Option<String>) {
    Application::new()
        .with_assets(fonts::EmbeddedAssets)
        .run(move |cx: &mut App| {
            let _ = fonts::load_bundled_fonts(cx);
            cx.bind_keys([
                gpui::KeyBinding::new("ctrl-l", FocusLocation, None),
                gpui::KeyBinding::new("ctrl-f", FocusSearch, None),
                gpui::KeyBinding::new("alt-left", GoBack, None),
                gpui::KeyBinding::new("alt-right", GoForward, None),
                gpui::KeyBinding::new("alt-up", GoUp, None),
                gpui::KeyBinding::new("f4", TogglePreview, None),
                gpui::KeyBinding::new("f5", RefreshFolder, None),
                gpui::KeyBinding::new("enter", OpenSelection, None),
                gpui::KeyBinding::new("space", QuickLook, None),
            ]);
            let bounds = Bounds::centered(None, size(px(WIN_W), px(WIN_H)), cx);
            let title = initial.clone().unwrap_or_else(|| loc::t("FjordFiles"));
            let _ = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(720.0), px(480.0))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(SharedString::from(format!("Fjord Files — {title}"))),
                        appears_transparent: true,
                        traffic_light_position: None,
                    }),
                    ..Default::default()
                },
                |_window, cx| cx.new(|cx| FjordFilesView::new(initial.clone(), cx)),
            );
            cx.activate(true);
        });
}

struct FjordFilesView {
    history: files::History,
    folder: Option<String>,
    entries: Vec<files::FileEntry>,
    visible: Vec<usize>,
    selected: Option<usize>,
    query: String,
    location: String,
    status: String,
    breadcrumb: String,
    loading: bool,
    preview_open: bool,
    quick_look: bool,
    preview: preview::Preview,
    navigation_cancel: Arc<AtomicBool>,
    preview_cancel: Arc<AtomicBool>,
    location_focus: FocusHandle,
    search_focus: FocusHandle,
    list_focus: FocusHandle,
    theme: UiTheme,
}

impl FjordFilesView {
    fn new(initial: Option<String>, cx: &mut Context<Self>) -> Self {
        let target = initial.unwrap_or_else(|| files::COMPUTER_LOCATION.to_owned());
        let mut this = Self {
            history: files::History::new(),
            folder: None,
            entries: Vec::new(),
            visible: Vec::new(),
            selected: None,
            query: String::new(),
            location: target.clone(),
            status: loc::t("Loading"),
            breadcrumb: String::new(),
            loading: false,
            preview_open: false,
            quick_look: false,
            preview: preview::Preview::NothingSelected,
            navigation_cancel: Arc::new(AtomicBool::new(false)),
            preview_cancel: Arc::new(AtomicBool::new(false)),
            location_focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            theme: UiTheme::new(win32::read_system_is_dark()),
        };
        this.navigate(target, None, cx);
        this
    }

    fn home() -> String {
        std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users".to_owned())
    }

    fn selected_entry(&self) -> Option<&files::FileEntry> {
        self.selected
            .and_then(|row| self.visible.get(row))
            .and_then(|index| self.entries.get(*index))
    }

    /// `Navigate`: cancel the previous read, resolve the path, update history.
    fn navigate(&mut self, target: String, slot: Option<usize>, cx: &mut Context<Self>) {
        self.navigation_cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.navigation_cancel = Arc::clone(&cancel);
        self.loading = true;
        self.status = loc::t("Loading");
        cx.notify();

        let home = Self::home();
        let base = self.folder.clone();
        let is_computer = files::is_computer_location(&target);
        let task_cancel = Arc::clone(&cancel);
        let task = cx.background_spawn(async move {
            if is_computer {
                return Ok((files::COMPUTER_LOCATION.to_owned(), files::read_drives()));
            }
            let Some(path) = files::navigation_target(&target, base.as_deref(), &home) else {
                return Err(loc::t("Missing"));
            };
            files::read_entries(&path, &task_cancel)
                .map(|entries| (path, entries))
                .map_err(|error| error.to_string())
        });
        drop(cx.spawn(async move |this, cx| {
            let result = task.await;
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let _ = this.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok((path, entries)) => {
                        view.folder = Some(path.clone());
                        view.entries = entries;
                        if let Some(index) = slot {
                            view.history.restore(index);
                        } else if view.history.current() != Some(path.as_str()) {
                            view.history.push(path.clone());
                        }
                        view.location = path.clone();
                        view.breadcrumb = if files::is_computer_location(&path) {
                            loc::t("FjordFilesComputer")
                        } else {
                            files::folder_trail(&path)
                        };
                        view.selected = None;
                        view.query.clear();
                        view.apply_filter();
                    }
                    Err(message) => {
                        view.status = message;
                        view.entries.clear();
                        view.visible.clear();
                    }
                }
                cx.notify();
                view.refresh_preview(cx);
            });
        }));
    }

    fn step_history(&mut self, delta: i32, cx: &mut Context<Self>) {
        if let Some((index, path)) = self.history.step(delta) {
            self.navigate(path, Some(index), cx);
        }
    }

    fn up(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.history.up_target() {
            self.navigate(path, None, cx);
        }
    }

    /// `ApplyFilter`, using the culture-aware fold from `fjord::files`.
    fn apply_filter(&mut self) {
        let culture = loc::language();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| files::matches_filter(&entry.name, &self.query, culture))
            .map(|(index, _)| index)
            .collect();
        if self
            .selected
            .is_some_and(|row| !self.visible.contains(&row))
        {
            self.selected = None;
        }
        self.status = if self.query.trim().is_empty() {
            loc::t("FjordFilesCount").replace("{0}", &self.visible.len().to_string())
        } else {
            loc::t("FjordFilesFilteredCount")
                .replace("{0}", &self.visible.len().to_string())
                .replace("{1}", &self.entries.len().to_string())
        };
    }

    fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected = Some(row);
        self.refresh_preview(cx);
        cx.notify();
    }

    fn open_selection(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        if entry.is_folder {
            self.navigate(entry.path, None, cx);
        } else if !files::open_file(&entry.path, None) {
            self.status = loc::t("OpenItemFailed");
            cx.notify();
        }
    }

    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_open = !self.preview_open;
        if !self.preview_open {
            self.preview = preview::Preview::NothingSelected;
        }
        self.refresh_preview(cx);
        cx.notify();
    }

    fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.preview_cancel = Arc::clone(&cancel);
        if !self.preview_open && !self.quick_look {
            return;
        }
        let entry = self.selected_entry().cloned();
        let task_cancel = Arc::clone(&cancel);
        let task = cx.background_spawn(async move { preview::build(entry.as_ref(), &task_cancel) });
        drop(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if !cancel.load(Ordering::SeqCst) {
                    view.preview = result;
                    cx.notify();
                }
            });
        }));
    }

    fn copy_path(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry() {
            self.status = if files::copy_path_to_clipboard(&entry.path) {
                loc::t("Copied")
            } else {
                loc::t("CopyFailed")
            };
            cx.notify();
        }
    }

    fn reveal(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry()
            && files::reveal_in_explorer(&entry.path)
        {
            self.status = loc::t("Revealed");
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if self.location_focus.is_focused(window) || self.search_focus.is_focused(window) {
            return; // each field owns its own keystrokes
        }
        if modifiers.control && matches!(key, "l" | "f") {
            let focus = if key == "l" {
                &self.location_focus
            } else {
                &self.search_focus
            };
            focus.focus(window);
            return;
        }
        if modifiers.alt {
            match key {
                "left" => self.step_history(-1, cx),
                "right" => self.step_history(1, cx),
                "up" => self.up(cx),
                _ => {}
            }
            return;
        }
        if modifiers.control || modifiers.platform {
            return;
        }
        match key {
            "f4" => self.toggle_preview(cx),
            "f5" => {
                if let Some(path) = self.folder.clone() {
                    self.navigate(path, None, cx);
                }
            }
            "enter" if self.list_focus.is_focused(window) => self.open_selection(cx),
            "space" if self.list_focus.is_focused(window) => {
                self.quick_look = !self.quick_look;
                self.refresh_preview(cx);
                cx.notify();
            }
            "escape" if self.quick_look => {
                self.quick_look = false;
                cx.notify();
            }
            _ => {}
        }
    }

    fn preview_summary(&self) -> AnyElement {
        let t = self.theme;
        let title = match &self.preview {
            preview::Preview::NothingSelected => loc::t("SelectPreview"),
            preview::Preview::Folder { name } => name.clone(),
            preview::Preview::Image { width, height } => format!("{width} × {height}"),
            preview::Preview::Pdf { pages } => {
                loc::t("PdfPages").replace("{0}", &pages.to_string())
            }
            preview::Preview::Text { text, .. } => text.clone(),
            preview::Preview::Unsupported => loc::t("UnsupportedPreview"),
            preview::Preview::Failed => loc::t("PreviewFailed"),
            preview::Preview::Cancelled => loc::t("PreviewDisabledHint"),
        };
        div()
            .id("fjord-preview")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .p(px(16.0))
            .text_color(rgba(t.text_primary))
            .text_size(px(12.0))
            .child(title)
            .into_any_element()
    }
}

impl Render for FjordFilesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let culture = loc::language();
        let can_back = self.history.can_back() && !self.loading;
        let can_forward = self.history.can_forward() && !self.loading;
        let entries: Vec<_> = self
            .visible
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .cloned()
            .collect();

        let navigation = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                div()
                    .when(can_back, |b| b.opacity(1.0))
                    .when(!can_back, |b| b.opacity(0.4))
                    .child(button(
                        "fjord-back",
                        &t,
                        "‹",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| this.step_history(-1, cx)),
                    )),
            )
            .child(
                div()
                    .when(can_forward, |b| b.opacity(1.0))
                    .when(!can_forward, |b| b.opacity(0.4))
                    .child(button(
                        "fjord-forward",
                        &t,
                        "›",
                        ButtonVariant::Secondary,
                        cx.listener(|this, _, _, cx| this.step_history(1, cx)),
                    )),
            )
            .child(button(
                "fjord-up",
                &t,
                "↑",
                ButtonVariant::Secondary,
                cx.listener(|this, _, _, cx| this.up(cx)),
            ))
            .child(button(
                "fjord-preview-toggle",
                &t,
                loc::t("FjordFilesPreview"),
                ButtonVariant::Secondary,
                cx.listener(|this, _, _, cx| this.toggle_preview(cx)),
            ));

        let search_focus = self.search_focus.clone();
        let search = div()
            .id("fjord-search")
            .track_focus(&search_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if !this.search_focus.is_focused(window) {
                    return;
                }
                let key = event.keystroke.key.as_str();
                let mut buffer = this.query.clone();
                if apply_keystroke(&mut buffer, key, event.keystroke.key_char.as_deref(), false) {
                    this.query = buffer;
                    this.apply_filter();
                    cx.notify();
                }
            }))
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(7.0))
            .w(px(210.0))
            .bg(rgba(t.surface_muted))
            .border_1()
            .border_color(rgba(t.border))
            .text_size(px(12.5))
            .text_color(rgba(t.text_primary))
            .child(if self.query.is_empty() {
                loc::t("FjordFilesSearch")
            } else {
                self.query.clone()
            });

        let location_focus = self.location_focus.clone();
        let location = div()
            .id("fjord-location")
            .track_focus(&location_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if !this.location_focus.is_focused(window) {
                    return;
                }
                let key = event.keystroke.key.as_str();
                if key == "enter" {
                    let target = this.location.clone();
                    this.navigate(target, None, cx);
                    return;
                }
                if key == "escape" {
                    this.location = this
                        .folder
                        .clone()
                        .unwrap_or_else(|| files::COMPUTER_LOCATION.to_owned());
                    cx.notify();
                    return;
                }
                let mut buffer = this.location.clone();
                if apply_keystroke(&mut buffer, key, event.keystroke.key_char.as_deref(), false) {
                    this.location = buffer;
                    cx.notify();
                }
            }))
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(7.0))
            .flex_1()
            .min_w_0()
            .bg(rgba(t.surface_muted))
            .border_1()
            .border_color(rgba(t.border))
            .text_size(px(12.5))
            .text_color(rgba(t.text_primary))
            .child(self.location.clone());

        let list_focus = self.list_focus.clone();
        let list = div()
            .id("fjord-list")
            .track_focus(&list_focus)
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(entries.into_iter().enumerate().map(|(row, entry)| {
                        let name = entry.name.clone();
                        let meta = if entry.is_folder {
                            loc::t("FjordFilesFolder")
                        } else {
                            format!("{} · {}", entry.size_label(','), entry.date_label(culture))
                        };
                        let selected = self.selected == Some(row);
                        let glyph = if entry.is_folder {
                            "\u{e8b7}"
                        } else {
                            "\u{e7c3}"
                        };
                        div()
                            .id(SharedString::from(format!("fjord-row-{row}")))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .h(px(ROW_H))
                            .px(px(10.0))
                            .rounded(px(5.0))
                            .bg(rgba(if selected { t.accent_soft } else { 0 }))
                            .text_color(rgba(t.text_primary))
                            .on_click(
                                cx.listener(move |this: &mut Self, _, _, cx| this.select(row, cx)),
                            )
                            .child(icon_glyph(glyph, 13.0, t.accent))
                            .child(div().min_w_0().flex_1().child(name))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgba(t.text_secondary))
                                    .child(meta),
                            )
                    })),
            );

        let sidebar = div()
            .w(px(184.0))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .p(px(8.0))
            .border_r_1()
            .border_color(rgba(t.border))
            .children(files::favorites(&Self::home()).into_iter().enumerate().map(
                |(index, (label, glyph, path))| {
                    div()
                        .id(SharedString::from(format!("fjord-place-{index}")))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .text_size(px(12.0))
                        .text_color(rgba(t.text_primary))
                        .hover(|style| style.bg(rgba(t.surface_hover)))
                        .on_click(cx.listener(move |this: &mut Self, _, _, cx| {
                            this.navigate(path.clone(), None, cx);
                        }))
                        .child(icon_glyph(glyph, 13.0, t.accent))
                        .child(loc::t(label))
                },
            ))
            .child(
                div()
                    .py(px(6.0))
                    .text_size(px(10.0))
                    .text_color(rgba(t.text_secondary))
                    .child(loc::t("FjordFilesVolumes")),
            )
            .children(
                files::read_drives()
                    .into_iter()
                    .enumerate()
                    .map(|(index, drive)| {
                        let path = drive.path.clone();
                        let name = drive.name.clone();
                        div()
                            .id(SharedString::from(format!("fjord-drive-{index}")))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .px(px(8.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .text_size(px(12.0))
                            .text_color(rgba(t.text_primary))
                            .hover(|style| style.bg(rgba(t.surface_hover)))
                            .on_click(cx.listener(move |this: &mut Self, _, _, cx| {
                                this.navigate(path.clone(), None, cx);
                            }))
                            .child(icon_glyph("\u{eda2}", 13.0, t.text_secondary))
                            .child(name)
                    }),
            );

        let mut body = div().flex().flex_1().min_h_0().child(sidebar).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .px(px(12.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .text_color(rgba(t.text_secondary))
                        .child(self.breadcrumb.clone()),
                )
                .child(list)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(5.0))
                        .border_t_1()
                        .border_color(rgba(t.border))
                        .text_size(px(11.0))
                        .text_color(rgba(t.text_secondary))
                        .child(self.status.clone())
                        .child(button(
                            "fjord-copy-path",
                            &t,
                            loc::t("FjordFilesCopyPath"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.copy_path(cx)),
                        ))
                        .child(button(
                            "fjord-reveal",
                            &t,
                            loc::t("FjordFilesReveal"),
                            ButtonVariant::Secondary,
                            cx.listener(|this, _, _, cx| this.reveal(cx)),
                        )),
                ),
        );
        if self.preview_open || self.quick_look {
            body = body.child(
                div()
                    .w(px(300.0))
                    .flex_shrink_0()
                    .border_l_1()
                    .border_color(rgba(t.border))
                    .flex()
                    .flex_col()
                    .child(self.preview_summary()),
            );
        }

        div()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .bg(rgba(t.canvas_bg))
            .text_color(rgba(t.text_primary))
            .font_family("Segoe UI Variable Text, Segoe UI")
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .p(px(10.0))
                    .border_b_1()
                    .border_color(rgba(t.border))
                    .child(navigation)
                    .child(location)
                    .child(search),
            )
            .child(body)
    }
}
