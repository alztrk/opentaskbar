# OpenTaskbar

<div align="center">

**A dock for Windows, like the one on macOS.**

[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D6?style=flat-square&logo=windows)](https://github.com/alztrk/opentaskbar)
[![Runtime](https://img.shields.io/badge/Rust-stable-orange?style=flat-square&logo=rust)](https://github.com/alztrk/opentaskbar)
[![Architecture](https://img.shields.io/badge/Architecture-x64%20%7C%20x86-blue?style=flat-square)](https://github.com/alztrk/opentaskbar)
[![License](https://img.shields.io/badge/License-MIT-green?style=flat-square)](LICENSE)
[![Tests](https://img.shields.io/badge/Tests-cargo%20test-brightgreen?style=flat-square)](crates/opentaskbar-core/tests)

<img src="docs/screenshots/hero-desktop-2026-11.jpg" alt="OpenTaskbar on the desktop" width="100%" />
<img src="docs/screenshots/hero-dock-crop-2026-11.png" alt="The floating dock" width="100%" />

</div>

OpenTaskbar replaces the Windows taskbar with a floating row of app icons. Hover over an icon and it grows; click it and the app opens. It works on Windows 10 and 11, and you don't need to install anything or be an administrator to use it.

## What you can do with it

- **See what's open.** Hover over an app icon to see its open windows. Click a window to switch to it, or use the × on its preview to close it.
- **Keep your apps handy.** Drag an app onto the dock to pin it. Move icons around, put related apps in a group, or make a folder stack. Drag an icon off the dock to unpin it.
- **Reach Windows controls.** Start, Search, Task View and the Recycle Bin have their own buttons. Right-click an app to see its Windows Jump List (pinned tasks and recent files).
- **Hide it when you want.** Choose to keep it always visible, hide it when a window covers it, or hide it until you move your mouse to its edge. It also gets out of the way for fullscreen games and videos.
- **Make it look like yours.** Move it to the bottom, left or right of your screen; change its size, corners and see-through look; pick one of six color themes or add your own. You can even change the app icons — see [Themes](docs/themes.md).
- **See useful things at a glance.** Clock, weather, music controls, computer and battery usage, Wi-Fi and Bluetooth switches, volume, focus timer, stopwatch, network speed and how much of your AI plan is left. You can also add your own widgets.
- **Use every screen.** Have a dock on each monitor, or use the optional strip at the top of the screen.
- **Never lose your taskbar.** If OpenTaskbar closes or crashes, the normal Windows taskbar comes back by itself. You can also restore it any time with `OpenTaskbar.exe --restore`.

<div align="center">

| Dock settings | Themes |
| :---: | :---: |
| <img src="docs/screenshots/settings-general.jpg" alt="Dock settings" width="460" /> | <img src="docs/screenshots/settings-themes.jpg" alt="Theme gallery" width="460" /> |

</div>

## Mouse shortcuts

| Do this | It does |
| :--- | :--- |
| `Shift + mouse wheel` over the dock | Scroll sideways through icons |
| `Ctrl + mouse wheel` over the dock | Scroll sideways, more precisely |
| `Middle mouse button + drag` | Move through icons that don't fit |
| `Alt + F4` | Close a window preview |
| `Esc` | Close an open menu or popup |

## Get it running

1. Go to [GitHub Releases](https://github.com/alztrk/opentaskbar/releases) and download `OpenTaskbar-v0.4.0-win-x64.exe` (most PCs) or `OpenTaskbar-v0.4.0-win-x86.exe` (32-bit Windows).
2. Double-click the file. That's it — no installer, no extra software, no admin rights.

You need Windows 10 (version 2004 or newer) or Windows 11. The portable build checks for new versions itself. Releases also include a ZIP, checksums and release notes.

To change the language or the look, open **Settings → Appearance**. There are 14 languages; see [Languages](docs/localization.md).

## Privacy and license

OpenTaskbar doesn't collect anything about you. It only goes online when you turn on a feature that needs it, such as weather or update checks, and your credentials stay on your computer. See [PRIVACY.md](PRIVACY.md). It's free under the MIT license — see [LICENSE](LICENSE).

<details>
<summary><strong>Technical details (for developers)</strong></summary>

### Build and check it

```powershell
git clone https://github.com/alztrk/opentaskbar.git
cd opentaskbar

cargo run --bin OpenTaskBar
cargo run --bin OpenTaskBar -- --settings

# Portable EXE and checks
.\scripts\publish-portable.ps1 -Runtime win-x64
.\scripts\check-portable.ps1 -OutputDir release -ExecutableName OpenTaskbar-x64.exe

# MSIX package
.\scripts\publish-rust-msix.ps1 -Runtime win-x64
```

You need Rust stable (see `rust-version` in `Cargo.toml`) and PowerShell 7+ for packaging.

Verify with `cargo check --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. Automated tests cover logic and settings; real Windows behavior needs the manual checks in [docs/windows-reliability-audit.md](docs/windows-reliability-audit.md).

### Command line

| Argument | Effect |
| :--- | :--- |
| *(none)* | Acquire single-instance lock, restore pending journal, load settings, run the dock |
| `--settings` | Open only the Settings window; exit when it closes |
| `--restore` | Replay the flyout/taskbar recovery journal and exit |
| `--extract-icons` | Icon extractor worker driven over stdin/stdout |
| `--software-render` | Force software rendering for this launch |

### Where data is stored

Everything lives in `%LOCALAPPDATA%\OpenTaskbar\`: `settings.json` (preferences and pins), `taskbar-state.txt` (taskbar recovery journal), `opentaskbar.log` (diagnostics, rotated to `opentaskbar.log.old`) and `Themes\` (theme packs). The old separate-folder layout is retired; import a full backup to migrate from it.

### Code layout

| Crate | Responsibility |
| :--- | :--- |
| `opentaskbar-core` | Settings, 14 locale catalogs, paths, deterministic logic |
| `opentaskbar-shell` | Taskbar hide/restore, recovery journal, watchdog |
| `opentaskbar-app` | GPUI dock, widgets, flyouts and Win32 platform layer |

Bundled resources live in `Fonts/`, `Assets/`, `themes/` and `tools/watchdog/`. CI runs checks, tests and an x86 watchdog build on every branch.

### Feature flags

Flags are read from `DockSettings.extra`; defaults match the code.

| Flag | Default | Effect |
| :--- | :--- | :--- |
| `enable_live_tray_host` | `false` | Registry discovery and tray strip leave the native tray untouched |
| `native_flyout_reposition` | `true` | Generation-cancelled Start/Search/Control Center positioning; `false` uses journal-only legacy positioning |
| `enable_vertical_dock` | `true` | Left/right edges enabled; `false` forces Bottom |
| `enable_vendor_battery_probes` | `false` | Generic HID capacity reading until vendor transports are enabled |
| `enable_theme_editor` | `false` | Theme list/import/export only; no token editing |
| `enable_ai_oauth` | `true` | AI account sign-in flows; missing token stays in the "Sign in" state |

</details>
