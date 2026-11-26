//! C# parity for `Services/App/NotificationService.cs`.
//!
//! Registers the `OpenTaskbar` AUMID and shows native toasts through the Windows Runtime
//! (invoked from a short-lived PowerShell host — the Rust workspace does not enable the
//! `windows` crate's `Data_Xml_Dom` / `UI_Notifications` features). Tracks active toasts with
//! a bounded lifecycle and exposes `UpdateRequested` as an event channel.
//!
//! NOTE (cross-WS): toast activation cannot be marshalled back into the GPUI event loop from
//! the PowerShell host. `handle_activation_argument("--update")` must be called from `main.rs`
//! (WS-B/lead owns that file) when the app is launched by a toast click.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

pub const APP_ID: &str = "OpenTaskbar";
pub const MAX_ACTIVE_TOASTS: usize = 8;
const TOAST_LIFETIME: Duration = Duration::from_secs(60);

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

/// Escape XML text/attributes before embedding in the toast template.
pub fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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

/// Registers the AUMID under `HKCU\Software\Classes\AppUserModelId\OpenTaskbar`.
pub fn ensure_registered() -> bool {
    if lock().registered {
        return true;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_SZ, RegCloseKey, RegCreateKeyExW,
            RegSetValueExW,
        };
        let subkey: Vec<u16> = format!(r"Software\Classes\AppUserModelId\{APP_ID}\0")
            .encode_utf16()
            .collect();
        let mut key = std::ptr::null_mut();
        // SAFETY: all buffers are null-terminated; `key` is closed on every successful open.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            return false;
        }
        let exe = std::env::current_exe().unwrap_or_default();
        let set_string = |name: &str, value: &str| unsafe {
            let name: Vec<u16> = format!("{name}\0").encode_utf16().collect();
            let wide: Vec<u16> = format!("{value}\0").encode_utf16().collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                bytes.as_ptr(),
                bytes.len() as u32,
            );
        };
        unsafe {
            set_string("DisplayName", APP_ID);
            if exe.is_file() {
                set_string("IconUri", &exe.to_string_lossy());
            }
            let name: Vec<u16> = "ShowInSettings\0".encode_utf16().collect();
            let one: u32 = 1;
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_DWORD,
                (&one as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            RegCloseKey(key);
        }
    }
    lock().registered = true;
    true
}

/// Shows an informational toast. Returns `false` when the toast host could not be started.
pub fn show_info_toast(message: &str) -> bool {
    show(ToastKind::Info, APP_ID, message, None)
}

/// Shows an update toast with a `Download` action. Clicking it must call
/// `handle_activation_argument("action=update")` via the app entry point.
pub fn show_update_toast(version_tag: &str, release_url: &str) -> bool {
    let title = "Update available";
    let body = format!("OpenTaskbar {version_tag} is ready to download.");
    let action = format!("Download {version_tag}");
    let _ = release_url;
    show(ToastKind::Update, title, &body, Some(&action))
}

fn show(kind: ToastKind, title: &str, body: &str, action: Option<&str>) -> bool {
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
mod tests {
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
