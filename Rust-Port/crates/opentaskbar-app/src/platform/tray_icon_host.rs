//! Real `Shell_NotifyIcon` hosting for dock-pinned background apps.
//!
//! Unlike C# `TrayHost` this does **not** hide Explorer's `Shell_TrayWnd` — doing so from
//! a second process is fragile and can remove the whole system tray. Instead it hosts a
//! second icon for apps the user explicitly pinned to the dock, using our own hidden
//! window and callback messages. Clicking that icon opens/focuses the app.

#![cfg(windows)]

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, HWND_MESSAGE, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    TranslateMessage, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WM_USER, WNDCLASSEXW,
};

const NIM_ADD: u32 = 0;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const TRAY_MESSAGE: u32 = WM_APP + 0x60;
const STOP_MESSAGE: u32 = WM_USER + 0x71;
const MAX_HOSTED_ICONS: usize = 16;

#[link(name = "shell32")]
unsafe extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *const NotifyIconDataW) -> i32;
    fn ExtractIconW(instance: HANDLE, path: *const u16, index: u32) -> HANDLE;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: HWND,
    u_id: u32,
    u_flags: u32,
    u_callback_message: u32,
    h_icon: HANDLE,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: Guid,
    h_balloon_icon: HANDLE,
}

impl Default for NotifyIconDataW {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[derive(Clone)]
struct HostedIcon {
    id: u32,
    executable_path: String,
    icon: HANDLE,
}

static HOST_HWND: AtomicUsize = AtomicUsize::new(0);
static HOST_STOP: AtomicBool = AtomicBool::new(false);
static HOST_JOIN: OnceLock<Mutex<Option<JoinHandle<()>>>> = OnceLock::new();

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        STOP_MESSAGE => {
            unsafe { PostQuitMessage(0) };
            0
        }
        TRAY_MESSAGE => {
            let event = (lparam as usize & 0xffff) as u32;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                let icons = ICONS.with(|icons| icons.borrow().clone());
                if let Some(icon) = icons.iter().find(|icon| icon.id == wparam as u32) {
                    let _ = std::process::Command::new(&icon.executable_path).spawn();
                    let mut point = POINT { x: 0, y: 0 };
                    unsafe {
                        GetCursorPos(&mut point);
                    }
                }
            }
            0
        }
        WM_DESTROY => {
            unsafe {
                let icons = ICONS.with(|icons| icons.borrow().clone());
                for icon in icons {
                    let data = NotifyIconDataW {
                        cb_size: size_of::<NotifyIconDataW>() as u32,
                        hwnd,
                        u_id: icon.id,
                        ..Default::default()
                    };
                    Shell_NotifyIconW(NIM_DELETE, &data);
                    if !icon.icon.is_null() {
                        DestroyIcon(icon.icon);
                    }
                }
                ICONS.with(|icons| icons.borrow_mut().clear());
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

thread_local! {
    static ICONS: std::cell::RefCell<Vec<HostedIcon>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Starts hosting `icons` (executable paths). Returns true when the hidden window is ready.
pub fn start(icons: &[String]) -> bool {
    if HOST_HWND.load(Ordering::Acquire) != 0 {
        return true;
    }
    HOST_STOP.store(false, Ordering::Release);
    let icons: Vec<String> = icons.iter().take(MAX_HOSTED_ICONS).cloned().collect();
    let Ok(handle) = std::thread::Builder::new()
        .name("ot-tray-icons".to_owned())
        .spawn(move || run(icons))
    else {
        return false;
    };
    let Some(mut slot) = HOST_JOIN.get_or_init(|| Mutex::new(None)).lock().ok() else {
        return false;
    };
    *slot = Some(handle);
    for _ in 0..100 {
        if HOST_HWND.load(Ordering::Acquire) != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

fn run(icons: Vec<String>) {
    unsafe {
        let class_name = wide("OpenTaskBarTrayIconHost");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(host_wnd_proc),
            hInstance: std::ptr::null_mut(),
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        HOST_HWND.store(hwnd as usize, Ordering::Release);

        ICONS.with(|hosted| {
            let mut hosted = hosted.borrow_mut();
            for (index, path) in icons.iter().enumerate() {
                let path_wide = wide(path);
                let icon = ExtractIconW(std::ptr::null_mut(), path_wide.as_ptr(), 0);
                let name = std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path)
                    .to_owned();
                let mut data = NotifyIconDataW {
                    cb_size: size_of::<NotifyIconDataW>() as u32,
                    hwnd,
                    u_id: index as u32 + 1,
                    u_flags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                    u_callback_message: TRAY_MESSAGE,
                    h_icon: icon,
                    ..Default::default()
                };
                let tip = wide(&name);
                let copy = tip.len().min(data.sz_tip.len());
                data.sz_tip[..copy].copy_from_slice(&tip[..copy]);
                if Shell_NotifyIconW(NIM_ADD, &data) != 0 {
                    hosted.push(HostedIcon {
                        id: data.u_id,
                        executable_path: path.clone(),
                        icon,
                    });
                } else if !icon.is_null() {
                    DestroyIcon(icon);
                }
            }
        });

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        HOST_HWND.store(0, Ordering::Release);
        DestroyWindow(hwnd);
    }
}

pub fn stop() {
    if HOST_STOP.swap(true, Ordering::AcqRel) {
        return;
    }
    let hwnd = HOST_HWND.load(Ordering::Acquire);
    if hwnd != 0 {
        unsafe {
            PostMessageW(hwnd as HWND, STOP_MESSAGE, 0, 0);
        }
    }
    if let Some(join) = HOST_JOIN
        .get()
        .and_then(|mutex| mutex.lock().ok())
        .and_then(|mut guard| guard.take())
    {
        let _ = join.join();
    }
}

pub fn is_running() -> bool {
    HOST_HWND.load(Ordering::Acquire) != 0
}
