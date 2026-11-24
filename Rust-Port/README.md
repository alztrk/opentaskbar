# OpenTaskbar (Rust + GPUI)

Clean-sheet Rust + GPUI Windows taskbar replacement focused on fast startup and low memory overhead.

## Crates
- `opentaskbar-core`: C# `settings.json` compatible settings model, minimal logger, single-instance lock.
- `opentaskbar-shell`: Non-blocking Explorer taskbar hide/restore, recovery journal, out-of-process `--watchdog`, running-window tracker.
- `opentaskbar-app`: GPUI floating dock (`WindowKind::PopUp`, transparent borderless Win32 `HWND` bridge), fisheye hover magnification, persistent raw-BGRA icon disk cache + short-lived `--extract-icons` worker.
