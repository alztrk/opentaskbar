//! N2: Dynamic Island File Shelf — lightweight drag-and-drop holding bay for files and text.
//!
//! Strict low-RAM invariants:
//! - For `ShelfKind::File`, only the path string is stored in RAM (file bytes are never read).
//! - For `ShelfKind::Text`, payloads are truncated to [`MAX_TEXT_BYTES`] (4096 bytes) on a valid
//!   UTF-8 character boundary.
//! - Item count is strictly bounded to `4..=32` (`MIN_SHELF_ITEMS..=MAX_SHELF_ITEMS`).
//! - Persistence uses atomic temp-file + rename under `%LOCALAPPDATA%\OpenTaskbar\shelf.json`.

use std::ffi::c_void;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use opentaskbar_core::paths;
use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::design;
use crate::platform::win32_helpers::wide;
use crate::ui::flyout::{FlyoutBlock, FlyoutSpec};
use crate::widgets::{IslandDetailAction, IslandDetailRow, IslandDetailSpec};

/// File name under `%LOCALAPPDATA%\OpenTaskbar`.
pub const SHELF_FILE_NAME: &str = "shelf.json";
/// Lower bound for configurable shelf capacity.
pub const MIN_SHELF_ITEMS: usize = 4;
/// Upper bound for configurable shelf capacity.
pub const MAX_SHELF_ITEMS: usize = 32;
/// Default shelf capacity when no override is provided.
pub const DEFAULT_MAX_ITEMS: usize = 16;
/// Maximum byte length stored for a `ShelfKind::Text` payload.
pub const MAX_TEXT_BYTES: usize = 4096;
/// Maximum character count for a 1-line text preview label.
pub const MAX_LABEL_CHARS: usize = 48;

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const GMEM_MOVEABLE: u32 = 0x0002;
const GMEM_ZEROINIT: u32 = 0x0040;
const TYMED_HGLOBAL: u32 = 1;
const DVASPECT_CONTENT: u32 = 1;
const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_LINK: u32 = 4;
const MK_LBUTTON: u32 = 0x0001;

const S_OK: i32 = 0;
const S_FALSE: i32 = 1;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const E_POINTER: i32 = 0x8000_4003u32 as i32;
const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
const E_OUTOFMEMORY: i32 = 0x8007_000Eu32 as i32;
const DV_E_FORMATETC: i32 = 0x8004_0064u32 as i32;
const DV_E_TYMED: i32 = 0x8004_0069u32 as i32;
const DATA_S_SAMEFORMATETC: i32 = 0x0004_0130;
const OLE_E_ADVISENOTSUPPORTED: i32 = 0x8004_0003u32 as i32;
const DRAGDROP_S_DROP: i32 = 0x0004_0100;
const DRAGDROP_S_CANCEL: i32 = 0x0004_0101;
const DRAGDROP_S_USEDEFAULTCURSORS: i32 = 0x0004_0102;
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

/// Flyout button click IDs.
const CLICK_OPEN_BASE: u32 = 100;
const CLICK_COPY_BASE: u32 = 200;
const CLICK_REMOVE_BASE: u32 = 300;
const CLICK_DRAG_ALL: u32 = 1;
const CLICK_PASTE_TEXT: u32 = 2;
const CLICK_CLEAR_ALL: u32 = 3;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(hwnd_new_owner: *mut c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalFree(mem: *mut c_void) -> *mut c_void;
    fn GlobalSize(mem: *mut c_void) -> usize;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn OleInitialize(reserved: *mut c_void) -> i32;
    fn OleUninitialize();
    fn DoDragDrop(
        data_obj: *mut c_void,
        drop_source: *mut c_void,
        ok_effects: u32,
        out_effect: *mut u32,
    ) -> i32;
}

/// Kind of entry held on the Dynamic Island File Shelf.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShelfKind {
    File,
    Text,
}

/// One lightweight item stored on the File Shelf.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShelfItem {
    pub id: u64,
    pub kind: ShelfKind,
    pub label: String,
    pub payload: String,
    pub selected: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedShelf {
    #[serde(default)]
    items: Vec<ShelfItem>,
}

/// Bounded in-memory and persisted state for the Dynamic Island File Shelf.
#[derive(Debug, Clone, Default)]
pub struct FileShelfState {
    items: Vec<ShelfItem>,
    next_id: u64,
}

impl FileShelfState {
    /// Default persistence path (`%LOCALAPPDATA%\OpenTaskbar\shelf.json`).
    pub fn default_path() -> PathBuf {
        paths::shared_app_data_dir().join(SHELF_FILE_NAME)
    }

    /// Loads shelf state from `%LOCALAPPDATA%\OpenTaskbar\shelf.json`, returning an empty
    /// shelf when the file is absent or malformed.
    pub fn load_default() -> Self {
        Self::load_from(&Self::default_path())
    }

    /// Atomically persists shelf state to `%LOCALAPPDATA%\OpenTaskbar\shelf.json`.
    pub fn save_default(&self) -> io::Result<()> {
        self.save_to(&Self::default_path())
    }

    /// Loads shelf state from an explicit file path and enforces all memory bounds.
    pub fn load_from(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(parsed) = serde_json::from_str::<PersistedShelf>(&raw) else {
            return Self::default();
        };
        let mut state = Self::default();
        let mut max_id = 0u64;
        for mut item in parsed.items.into_iter().take(MAX_SHELF_ITEMS) {
            if item.payload.trim().is_empty() {
                continue;
            }
            if item.kind == ShelfKind::Text {
                item.payload = truncate_utf8_bytes(&item.payload, MAX_TEXT_BYTES);
                item.label = format_text_preview(&item.payload, MAX_LABEL_CHARS);
            } else if item.label.trim().is_empty() {
                item.label = file_basename_label(Path::new(&item.payload));
            }
            if item.id == 0 {
                max_id = max_id.saturating_add(1);
                item.id = max_id;
            } else {
                max_id = max_id.max(item.id);
            }
            state.items.push(item);
        }
        state.next_id = max_id.saturating_add(1);
        state
    }

    /// Atomically writes shelf state to `path` via sibling temp file + rename.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        let persisted = PersistedShelf {
            items: self.items.clone(),
        };
        let json = serde_json::to_vec_pretty(&persisted)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        atomic_write_bytes(path, &json)
    }

    fn alloc_id(&mut self) -> u64 {
        if self.next_id == 0 {
            self.next_id = self
                .items
                .iter()
                .map(|item| item.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Removes any `ShelfKind::File` items whose path no longer exists on disk.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.items.len();
        self.items
            .retain(|item| item.kind != ShelfKind::File || Path::new(&item.payload).exists());
        before.saturating_sub(self.items.len())
    }

    /// Adds file paths to the shelf without reading file bytes into RAM.
    ///
    /// Prunes missing existing file entries, deduplicates existing paths (case-insensitively),
    /// clamps `max_items` to `4..=32`, and returns the number of newly added items.
    pub fn add_files(&mut self, paths_in: &[PathBuf], max_items: usize) -> usize {
        self.prune_missing();
        let cap = clamp_max_items(max_items);
        let mut added = 0usize;
        for path in paths_in {
            let raw = path.to_string_lossy();
            let trimmed = raw.trim();
            if trimmed.is_empty() || is_device_path(trimmed) || !path.exists() {
                continue;
            }
            if self.items.iter().any(|item| {
                item.kind == ShelfKind::File && item.payload.eq_ignore_ascii_case(trimmed)
            }) {
                continue;
            }
            let id = self.alloc_id();
            let label = file_basename_label(path);
            self.items.insert(
                0,
                ShelfItem {
                    id,
                    kind: ShelfKind::File,
                    label,
                    payload: trimmed.to_owned(),
                    selected: false,
                },
            );
            added += 1;
        }
        self.enforce_cap(cap);
        added
    }

    /// Adds a text snippet (capped to 4096 bytes) to the shelf.
    ///
    /// Deduplicates identical snippets by promoting the existing item to the top and returns
    /// `true` when a new item was inserted.
    pub fn add_text(&mut self, text: &str, max_items: usize) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        let cap = clamp_max_items(max_items);
        let payload = truncate_utf8_bytes(text, MAX_TEXT_BYTES);
        if let Some(pos) = self
            .items
            .iter()
            .position(|item| item.kind == ShelfKind::Text && item.payload == payload)
        {
            let existing = self.items.remove(pos);
            self.items.insert(0, existing);
            self.enforce_cap(cap);
            return false;
        }
        let id = self.alloc_id();
        let label = format_text_preview(&payload, MAX_LABEL_CHARS);
        self.items.insert(
            0,
            ShelfItem {
                id,
                kind: ShelfKind::Text,
                label,
                payload,
                selected: false,
            },
        );
        self.enforce_cap(cap);
        true
    }

    fn enforce_cap(&mut self, cap: usize) {
        while self.items.len() > cap {
            if let Some(idx) = self.items.iter().rposition(|item| !item.selected) {
                self.items.remove(idx);
            } else {
                self.items.pop();
            }
        }
    }

    /// Toggles the selection state of item `id`.
    pub fn toggle_select(&mut self, id: u64) -> bool {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return false;
        };
        item.selected = !item.selected;
        true
    }

    /// Removes item `id` from the shelf.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() != before
    }

    /// Removes all currently selected items and returns how many were removed.
    pub fn remove_selected(&mut self) -> usize {
        let before = self.items.len();
        self.items.retain(|item| !item.selected);
        before.saturating_sub(self.items.len())
    }

    /// Clears every item from the shelf.
    pub fn clear_all(&mut self) {
        self.items.clear();
    }

    /// Read-only view of shelf items (newest first).
    pub fn items(&self) -> &[ShelfItem] {
        &self.items
    }

    /// Number of items currently held on the shelf.
    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` when the shelf holds no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Opens a file item via `ShellExecuteW` or copies a text item to the clipboard.
    pub fn open_item(&self, id: u64) -> bool {
        let Some(item) = self.items.iter().find(|item| item.id == id) else {
            return false;
        };
        match item.kind {
            ShelfKind::File => shell_open_path(&item.payload),
            ShelfKind::Text => set_clipboard_unicode_text(&item.payload),
        }
    }

    /// Copies the item's path (for files) or snippet (for text) to the clipboard.
    pub fn copy_item(&self, id: u64) -> bool {
        let Some(item) = self.items.iter().find(|item| item.id == id) else {
            return false;
        };
        set_clipboard_unicode_text(&item.payload)
    }

    /// Starts a Win32 OLE outbound drag (`DoDragDrop`) for `ids` (or all selected items when
    /// `ids` is empty; falls back to all items if none are selected).
    ///
    /// Serves `CF_HDROP` for file items or `CF_UNICODETEXT` for text items. If OLE drag
    /// initialization or execution fails, safely copies the payload to the clipboard as a
    /// fallback. Returns `true` when the user completed an outbound drop.
    pub fn start_outbound_drag(&self, ids: &[u64]) -> bool {
        let targets: Vec<&ShelfItem> = if !ids.is_empty() {
            self.items
                .iter()
                .filter(|item| ids.contains(&item.id))
                .collect()
        } else {
            let selected: Vec<&ShelfItem> =
                self.items.iter().filter(|item| item.selected).collect();
            if selected.is_empty() {
                self.items.iter().collect()
            } else {
                selected
            }
        };
        if targets.is_empty() {
            return false;
        }

        let files: Vec<String> = targets
            .iter()
            .filter(|item| item.kind == ShelfKind::File && Path::new(&item.payload).exists())
            .map(|item| item.payload.clone())
            .collect();

        if !files.is_empty() {
            match run_ole_drag_drop(DragPayload::Files(&files)) {
                DragOutcome::Dropped => return true,
                DragOutcome::Cancelled => return false,
                DragOutcome::Failed => {
                    let joined = files.join("\r\n");
                    let _ = set_clipboard_unicode_text(&joined);
                    return false;
                }
            }
        }

        let texts: Vec<&str> = targets
            .iter()
            .filter(|item| item.kind == ShelfKind::Text)
            .map(|item| item.payload.as_str())
            .collect();
        if texts.is_empty() {
            return false;
        }
        let joined = texts.join("\r\n");
        match run_ole_drag_drop(DragPayload::Text(&joined)) {
            DragOutcome::Dropped => true,
            DragOutcome::Cancelled => false,
            DragOutcome::Failed => {
                let _ = set_clipboard_unicode_text(&joined);
                false
            }
        }
    }

    /// Builds the Dynamic Island expanded detail card for the File Shelf (up to 4 rows,
    /// up to 3 quick actions).
    pub fn build_island_detail(&self) -> IslandDetailSpec {
        let count = self.items.len();
        let selected_count = self.items.iter().filter(|item| item.selected).count();
        let headline = match count {
            0 => "Shelf Empty".to_owned(),
            1 => "1 item".to_owned(),
            n => format!("{n} items"),
        };
        let subtitle = if count == 0 {
            "Drop files or notes here".to_owned()
        } else if selected_count > 0 {
            format!("{selected_count} selected · Ready to drag")
        } else {
            let files = self
                .items
                .iter()
                .filter(|i| i.kind == ShelfKind::File)
                .count();
            let texts = count.saturating_sub(files);
            match (files, texts) {
                (f, 0) => format!("{f} file{}", if f == 1 { "" } else { "s" }),
                (0, t) => format!("{t} note{}", if t == 1 { "" } else { "s" }),
                (f, t) => format!("{f} files · {t} notes"),
            }
        };

        let mut rows: Vec<IslandDetailRow> = self
            .items
            .iter()
            .take(4)
            .map(|item| {
                let tag = match (item.kind.clone(), item.selected) {
                    (ShelfKind::File, true) => "✓ File",
                    (ShelfKind::File, false) => "File",
                    (ShelfKind::Text, true) => "✓ Text",
                    (ShelfKind::Text, false) => "Text",
                };
                IslandDetailRow {
                    label: tag.to_owned(),
                    value: item.label.clone(),
                    progress: None,
                }
            })
            .collect();
        if rows.is_empty() {
            rows.push(IslandDetailRow {
                label: "Tip".to_owned(),
                value: "Drag files onto the Island or paste text".to_owned(),
                progress: None,
            });
        }

        let actions = if count == 0 {
            vec![IslandDetailAction {
                id: "paste_text".to_owned(),
                label: "Add Clipboard".to_owned(),
                primary: true,
            }]
        } else {
            vec![
                IslandDetailAction {
                    id: "open_first".to_owned(),
                    label: "Open / Copy".to_owned(),
                    primary: true,
                },
                IslandDetailAction {
                    id: "drag_all".to_owned(),
                    label: "Drag Out".to_owned(),
                    primary: false,
                },
                IslandDetailAction {
                    id: "clear".to_owned(),
                    label: "Clear".to_owned(),
                    primary: false,
                },
            ]
        };

        IslandDetailSpec {
            widget_id: "shelf".to_owned(),
            title: "File Shelf".to_owned(),
            headline,
            subtitle,
            rows,
            actions,
            has_full_flyout: true,
        }
    }

    /// Builds the interactive `DockFlyout` spec for the File Shelf.
    pub fn build_flyout(&self, is_dark: bool) -> FlyoutSpec {
        let pal = design::palette(is_dark);
        let count = self.items.len();
        let mut blocks = vec![FlyoutBlock::HeroHeader {
            glyph: 0xE8B7, // Folder
            glyph_color: pal.widget_accent,
            title: "File Shelf".to_owned(),
            subtitle: Some(if count == 0 {
                "Hold files and snippets for quick drag-out".to_owned()
            } else {
                format!("{count} / {MAX_SHELF_ITEMS} held")
            }),
            right_value: Some(count.to_string()),
        }];

        if self.items.is_empty() {
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::Note(
                "Drop files onto the Dynamic Island or add text from your clipboard.".to_owned(),
            ));
            blocks.push(FlyoutBlock::ButtonBar(vec![(
                CLICK_PASTE_TEXT,
                "Add Clipboard Text".to_owned(),
                true,
            )]));
        } else {
            blocks.push(FlyoutBlock::Separator);
            for (idx, item) in self.items.iter().take(12).enumerate() {
                let (glyph, color, badge) = match item.kind {
                    ShelfKind::File => (0xE8A5, pal.widget_blue, "Open"),
                    ShelfKind::Text => (0xE8C8, pal.widget_accent, "Copy"),
                };
                let subtitle = match item.kind {
                    ShelfKind::File => Path::new(&item.payload)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned()),
                    ShelfKind::Text => Some(format!("{} bytes", item.payload.len())),
                };
                blocks.push(FlyoutBlock::GlyphActionRow {
                    id: CLICK_OPEN_BASE + idx as u32,
                    glyph,
                    glyph_color: color,
                    title: if item.selected {
                        format!("✓ {}", item.label)
                    } else {
                        item.label.clone()
                    },
                    subtitle,
                    badge: Some(badge.to_owned()),
                });
                blocks.push(FlyoutBlock::ButtonBar(vec![
                    (CLICK_COPY_BASE + idx as u32, "Copy".to_owned(), false),
                    (CLICK_REMOVE_BASE + idx as u32, "Remove".to_owned(), false),
                ]));
            }
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::ButtonBar(vec![
                (CLICK_DRAG_ALL, "Drag Out".to_owned(), true),
                (CLICK_PASTE_TEXT, "Add Text".to_owned(), false),
                (CLICK_CLEAR_ALL, "Clear All".to_owned(), false),
            ]));
        }

        FlyoutSpec {
            title: String::new(),
            subtitle: None,
            width_dips: 320.0,
            blocks,
        }
    }

    /// Resolves a numeric `LiveEvent::Click(id)` emitted by [`Self::build_flyout`] into the
    /// symbolic `action_id` accepted by [`Self::handle_flyout_action`].
    pub fn flyout_action_id_for_click(&self, click_id: u32) -> Option<String> {
        match click_id {
            CLICK_DRAG_ALL => Some("drag_all".to_owned()),
            CLICK_PASTE_TEXT => Some("paste_text".to_owned()),
            CLICK_CLEAR_ALL => Some("clear_all".to_owned()),
            id if (CLICK_OPEN_BASE..CLICK_COPY_BASE).contains(&id) => {
                let idx = (id - CLICK_OPEN_BASE) as usize;
                self.items.get(idx).map(|item| format!("open:{}", item.id))
            }
            id if (CLICK_COPY_BASE..CLICK_REMOVE_BASE).contains(&id) => {
                let idx = (id - CLICK_COPY_BASE) as usize;
                self.items.get(idx).map(|item| format!("copy:{}", item.id))
            }
            id if id >= CLICK_REMOVE_BASE => {
                let idx = (id - CLICK_REMOVE_BASE) as usize;
                self.items
                    .get(idx)
                    .map(|item| format!("remove:{}", item.id))
            }
            _ => None,
        }
    }

    /// Dispatches a flyout or Dynamic Island detail action string.
    ///
    /// Supported actions: `"open:<id>"`, `"copy:<id>"`, `"select:<id>"`, `"remove:<id>"`,
    /// `"open_first"`, `"drag_all"`, `"remove_selected"`, `"paste_text"`, `"clear"`, `"clear_all"`.
    pub fn handle_flyout_action(&mut self, action_id: &str) -> bool {
        if let Some(id_str) = action_id.strip_prefix("open:") {
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.open_item(id));
        }
        if let Some(id_str) = action_id.strip_prefix("copy:") {
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.copy_item(id));
        }
        if let Some(id_str) = action_id.strip_prefix("select:") {
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.toggle_select(id));
        }
        if let Some(id_str) = action_id.strip_prefix("remove:") {
            return id_str.parse::<u64>().ok().is_some_and(|id| self.remove(id));
        }
        match action_id {
            "open_first" => self
                .items
                .first()
                .is_some_and(|item| self.open_item(item.id)),
            "drag_all" => self.start_outbound_drag(&[]),
            "remove_selected" => self.remove_selected() > 0,
            "paste_text" => read_clipboard_unicode_text()
                .is_some_and(|text| self.add_text(&text, DEFAULT_MAX_ITEMS)),
            "clear" | "clear_all" => {
                if self.items.is_empty() {
                    return false;
                }
                self.clear_all();
                true
            }
            _ => false,
        }
    }
}

fn clamp_max_items(max_items: usize) -> usize {
    max_items.clamp(MIN_SHELF_ITEMS, MAX_SHELF_ITEMS)
}

fn is_device_path(path: &str) -> bool {
    let p = path.trim_start_matches('"');
    p.starts_with(r"\\.\")
        || p.starts_with(r"\\?\")
        || p.starts_with("//./")
        || p.starts_with("//?/")
}

fn file_basename_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn truncate_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn format_text_preview(text: &str, max_chars: usize) -> String {
    let mut collapsed = String::new();
    let mut prev_space = false;
    for ch in text.trim().chars() {
        let is_ws = ch.is_whitespace();
        if is_ws {
            if !prev_space {
                collapsed.push(' ');
                prev_space = true;
            }
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
        if collapsed.chars().count() > max_chars {
            break;
        }
    }
    if collapsed.chars().count() > max_chars {
        let prefix: String = collapsed
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect();
        format!("{}…", prefix.trim_end())
    } else {
        collapsed
    }
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(SHELF_FILE_NAME);
    let tmp_path = path.with_file_name(format!("{file_name}.tmp.{}.{seq}", std::process::id()));
    let write_res = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

fn shell_open_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || is_device_path(trimmed) || !Path::new(trimmed).exists() {
        return false;
    }
    let verb = wide("open");
    let target = wide(trimmed);
    // SAFETY: both wide buffers are NUL-terminated and live for the duration of the call.
    let instance = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    (instance as isize) > 32
}

fn alloc_hglobal_bytes(bytes: &[u8]) -> *mut c_void {
    if bytes.is_empty() {
        return ptr::null_mut();
    }
    // SAFETY: GlobalAlloc/GlobalLock/GlobalUnlock follow Win32 HGLOBAL ownership rules.
    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len());
        if hmem.is_null() {
            return ptr::null_mut();
        }
        let locked = GlobalLock(hmem);
        if locked.is_null() {
            let _ = GlobalFree(hmem);
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(bytes.as_ptr(), locked.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(hmem);
        hmem
    }
}

fn set_clipboard_unicode_text(text: &str) -> bool {
    let utf16 = wide(text);
    let byte_len = utf16.len() * size_of::<u16>();
    // SAFETY: utf16 is contiguous u16 memory of `byte_len` bytes.
    let raw_bytes = unsafe { std::slice::from_raw_parts(utf16.as_ptr().cast::<u8>(), byte_len) };
    let hmem = alloc_hglobal_bytes(raw_bytes);
    if hmem.is_null() {
        return false;
    }
    // SAFETY: OpenClipboard/EmptyClipboard/SetClipboardData/CloseClipboard are paired cleanly;
    // on SetClipboardData failure we free `hmem` because ownership did not transfer to Windows.
    unsafe {
        if OpenClipboard(ptr::null_mut()) == 0 {
            let _ = GlobalFree(hmem);
            return false;
        }
        let _ = EmptyClipboard();
        let ok = !SetClipboardData(CF_UNICODETEXT, hmem).is_null();
        let _ = CloseClipboard();
        if !ok {
            let _ = GlobalFree(hmem);
        }
        ok
    }
}

fn read_clipboard_unicode_text() -> Option<String> {
    // SAFETY: clipboard handle is locked and unlocked before CloseClipboard.
    unsafe {
        if OpenClipboard(ptr::null_mut()) == 0 {
            return None;
        }
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() {
                return None;
            }
            let size_bytes = GlobalSize(handle);
            let max_units = (size_bytes / size_of::<u16>()).min(MAX_TEXT_BYTES * 2);
            if max_units == 0 {
                return None;
            }
            let locked = GlobalLock(handle);
            if locked.is_null() {
                return None;
            }
            let slice = std::slice::from_raw_parts(locked.cast::<u16>(), max_units);
            let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
            let text = String::from_utf16_lossy(&slice[..len]);
            let _ = GlobalUnlock(handle);
            (!text.trim().is_empty()).then_some(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

// ---------------------------------------------------------------------------
// Minimal Win32 OLE outbound drag-and-drop (IDropSource + IDataObject + IEnumFORMATETC)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum DragPayload<'a> {
    Files(&'a [String]),
    Text(&'a str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragOutcome {
    Dropped,
    Cancelled,
    Failed,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct ComGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const IID_IUNKNOWN: ComGuid = ComGuid {
    data1: 0x0000_0000,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};
const IID_IDROPSOURCE: ComGuid = ComGuid {
    data1: 0x0000_0121,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};
const IID_IDATAOBJECT: ComGuid = ComGuid {
    data1: 0x0000_010E,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};
const IID_IENUMFORMATETC: ComGuid = ComGuid {
    data1: 0x0000_0103,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

#[repr(C)]
#[derive(Clone, Copy)]
struct FormatEtc {
    cf_format: u16,
    ptd: *mut c_void,
    dw_aspect: u32,
    lindex: i32,
    tymed: u32,
}

#[repr(C)]
struct StgMedium {
    tymed: u32,
    hglobal: *mut c_void,
    punk_for_release: *mut c_void,
}

#[repr(C)]
struct DropFilesHeader {
    p_files: u32,
    pt_x: i32,
    pt_y: i32,
    f_nc: i32,
    f_wide: i32,
}

fn build_hdrop_bytes(paths: &[String]) -> Vec<u8> {
    let header_size = size_of::<DropFilesHeader>();
    let mut wide_chars: Vec<u16> = Vec::new();
    for path in paths {
        wide_chars.extend(path.encode_utf16());
        wide_chars.push(0);
    }
    wide_chars.push(0);
    let mut buf = Vec::with_capacity(header_size + wide_chars.len() * 2);
    let header = DropFilesHeader {
        p_files: header_size as u32,
        pt_x: 0,
        pt_y: 0,
        f_nc: 0,
        f_wide: 1,
    };
    // SAFETY: `DropFilesHeader` is plain POD `#[repr(C)]` with no padding holes.
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&raw const header).cast::<u8>(),
            size_of::<DropFilesHeader>(),
        )
    };
    buf.extend_from_slice(header_bytes);
    for unit in wide_chars {
        buf.extend_from_slice(&unit.to_ne_bytes());
    }
    buf
}

fn build_unicode_bytes(text: &str) -> Vec<u8> {
    let units = wide(text);
    let mut buf = Vec::with_capacity(units.len() * 2);
    for unit in units {
        buf.extend_from_slice(&unit.to_ne_bytes());
    }
    buf
}

// ---- IDropSource COM object ----

#[repr(C)]
struct DropSourceVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const ComGuid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    query_continue_drag: unsafe extern "system" fn(*mut c_void, i32, u32) -> i32,
    give_feedback: unsafe extern "system" fn(*mut c_void, u32) -> i32,
}

#[repr(C)]
struct ShelfDropSource {
    vtbl: *const DropSourceVtbl,
    refs: AtomicU32,
}

static DROP_SOURCE_VTBL: DropSourceVtbl = DropSourceVtbl {
    query_interface: ds_query_interface,
    add_ref: ds_add_ref,
    release: ds_release,
    query_continue_drag: ds_query_continue_drag,
    give_feedback: ds_give_feedback,
};

unsafe extern "system" fn ds_query_interface(
    this: *mut c_void,
    riid: *const ComGuid,
    ppv: *mut *mut c_void,
) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    if riid.is_null() {
        unsafe { *ppv = ptr::null_mut() };
        return E_INVALIDARG;
    }
    let iid = unsafe { *riid };
    if iid == IID_IUNKNOWN || iid == IID_IDROPSOURCE {
        unsafe { *ppv = this };
        let _ = unsafe { ds_add_ref(this) };
        S_OK
    } else {
        unsafe { *ppv = ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn ds_add_ref(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this.cast::<ShelfDropSource>()) };
    obj.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn ds_release(this: *mut c_void) -> u32 {
    let prev = {
        let obj = unsafe { &*(this.cast::<ShelfDropSource>()) };
        obj.refs.fetch_sub(1, Ordering::Release)
    };
    let remaining = prev.saturating_sub(1);
    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(unsafe { Box::from_raw(this.cast::<ShelfDropSource>()) });
    }
    remaining
}

unsafe extern "system" fn ds_query_continue_drag(
    _this: *mut c_void,
    escape_pressed: i32,
    key_state: u32,
) -> i32 {
    if escape_pressed != 0 {
        DRAGDROP_S_CANCEL
    } else if key_state & MK_LBUTTON == 0 {
        DRAGDROP_S_DROP
    } else {
        S_OK
    }
}

unsafe extern "system" fn ds_give_feedback(_this: *mut c_void, _effect: u32) -> i32 {
    DRAGDROP_S_USEDEFAULTCURSORS
}

// ---- IEnumFORMATETC COM object ----

#[repr(C)]
struct EnumFormatEtcVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const ComGuid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut FormatEtc, *mut u32) -> i32,
    skip: unsafe extern "system" fn(*mut c_void, u32) -> i32,
    reset: unsafe extern "system" fn(*mut c_void) -> i32,
    clone_enum: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct ShelfEnumFormatEtc {
    vtbl: *const EnumFormatEtcVtbl,
    refs: AtomicU32,
    format: FormatEtc,
    index: AtomicU32,
}

static ENUM_FORMATETC_VTBL: EnumFormatEtcVtbl = EnumFormatEtcVtbl {
    query_interface: ef_query_interface,
    add_ref: ef_add_ref,
    release: ef_release,
    next: ef_next,
    skip: ef_skip,
    reset: ef_reset,
    clone_enum: ef_clone,
};

unsafe extern "system" fn ef_query_interface(
    this: *mut c_void,
    riid: *const ComGuid,
    ppv: *mut *mut c_void,
) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    if riid.is_null() {
        unsafe { *ppv = ptr::null_mut() };
        return E_INVALIDARG;
    }
    let iid = unsafe { *riid };
    if iid == IID_IUNKNOWN || iid == IID_IENUMFORMATETC {
        unsafe { *ppv = this };
        let _ = unsafe { ef_add_ref(this) };
        S_OK
    } else {
        unsafe { *ppv = ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn ef_add_ref(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this.cast::<ShelfEnumFormatEtc>()) };
    obj.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn ef_release(this: *mut c_void) -> u32 {
    let prev = {
        let obj = unsafe { &*(this.cast::<ShelfEnumFormatEtc>()) };
        obj.refs.fetch_sub(1, Ordering::Release)
    };
    let remaining = prev.saturating_sub(1);
    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(unsafe { Box::from_raw(this.cast::<ShelfEnumFormatEtc>()) });
    }
    remaining
}

unsafe extern "system" fn ef_next(
    this: *mut c_void,
    celt: u32,
    rgelt: *mut FormatEtc,
    fetched_out: *mut u32,
) -> i32 {
    if rgelt.is_null() {
        return E_INVALIDARG;
    }
    let obj = unsafe { &*(this.cast::<ShelfEnumFormatEtc>()) };
    let idx = obj.index.load(Ordering::Relaxed);
    let copied = if celt > 0 && idx == 0 {
        unsafe { *rgelt = obj.format };
        obj.index.store(1, Ordering::Relaxed);
        1u32
    } else {
        0u32
    };
    if !fetched_out.is_null() {
        unsafe { *fetched_out = copied };
    }
    if copied == celt { S_OK } else { S_FALSE }
}

unsafe extern "system" fn ef_skip(this: *mut c_void, celt: u32) -> i32 {
    let obj = unsafe { &*(this.cast::<ShelfEnumFormatEtc>()) };
    let next = obj.index.load(Ordering::Relaxed).saturating_add(celt);
    obj.index.store(next.min(1), Ordering::Relaxed);
    if next <= 1 { S_OK } else { S_FALSE }
}

unsafe extern "system" fn ef_reset(this: *mut c_void) -> i32 {
    let obj = unsafe { &*(this.cast::<ShelfEnumFormatEtc>()) };
    obj.index.store(0, Ordering::Relaxed);
    S_OK
}

unsafe extern "system" fn ef_clone(this: *mut c_void, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    let obj = unsafe { &*(this.cast::<ShelfEnumFormatEtc>()) };
    let cloned = Box::new(ShelfEnumFormatEtc {
        vtbl: &raw const ENUM_FORMATETC_VTBL,
        refs: AtomicU32::new(1),
        format: obj.format,
        index: AtomicU32::new(obj.index.load(Ordering::Relaxed)),
    });
    unsafe { *out = Box::into_raw(cloned).cast() };
    S_OK
}

// ---- IDataObject COM object ----

#[repr(C)]
struct DataObjectVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const ComGuid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_data: unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut StgMedium) -> i32,
    get_data_here: unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut StgMedium) -> i32,
    query_get_data: unsafe extern "system" fn(*mut c_void, *const FormatEtc) -> i32,
    get_canonical_format_etc:
        unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut FormatEtc) -> i32,
    set_data:
        unsafe extern "system" fn(*mut c_void, *const FormatEtc, *const StgMedium, i32) -> i32,
    enum_format_etc: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    d_advise:
        unsafe extern "system" fn(*mut c_void, *const FormatEtc, u32, *mut c_void, *mut u32) -> i32,
    d_unadvise: unsafe extern "system" fn(*mut c_void, u32) -> i32,
    enum_d_advise: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct ShelfDataObject {
    vtbl: *const DataObjectVtbl,
    refs: AtomicU32,
    cf_format: u16,
    bytes: Vec<u8>,
}

static DATA_OBJECT_VTBL: DataObjectVtbl = DataObjectVtbl {
    query_interface: do_query_interface,
    add_ref: do_add_ref,
    release: do_release,
    get_data: do_get_data,
    get_data_here: do_get_data_here,
    query_get_data: do_query_get_data,
    get_canonical_format_etc: do_get_canonical_format_etc,
    set_data: do_set_data,
    enum_format_etc: do_enum_format_etc,
    d_advise: do_d_advise,
    d_unadvise: do_d_unadvise,
    enum_d_advise: do_enum_d_advise,
};

fn format_matches(obj: &ShelfDataObject, fmt: &FormatEtc) -> Result<(), i32> {
    if fmt.cf_format != obj.cf_format
        || fmt.dw_aspect != DVASPECT_CONTENT
        || (fmt.lindex != -1 && fmt.lindex != 0)
    {
        return Err(DV_E_FORMATETC);
    }
    if fmt.tymed & TYMED_HGLOBAL == 0 {
        return Err(DV_E_TYMED);
    }
    Ok(())
}

unsafe extern "system" fn do_query_interface(
    this: *mut c_void,
    riid: *const ComGuid,
    ppv: *mut *mut c_void,
) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    if riid.is_null() {
        unsafe { *ppv = ptr::null_mut() };
        return E_INVALIDARG;
    }
    let iid = unsafe { *riid };
    if iid == IID_IUNKNOWN || iid == IID_IDATAOBJECT {
        unsafe { *ppv = this };
        let _ = unsafe { do_add_ref(this) };
        S_OK
    } else {
        unsafe { *ppv = ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn do_add_ref(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this.cast::<ShelfDataObject>()) };
    obj.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn do_release(this: *mut c_void) -> u32 {
    let prev = {
        let obj = unsafe { &*(this.cast::<ShelfDataObject>()) };
        obj.refs.fetch_sub(1, Ordering::Release)
    };
    let remaining = prev.saturating_sub(1);
    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(unsafe { Box::from_raw(this.cast::<ShelfDataObject>()) });
    }
    remaining
}

unsafe extern "system" fn do_get_data(
    this: *mut c_void,
    fmt_ptr: *const FormatEtc,
    med_ptr: *mut StgMedium,
) -> i32 {
    if fmt_ptr.is_null() || med_ptr.is_null() {
        return E_INVALIDARG;
    }
    let obj = unsafe { &*(this.cast::<ShelfDataObject>()) };
    let fmt = unsafe { &*fmt_ptr };
    if let Err(hr) = format_matches(obj, fmt) {
        return hr;
    }
    let hglobal = alloc_hglobal_bytes(&obj.bytes);
    if hglobal.is_null() {
        return E_OUTOFMEMORY;
    }
    unsafe {
        (*med_ptr).tymed = TYMED_HGLOBAL;
        (*med_ptr).hglobal = hglobal;
        (*med_ptr).punk_for_release = ptr::null_mut();
    }
    S_OK
}

unsafe extern "system" fn do_get_data_here(
    _this: *mut c_void,
    _fmt: *const FormatEtc,
    _med: *mut StgMedium,
) -> i32 {
    E_NOTIMPL
}

unsafe extern "system" fn do_query_get_data(this: *mut c_void, fmt_ptr: *const FormatEtc) -> i32 {
    if fmt_ptr.is_null() {
        return E_INVALIDARG;
    }
    let obj = unsafe { &*(this.cast::<ShelfDataObject>()) };
    let fmt = unsafe { &*fmt_ptr };
    match format_matches(obj, fmt) {
        Ok(()) => S_OK,
        Err(hr) => hr,
    }
}

unsafe extern "system" fn do_get_canonical_format_etc(
    _this: *mut c_void,
    _fmt_in: *const FormatEtc,
    fmt_out: *mut FormatEtc,
) -> i32 {
    if !fmt_out.is_null() {
        unsafe { (*fmt_out).ptd = ptr::null_mut() };
    }
    DATA_S_SAMEFORMATETC
}

unsafe extern "system" fn do_set_data(
    _this: *mut c_void,
    _fmt: *const FormatEtc,
    _med: *const StgMedium,
    _release: i32,
) -> i32 {
    E_NOTIMPL
}

unsafe extern "system" fn do_enum_format_etc(
    this: *mut c_void,
    direction: u32,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    // DATADIR_GET = 1
    if direction != 1 {
        unsafe { *out = ptr::null_mut() };
        return E_NOTIMPL;
    }
    let obj = unsafe { &*(this.cast::<ShelfDataObject>()) };
    let enumerator = Box::new(ShelfEnumFormatEtc {
        vtbl: &raw const ENUM_FORMATETC_VTBL,
        refs: AtomicU32::new(1),
        format: FormatEtc {
            cf_format: obj.cf_format,
            ptd: ptr::null_mut(),
            dw_aspect: DVASPECT_CONTENT,
            lindex: -1,
            tymed: TYMED_HGLOBAL,
        },
        index: AtomicU32::new(0),
    });
    unsafe { *out = Box::into_raw(enumerator).cast() };
    S_OK
}

unsafe extern "system" fn do_d_advise(
    _this: *mut c_void,
    _fmt: *const FormatEtc,
    _advf: u32,
    _sink: *mut c_void,
    _conn: *mut u32,
) -> i32 {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn do_d_unadvise(_this: *mut c_void, _conn: u32) -> i32 {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn do_enum_d_advise(_this: *mut c_void, out: *mut *mut c_void) -> i32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    OLE_E_ADVISENOTSUPPORTED
}

fn run_ole_drag_drop(payload: DragPayload<'_>) -> DragOutcome {
    let (cf_format, bytes) = match payload {
        DragPayload::Files(paths) => (CF_HDROP as u16, build_hdrop_bytes(paths)),
        DragPayload::Text(text) => (CF_UNICODETEXT as u16, build_unicode_bytes(text)),
    };
    if bytes.is_empty() {
        return DragOutcome::Failed;
    }

    // SAFETY: OleInitialize is balanced with OleUninitialize only when we own initialization.
    let ole_hr = unsafe { OleInitialize(ptr::null_mut()) };
    let owns_ole = ole_hr >= 0;
    if ole_hr < 0 && ole_hr != RPC_E_CHANGED_MODE {
        return DragOutcome::Failed;
    }

    let data_obj = Box::into_raw(Box::new(ShelfDataObject {
        vtbl: &raw const DATA_OBJECT_VTBL,
        refs: AtomicU32::new(1),
        cf_format,
        bytes,
    }))
    .cast::<c_void>();
    let drop_source = Box::into_raw(Box::new(ShelfDropSource {
        vtbl: &raw const DROP_SOURCE_VTBL,
        refs: AtomicU32::new(1),
    }))
    .cast::<c_void>();

    let mut effect = 0u32;
    // SAFETY: both COM objects have valid vtables and initial refcount 1; DoDragDrop runs
    // synchronously and we release our references immediately after.
    let hr = unsafe {
        DoDragDrop(
            data_obj,
            drop_source,
            DROPEFFECT_COPY | DROPEFFECT_LINK,
            &mut effect,
        )
    };
    unsafe {
        let _ = do_release(data_obj);
        let _ = ds_release(drop_source);
        if owns_ole {
            OleUninitialize();
        }
    }

    if hr == DRAGDROP_S_DROP && effect != 0 {
        DragOutcome::Dropped
    } else if hr == DRAGDROP_S_CANCEL || hr == DRAGDROP_S_DROP {
        DragOutcome::Cancelled
    } else {
        DragOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(tag: &str) -> PathBuf {
        let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("otb-shelf-test-{}-{tag}-{seq}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn max_item_cap_dedup_and_missing_prune() {
        let dir = temp_test_dir("cap");
        let mut files = Vec::new();
        for i in 0..6 {
            let p = dir.join(format!("doc-{i}.txt"));
            fs::write(&p, b"ok").expect("write temp file");
            files.push(p);
        }

        let mut shelf = FileShelfState::default();
        // Requested cap of 2 is clamped up to MIN_SHELF_ITEMS (4).
        let added = shelf.add_files(&files, 2);
        assert_eq!(added, 6);
        assert_eq!(shelf.count(), MIN_SHELF_ITEMS);

        // Adding duplicates adds 0 new items.
        let dup_added = shelf.add_files(&files[2..], 4);
        assert_eq!(dup_added, 0);
        assert_eq!(shelf.count(), MIN_SHELF_ITEMS);

        // Deleting one file on disk causes the next `add_files` call to prune it cleanly.
        let deleted = PathBuf::from(&shelf.items()[0].payload);
        fs::remove_file(&deleted).expect("remove temp file");
        let after_prune = shelf.add_files(&[], 4);
        assert_eq!(after_prune, 0);
        assert_eq!(shelf.count(), MIN_SHELF_ITEMS - 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_truncation_is_utf8_safe_and_builds_48_char_preview() {
        let mut shelf = FileShelfState::default();
        // Multi-byte UTF-8 characters ("é" = 2 bytes) exceeding MAX_TEXT_BYTES.
        let long_text = format!("  hello\n\t{}  ", "é".repeat(3000));
        assert!(shelf.add_text(&long_text, 16));
        assert_eq!(shelf.count(), 1);
        let item = &shelf.items()[0];
        assert_eq!(item.kind, ShelfKind::Text);
        assert!(item.payload.len() <= MAX_TEXT_BYTES);
        assert!(item.label.chars().count() <= MAX_LABEL_CHARS);
        assert!(!item.label.contains('\n'));
        assert!(item.label.ends_with('…'));

        // Re-adding identical text deduplicates rather than adding a second entry.
        assert!(!shelf.add_text(&long_text, 16));
        assert_eq!(shelf.count(), 1);
    }

    #[test]
    fn persistence_round_trip_and_selection_removal() {
        let dir = temp_test_dir("persist");
        let json_path = dir.join("shelf.json");
        let sample = dir.join("notes.md");
        fs::write(&sample, b"# notes").expect("write sample");

        let mut shelf = FileShelfState::default();
        assert_eq!(shelf.add_files(&[sample], 16), 1);
        assert!(shelf.add_text("Quick snippet", 16));
        let first_id = shelf.items()[0].id;
        assert!(shelf.toggle_select(first_id));
        assert!(shelf.items()[0].selected);

        shelf.save_to(&json_path).expect("save shelf");
        let reloaded = FileShelfState::load_from(&json_path);
        assert_eq!(reloaded.items(), shelf.items());

        let mut trimmed = reloaded;
        assert_eq!(trimmed.remove_selected(), 1);
        assert_eq!(trimmed.count(), 1);
        trimmed.clear_all();
        assert!(trimmed.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn island_detail_and_flyout_generation() {
        let mut shelf = FileShelfState::default();
        let empty_detail = shelf.build_island_detail();
        assert_eq!(empty_detail.widget_id, "shelf");
        assert!(empty_detail.rows.len() <= 4);
        assert!(empty_detail.actions.len() <= 3);

        for i in 0..6 {
            assert!(shelf.add_text(&format!("Snippet #{i}"), 8));
        }
        let detail = shelf.build_island_detail();
        assert_eq!(detail.rows.len(), 4);
        assert_eq!(detail.actions.len(), 3);
        assert!(detail.has_full_flyout);

        let flyout = shelf.build_flyout(true);
        assert!(!flyout.blocks.is_empty());
        assert_eq!(
            shelf.flyout_action_id_for_click(CLICK_CLEAR_ALL).as_deref(),
            Some("clear_all")
        );
        let first_remove = shelf
            .flyout_action_id_for_click(CLICK_REMOVE_BASE)
            .expect("remove action");
        assert!(shelf.handle_flyout_action(&first_remove));
        assert_eq!(shelf.count(), 5);
        assert!(shelf.handle_flyout_action("clear_all"));
        assert!(shelf.is_empty());
    }

    #[test]
    fn hdrop_payload_has_valid_header_and_double_nul() {
        let bytes = build_hdrop_bytes(&[r"C:\a.txt".to_owned(), r"C:\b.txt".to_owned()]);
        assert!(bytes.len() > size_of::<DropFilesHeader>());
        let p_files = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(p_files as usize, size_of::<DropFilesHeader>());
        let f_wide = i32::from_ne_bytes(bytes[16..20].try_into().unwrap());
        assert_eq!(f_wide, 1);
        // Trailing double NUL in UTF-16 (4 zero bytes).
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
    }
}
