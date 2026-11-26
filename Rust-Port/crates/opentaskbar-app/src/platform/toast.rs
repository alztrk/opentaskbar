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

const APP_ID: &str = "OpenTaskbar";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn escape_xml(value: &str) -> String {
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
pub fn ensure_registered() {
    let subkey = wide(r"Software\Classes\AppUserModelId\OpenTaskbar");
    let mut key: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: null-terminated wide strings and a local HKEY out parameter.
    unsafe {
        if RegCreateKeyW(HKEY_CURRENT_USER, subkey.as_ptr(), &mut key) != 0 {
            return;
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
    notifier.Show(&toast).is_ok()
}
