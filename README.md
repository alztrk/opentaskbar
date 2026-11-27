# OpenTaskbar

<div align="center">

**A modern, customizable floating dock and modular widget system for Windows 10 and Windows 11.**

[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D6?style=flat-square&logo=windows)](https://github.com/alztrk/opentaskbar)
[![Runtime](https://img.shields.io/badge/Rust-stable-orange?style=flat-square&logo=rust)](https://github.com/alztrk/opentaskbar)
[![Architecture](https://img.shields.io/badge/Architecture-x64%20%7C%20x86-blue?style=flat-square)](https://github.com/alztrk/opentaskbar)
[![License](https://img.shields.io/badge/License-MIT-green?style=flat-square)](LICENSE)
[![Tests](https://img.shields.io/badge/Tests-cargo%20test-brightgreen?style=flat-square)](Rust-Port/crates/opentaskbar-core/tests)

<br />

<img src="docs/screenshots/hero-desktop-2026-11.jpg" alt="OpenTaskbar Desktop Showcase" width="100%" />

<br />

<img src="docs/screenshots/hero-dock-crop-2026-11.png" alt="OpenTaskbar Floating Dock" width="100%" />

</div>

---

## Overview

OpenTaskbar is a lightweight, macOS-inspired floating dock and taskbar replacement for Windows 10 and Windows 11. Built as a self-contained Rust/GPUI application, it delivers high performance, fluid magnification, live window thumbnail previews, modular hardware and media widgets, and foolproof native taskbar recovery.

---

## Key Features

### Optical Glass Dock
- **Aurora Visual Language:** Dark (Midnight Obsidian) and Light glass materials with top specular sheen, 1px optical bevel, and soft ambient drop shadows.
- **Dock Edge Placement:** Position the dock at the Bottom, Left, or Right edge of your primary screen, with independent alignment (Center, Left, Right, or Full-Width).
- **Size, Scale & Corner Radius:** Choose between Compact (48 DIP), Medium (58 DIP), and Large (66 DIP) presets, fine-tune scale from 75% to 150%, and customize frame corner radius from 0 px (sharp) to 32 px while screen edge spacing adjusts automatically from the active profile.
- **Per-Surface Materials:** Configure the dock, menus, widgets and Settings window independently. Each surface has Glass (translucent), Solid (fully opaque), True black (OLED, opaque `#000000` with no rim or shadow) or None (background, rim and shadow completely removed) with its own 0–100% opacity and live preview.
- **OLED / Display Care:** Reduce glow and shadows, drift the dock a pixel or two on a configurable interval to discourage burn-in, apply a temporary true-black look on low battery or Night Light, collapse to a minimal clock/battery strip after five minutes idle, and cap resource-based animations at 30/60/120 FPS. Every option is independent and lives in Settings → Appearance.
- **Two-Sided Dock Resizing & Edge Scrolling:** Drag either outer border to narrow or expand the dock while keeping widgets at full width. Overflowing items scroll smoothly via mouse wheel, middle-click panning, or clickable edge chevrons with directional gradient scrims aligned to the outer dock borders.

### Theme Packs
- **Theme Packs:** Restyle the dock with six bundled themes installed in `%LOCALAPPDATA%\OpenTaskbar\Themes` on first launch, or add your own theme packs there. Select them in Settings → Appearance → Theme.
- **Languages:** The first launch follows the Windows display language when OpenTaskbar has a translation for it. Choose a language in Settings → Appearance; see [docs/localization.md](docs/localization.md) to add another locale.
- **Icon Packs:** Replace app, Start button and Recycle Bin icons; match by executable name, full path, Store app ID, or simply name a file after the executable (`firefox.png`).
- **Fallback Plates:** Apps without a pack image are placed on a Leaf, Petal, Rounded, or Circle plate so the whole dock matches.
- **Widget Packs:** Replace design tokens (colors, gradients, corner radii, sizes, fonts) for the dock, widgets, menus, and Settings. See [docs/themes.md](docs/themes.md).
- **Prism Theme:** A ready-made theme in [themes/prism](themes/prism) with category icons on lit squircle plates and a graphite, violet-accented style. No application logos; duotone glyphs from Phosphor (MIT).

### Interaction and Motion
- **macOS-Style Fisheye Magnification:** Spring-physics raised-cosine magnification (`DockFisheyeLayout`) that smoothly expands icon slots and the floating dock frame when monitor space allows, caps peak scale automatically to stay overlap-free on full-width or scrolling docks, and dynamically lifts thumbnail previews clear of magnified icons.
- **Launch Feedback:** Tactile vertical launch bounce animation on application click.
- **Drag-and-Drop & App Groups:** Drag applications directly onto the dock to pin, reorder freely across apps/stacks/widgets, organize apps into collapsible dock groups with member count badges, or drag out to unpin.

### Intelligent Window Management
- **Live DWM Thumbnails:** Interactive live window thumbnail previews with window titles and close buttons.
- **Native Jump Lists:** Right-click any application to open its native Windows Jump List with pinned tasks and recent files.
- **Foreground Highlighting:** Active foreground windows are clearly distinguished with illuminated accent capsules, independent of keyboard focus.
- **Packaged App Icons:** Running Store and packaged apps without executable icon resources show their Start menu icon. Windows groups these apps by their AppUserModelId (AUMID); apps hosted by ApplicationFrameHost share one dock cell that shows the icon of the topmost hosted app.
- **Multi-Monitor Docks:** Optional secondary docks for multi-monitor setups.

### Smart Auto-Hide (IntelliHide)
- **Three Auto-Hide Modes:** Disabled (always visible), Smart (IntelliHide), and Always (hidden until edge hover).
- **IntelliHide Logic:** The dock stays visible on the desktop and behind non-overlapping windows. When an active window overlaps the dock region, the dock smoothly slides off-screen.
- **Fullscreen Detection:** Automatically yields and hides when full-screen applications, borderless games, or browser videos are playing.

### Modular Widget System
- **AI Usage Providers:** Dedicated per-provider widgets for Claude, Codex, OpenCode, GitHub Copilot, DeepSeek, and OpenRouter with session/weekly quota bars or rings and reset countdowns. Credentials are discovered locally without logging or exporting them; DeepSeek/OpenRouter/OpenCode keys can be stored encrypted with Windows DPAPI.
- **Bluetooth Battery:** Live Windows PnP battery levels for connected Bluetooth headsets, mice, keyboards, controllers, watches, and phones with automatic device-type icons and active-node filtering.
- **Real-Time Hardware Meters:** CPU usage, RAM usage, GPU usage, and GPU temperature rings with customizable thresholds. Temperature uses vendor-neutral WDDM adapter data first (AMD/Intel/NVIDIA, WDDM 2.4+), then optional NVIDIA NVML and AMD ADL libraries. Intel GPUs need driver-published WDDM data; there is no non-admin Intel API.
- **Media Controls:** Now Playing track information, artist metadata, track seek bar, and volume slider.
- **Clock, Calendar and Weather:** Unified date and time capsule with detailed optical glass weather and multi-day forecast flyout.
- **Quick Settings and Connectivity:** Wi-Fi, Bluetooth and Night Light controls sit beside shortcuts to native Windows quick actions and settings, including available networks, casting, airplane mode, mobile hotspot, accessibility and Live captions. Direct controls show while Windows applies a change. Audio offers output/input selection and a per-app volume mixer.
- **Productivity Widgets:** Focus Pomodoro timer, Stopwatch, Countdown timer, Storage drive usage, Network Speed, and Day Progress bar.
- **System Tray Management:** Background tray apps manager with native tray context menus, window restore, and confirmed process termination.
- **Interactive Catalog:** Built-in widget catalog grouped under inline category headings with live widget previews and expandable configuration drawers with instant auto-save.
- **Optional Dynamic Island:** When enabled, the top-attached island remains visible on every connected display, even before widgets are assigned. Assign each widget to the dock or a chosen display's island; assigned widgets move off that display's dock. The island supports per-display paging, visibility rules, and full-screen hiding.

### Extras and Integration
- **Search Button:** A Start-adjacent dock button that opens the native Windows Search flyout (Win+S), with the same cell size as app icons and a Settings toggle.
- **Permanent Shell Buttons:** Task View with previous/next/new-desktop shortcuts, a background-apps (tray) button with its native-style flyout, and a slim Show Desktop strip (Win+D).
- **Folder Stacks:** Quick access to folders (such as Downloads) with 2x2 miniature icon previews.
- **Recycle Bin Integration:** Live recycle bin icon supporting drag-and-drop file deletion and one-click emptying.
- **Dock Menu:** Right-click the dock for Windows Search, folder stacks, profiles, Task Manager, update checks, and settings.

### Foolproof Taskbar Recovery
- **Zero Risk:** State journaling records every taskbar replacement action. On exit, system shutdown, or unexpected termination, OpenTaskbar automatically restores the native Windows taskbar.
- **Verified Restore:** Recovery re-asserts the original taskbar state and visibility several times and keeps the journal until the taskbar is actually visible, so an Explorer restart cannot silently lose the recovery state.
- **Self-Healing Startup:** A pending journal is repaired after the singleton check and before the dock starts.
- **Crash Watchdog:** A detached lightweight native C Win32 helper (`tools/watchdog/watchdog.c`) handles cases no in-process handler can (End Task, `taskkill /F`, FailFast, or unexpected exit), restores the Windows taskbar, and exits immediately.
- **Manual Escape Hatch:** `OpenTaskbar.exe --restore` forces the native taskbar back at any time, even when OpenTaskbar is not running.

---

## Visual Showcase

<div align="center">

| Dock Settings | Themes Gallery |
| :---: | :---: |
| <img src="docs/screenshots/settings-general.jpg" alt="OpenTaskbar Dock Settings" width="460" /> | <img src="docs/screenshots/settings-themes.jpg" alt="OpenTaskbar Themes Gallery" width="460" /> |

</div>

### Star Growth

<div align="center">

<img src="https://api.star-history.com/svg?repos=alztrk/opentaskbar&type=Date" alt="OpenTaskbar star growth over time" width="100%" />

</div>

---

## Keyboard Shortcuts

| Shortcut | Description |
| :--- | :--- |
| `Shift + Mouse Wheel` | Scroll dock contents horizontally |
| `Ctrl + Mouse Wheel` | Scroll dock contents horizontally (high precision) |
| `Middle Click + Drag` | Pan overflowing dock contents |
| `Alt + F4` | Close window preview card |
| `Esc` | Close active flyout or stack |

---

## Requirements

- **Operating System:** Windows 10 (version 2004 / build 19041+) or Windows 11 (x64 or x86)
- **Runtime:** None. The Rust binary is self-contained; the native watchdog is compiled into it.
- **Building from source:** Rust stable toolchain (see `rust-version` in `Rust-Port/Cargo.toml`) and PowerShell 7+ for the packaging scripts.

---

## Getting Started

### Portable Release

Download `OpenTaskbar-Rust-v<version>-win-x64.exe` or `OpenTaskbar-Rust-v<version>-win-x86.exe` for your architecture from [GitHub Releases](https://github.com/alztrk/opentaskbar/releases) and launch it directly. No installer, administrator privileges or separate runtime required. Each release also ships a deterministic ZIP with the EXE, `Rust-Port/README.md` and `LICENSE`, plus `checksums.txt`, `release-manifest.json` and release notes. The in-app updater automatically picks the matching architecture.

### Build from Source

```powershell
# Clone the repository
git clone https://github.com/alztrk/opentaskbar.git
cd opentaskbar

# Run directly from source (binary name is OpenTaskBar)
cargo run --manifest-path Rust-Port/Cargo.toml --bin OpenTaskBar

# Run with the Settings window open on startup
cargo run --manifest-path Rust-Port/Cargo.toml --bin OpenTaskBar -- --settings

# Build self-contained portable executables into release\
powershell -ExecutionPolicy Bypass -File ".\scripts\publish-rust-portable.ps1"
powershell -ExecutionPolicy Bypass -File ".\scripts\publish-rust-portable.ps1" -Runtime win-x86

# Verify the built EXEs (PE architecture, embedded watchdog, icon worker smoke test)
powershell -ExecutionPolicy Bypass -File ".\scripts\check-rust-portable.ps1" -ExecutableName OpenTaskbar-Rust-x64.exe
powershell -ExecutionPolicy Bypass -File ".\scripts\check-rust-portable.ps1" -ExecutableName OpenTaskbar-Rust-x86.exe

# Build MSIX packages (release\msix-rust\OpenTaskbarRust_<version>_<arch>.msix)
powershell -ExecutionPolicy Bypass -File ".\scripts\publish-rust-msix.ps1" -Runtime win-x64

# Package both builds into release\: versioned EXEs/ZIPs, checksums.txt,
# release-manifest.json and release-notes.md (same step the release workflow uses)
powershell -ExecutionPolicy Bypass -File ".\scripts\package-rust-release.ps1" -Tag "v0.4.0"

# Rust workspace checks (same commands as CI)
cargo fmt --manifest-path Rust-Port/Cargo.toml --all -- --check
cargo clippy --manifest-path Rust-Port/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path Rust-Port/Cargo.toml --workspace
```

`cargo test --workspace` runs the deterministic logic, i18n and settings-compatibility checks; live Windows shell behavior still needs the manual smoke list in [docs/windows-reliability-audit.md](docs/windows-reliability-audit.md).

### Fjord top bar

To open the optional top bar, run `OpenTaskBar.exe --fjord-bar` or `cargo run --manifest-path Rust-Port/Cargo.toml --bin OpenTaskBar -- --fjord-bar`. The bar opens on each display and can be closed from its desktop menu. The same menu opens Fjord Files.

Use **Settings → Dock → Enable Fjord Files** to control the file browser. Turning it off closes any open Fjord Files windows and removes its entries from the dock and the top bar. A `--files` shortcut with an explicit folder or This PC target then opens in Windows File Explorer. A shortcut with no target simply exits.

Click the Wi-Fi icon to scan, connect, or disconnect. For a new network, **Remember network** saves a per-user Windows Wi-Fi profile after a successful connection. Saved profiles support manual reconnection with the stored credentials. On Windows versions that restrict nearby Wi-Fi access, the panel reports when Windows location permissions block the scan.

---

## Configuration

User preferences, pinned item order, and widget configurations are stored locally in `%LOCALAPPDATA%\OpenTaskbar\`:

- `settings.json` : User preferences, dock scaling, pinned applications, and active widgets (PascalCase schema).
- `taskbar-state.txt` : Native Windows taskbar state recovery journal.
- `opentaskbar-rust.log` : Diagnostic and operational log (rotation: `opentaskbar-rust.log.old`).
- `Themes\` : Theme packs (icon and widget packs). See [docs/themes.md](docs/themes.md).

The Rust application is the only writer of this shared folder; the old `%LOCALAPPDATA%\OpenTaskBar-Rust\` location is retired and no longer read on startup. Settings compatibility with existing installations is preserved, including keys carried over from earlier builds. Use a full-backup import when migrating from the older separate-folder layout.

---

## Design System

For in-depth documentation on OpenTaskbar's design tokens, materials, typography, and layout metrics, see [Design/README.md](Design/README.md). All shipped UI resources live under `Design/*`.

---

## Source layout and release verification

The application is implemented in Rust + GPUI under [`Rust-Port/`](Rust-Port/README.md): `crates/opentaskbar-core` (settings, i18n, paths, deterministic logic), `crates/opentaskbar-shell` (shell journal, taskbar/app-bar, native watchdog embedding) and `crates/opentaskbar-app` (GPUI dock, widgets and Win32 platform layer). Locale catalogs cover 14 languages, theme packs and the recovery journal live in the shared `%LOCALAPPDATA%\OpenTaskbar` folder, and diagnostics go to `opentaskbar-rust.log`.

`scripts/publish-rust-portable.ps1 -Runtime win-x64` (or `win-x86`) builds the portable EXE; `scripts/check-rust-portable.ps1` verifies its PE architecture, embedded watchdog and icon-worker startup. Release assets are generated by `scripts/package-rust-release.ps1` and include versioned x64/x86 EXEs and deterministic ZIPs, `checksums.txt`, a schemaVersion 2 `release-manifest.json` (`rustPort.crateVersion`, per-arch watchdog PE machine and SHA-256) and `release-notes.md`. They are covered by SLSA provenance via `actions/attest` and an optional report-only VirusTotal scan. `scripts/check-release-version.ps1` gates the release tag against the Cargo workspace version (`0.4.0`) and the Rust MSIX identity (`0.4.0.0`) before any build.

---

## License

Distributed under the MIT License. See [LICENSE](LICENSE) for details.
