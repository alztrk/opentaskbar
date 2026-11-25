# OpenTaskbar (Rust + GPUI)

Clean-sheet Rust + GPUI Windows taskbar replacement focused on fast startup and low memory overhead.

## Crates
- `opentaskbar-core`: C# `settings.json` compatible settings model, minimal logger, single-instance lock.
- `opentaskbar-shell`: Non-blocking Explorer taskbar hide/restore, recovery journal, out-of-process `--watchdog`, running-window tracker.
- `opentaskbar-app`: GPUI floating dock (`WindowKind::PopUp`, transparent borderless Win32 `HWND` bridge), fisheye hover magnification, persistent raw-BGRA icon disk cache + short-lived `--extract-icons` worker.

## Settings and background apps

- Settings uses grouped controls, light and dark palettes, and a navigation pane that moves above the page in narrow windows. Use `Tab` and `Shift+Tab` to move between the navigation and settings controls.
- In Background Apps, select an app name to open it. Use the pin icon to add or remove its shortcut from the dock. The close icon asks the app's window to close. Apps that stay in the Windows tray can still appear in the list after receiving this request.
