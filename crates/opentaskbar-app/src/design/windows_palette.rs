//! reference parity for `Services/Settings/WindowsPalette.cs` and `WindowsPaletteListener.cs`.
//!
//! Reads the eight RGBA stops of `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Accent\AccentPalette`,
//! falling back to DWM `AccentColor` (ABGR) and `ColorizationColor` (ARGB), and derives ~30 design
//! tokens. A hidden, zero-sized `WS_EX_TOOLWINDOW` HWND watches the personalization broadcasts.
//! **`SystemUsesLightTheme` is authoritative** (OpenTaskbar replaces the taskbar); `AppsUseLightTheme`
//! is only a fallback. `platform::win32::read_system_is_dark()` still reads `AppsUseLightTheme` only —
//! callers that need reference parity must use `system_is_dark()` from this module (WS-B owns that file).

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use super::tokens::{Gradient, Rgba, TokenMap, TokenValue};
use opentaskbar_core::settings::AppTheme;

const ACCENT_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
const DWM_KEY: &str = r"Software\Microsoft\Windows\DWM";
const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// A single read of the user's Windows personalization colors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowsPalette {
    /// Token name → typed value. Empty when Windows exposes no usable accent color.
    tokens: TokenMap,
    dark: bool,
    /// Eight RGBA stops: light3, light2, light1, accent, dark1, dark2, dark3, dark4.
    stops: [Rgba; 8],
    has_accent: bool,
}

impl WindowsPalette {
    /// Number of tokens `WindowsPalette.Read` produces (33, including gradients).
    pub const TOKEN_COUNT: usize = 33;

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn dark(&self) -> bool {
        self.dark
    }

    pub fn accent(&self) -> Rgba {
        self.stops[3]
    }

    pub fn stops(&self) -> &[Rgba; 8] {
        &self.stops
    }

    pub fn tokens(&self) -> &TokenMap {
        &self.tokens
    }

    pub fn get(&self, key: &str) -> Option<&TokenValue> {
        self.tokens.get(key)
    }

    pub fn color(&self, key: &str) -> Option<Rgba> {
        match self.tokens.get(key) {
            Some(TokenValue::Solid(c)) | Some(TokenValue::Color(c)) => Some(*c),
            _ => None,
        }
    }

    /// Merges these tokens *below* the theme pack layer.
    pub fn apply_to(&self, layers: &mut Vec<TokenMap>) {
        if !self.tokens.is_empty() {
            layers.insert(0, self.tokens.clone());
        }
    }

    fn put(&mut self, key: &str, value: TokenValue) {
        self.tokens.insert(key.to_owned(), value);
    }

    fn solid(&mut self, key: &str, color: Rgba, alpha: u8) {
        self.put(key, TokenValue::Solid(color.with_alpha(alpha)));
    }

    fn gradient(&mut self, key: &str, start: Rgba, end: Rgba) {
        self.put(key, TokenValue::Gradient(Gradient::vertical(start, end)));
    }

    fn build(dark: bool, stops: [Rgba; 8], has_accent: bool) -> Self {
        let mut p = Self {
            tokens: TokenMap::new(),
            dark,
            stops,
            has_accent,
        };
        if !has_accent {
            return p;
        }
        let [
            light3,
            _light2,
            light1,
            accent,
            _dark1,
            dark2,
            _dark3,
            _dark4,
        ] = stops;
        let foreground = accent.contrast_foreground();
        let overlay = if dark {
            Rgba::from_rgb(255, 255, 255)
        } else {
            Rgba::from_rgb(0, 0, 0)
        };
        let overlay_alpha = if dark { 0x14 } else { 0x0D };
        let strong_alpha = if dark { 0x22 } else { 0x16 };

        // Accent is reserved for interactive controls; chrome surfaces stay neutral.
        p.solid("AccentBrush", accent, 0xFF);
        p.solid("AccentText", foreground, 0xFF);
        p.solid("AccentTrack", dark2, 0xFF);
        p.solid("StrokeFocus", accent, 0xFF);
        p.solid("IndicatorBrush", accent, 0x90);
        p.solid("IndicatorForegroundBrush", foreground, 0xFF);
        p.solid("WidgetAccent", accent, 0xFF);
        p.solid("WidgetAccentText", foreground, 0xFF);
        p.gradient("WidgetAccentFill", accent, light1);
        p.gradient("WindowsLogoBrush", light1, accent);
        p.solid(
            "SurfaceKeyboardFocus",
            if dark { dark2 } else { light3 },
            0xFF,
        );

        // Windows Settings-style neutral ramp.
        let hex = |s: &str| Rgba::from_hex(s).unwrap_or_default();
        let page = hex(if dark { "#1B2029" } else { "#F3F3F3" });
        let sidebar = hex(if dark { "#171C26" } else { "#EEEEEE" });
        let card = hex(if dark { "#252B37" } else { "#FFFFFF" });
        let track = hex(if dark { "#141922" } else { "#E9E9E9" });
        let border = hex(if dark { "#3A4352" } else { "#E5E5E5" });
        let divider = hex(if dark { "#323A48" } else { "#E8E8E8" });

        p.solid("SettingsBackground", page, 0xFF);
        p.solid("SettingsControl", card, 0xFF);
        p.solid("SettingsControlHover", overlay, overlay_alpha);
        p.solid("SettingsTrack", track, 0xFF);
        p.solid("SettingsSidebarOverlay", sidebar, 0xFF);
        p.solid("SurfacePopup", card, 0xF7);
        p.solid("StrokeSubtle", border, 0xFF);
        p.solid("MenuDivider", divider, 0xFF);
        p.solid("SurfaceHover", overlay, overlay_alpha);
        p.solid("SurfacePressed", overlay, strong_alpha);
        p.solid("SurfaceSelected", overlay, overlay_alpha);
        p.solid("SurfaceSelectedStrong", overlay, strong_alpha);
        p.solid("ShellButtonHover", overlay, if dark { 0x22 } else { 0x14 });
        p.solid(
            "ShellButtonPressed",
            overlay,
            if dark { 0x33 } else { 0x22 },
        );
        p.put(
            "SurfaceHoverColor",
            TokenValue::Color(overlay.with_alpha(overlay_alpha)),
        );
        p.solid("WidgetCardHover", overlay, if dark { 0x22 } else { 0x18 });
        p.solid("WidgetCardPressed", overlay, if dark { 0x38 } else { 0x2A });

        p.solid(
            "TextPrimary",
            if dark {
                Rgba::from_rgb(243, 244, 246)
            } else {
                Rgba::from_rgb(27, 27, 27)
            },
            0xFF,
        );
        p.solid(
            "TextSecondary",
            if dark {
                Rgba::from_rgb(170, 178, 192)
            } else {
                Rgba::from_rgb(97, 102, 110)
            },
            0xFF,
        );
        p.solid("TextMuted", Rgba::from_rgb(124, 134, 152), 0x80);

        // Glass sources; ThemeManager applies each surface's own opacity on top.
        p.gradient(
            "DockMaterial",
            if dark {
                hex("#2B313E")
            } else {
                Rgba::from_rgb(255, 255, 255)
            },
            page,
        );
        p.gradient(
            "MenuMaterial",
            if dark {
                hex("#2A303C")
            } else {
                Rgba::from_rgb(255, 255, 255)
            },
            if dark { hex("#1F2530") } else { hex("#F7F7F7") },
        );
        p
    }

    /// Builds a palette from explicit stops (also used by unit tests).
    pub fn from_stops(stops: [Rgba; 8], dark: bool) -> Self {
        Self::build(dark, stops, true)
    }
}

use opentaskbar_core::wide;

#[cfg(windows)]
fn read_binary(subkey: &str, name: &str, buffer: &mut [u8]) -> Option<usize> {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW,
    };
    let key = wide(subkey);
    let value = wide(name);
    let mut size = buffer.len() as u32;
    // SAFETY: pointers are valid for the duration of the call; RegGetValueW writes at most `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == 0 && size as usize <= buffer.len()).then_some(size as usize)
}

#[cfg(windows)]
fn read_dword(subkey: &str, name: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let key = wide(subkey);
    let value = wide(name);
    let mut data: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reading a single DWORD from HKCU into a correctly sized, aligned local.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

#[cfg(not(windows))]
fn read_binary(_: &str, _: &str, _: &mut [u8]) -> Option<usize> {
    None
}

#[cfg(not(windows))]
fn read_dword(_: &str, _: &str) -> Option<u32> {
    None
}

/// Reads the taskbar/System light flag first, then falls back to the apps flag.
/// `ThemeManager.WindowsTheme()` uses exactly this precedence.
pub fn system_is_dark() -> bool {
    match read_dword(PERSONALIZE_KEY, "SystemUsesLightTheme") {
        Some(light) => light == 0,
        None => read_dword(PERSONALIZE_KEY, "AppsUseLightTheme")
            .map(|light| light == 0)
            .unwrap_or(true),
    }
}

/// Resolves a requested theme to a concrete dark/light mode.
pub fn effective_is_dark(theme: AppTheme) -> bool {
    match theme {
        AppTheme::Dark => true,
        AppTheme::Light => false,
        AppTheme::System => system_is_dark(),
    }
}

/// Reads the live Windows accent palette. Empty when no accent is available.
pub fn read() -> WindowsPalette {
    read_for(system_is_dark())
}

/// Reads with an explicit light/dark mode (useful for previews and tests).
pub fn read_for(dark: bool) -> WindowsPalette {
    let mut bytes = [0u8; 32];
    let mut stops = [Rgba::from_rgb(0, 0, 0); 8];
    if let Some(len) = read_binary(ACCENT_KEY, "AccentPalette", &mut bytes)
        && len >= 28
    {
        for i in 0..8 {
            stops[i] = Rgba::from_rgb(bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2]);
        }
        return WindowsPalette::build(dark, stops, true);
    }

    // AccentColor is ABGR; ColorizationColor is ARGB.
    let accent = read_dword(DWM_KEY, "AccentColor")
        .map(|v| Rgba::new(v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8))
        .or_else(|| {
            read_dword(DWM_KEY, "ColorizationColor")
                .map(|v| Rgba::new((v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8))
        });
    let Some(accent) = accent else {
        return WindowsPalette::default();
    };
    stops[3] = accent;
    stops[0] = accent.mix(Rgba::from_rgb(255, 255, 255), 0.75);
    stops[1] = accent.mix(Rgba::from_rgb(255, 255, 255), 0.55);
    stops[2] = accent.mix(Rgba::from_rgb(255, 255, 255), 0.35);
    stops[4] = accent.mix(Rgba::from_rgb(0, 0, 0), 0.25);
    stops[5] = accent.mix(Rgba::from_rgb(0, 0, 0), 0.45);
    stops[6] = accent.mix(Rgba::from_rgb(0, 0, 0), 0.65);
    stops[7] = stops[6];
    WindowsPalette::build(dark, stops, true)
}

/// Handle for the hidden palette listener thread. Dropping it stops the listener.
pub struct PaletteListener {
    rx: Option<Receiver<WindowsPalette>>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl PaletteListener {
    /// Receives the latest palette after a personalization broadcast (blocking).
    pub fn recv(&self) -> Option<WindowsPalette> {
        self.rx.as_ref().and_then(|rx| rx.recv().ok())
    }

    pub fn try_recv(&self) -> Option<WindowsPalette> {
        self.rx.as_ref().and_then(|rx| rx.try_recv().ok())
    }

    /// Stops the message loop and waits for the hidden HWND to be destroyed.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.rx = None;
    }
}

impl Drop for PaletteListener {
    fn drop(&mut self) {
        self.stop();
    }
}

thread_local! {
    static LISTENER_TX: RefCell<Option<Sender<WindowsPalette>>> = const { RefCell::new(None) };
}

/// Installs the hidden HWND listener (C5 API). Use `recv()` / `try_recv()` on the
/// returned handle to react to changes, or `install_listener_with` for a callback.
pub fn install_listener() -> PaletteListener {
    let (change_tx, change_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("ot-palette-listener".to_owned())
        .spawn(move || listener_loop(change_tx, stop_rx))
        .expect("spawn palette listener");
    PaletteListener {
        rx: Some(change_rx),
        stop: Some(stop_tx),
        thread: Some(thread),
    }
}

/// Variant that invokes `on_change` on a dedicated dispatcher thread. The returned handle
/// stops both the hidden HWND loop and the dispatcher.
pub fn install_listener_with<F>(mut on_change: F) -> PaletteListener
where
    F: FnMut(WindowsPalette) + Send + 'static,
{
    let mut listener = install_listener();
    let rx = listener.rx.take().expect("listener channel");
    // Dropping this sender at dispatcher exit disconnects the hidden HWND's stop channel.
    let inner_stop = listener.stop.take();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("ot-palette-dispatcher".to_owned())
        .spawn(move || {
            let _inner_stop = inner_stop;
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(palette) => on_change(palette),
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("spawn palette dispatcher");
    PaletteListener {
        rx: None,
        stop: Some(stop_tx),
        thread: Some(thread),
    }
}

#[cfg(windows)]
fn is_color_change(section: *const u16) -> bool {
    if section.is_null() {
        return false;
    }
    // SAFETY: WM_SETTINGCHANGE's lParam is a null-terminated UTF-16 section name; cap the
    // scan at 128 units so a malformed broadcast cannot run off the string.
    let (units, terminated) = unsafe {
        let mut units = Vec::new();
        let mut i = 0;
        while i < 128 {
            let unit = *section.add(i);
            if unit == 0 {
                break;
            }
            units.push(unit);
            i += 1;
        }
        (units, i < 128)
    };
    if !terminated {
        return false;
    }
    let section_text = String::from_utf16_lossy(&units).to_ascii_lowercase();
    section_text == "immersivecolorset" || section_text == "windowsthemeelement"
}

#[cfg(windows)]
fn listener_loop(change_tx: Sender<WindowsPalette>, stop_rx: Receiver<()>) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, MSG, PostMessageW,
        PostQuitMessage, RegisterClassW, TranslateMessage, UnregisterClassW, WNDCLASSW,
        WS_EX_TOOLWINDOW,
    };

    const WM_APP_STOP: u32 = 0x8000 + 17;

    LISTENER_TX.with(|slot| *slot.borrow_mut() = Some(change_tx));

    let class_name = wide("OpenTaskbarPaletteListener");
    let window_name = wide("OpenTaskbarPaletteListener");
    // SAFETY: all buffers are null-terminated and the WNDCLASSW is zero-initialized.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(palette_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        if RegisterClassW(&class) == 0 {
            LISTENER_TX.with(|slot| *slot.borrow_mut() = None);
            return;
        }
        // WS_EX_TOOLWINDOW keeps the hidden HWND out of the taskbar and Alt+Tab, while a
        // top-level (non-HWND_MESSAGE) window still receives WM_SETTINGCHANGE broadcasts.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            window_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            UnregisterClassW(class_name.as_ptr(), module);
            LISTENER_TX.with(|slot| *slot.borrow_mut() = None);
            return;
        }
        // HWND is a raw pointer and therefore !Send: pass it as an integer across threads.
        let hwnd_bits = hwnd as usize;
        let _ = std::thread::spawn(move || {
            let _ = stop_rx.recv();
            let hwnd = hwnd_bits as HWND;
            // SAFETY: hwnd was created on the listener thread and is still alive.
            PostMessageW(hwnd, WM_APP_STOP, WPARAM::default(), LPARAM::default());
        });

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_APP_STOP {
                DestroyWindow(hwnd);
                PostQuitMessage(0);
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnregisterClassW(class_name.as_ptr(), module);
        LISTENER_TX.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(windows)]
extern "system" fn palette_wnd_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_DWMCOLORIZATIONCOLORCHANGED, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE,
        WM_THEMECHANGED,
    };
    let changed = match message {
        WM_SETTINGCHANGE => is_color_change(lparam as *const u16),
        WM_SYSCOLORCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED => true,
        _ => false,
    };
    if changed {
        LISTENER_TX.with(|slot| {
            if let Some(tx) = slot.borrow().as_ref() {
                let _ = tx.send(read());
            }
        });
    }
    // SAFETY: default dispatch for all unhandled messages on our own HWND.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(not(windows))]
fn listener_loop(_change_tx: Sender<WindowsPalette>, stop_rx: Receiver<()>) {
    let _ = stop_rx.recv();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> [Rgba; 8] {
        let mut stops = [Rgba::from_rgb(0, 0, 0); 8];
        stops[3] = Rgba::from_rgb(0x38, 0xBD, 0xF8);
        stops[2] = Rgba::from_rgb(0x7D, 0xD3, 0xFB);
        stops[5] = Rgba::from_rgb(0x1E, 0x5C, 0x7A);
        stops
    }

    #[test]
    fn derives_33_tokens_and_wcag_foreground() {
        let palette = WindowsPalette::from_stops(sample(), true);
        assert_eq!(palette.len(), WindowsPalette::TOKEN_COUNT);
        assert_eq!(
            palette.get("AccentBrush"),
            Some(&TokenValue::Solid(Rgba::from_rgb(0x38, 0xBD, 0xF8)))
        );
        assert_eq!(palette.color("AccentText"), Some(Rgba::from_rgb(5, 7, 11)));
        assert_eq!(
            palette.color("IndicatorBrush"),
            Some(Rgba::from_rgb(0x38, 0xBD, 0xF8).with_alpha(0x90))
        );
        assert!(matches!(
            palette.get("DockMaterial"),
            Some(TokenValue::Gradient(_))
        ));
        assert_eq!(
            palette.color("TextMuted"),
            Some(Rgba::from_rgb(124, 134, 152).with_alpha(0x80))
        );
    }

    #[test]
    fn fallback_stops_follow_dwm_mix_rule() {
        let accent = Rgba::from_rgb(0x38, 0xBD, 0xF8);
        let mut bytes = [0u8; 32];
        for i in 0..8 {
            bytes[i * 4] = accent.r;
            bytes[i * 4 + 1] = accent.g;
            bytes[i * 4 + 2] = accent.b;
            bytes[i * 4 + 3] = 0xFF;
        }
        // Pure derivation helper used when DWM exposes only AccentColor.
        let mut stops = [Rgba::from_rgb(0, 0, 0); 8];
        stops[3] = accent;
        stops[2] = accent.mix(Rgba::from_rgb(255, 255, 255), 0.35);
        stops[5] = accent.mix(Rgba::from_rgb(0, 0, 0), 0.45);
        let palette = WindowsPalette::from_stops(stops, false);
        assert_eq!(palette.stops()[2], stops[2]);
        assert_eq!(palette.stops()[5], stops[5]);
        assert_eq!(palette.accent(), accent);
    }
}
