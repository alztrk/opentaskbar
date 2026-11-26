//! Native Windows toast notifications for update availability (C# `NotificationService` parity).
//!
//! Registers the `OpenTaskbar` AppUserModelId under HKCU so unpackaged builds can show
//! Action Center notifications, then renders a `ToastGeneric` XML template. The action
//! button uses `activationType="protocol"`, so no WinRT event sink is required.

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::core::HSTRING;
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RegCloseKey, RegCreateKeyW, RegSetValueExW,
};

pub const APP_ID: &str = "OpenTaskbar";
pub const MAX_ACTIVE_TOASTS: usize = 8;
const TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);
static REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

pub fn escape_xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Registers the AUMID once per process. Failures are intentionally silent: a toast
/// that cannot be shown must never affect the dock.
pub fn ensure_registered() -> bool {
    if REGISTERED.load(std::sync::atomic::Ordering::Acquire) {
        return true;
    }
    let subkey = wide(r"Software\Classes\AppUserModelId\OpenTaskbar");
    let mut key: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: null-terminated wide strings and a local HKEY out parameter.
    unsafe {
        if RegCreateKeyW(HKEY_CURRENT_USER, subkey.as_ptr(), &mut key) != 0 {
            return false;
        }
        let name = wide("DisplayName");
        let value = wide(APP_ID);
        let _ = RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        );
        if let Ok(exe) = std::env::current_exe() {
            let icon = wide(&exe.to_string_lossy());
            let name = wide("IconUri");
            let _ = RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                icon.as_ptr().cast(),
                (icon.len() * 2) as u32,
            );
        }
        let name = wide("ShowInSettings");
        let enabled: u32 = 1;
        let _ = RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_DWORD,
            (&enabled as *const u32).cast(),
            4,
        );
        let _ = RegCloseKey(key);
    }
    REGISTERED.store(true, std::sync::atomic::Ordering::Release);
    true
}

/// WinRT toast activation needs an initialized COM apartment. The update check runs on a raw
/// background thread, so initialize here and uninitialize only when we actually initialized COM.
struct ComApartment(bool);

impl ComApartment {
    fn enter() -> Self {
        // SAFETY: `CoInitializeEx` takes no pointers; the matching `CoUninitialize` is in Drop.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        // S_OK = 0, S_FALSE = 1 (already initialized). Both mean this call owns a reference.
        Self(hr == 0 || hr == 1)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with the successful `CoInitializeEx` above.
            unsafe { CoUninitialize() };
        }
    }
}

/// Shows an update toast with one button that opens `target_url` in the browser.
pub fn show_update_toast(title: &str, body: &str, action: &str, target_url: &str) -> bool {
    let _apartment = ComApartment::enter();
    ensure_registered();
    let xml = format!(
        "<toast launch=\"{}\"><visual><binding template=\"ToastGeneric\">\
         <text>{}</text><text>{}</text></binding></visual><actions>\
         <action content=\"{}\" arguments=\"{}\" activationType=\"protocol\"/>\
         </actions></toast>",
        escape_xml(target_url),
        escape_xml(title),
        escape_xml(body),
        escape_xml(action),
        escape_xml(target_url),
    );
    let document = match XmlDocument::new() {
        Ok(document) => document,
        Err(_) => return false,
    };
    if document.LoadXml(&HSTRING::from(xml)).is_err() {
        return false;
    }
    let Some(toast) = ToastNotification::CreateToastNotification(&document).ok() else {
        return false;
    };
    let Ok(notifier) = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
    else {
        return false;
    };
    let shown = notifier.Show(&toast).is_ok();
    track(ToastKind::Update, title, shown);
    shown
}

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

// ---- Toast lifecycle / activation (WS-F) ----
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Update,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastState {
    Shown,
    Dismissed,
    Failed,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveToast {
    pub id: u64,
    pub kind: ToastKind,
    pub title: String,
    pub created: Instant,
    pub state: ToastState,
}

#[derive(Default)]
struct ToastStateStore {
    active: Vec<ActiveToast>,
    update_subscribers: Vec<Sender<()>>,
    registered: bool,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn store() -> &'static Mutex<ToastStateStore> {
    static STORE: std::sync::LazyLock<Mutex<ToastStateStore>> =
        std::sync::LazyLock::new(|| Mutex::new(ToastStateStore::default()));
    &STORE
}

fn lock() -> std::sync::MutexGuard<'static, ToastStateStore> {
    store().lock().unwrap_or_else(|p| p.into_inner())
}

/// Creates the C# toast XML (`action=update` launch argument for update toasts).
pub fn build_toast_xml(
    kind: ToastKind,
    title: &str,
    body: &str,
    action_text: Option<&str>,
) -> String {
    let title = escape_xml(title);
    let body = escape_xml(body);
    match (kind, action_text) {
        (ToastKind::Update, Some(action)) => format!(
            "<toast launch=\"action=update\"><visual><binding template=\"ToastGeneric\">\
             <text>{title}</text><text>{body}</text></binding></visual><actions>\
             <action content=\"{}\" arguments=\"action=update\" /></actions></toast>",
            escape_xml(action)
        ),
        _ => format!(
            "<toast><visual><binding template=\"ToastGeneric\">\
             <text>{title}</text><text>{body}</text></binding></visual></toast>"
        ),
    }
}

/// Shows an informational toast. Returns `false` when the toast host could not be started.
pub fn show_info_toast(message: &str) -> bool {
    show_via_host(ToastKind::Info, "OpenTaskbar", message, None)
}

/// Shows an update toast with a `Download` action. Clicking it must call
/// `handle_activation_argument("action=update")` via the app entry point.
pub fn show_update_toast_for_release(version_tag: &str, release_url: &str) -> bool {
    let title = "Update available";
    let body = format!("OpenTaskbar {version_tag} is ready to download.");
    let action = format!("Download {version_tag}");
    let _ = release_url;
    show_via_host(ToastKind::Update, title, &body, Some(&action))
}

fn show_via_host(kind: ToastKind, title: &str, body: &str, action: Option<&str>) -> bool {
    ensure_registered();
    let xml = build_toast_xml(kind, title, body, action);
    let script = powershell_script(&xml);
    let path = script_path();
    let written = fs::write(&path, &script).is_ok();
    let spawned = written
        && std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&path)
            .spawn()
            .is_ok();
    track(kind, title, spawned);
    spawned
}

/// Tracks a toast in the active list without spawning a host process (also used by tests).
pub fn track(kind: ToastKind, title: &str, spawned: bool) -> u64 {
    let mut state = lock();
    prune_locked(&mut state);
    if state.active.len() >= MAX_ACTIVE_TOASTS
        && let Some(oldest) = state.active.first_mut()
    {
        oldest.state = ToastState::Dismissed;
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    state.active.push(ActiveToast {
        id,
        kind,
        title: title.to_owned(),
        created: Instant::now(),
        state: if spawned {
            ToastState::Shown
        } else {
            ToastState::Failed
        },
    });
    id
}

fn script_path() -> PathBuf {
    std::env::temp_dir()
        .join("OpenTaskbar")
        .join(format!("toast-{}.ps1", std::process::id()))
}

/// PowerShell host for WinRT toast APIs (single-quoted XML literal, `'` doubled).
fn powershell_script(xml: &str) -> String {
    let literal = xml.replace('\'', "''");
    format!(
        "[Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom,ContentType=WindowsRuntime] | Out-Null\n\
         [Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime] | Out-Null\n\
         $doc = New-Object Windows.Data.Xml.Dom.XmlDocument\n\
         $doc.LoadXml('{literal}')\n\
         $toast = New-Object Windows.UI.Notifications.ToastNotification($doc)\n\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{APP_ID}').Show($toast)\n"
    )
}

fn prune_locked(state: &mut ToastStateStore) {
    let now = Instant::now();
    for toast in &mut state.active {
        if toast.state == ToastState::Shown && now.duration_since(toast.created) > TOAST_LIFETIME {
            toast.state = ToastState::Expired;
        }
    }
    state
        .active
        .retain(|toast| now.duration_since(toast.created) <= TOAST_LIFETIME * 2);
    while state.active.len() > MAX_ACTIVE_TOASTS {
        state.active.remove(0);
    }
}

/// Marks a toast dismissed (called by activation/dismiss handlers).
pub fn dismiss(id: u64) {
    if let Some(toast) = lock().active.iter_mut().find(|toast| toast.id == id) {
        toast.state = ToastState::Dismissed;
    }
}

pub fn fail(id: u64) {
    if let Some(toast) = lock().active.iter_mut().find(|toast| toast.id == id) {
        toast.state = ToastState::Failed;
    }
}

/// Snapshot of currently tracked toasts (expired ones are pruned first).
pub fn active_toasts() -> Vec<ActiveToast> {
    let mut state = lock();
    prune_locked(&mut state);
    state.active.clone()
}

/// Subscribes to the `UpdateRequested` event.
pub fn subscribe_update_requests() -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    lock().update_subscribers.push(tx);
    rx
}

/// Raises `UpdateRequested`. Called by `handle_activation_argument`.
pub fn notify_update_requested() {
    let mut state = lock();
    state.update_subscribers.retain(|tx| tx.send(()).is_ok());
}

/// Handles a toast activation CLI argument (`--update`, `action=update` or an update URL).
pub fn handle_activation_argument(argument: &str) -> bool {
    let normalized = argument.trim().trim_start_matches('-').to_ascii_lowercase();
    if normalized == "update" || normalized.starts_with("action=update") {
        notify_update_requested();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tracking_tests {
    use super::*;

    #[test]
    fn xml_escaping_and_templates() {
        assert_eq!(
            escape_xml("A & B <c> \"d\" 'e'"),
            "A &amp; B &lt;c&gt; &quot;d&quot; &apos;e&apos;"
        );
        let update = build_toast_xml(ToastKind::Update, "Update", "v1.2", Some("Download"));
        assert!(update.contains("launch=\"action=update\""));
        assert!(update.contains("arguments=\"action=update\""));
        let info = build_toast_xml(ToastKind::Info, "OpenTaskbar", "Hello", None);
        assert!(!info.contains("<actions>"));
    }

    #[test]
    fn activation_raises_event_only_for_update() {
        let rx = subscribe_update_requests();
        assert!(handle_activation_argument("--update"));
        assert!(handle_activation_argument("action=update"));
        assert!(!handle_activation_argument("--settings"));
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn powershell_script_contains_toast_types() {
        let script = powershell_script("<toast/>");
        assert!(script.contains("ToastNotificationManager"));
        assert!(script.contains("$doc.LoadXml('<toast/>')"));
    }

    #[test]
    fn lifecycle_bounds_active_toasts() {
        for _ in 0..MAX_ACTIVE_TOASTS + 3 {
            track(ToastKind::Info, "t", true);
        }
        assert_eq!(active_toasts().len(), MAX_ACTIVE_TOASTS);
        let id = active_toasts().first().map(|toast| toast.id).unwrap();
        dismiss(id);
        assert!(
            active_toasts()
                .iter()
                .any(|toast| toast.state == ToastState::Dismissed)
        );
    }
}
