//! N3: Local Clipboard History — zero-thread, strictly bounded text clipboard store.
//!
//! Strict low-RAM invariants:
//! - Polls `GetClipboardSequenceNumber()` on the existing widget tick (zero hidden HWND,
//!   zero background thread).
//! - History length is clamped to `5..=50` (default `15`) and each entry's text is capped at
//!   [`MAX_ENTRY_BYTES`] (16,384 bytes -> worst-case `<= 256 KB` at default capacity).
//! - Honors Windows / password-manager sensitive-content clipboard markers
//!   (`ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory`,
//!   `Clipboard Viewer Ignore`) when `settings.clipboard_ignore_sensitive()` is enabled.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use opentaskbar_core::settings::DockSettings;

use crate::design;
use crate::platform::win32_helpers::wide;
use crate::ui::flyout::{FlyoutBlock, FlyoutSpec};
use crate::widgets::{IslandDetailAction, IslandDetailRow, IslandDetailSpec};

/// Minimum allowed clipboard history capacity.
pub const MIN_MAX_ITEMS: usize = 5;
/// Maximum allowed clipboard history capacity.
pub const MAX_MAX_ITEMS: usize = 50;
/// Default clipboard history capacity.
pub const DEFAULT_MAX_ITEMS: usize = 15;
/// Maximum UTF-8 byte length stored per clipboard entry (16 KB).
pub const MAX_ENTRY_BYTES: usize = 16_384;
/// Maximum preview characters shown in chips, rows, and island details.
pub const MAX_PREVIEW_CHARS: usize = 64;

/// Standard Windows / password-manager clipboard format names marking sensitive content.
pub const SENSITIVE_CLIPBOARD_FORMATS: [&str; 3] = [
    "ExcludeClipboardContentFromMonitorProcessing",
    "CanIncludeInClipboardHistory",
    "Clipboard Viewer Ignore",
];

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
const GMEM_ZEROINIT: u32 = 0x0040;

const CLICK_COPY_BASE: u32 = 100;
const CLICK_PIN_BASE: u32 = 200;
const CLICK_REMOVE_BASE: u32 = 300;
const CLICK_CLEAR_UNPINNED: u32 = 1;
const CLICK_CLEAR_ALL: u32 = 2;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetClipboardSequenceNumber() -> u32;
    fn OpenClipboard(hwnd_new_owner: *mut c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
    fn RegisterClipboardFormatW(format_name: *const u16) -> u32;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalFree(mem: *mut c_void) -> *mut c_void;
    fn GlobalSize(mem: *mut c_void) -> usize;
}

/// One text entry stored in the bounded local clipboard history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardEntry {
    pub id: u64,
    pub text: String,
    pub preview: String,
    pub pinned: bool,
    pub timestamp_unix: u64,
}

/// Pure helper checking whether any present clipboard format name requires skipping capture.
pub fn should_skip_sensitive_formats(ignore_sensitive: bool, present_formats: &[&str]) -> bool {
    if !ignore_sensitive {
        return false;
    }
    present_formats.iter().any(|name| {
        SENSITIVE_CLIPBOARD_FORMATS
            .iter()
            .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
    })
}

/// Bounded local clipboard history store designed to live behind `Option<Box<ClipboardStore>>`.
#[derive(Debug, Clone)]
pub struct ClipboardStore {
    entries: Vec<ClipboardEntry>,
    next_id: u64,
    last_seq: u32,
    sensitive_format_ids: Option<[u32; 3]>,
}

impl Default for ClipboardStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardStore {
    /// Creates an empty store seeded with the current OS clipboard sequence number so
    /// pre-existing clipboard contents are not ingested unexpectedly before the next copy.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            last_seq: current_clipboard_seq(),
            sensitive_format_ids: None,
        }
    }

    fn ensure_sensitive_format_ids(&mut self) -> [u32; 3] {
        if let Some(ids) = self.sensitive_format_ids {
            return ids;
        }
        let ids = SENSITIVE_CLIPBOARD_FORMATS.map(|name| {
            let name_w = wide(name);
            // SAFETY: `name_w` is NUL-terminated UTF-16.
            unsafe { RegisterClipboardFormatW(name_w.as_ptr()) }
        });
        self.sensitive_format_ids = Some(ids);
        ids
    }

    /// Lightweight sequence-number poll (`GetClipboardSequenceNumber`).
    ///
    /// Returns `true` when the store's entries changed.
    pub fn poll(&mut self, settings: &DockSettings) -> bool {
        let seq = current_clipboard_seq();
        if seq == 0 || seq == self.last_seq {
            return false;
        }

        let ignore_sensitive = settings.clipboard_ignore_sensitive();
        let sensitive_ids = if ignore_sensitive {
            self.ensure_sensitive_format_ids()
        } else {
            [0; 3]
        };

        let Some(guard) = ClipboardGuard::open() else {
            // Another process transiently holds the clipboard; keep `last_seq` so we retry next tick.
            return false;
        };
        self.last_seq = seq;

        if ignore_sensitive && has_any_clipboard_format(&sensitive_ids) {
            drop(guard);
            return false;
        }
        let text = guard.read_unicode_text();
        drop(guard);

        let Some(text) = text else {
            return false;
        };
        let max_items = settings
            .clipboard_max_items()
            .clamp(MIN_MAX_ITEMS, MAX_MAX_ITEMS);
        self.ingest_text(&text, max_items, now_unix_secs())
    }

    /// Inserts or promotes a clipboard text item, enforcing the 16 KB per-entry cap and
    /// pinned-preserving LRU eviction.
    pub fn ingest_text(&mut self, text: &str, max_items: usize, timestamp_unix: u64) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        let cap = max_items.clamp(MIN_MAX_ITEMS, MAX_MAX_ITEMS);
        let bounded = truncate_utf8_bytes(text, MAX_ENTRY_BYTES);

        if let Some(pos) = self.entries.iter().position(|entry| entry.text == bounded) {
            let mut existing = self.entries.remove(pos);
            existing.timestamp_unix = timestamp_unix;
            self.entries.insert(0, existing);
            self.evict_excess(cap);
            return true;
        }

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let preview = build_preview(&bounded, MAX_PREVIEW_CHARS);
        self.entries.insert(
            0,
            ClipboardEntry {
                id,
                text: bounded,
                preview,
                pinned: false,
                timestamp_unix,
            },
        );
        self.evict_excess(cap);
        true
    }

    fn evict_excess(&mut self, cap: usize) {
        while self.entries.len() > cap {
            if let Some(idx) = self.entries.iter().rposition(|entry| !entry.pinned) {
                self.entries.remove(idx);
            } else {
                // Every remaining item is pinned; preserve pinned items cleanly.
                break;
            }
        }
    }

    /// Copies entry `id` back to the Windows clipboard and advances `last_seq` so the next
    /// `poll()` does not re-ingest its own write.
    pub fn copy_entry_to_clipboard(&mut self, id: u64) -> bool {
        let Some(text) = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.text.clone())
        else {
            return false;
        };
        if !write_clipboard_unicode_text(&text) {
            return false;
        }
        self.last_seq = current_clipboard_seq();
        true
    }

    /// Toggles the `pinned` flag on entry `id`.
    pub fn toggle_pin(&mut self, id: u64) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.pinned = !entry.pinned;
        true
    }

    /// Removes entry `id` from history.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() != before
    }

    /// Clears all unpinned entries while keeping pinned entries intact.
    pub fn clear_unpinned(&mut self) {
        self.entries.retain(|entry| entry.pinned);
    }

    /// Clears every entry (including pinned ones).
    pub fn clear_all(&mut self) {
        self.entries.clear();
    }

    /// Read-only slice of clipboard entries (most recent first).
    pub fn entries(&self) -> &[ClipboardEntry] {
        &self.entries
    }

    /// Builds the Dynamic Island expanded detail card for Clipboard History.
    pub fn build_island_detail(&self) -> IslandDetailSpec {
        let count = self.entries.len();
        let pinned_count = self.entries.iter().filter(|entry| entry.pinned).count();
        let headline = match count {
            0 => "No History".to_owned(),
            1 => "1 clip".to_owned(),
            n => format!("{n} clips"),
        };
        let subtitle = if count == 0 {
            "Copy text to capture locally".to_owned()
        } else if pinned_count > 0 {
            format!("{pinned_count} pinned · Local memory only")
        } else {
            "Local memory only · Sensitive filtered".to_owned()
        };

        let mut rows: Vec<IslandDetailRow> = self
            .entries
            .iter()
            .take(4)
            .enumerate()
            .map(|(idx, entry)| IslandDetailRow {
                label: if entry.pinned {
                    "Pinned".to_owned()
                } else {
                    format!("#{}", idx + 1)
                },
                value: entry.preview.clone(),
                progress: None,
            })
            .collect();
        if rows.is_empty() {
            rows.push(IslandDetailRow {
                label: "Ready".to_owned(),
                value: "Waiting for text copies".to_owned(),
                progress: None,
            });
        }

        let actions = if count == 0 {
            Vec::new()
        } else {
            vec![
                IslandDetailAction {
                    id: "copy_first".to_owned(),
                    label: "Copy Latest".to_owned(),
                    primary: true,
                },
                IslandDetailAction {
                    id: "clear_unpinned".to_owned(),
                    label: "Clear Unpinned".to_owned(),
                    primary: false,
                },
                IslandDetailAction {
                    id: "clear_all".to_owned(),
                    label: "Clear All".to_owned(),
                    primary: false,
                },
            ]
        };

        IslandDetailSpec {
            widget_id: "clipboard".to_owned(),
            title: "Clipboard".to_owned(),
            headline,
            subtitle,
            rows,
            actions,
            has_full_flyout: true,
        }
    }

    /// Builds the interactive `DockFlyout` spec for Clipboard History.
    pub fn build_flyout(&self, is_dark: bool) -> FlyoutSpec {
        let pal = design::palette(is_dark);
        let count = self.entries.len();
        let mut blocks = vec![FlyoutBlock::HeroHeader {
            glyph: 0xE77F, // Paste / Clipboard
            glyph_color: pal.widget_blue,
            title: "Clipboard History".to_owned(),
            subtitle: Some(if count == 0 {
                "Local, bounded, privacy-aware".to_owned()
            } else {
                let pinned = self.entries.iter().filter(|e| e.pinned).count();
                format!("{count} items · {pinned} pinned")
            }),
            right_value: Some(count.to_string()),
        }];

        if self.entries.is_empty() {
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::Note(
                "Copy any text and it will appear here. Password-manager secrets are skipped."
                    .to_owned(),
            ));
        } else {
            blocks.push(FlyoutBlock::Separator);
            for (idx, entry) in self.entries.iter().take(15).enumerate() {
                blocks.push(FlyoutBlock::GlyphActionRow {
                    id: CLICK_COPY_BASE + idx as u32,
                    glyph: if entry.pinned { 0xE718 } else { 0xE8C8 },
                    glyph_color: if entry.pinned {
                        pal.warning
                    } else {
                        pal.widget_accent
                    },
                    title: entry.preview.clone(),
                    subtitle: Some(format!("{} bytes", entry.text.len())),
                    badge: Some(if entry.pinned { "Pinned" } else { "Copy" }.to_owned()),
                });
                blocks.push(FlyoutBlock::ButtonBar(vec![
                    (
                        CLICK_PIN_BASE + idx as u32,
                        if entry.pinned { "Unpin" } else { "Pin" }.to_owned(),
                        entry.pinned,
                    ),
                    (CLICK_REMOVE_BASE + idx as u32, "Remove".to_owned(), false),
                ]));
            }
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::ButtonBar(vec![
                (CLICK_CLEAR_UNPINNED, "Clear Unpinned".to_owned(), false),
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
            CLICK_CLEAR_UNPINNED => Some("clear_unpinned".to_owned()),
            CLICK_CLEAR_ALL => Some("clear_all".to_owned()),
            id if (CLICK_COPY_BASE..CLICK_PIN_BASE).contains(&id) => {
                let idx = (id - CLICK_COPY_BASE) as usize;
                self.entries.get(idx).map(|e| format!("copy:{}", e.id))
            }
            id if (CLICK_PIN_BASE..CLICK_REMOVE_BASE).contains(&id) => {
                let idx = (id - CLICK_PIN_BASE) as usize;
                self.entries.get(idx).map(|e| format!("pin:{}", e.id))
            }
            id if id >= CLICK_REMOVE_BASE => {
                let idx = (id - CLICK_REMOVE_BASE) as usize;
                self.entries.get(idx).map(|e| format!("remove:{}", e.id))
            }
            _ => None,
        }
    }

    /// Dispatches a flyout or Dynamic Island detail action string.
    ///
    /// Supported actions: `"copy:<id>"`, `"pin:<id>"`, `"remove:<id>"`, `"copy_first"`,
    /// `"clear_unpinned"`, `"clear"`, `"clear_all"`.
    pub fn handle_flyout_action(&mut self, action_id: &str) -> bool {
        if let Some(id_str) = action_id.strip_prefix("copy:") {
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.copy_entry_to_clipboard(id));
        }
        if let Some(id_str) = action_id.strip_prefix("pin:") {
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.toggle_pin(id));
        }
        if let Some(id_str) = action_id.strip_prefix("remove:") {
            return id_str.parse::<u64>().ok().is_some_and(|id| self.remove(id));
        }
        match action_id {
            "copy_first" => self
                .entries
                .first()
                .map(|entry| entry.id)
                .is_some_and(|id| self.copy_entry_to_clipboard(id)),
            "clear_unpinned" => {
                let before = self.entries.len();
                self.clear_unpinned();
                self.entries.len() != before
            }
            "clear" | "clear_all" => {
                if self.entries.is_empty() {
                    return false;
                }
                self.clear_all();
                true
            }
            _ => false,
        }
    }
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Option<Self> {
        // SAFETY: `OpenClipboard(null)` opens the clipboard for the current task; balanced in `Drop`.
        (unsafe { OpenClipboard(ptr::null_mut()) } != 0).then_some(Self)
    }

    fn read_unicode_text(&self) -> Option<String> {
        // SAFETY: clipboard is open for the lifetime of `self`; GlobalSize bounds slice length.
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
                return None;
            }
            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() {
                return None;
            }
            let size_bytes = GlobalSize(handle);
            // Cap UTF-16 scan to twice MAX_ENTRY_BYTES units so huge clipboard blobs never spike RAM.
            let max_units = (size_bytes / size_of::<u16>()).min(MAX_ENTRY_BYTES * 2);
            if max_units == 0 {
                return None;
            }
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                return None;
            }
            let slice = std::slice::from_raw_parts(ptr.cast::<u16>(), max_units);
            let len = slice.iter().position(|&ch| ch == 0).unwrap_or(slice.len());
            let text = String::from_utf16_lossy(&slice[..len]);
            let _ = GlobalUnlock(handle);
            Some(text)
        }
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: balances the successful `OpenClipboard` in `ClipboardGuard::open`.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn current_clipboard_seq() -> u32 {
    // SAFETY: `GetClipboardSequenceNumber` takes no pointers and requires no clipboard lock.
    unsafe { GetClipboardSequenceNumber() }
}

fn has_any_clipboard_format(format_ids: &[u32; 3]) -> bool {
    format_ids
        .iter()
        .any(|&id| id != 0 && unsafe { IsClipboardFormatAvailable(id) } != 0)
}

fn write_clipboard_unicode_text(text: &str) -> bool {
    let utf16 = wide(text);
    let byte_len = utf16.len() * size_of::<u16>();
    // SAFETY: GlobalAlloc/GlobalLock/SetClipboardData ownership transfers on success.
    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, byte_len);
        if hmem.is_null() {
            return false;
        }
        let locked = GlobalLock(hmem);
        if locked.is_null() {
            let _ = GlobalFree(hmem);
            return false;
        }
        ptr::copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), locked.cast::<u8>(), byte_len);
        let _ = GlobalUnlock(hmem);
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

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

fn build_preview(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut prev_ws = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
        if out.chars().count() > max_chars {
            break;
        }
    }
    if out.chars().count() > max_chars {
        let prefix: String = out.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", prefix.trim_end())
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_item_eviction_preserves_pinned_entries() {
        let mut store = ClipboardStore {
            entries: Vec::new(),
            next_id: 1,
            last_seq: 0,
            sensitive_format_ids: None,
        };

        assert!(store.ingest_text("keep-me", 5, 100));
        let pinned_id = store.entries()[0].id;
        assert!(store.toggle_pin(pinned_id));

        for i in 1..=8 {
            assert!(store.ingest_text(&format!("item-{i}"), 5, 100 + i));
        }

        assert_eq!(store.entries().len(), 5);
        assert!(
            store
                .entries()
                .iter()
                .any(|e| e.id == pinned_id && e.text == "keep-me" && e.pinned),
            "pinned entry must survive LRU eviction"
        );
        // Most recent unpinned items remain.
        assert_eq!(store.entries()[0].text, "item-8");
        store.clear_unpinned();
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].id, pinned_id);
    }

    #[test]
    fn dedup_promotes_existing_entry_and_preserves_pin() {
        let mut store = ClipboardStore {
            entries: Vec::new(),
            next_id: 1,
            last_seq: 0,
            sensitive_format_ids: None,
        };

        store.ingest_text("alpha", 10, 10);
        let alpha_id = store.entries()[0].id;
        store.toggle_pin(alpha_id);
        store.ingest_text("beta", 10, 20);
        store.ingest_text("gamma", 10, 30);
        assert_eq!(store.entries()[0].text, "gamma");

        // Re-ingesting "alpha" moves it back to index 0 without duplicating or clearing `pinned`.
        assert!(store.ingest_text("alpha", 10, 40));
        assert_eq!(store.entries().len(), 3);
        assert_eq!(store.entries()[0].id, alpha_id);
        assert!(store.entries()[0].pinned);
        assert_eq!(store.entries()[0].timestamp_unix, 40);
    }

    #[test]
    fn entry_text_is_capped_at_16kb_on_char_boundary() {
        let mut store = ClipboardStore {
            entries: Vec::new(),
            next_id: 1,
            last_seq: 0,
            sensitive_format_ids: None,
        };
        // 3-byte UTF-8 characters ("€") exceeding 16 KB.
        let huge = "€".repeat(8_000);
        assert!(store.ingest_text(&huge, 15, 50));
        let entry = &store.entries()[0];
        assert!(entry.text.len() <= MAX_ENTRY_BYTES);
        assert!(entry.text.len() >= MAX_ENTRY_BYTES - 3);
        assert!(entry.preview.chars().count() <= MAX_PREVIEW_CHARS);
    }

    #[test]
    fn sensitive_format_skip_helper_matches_all_markers() {
        assert!(should_skip_sensitive_formats(
            true,
            &[
                "CF_UNICODETEXT",
                "ExcludeClipboardContentFromMonitorProcessing"
            ]
        ));
        assert!(should_skip_sensitive_formats(
            true,
            &["CanIncludeInClipboardHistory"]
        ));
        assert!(should_skip_sensitive_formats(
            true,
            &["clipboard viewer ignore"]
        ));
        assert!(!should_skip_sensitive_formats(
            false,
            &["ExcludeClipboardContentFromMonitorProcessing"]
        ));
        assert!(!should_skip_sensitive_formats(
            true,
            &["CF_UNICODETEXT", "HTML Format"]
        ));
    }

    #[test]
    fn detail_and_flyout_actions_work() {
        let mut store = ClipboardStore {
            entries: Vec::new(),
            next_id: 1,
            last_seq: 0,
            sensitive_format_ids: None,
        };
        store.ingest_text("first clip", 15, 1);
        store.ingest_text("second clip", 15, 2);

        let detail = store.build_island_detail();
        assert_eq!(detail.widget_id, "clipboard");
        assert_eq!(detail.rows.len(), 2);
        assert_eq!(detail.actions.len(), 3);

        let flyout = store.build_flyout(true);
        assert!(!flyout.blocks.is_empty());
        let pin_action = store
            .flyout_action_id_for_click(CLICK_PIN_BASE)
            .expect("pin action");
        assert!(store.handle_flyout_action(&pin_action));
        assert!(store.entries()[0].pinned);
        assert!(store.handle_flyout_action("clear_unpinned"));
        assert_eq!(store.entries().len(), 1);
        assert!(store.handle_flyout_action("clear_all"));
        assert!(store.entries().is_empty());
    }
}
