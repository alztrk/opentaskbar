//! Live application window picker — pure policy + public entry point (issue #174).
//!
//! Rust implementation of `AppWindowPicker` / `MainWindow.WindowPicker` pair. All geometry,
//! ordering, paging and navigation decisions live here as side-effect-free functions
//! so they can be unit tested against the reference vectors; `dock::window_picker` owns the
//! native Win32 popup, DWM thumbnails and the modal message loop.

use std::collections::HashSet;

use opentaskbar_core::i18n as loc;
use opentaskbar_shell::tracker::RunningWindow;

/// `AppWindowPicker.Gap` (DIPs).
pub const PICKER_GAP_DIPS: f32 = 12.0;
/// `AppWindowPicker.FramePadding` (DIPs).
pub const PICKER_FRAME_PADDING_DIPS: f32 = 18.0;
/// `_cardWidth` cap (DIPs).
pub const PICKER_MAX_CARD_WIDTH_DIPS: f32 = 312.0;
/// `_cardHeight` bounds (DIPs).
pub const PICKER_MIN_CARD_HEIGHT_DIPS: f32 = 64.0;
pub const PICKER_MAX_CARD_HEIGHT_DIPS: f32 = 222.0;
/// reference page-bar height, added only when more than one page exists.
pub const PICKER_PAGING_HEIGHT_DIPS: f32 = 43.0;
/// Header + frame chrome below the grid (`58` in `BuildPage`).
pub const PICKER_CHROME_HEIGHT_DIPS: f32 = 58.0;

/// App identity used by [`matches_window`]. reference passes the clicked `DockApp`;
/// AUMID wins whenever both sides have one, executable path is the fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerApp {
    /// `DockApp.AppUserModelId` (empty for classic Win32 pins).
    pub aumid: String,
    /// `DockApp.ExecutablePath`.
    pub executable_path: String,
}

impl PickerApp {
    pub fn new(aumid: impl Into<String>, executable_path: impl Into<String>) -> Self {
        Self {
            aumid: aumid.into(),
            executable_path: executable_path.into(),
        }
    }
}

/// `AppWindowPicker.Matches`: AUMID is compared first when both are present,
/// otherwise the executable paths are compared. An entirely empty identity matches
/// every window — that is the mode used by [`show`], whose caller has already
/// scoped the snapshot to the clicked dock icon.
pub fn matches_window(app: &PickerApp, window: &RunningWindow) -> bool {
    let app_aumid = app.aumid.trim();
    let win_aumid = window.aumid.trim();
    if app_aumid.is_empty() && app.executable_path.trim().is_empty() {
        return true;
    }
    if !app_aumid.is_empty() && !win_aumid.is_empty() {
        return app_aumid.eq_ignore_ascii_case(win_aumid);
    }
    app.executable_path.eq_ignore_ascii_case(&window.exe_path)
}

/// `AppWindowPicker.OrderWindows`: drop null handles and non-matching apps,
/// dedupe by HWND (first wins), then put windows from `monitor` first. The sort is
/// stable, exactly like LINQ `OrderByDescending`, so same-monitor order is preserved.
pub fn order_windows(
    windows: Vec<RunningWindow>,
    app: &PickerApp,
    monitor: usize,
    monitor_for_window: impl Fn(usize) -> usize,
) -> Vec<RunningWindow> {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut ranked: Vec<(bool, RunningWindow)> = Vec::with_capacity(windows.len());
    for window in windows {
        if window.hwnd == 0 || !matches_window(app, &window) || !seen.insert(window.hwnd) {
            continue;
        }
        ranked.push((monitor_for_window(window.hwnd) == monitor, window));
    }
    // `sort_by` is stable; `true` must come first.
    ranked.sort_by_key(|item| std::cmp::Reverse(item.0));
    ranked.into_iter().map(|(_, window)| window).collect()
}

/// `(_windows.Count - 1) / _pageSize + 1`, with zero items giving zero pages.
pub fn page_count(item_count: usize, page_size: usize) -> usize {
    if item_count == 0 || page_size == 0 {
        return 0;
    }
    (item_count - 1) / page_size + 1
}

/// One cell of the row-major `UniformGrid` used by the reference picker. `index` is the
/// absolute position in the ordered window list, not the page-local position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridCell {
    pub index: usize,
    pub row: usize,
    pub col: usize,
}

/// `UniformGrid` layout for a single page: `Rows = ceil(visible / Columns)`,
/// columns fixed at `columns`, cells filled row-major. `page_size` bounds the slice.
pub fn grid_cells(
    item_count: usize,
    page: usize,
    page_size: usize,
    columns: usize,
) -> Vec<GridCell> {
    let columns = columns.max(1);
    let page_size = page_size.max(1);
    let start = page.saturating_mul(page_size).min(item_count);
    let end = start.saturating_add(page_size).min(item_count);
    (start..end)
        .enumerate()
        .map(|(slot, index)| GridCell {
            index,
            row: slot / columns,
            col: slot % columns,
        })
        .collect()
}

/// Result of the picker geometry calculation (all sizes in DIPs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickerGeometry {
    pub columns: usize,
    pub rows: usize,
    pub page_size: usize,
    pub card_width: f32,
    pub card_height: f32,
}

impl PickerGeometry {
    /// Number of grid rows actually occupied on a page with `visible_count` cards.
    pub fn visible_rows(&self, visible_count: usize) -> usize {
        let columns = self.columns.max(1);
        visible_count.div_ceil(columns)
    }

    /// `BuildPage`: `Width = columns * (cardWidth + Gap) + Padding * 2 + 2`,
    /// `Height = rows * (cardHeight + Gap) + Padding * 2 + 58 (+43 with paging)`.
    pub fn frame_size(&self, visible_count: usize, paging_visible: bool) -> (i32, i32) {
        let columns = self.columns.max(1) as f32;
        let rows = self.visible_rows(visible_count).max(1) as f32;
        let width =
            columns * (self.card_width + PICKER_GAP_DIPS) + PICKER_FRAME_PADDING_DIPS * 2.0 + 2.0;
        let height = rows * (self.card_height + PICKER_GAP_DIPS)
            + PICKER_FRAME_PADDING_DIPS * 2.0
            + PICKER_CHROME_HEIGHT_DIPS
            + if paging_visible {
                PICKER_PAGING_HEIGHT_DIPS
            } else {
                0.0
            };
        (width.round() as i32, height.round() as i32)
    }
}

/// Exact integer equivalent of the reference constructor:
/// `width = max(1, WorkArea.Width - 36 - Padding*2)`,
/// `height = max(1, WorkArea.Height - 36 - Padding*2 - 96)`,
/// `columns = min(count, clamp((width + 12) / 254, 1, 3))`,
/// `rows = clamp((height + 12) / (cardHeight + 12), 1, 3)`.
pub fn geometry_for_area(work_width: i32, work_height: i32, window_count: usize) -> PickerGeometry {
    let width = (work_width - 36 - (PICKER_FRAME_PADDING_DIPS as i32) * 2).max(1);
    let height = (work_height - 36 - (PICKER_FRAME_PADDING_DIPS as i32) * 2 - 96).max(1);

    let raw_columns =
        ((width + PICKER_GAP_DIPS as i32) / (242 + PICKER_GAP_DIPS as i32)).clamp(1, 3) as usize;
    let columns = window_count.min(raw_columns).max(1);

    let card_width = PICKER_MAX_CARD_WIDTH_DIPS
        .min((width - PICKER_GAP_DIPS as i32 * (columns as i32 - 1)) as f32 / columns as f32);
    let card_height =
        PICKER_MAX_CARD_HEIGHT_DIPS.min((height.max(PICKER_MIN_CARD_HEIGHT_DIPS as i32)) as f32);
    let rows = ((height + PICKER_GAP_DIPS as i32) / (card_height as i32 + PICKER_GAP_DIPS as i32))
        .clamp(1, 3) as usize;

    PickerGeometry {
        columns,
        rows,
        page_size: rows * columns,
        card_width,
        card_height,
    }
}

/// `Position`: centre the frame inside the invoking display's work area. Works
/// with signed virtual-desktop coordinates (monitors left/above the primary one).
pub fn center_in_area(work_area: (i32, i32, i32, i32), width: i32, height: i32) -> (i32, i32) {
    let (left, top, right, bottom) = work_area;
    let x = left + ((right - left).max(0) - width) / 2;
    let y = top + ((bottom - top).max(0) - height) / 2;
    (x, y)
}

/// Keys handled by `HandleKeys` (`Enter`/`Space` are left to UI buttons,
/// this port maps them explicitly to `Confirm` for the single focused window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKey {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Escape,
    Confirm,
}

/// Navigation decision returned by [`navigate`]. The popup applies `Move` inside
/// the current page and `SetPage` by rebuilding page 0 of the target page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerAction {
    Move(usize),
    SetPage(usize),
    Confirm,
    Close,
    Ignore,
}

/// `HandleKeys` + `FocusCard` + `ChangePage` as a pure function. Indices are
/// clamped to the visible page, exactly like `FocusCard(index)`; page changes
/// reset the selection to the first card of the new page (`BuildPage(0)`).
pub fn navigate(
    key: PickerKey,
    selected: usize,
    visible_count: usize,
    columns: usize,
    page: usize,
    pages: usize,
) -> PickerAction {
    if visible_count == 0 {
        return match key {
            PickerKey::Escape => PickerAction::Close,
            _ => PickerAction::Ignore,
        };
    }
    let columns = columns.max(1);
    let last = visible_count - 1;
    let clamp = |index: isize| -> usize { index.clamp(0, last as isize) as usize };
    let selected = selected.min(last);
    match key {
        PickerKey::Escape => PickerAction::Close,
        PickerKey::Left => PickerAction::Move(clamp(selected as isize - 1)),
        PickerKey::Right => PickerAction::Move(clamp(selected as isize + 1)),
        PickerKey::Up => PickerAction::Move(clamp(selected as isize - columns as isize)),
        PickerKey::Down => PickerAction::Move(clamp(selected as isize + columns as isize)),
        PickerKey::Home => PickerAction::Move(0),
        PickerKey::End => PickerAction::Move(last),
        PickerKey::PageUp if page > 0 => PickerAction::SetPage(page - 1),
        PickerKey::PageDown if pages > 0 && page + 1 < pages => PickerAction::SetPage(page + 1),
        PickerKey::PageUp | PickerKey::PageDown => PickerAction::Ignore,
        PickerKey::Confirm => PickerAction::Confirm,
    }
}

/// Translate a new i18n key with an English fallback until WS-2 lands the
/// 14-language catalogs. `loc::t` returns the key itself when it is missing.
pub fn label(key: &str, fallback: &str) -> String {
    let value = loc::t(key);
    if value == key {
        fallback.to_owned()
    } else {
        value
    }
}

/// Shows the picker modal on the UI thread. `monitor` is the index into
/// `platform::win32::enumerate_monitors()` of the invoking dock (an out-of-range
/// index falls back to the display containing `anchor`'s centre). `anchor` is the
/// physical dock-button rect `(left, top, right, bottom)`.
///
/// Returns `true` when the picker was displayed and dismissed, `false` when there
/// were no windows or the native popup could not be created.
pub fn show(monitor: usize, windows: Vec<RunningWindow>, anchor: (i32, i32, i32, i32)) -> bool {
    crate::dock::window_picker::show(monitor, windows, anchor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(hwnd: usize, aumid: &str, exe: &str, title: &str) -> RunningWindow {
        RunningWindow {
            hwnd,
            pid: hwnd as u32 + 100,
            title: title.to_owned(),
            exe_path: exe.to_owned(),
            aumid: aumid.to_owned(),
            is_foreground: false,
        }
    }

    #[test]
    fn matches_window_prefers_aumid_case_insensitively() {
        let app = PickerApp::new("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App", "");
        let w = window(
            1,
            "microsoft.windowsterminal_8wekyb3d8bbwe!APP",
            r"C:\Windows\System32\wt.exe",
            "Term",
        );
        assert!(matches_window(&app, &w));
        let other = window(2, "Other.App!App", r"C:\Windows\System32\wt.exe", "Term");
        assert!(!matches_window(&app, &other));
    }

    #[test]
    fn matches_window_falls_back_to_executable_path() {
        let app = PickerApp::new("Packaged.Only!App", r"C:\Tools\App.EXE");
        // Classic Win32 window has no AUMID -> exe fallback, ignoring the app AUMID.
        let w = window(1, "", r"c:\tools\app.exe", "App");
        assert!(matches_window(&app, &w));
        // Both AUMIDs present -> exe is not consulted.
        let packaged_other_exe = window(2, "Packaged.Only!App", r"C:\Elsewhere\other.exe", "App");
        assert!(matches_window(&app, &packaged_other_exe));
    }

    #[test]
    fn matches_window_empty_identity_matches_everything() {
        let w = window(1, "", "", "Any");
        assert!(matches_window(&PickerApp::default(), &w));
    }

    #[test]
    fn order_windows_dedupes_and_puts_calling_monitor_first() {
        let windows = vec![
            window(1, "", r"C:\a.exe", "other-1"),
            window(2, "", r"C:\a.exe", "here-1"),
            window(2, "", r"C:\a.exe", "duplicate"),
            window(3, "", r"C:\a.exe", "other-2"),
            window(0, "", r"C:\a.exe", "invalid"),
            window(4, "", r"C:\b.exe", "filtered"),
        ];
        let app = PickerApp::new("", r"C:\a.exe");
        let monitor_of = |hwnd: usize| if hwnd == 2 || hwnd == 4 { 7 } else { 9 };
        let ordered = order_windows(windows, &app, 7, monitor_of);
        assert_eq!(
            ordered.iter().map(|w| w.hwnd).collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
        assert_eq!(
            ordered[0].title, "here-1",
            "first occurrence must win dedupe"
        );
    }

    #[test]
    fn order_windows_is_stable_inside_each_monitor_group() {
        let windows = vec![
            window(1, "", r"C:\a.exe", "far-1"),
            window(2, "", r"C:\a.exe", "near-1"),
            window(3, "", r"C:\a.exe", "far-2"),
            window(4, "", r"C:\a.exe", "near-2"),
        ];
        let monitor_of = |hwnd: usize| if hwnd.is_multiple_of(2) { 1 } else { 2 };
        let ordered = order_windows(windows, &PickerApp::default(), 1, monitor_of);
        assert_eq!(
            ordered.iter().map(|w| w.title.as_str()).collect::<Vec<_>>(),
            vec!["near-1", "near-2", "far-1", "far-2"]
        );
    }

    #[test]
    fn page_count_matches_reference_integer_expression() {
        assert_eq!(page_count(0, 9), 0);
        assert_eq!(page_count(1, 9), 1);
        assert_eq!(page_count(9, 9), 1);
        assert_eq!(page_count(10, 9), 2);
        assert_eq!(page_count(18, 9), 2);
        assert_eq!(page_count(19, 9), 3);
    }

    #[test]
    fn grid_cells_are_row_major_three_columns() {
        let cells = grid_cells(8, 0, 9, 3);
        assert_eq!(cells.len(), 8);
        assert_eq!(
            cells[0],
            GridCell {
                index: 0,
                row: 0,
                col: 0
            }
        );
        assert_eq!(
            cells[2],
            GridCell {
                index: 2,
                row: 0,
                col: 2
            }
        );
        assert_eq!(
            cells[3],
            GridCell {
                index: 3,
                row: 1,
                col: 0
            }
        );
        assert_eq!(
            cells[7],
            GridCell {
                index: 7,
                row: 2,
                col: 1
            }
        );
    }

    #[test]
    fn grid_cells_second_page_uses_absolute_indices() {
        let cells = grid_cells(14, 1, 9, 3);
        assert_eq!(cells.len(), 5);
        assert_eq!(
            cells[0],
            GridCell {
                index: 9,
                row: 0,
                col: 0
            }
        );
        assert_eq!(
            cells[4],
            GridCell {
                index: 13,
                row: 1,
                col: 1
            }
        );
        assert!(grid_cells(14, 2, 9, 3).is_empty());
    }

    #[test]
    fn geometry_matches_reference_vector_for_1080p_work_area() {
        // 1920x1080 screen minus a 48px taskbar -> 1920x1032 work area.
        let g = geometry_for_area(1920, 1032, 12);
        assert_eq!(g.columns, 3);
        assert_eq!(g.rows, 3);
        assert_eq!(g.page_size, 9);
        assert_eq!(g.card_width, 312.0);
        assert_eq!(g.card_height, 222.0);
        // 3*(312+12) + 36 + 2 = 1010; 3*(222+12) + 36 + 58 + 43 = 839
        assert_eq!(g.frame_size(9, true), (1010, 839));
        // Last page has ceil(4/3) = 2 rows and no pager.
        assert_eq!(g.frame_size(4, false), (1010, 562));
    }

    #[test]
    fn geometry_clamps_small_and_large_work_areas() {
        let small = geometry_for_area(400, 300, 8);
        assert_eq!(small.columns, 1);
        assert_eq!(small.rows, 1);
        assert_eq!(small.page_size, 1);
        assert_eq!(small.card_height, 132.0);

        let tiny = geometry_for_area(1, 1, 1);
        assert_eq!(tiny.columns, 1);
        assert_eq!(tiny.card_width, 1.0);
        assert_eq!(tiny.card_height, 64.0);

        let wide = geometry_for_area(3840, 2160, 1);
        assert_eq!(wide.columns, 1, "columns never exceed the window count");
    }

    #[test]
    fn center_in_area_handles_negative_virtual_desktop_origin() {
        assert_eq!(center_in_area((0, 0, 1920, 1032), 1004, 769), (458, 131));
        // 1080-high left monitor: y = -200 + (1080 - 769) / 2 = -45.
        assert_eq!(
            center_in_area((-1920, -200, 0, 880), 1004, 769),
            (-1462, -45)
        );
    }

    #[test]
    fn navigation_arrows_and_home_end_clamp_to_visible_page() {
        assert_eq!(
            navigate(PickerKey::Left, 0, 9, 3, 0, 3),
            PickerAction::Move(0)
        );
        assert_eq!(
            navigate(PickerKey::Right, 8, 9, 3, 0, 3),
            PickerAction::Move(8)
        );
        assert_eq!(
            navigate(PickerKey::Right, 4, 9, 3, 0, 3),
            PickerAction::Move(5)
        );
        assert_eq!(
            navigate(PickerKey::Up, 1, 9, 3, 0, 3),
            PickerAction::Move(0)
        );
        assert_eq!(
            navigate(PickerKey::Down, 7, 9, 3, 0, 3),
            PickerAction::Move(8)
        );
        assert_eq!(
            navigate(PickerKey::Home, 5, 9, 3, 0, 3),
            PickerAction::Move(0)
        );
        assert_eq!(
            navigate(PickerKey::End, 0, 9, 3, 0, 3),
            PickerAction::Move(8)
        );
    }

    #[test]
    fn navigation_page_keys_change_page_and_reset_selection() {
        assert_eq!(
            navigate(PickerKey::PageDown, 7, 9, 3, 0, 3),
            PickerAction::SetPage(1)
        );
        assert_eq!(
            navigate(PickerKey::PageUp, 0, 9, 3, 2, 3),
            PickerAction::SetPage(1)
        );
        assert_eq!(
            navigate(PickerKey::PageDown, 0, 9, 3, 2, 3),
            PickerAction::Ignore
        );
        assert_eq!(
            navigate(PickerKey::PageUp, 0, 9, 3, 0, 3),
            PickerAction::Ignore
        );
    }

    #[test]
    fn navigation_escape_and_confirm() {
        assert_eq!(
            navigate(PickerKey::Escape, 3, 9, 3, 0, 3),
            PickerAction::Close
        );
        assert_eq!(
            navigate(PickerKey::Confirm, 3, 9, 3, 0, 3),
            PickerAction::Confirm
        );
        assert_eq!(
            navigate(PickerKey::Right, 0, 0, 3, 0, 0),
            PickerAction::Ignore
        );
        assert_eq!(
            navigate(PickerKey::Escape, 0, 0, 3, 0, 0),
            PickerAction::Close
        );
    }
}
