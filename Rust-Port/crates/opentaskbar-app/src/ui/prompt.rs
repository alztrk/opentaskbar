//! Native Win32 UI dialogs: a themed text prompt (for naming profiles/groups) and
//! modern IFileDialog file/folder pickers (C# SaveFileDialog / OpenFileDialog /
//! OpenFolderDialog parity) used by Settings backups, theme import/export and folder stacks.
//!
//! The pickers use the modern COM API so dark mode, Acrylic shell and per-monitor DPI of the
//! rest of the app are preserved.

use opentaskbar_core::i18n as loc;

use std::cell::Cell;
use std::rc::Rc;

use crate::design::fonts;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, FillRect, GetMonitorInfoW, HBRUSH, HDC, HFONT,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, SetBkColor, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, SetFocus, VK_ESCAPE, VK_RETURN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, GWLP_USERDATA, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IsDialogMessageW,
    LoadCursorW, MSG, RegisterClassExW, SW_SHOW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_KEYDOWN, WM_SETFONT, WNDCLASSEXW,
    WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_DLGMODALFRAME, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};

use crate::design::{self, Rgb};
use crate::platform::win32;

const DWMWCP_ROUND: u32 = 2;
const ID_EDIT: usize = 101;
const ID_OK: usize = 102;
const ID_CANCEL: usize = 103;
const EM_SETSEL: u32 = 0x00B1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn colorref(c: Rgb) -> COLORREF {
    u32::from(c.0) | (u32::from(c.1) << 8) | (u32::from(c.2) << 16)
}

struct PromptState {
    edit_hwnd: HWND,
    bg_brush: HBRUSH,
    edit_brush: HBRUSH,
    bg_color: COLORREF,
    edit_bg_color: COLORREF,
    text_color: COLORREF,
    sub_color: COLORREF,
    result: Option<String>,
    done: Rc<Cell<bool>>,
}

fn read_edit_text(edit: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(edit);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..copied.max(0) as usize])
            .trim()
            .to_owned()
    }
}

unsafe extern "system" fn prompt_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };

    match msg {
        WM_ERASEBKGND => {
            let hdc = wparam as HDC;
            let mut rc: RECT = unsafe { std::mem::zeroed() };
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
                FillRect(hdc, &rc, state.bg_brush);
            }
            1
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as HDC;
            unsafe {
                SetTextColor(hdc, state.sub_color);
                SetBkColor(hdc, state.bg_color);
            }
            state.bg_brush as LRESULT
        }
        WM_CTLCOLOREDIT => {
            let hdc = wparam as HDC;
            unsafe {
                SetTextColor(hdc, state.text_color);
                SetBkColor(hdc, state.edit_bg_color);
            }
            state.edit_brush as LRESULT
        }
        WM_CTLCOLORBTN => {
            let hdc = wparam as HDC;
            unsafe {
                SetTextColor(hdc, state.text_color);
                SetBkColor(hdc, state.bg_color);
            }
            state.bg_brush as LRESULT
        }
        WM_COMMAND => {
            let id = wparam & 0xffff;
            if id == ID_OK || id == 1 {
                let text = read_edit_text(state.edit_hwnd);
                if !text.is_empty() {
                    state.result = Some(text);
                }
                state.done.set(true);
                return 0;
            }
            if id == ID_CANCEL || id == 2 {
                state.result = None;
                state.done.set(true);
                return 0;
            }
            0
        }
        WM_KEYDOWN => {
            let vk = wparam as u16;
            if vk == VK_ESCAPE {
                state.result = None;
                state.done.set(true);
                return 0;
            }
            if vk == VK_RETURN {
                let text = read_edit_text(state.edit_hwnd);
                if !text.is_empty() {
                    state.result = Some(text);
                }
                state.done.set(true);
                return 0;
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CLOSE => {
            state.result = None;
            state.done.set(true);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Shows a compact, DPI-aware themed modal text input dialog and returns the trimmed non-empty
/// text entered by the user, or `None` if cancelled or empty.
pub fn ask_name(
    owner_hwnd: usize,
    is_dark: bool,
    title: &str,
    label: &str,
    initial: &str,
) -> Option<String> {
    let class_name = wide("OpenTaskBarNamePrompt");
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(prompt_wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        RegisterClassExW(&wc);
    }

    let dpi = win32::dpi_scale(owner_hwnd);
    let s = |dips: f32| -> i32 { (dips * dpi).round() as i32 };

    let win_w = s(340.0);
    let win_h = s(156.0);

    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
    }
    let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    let work = if unsafe { GetMonitorInfoW(mon, &mut mi) } != 0 {
        mi.rcWork
    } else {
        let (sw, sh) = win32::primary_screen_px();
        RECT {
            left: 0,
            top: 0,
            right: sw,
            bottom: sh,
        }
    };
    let x = (pt.x - win_w / 2).clamp(work.left + 12, (work.right - win_w - 12).max(work.left));
    let y = (pt.y - win_h - 16).clamp(work.top + 12, (work.bottom - win_h - 12).max(work.top));

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_DLGMODALFRAME,
            class_name.as_ptr(),
            wide(title).as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            win_w,
            win_h,
            owner_hwnd as HWND,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return None;
    }

    let pal = design::palette(is_dark);
    let bg_rgb = pal.surface;
    let edit_rgb = if is_dark {
        (0x23, 0x26, 0x2c)
    } else {
        (0xff, 0xff, 0xff)
    };
    let bg_color = colorref(bg_rgb);
    let edit_bg_color = colorref(edit_rgb);
    let text_color = colorref(pal.text_primary);
    let sub_color = colorref(pal.text_secondary);

    let bg_brush = unsafe { CreateSolidBrush(bg_color) };
    let edit_brush = unsafe { CreateSolidBrush(edit_bg_color) };
    let font_face = wide(fonts::native_ui_face());
    let font: HFONT = unsafe {
        CreateFontW(
            -s(fonts::scaled_size(13.0)),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            0,
            0,
            0,
            5,
            0,
            font_face.as_ptr(),
        )
    };

    unsafe {
        let dark_flag: u32 = u32::from(is_dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark_flag as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const u32).cast(),
            size_of::<u32>() as u32,
        );
    }

    let mut client: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
    }
    let cw = (client.right - client.left).max(s(260.0));
    let pad = s(16.0);

    let label_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("STATIC").as_ptr(),
            wide(label).as_ptr(),
            WS_CHILD | WS_VISIBLE,
            pad,
            s(12.0),
            cw - pad * 2,
            s(18.0),
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };

    let edit_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("EDIT").as_ptr(),
            wide(initial).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            pad,
            s(34.0),
            cw - pad * 2,
            s(28.0),
            hwnd,
            ID_EDIT as _,
            hinstance,
            std::ptr::null(),
        )
    };

    let btn_w = s(84.0);
    let btn_h = s(28.0);
    let btn_y = s(76.0);
    let save_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide(&loc::t("ThemeSaveChanges")).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            cw - pad - btn_w,
            btn_y,
            btn_w,
            btn_h,
            hwnd,
            ID_OK as _,
            hinstance,
            std::ptr::null(),
        )
    };
    let cancel_hwnd = unsafe {
        CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide(&loc::t("Cancel")).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            cw - pad - btn_w * 2 - s(8.0),
            btn_y,
            btn_w,
            btn_h,
            hwnd,
            ID_CANCEL as _,
            hinstance,
            std::ptr::null(),
        )
    };

    for child in [label_hwnd, edit_hwnd, save_hwnd, cancel_hwnd] {
        if !child.is_null() && !font.is_null() {
            unsafe {
                SendMessageW(child, WM_SETFONT, font as WPARAM, 1);
            }
        }
    }

    let done = Rc::new(Cell::new(false));
    let mut state = Box::new(PromptState {
        edit_hwnd,
        bg_brush,
        edit_brush,
        bg_color,
        edit_bg_color,
        text_color,
        sub_color,
        result: None,
        done: Rc::clone(&done),
    });
    let state_ptr = &mut *state as *mut PromptState;
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
        if owner_hwnd != 0 {
            let _ = EnableWindow(owner_hwnd as HWND, 0);
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(edit_hwnd);
        SendMessageW(edit_hwnd, EM_SETSEL, 0, -1);
    }

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while !done.get() {
        let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if ret <= 0 {
            break;
        }
        if msg.message == WM_KEYDOWN && msg.wParam as u16 == VK_RETURN {
            unsafe {
                SendMessageW(hwnd, WM_COMMAND, ID_OK, 0);
            }
            continue;
        }
        if msg.message == WM_KEYDOWN && msg.wParam as u16 == VK_ESCAPE {
            unsafe {
                SendMessageW(hwnd, WM_COMMAND, ID_CANCEL, 0);
            }
            continue;
        }
        unsafe {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    let out = state.result.take();
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        if owner_hwnd != 0 {
            let _ = EnableWindow(owner_hwnd as HWND, 1);
        }
        DestroyWindow(hwnd);
        if !font.is_null() {
            DeleteObject(font as _);
        }
        if !bg_brush.is_null() {
            DeleteObject(bg_brush as _);
        }
        if !edit_brush.is_null() {
            DeleteObject(edit_brush as _);
        }
    }
    out
}

// Native Windows file/folder pickers (C# Microsoft.Win32.SaveFileDialog / OpenFileDialog /
// OpenFolderDialog parity) used by Settings backups, theme import/export and folder stacks.
// Uses the modern IFileDialog COM API instead of the legacy common-dialog controls so the
// dark mode, Acrylic shell and per-monitor DPI of the rest of the app are preserved.

use std::path::PathBuf;

use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog,
    IFileDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PWSTR};

fn wide_hstring(text: &str) -> HSTRING {
    HSTRING::from(text)
}

fn init_com() {
    // RPC_E_CHANGED_MODE is fine: the dialog marshals to the owning STA itself.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
}

fn result_path(dialog: &IFileDialog) -> Option<PathBuf> {
    let item: IShellItem = unsafe { dialog.GetResult().ok()? };
    let path: PWSTR = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH).ok()? };
    let text = unsafe { path.to_string().ok()?.to_string() };
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}

fn set_filter(dialog: &IFileDialog, name: &str, pattern: &str) -> Option<Vec<HSTRING>> {
    let name = wide_hstring(name);
    let pattern = wide_hstring(pattern);
    let spec = [COMDLG_FILTERSPEC {
        pszName: windows::core::PCWSTR(name.as_ptr()),
        pszSpec: windows::core::PCWSTR(pattern.as_ptr()),
    }];
    unsafe { dialog.SetFileTypes(&spec).ok()? };
    unsafe { dialog.SetFileTypeIndex(1).ok()? };
    Some(vec![name, pattern])
}

fn base_dialog(clsid: &windows::core::GUID, title: &str) -> Option<IFileDialog> {
    init_com();
    let dialog: IFileDialog = unsafe { CoCreateInstance(clsid, None, CLSCTX_ALL).ok()? };
    let title = wide_hstring(title);
    unsafe {
        dialog
            .SetTitle(windows::core::PCWSTR(title.as_ptr()))
            .ok()?
    };
    Some(dialog)
}

/// Native "Open file" picker. Returns `None` when the user cancels.
pub fn open_file(title: &str, filter_name: &str, filter_pattern: &str) -> Option<PathBuf> {
    let dialog = base_dialog(&FileOpenDialog, title)?;
    let _filters = set_filter(&dialog, filter_name, filter_pattern)?;
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_FILEMUSTEXIST;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}

/// Native "Save file" picker with overwrite confirmation. Returns `None` when cancelled.
pub fn save_file(
    title: &str,
    default_name: &str,
    filter_name: &str,
    filter_pattern: &str,
) -> Option<PathBuf> {
    let dialog = base_dialog(&FileSaveDialog, title)?;
    let _filters = set_filter(&dialog, filter_name, filter_pattern)?;
    let name = wide_hstring(default_name);
    unsafe {
        dialog
            .SetFileName(windows::core::PCWSTR(name.as_ptr()))
            .ok()?
    };
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_OVERWRITEPROMPT;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}

/// Native folder picker (used by Settings folder stacks and theme folder import).
pub fn open_folder(title: &str) -> Option<PathBuf> {
    let dialog = base_dialog(&FileOpenDialog, title)?;
    let options = unsafe { dialog.GetOptions().ok()? } | FOS_PICKFOLDERS;
    unsafe { dialog.SetOptions(options).ok()? };
    unsafe { dialog.Show(None).ok()? };
    result_path(&dialog)
}
