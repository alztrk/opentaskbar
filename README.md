# OpenTaskbar

<div align="center">

**A modern, customizable floating dock and modular widget system for Windows 10 and Windows 11.**

[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D6?style=flat-square&logo=windows)](https://github.com/alztrk/opentaskbar)
[![Runtime](https://img.shields.io/badge/Rust-1.95%2B-orange?style=flat-square&logo=rust)](https://github.com/alztrk/opentaskbar)
[![Architecture](https://img.shields.io/badge/Architecture-x64%20%7C%20x86-blue?style=flat-square)](https://github.com/alztrk/opentaskbar)
[![License](https://img.shields.io/badge/License-MIT-green?style=flat-square)](LICENSE)
[![Tests](https://img.shields.io/badge/Tests-cargo%20test-brightgreen?style=flat-square)](crates/opentaskbar-core/tests)

<br />

<img src="docs/screenshots/hero-desktop-2026-11.jpg" alt="OpenTaskbar Desktop Showcase" width="100%" />

<br />

<img src="docs/screenshots/hero-dock-crop-2026-11.png" alt="OpenTaskbar Floating Dock" width="100%" />

</div>

---

## Overview

OpenTaskbar is a lightweight, macOS-inspired floating dock and taskbar replacement for Windows 10 and Windows 11. It is a Rust-only application built on GPUI with a thin Win32 platform layer: a per-monitor dock, an on-demand Settings window, 22 catalog widgets, six bundled theme packs and a native watchdog that restores the Windows taskbar after a crash.

Version: **0.4.0**. The repository root is a Cargo workspace (`Cargo.toml`, `crates/`); all build, test and packaging commands in this document are run from that root.

---

## Key Features

### Optical Glass Dock

- **Aurora visual language:** Dark (Midnight Obsidian) and Light glass materials with a top specular sheen, 1 px optical bevel and soft ambient shadows.
- **Placement and alignment:** Bottom, Left or Right edge; centered, left, right or full-width content with optional independent per-surface alignment.
- **Size, scale and corners:** Compact / Medium / Large presets (48 / 58 / 66 DIP shelf), 75–150% scale and an adjustable corner radius; edge spacing follows the active size profile.
- **Per-surface materials:** Dock, menus, widgets and Settings are configured independently: Glass, Solid, True black (opaque `#000000` for OLED) or None, each with its own opacity.
- **Display care:** Reduce glow, burn-in shift (5–120 minute interval), automatic OLED look on low battery or Night Light, ambient idle strip and an animation frame cap (unlimited / 30 / 60 / 120 FPS).
- **Two-sided resizing and overflow:** Drag either outer edge to resize; wheel, middle-click drag and edge chevrons scroll an overflowing dock.
- **Fisheye magnification:** Spring-physics raised-cosine magnification with an overlap guard, plus a launch bounce on click.

### Widgets (22 catalog cards)

The Settings catalog is generated from `crates/opentaskbar-app/src/widgets/registry.rs`:

| Category | Widgets |
| :--- | :--- |
| Time & Weather | Clock & Date, Weather, Day Progress, Stopwatch, Countdown |
| System & Hardware | System Stats, Net Speed, Battery, Bluetooth Battery, Storage |
| AI Quotas | Claude, Codex, OpenCode, Copilot, DeepSeek, OpenRouter, Cursor |
| Media & Controls | Media, Audio, Quick Settings, Keyboard Layout |
| Productivity | Focus (Pomodoro timer) |

Folder Stacks and App Groups are dock-only helpers; Show Desktop is a shell action. The seven AI quota widgets also have a virtual aggregate (`AI Usage`). Catalog names and categories are localized in all 14 languages.

Notable widget capabilities:

- **Weather:** current conditions and a multi-day forecast from Open-Meteo, with an optional Windows location lookup, country/city search and °C/°F units.
- **System Stats:** CPU, memory, GPU and GPU temperature meters (WDDM first, optional NVIDIA NVML / AMD ADL).
- **AI usage:** per-provider quota bars/rings and reset countdowns. Credentials are discovered from local CLI/config files or DPAPI-encrypted local storage; no secret is logged or exported.
- **Media and audio:** track metadata, seek bar, per-app volume via WASAPI session matching; an on-demand Audio widget with device selection.
- **Quick Settings:** real Wi-Fi / Bluetooth radio toggles and Night Light, with shortcuts to Windows settings.
- **Focus, timers and tools:** Pomodoro sessions with breaks, stopwatch (centiseconds), countdown, storage usage, network speed and a day-progress bar.
- **Bluetooth Battery:** live PnP battery levels for headsets, mice, keyboards and controllers.

### Shell Integration

- **Live DWM thumbnails:** interactive previews with titles, close buttons, Aero Peek and foreground highlighting.
- **Native Jump Lists:** right-click an app for pinned tasks and recent files.
- **Packaged apps:** Store/app icons are resolved through AppUserModelIDs, including ApplicationFrameHost groups.
- **Start, Search and Task View:** permanent dock buttons; Start/Search/Control Center flyouts are anchored to the dock and their original positions are journaled.
- **Background Apps and Recycle Bin:** a pinned background-app strip with a flyout, plus a live Recycle Bin with drag-and-drop deletion.
- **Multi-monitor:** optional mirrored and secondary docks with per-display scoping.
- **Smart Auto-Hide:** Disabled / Smart / Always modes, shell-overlay filtering and fullscreen detection per monitor.

### Themes and Languages

- **Six bundled themes:** Catppuccin, Gruvbox, Nord, Prism, Solarized and Tokyo Night, installed on first launch into `%LOCALAPPDATA%\OpenTaskbar\Themes`.
- **Icon packs:** optional custom packs match by AUMID, package name, full path, file name or name; images named after the executable need no mapping. Bundled themes keep original app, Start and Recycle Bin icons.
- **Widget packs:** JSON design-token overrides for colors, gradients, radii, sizes and fonts, with separate light/dark layers. See [docs/themes.md](docs/themes.md).
- **Flags and fonts:** flag imagery is bundled in `Assets/Flags/`, UI and widget fonts in `Fonts/` (Inter plus Segoe MDL2 Assets glyphs).
- **14 languages:** `de-DE`, `en-US`, `es-ES`, `fr-FR`, `it-IT`, `ja-JP`, `ko-KR`, `nl-NL`, `pl-PL`, `pt-BR`, `ru-RU`, `tr-TR`, `uk-UA`, `zh-CN`. First launch follows the Windows UI language. See [docs/localization.md](docs/localization.md).

### Foolproof Taskbar Recovery

- **Journalled replacement:** Explorer's taskbar state is recorded to `taskbar-state.txt` before hiding; a pending journal is repaired at startup and cleared only after the visible taskbar is verified.
- **Crash watchdog:** an architecture-matched native C Win32 helper (`tools/watchdog/watchdog.c`) is embedded into the EXE by `build.rs`. It runs as a separate process with a 1 s heartbeat and restores the taskbar after three missed 4 s timeouts — including End Task, `taskkill /F` and FailFast cases.
- **Clean exit:** the dock signals the watchdog and ends the replacement explicitly; a UI-thread panic triggers restore before exit.
- **Manual escape hatch:** `OpenTaskbar.exe --restore` restores the native taskbar even when the dock is not running.

---

## Requirements

- **OS:** Windows 10 (version 2004 / build 19041+) or Windows 11, x64 or x86.
- **Runtime:** none; the launcher is a single self-contained EXE with the watchdog embedded.
- **Building from source:** Rust 1.95+ (edition 2024, MSVC target on Windows) and PowerShell 7+ for the packaging scripts.

---

## Getting Started

### Portable release

Download `OpenTaskbar-v0.4.0-win-x64.exe` or `OpenTaskbar-v0.4.0-win-x86.exe` from [GitHub Releases](https://github.com/alztrk/opentaskbar/releases) and launch it. Each release also ships a deterministic `OpenTaskbar-v0.4.0-win-{x64,x86}.zip`, `checksums.txt`, `release-manifest.json` and release notes. The in-app updater selects the matching architecture.

### Build and verify from source

```powershell
# Clone and run from the workspace root
git clone https://github.com/alztrk/opentaskbar.git
cd opentaskbar

cargo run --bin OpenTaskBar                 # start the dock
cargo run --bin OpenTaskBar -- --settings   # open Settings only

# Same gates as CI
cargo check --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Locale and release version gates
powershell -ExecutionPolicy Bypass -File .\scripts\check-locale-parity.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\check-release-version.ps1 -Tag v0.4.0
```

`cargo test --workspace` covers deterministic settings/i18n/shell-recovery logic; live Windows shell behavior still needs the manual smoke list in [docs/windows-reliability-audit.md](docs/windows-reliability-audit.md).

```powershell
# Portable EXE + verification
powershell -ExecutionPolicy Bypass -File .\scripts\publish-portable.ps1 -Runtime win-x64
powershell -ExecutionPolicy Bypass -File .\scripts\check-portable.ps1 -ExecutableName OpenTaskbar-x64.exe

# MSIX package (identity version 0.4.0.0)
powershell -ExecutionPolicy Bypass -File .\scripts\publish-rust-msix.ps1 -Runtime win-x64

# Assemble versioned release artifacts
powershell -ExecutionPolicy Bypass -File .\scripts\package-rust-release.ps1 -Tag v0.4.0
```

`publish-portable.ps1` installs the requested Rust target, builds `--release` and writes a single `OpenTaskbar-<arch>.exe` with the architecture-matched watchdog embedded. `check-portable.ps1` re-verifies the PE architecture, embedded watchdog and icon-worker startup. The release script produces the x64/x86 EXEs and ZIPs, `checksums.txt`, a schemaVersion 2 `release-manifest.json` (including `rustPort.crateVersion` and per-architecture watchdog PE machine/SHA-256) and `release-notes.md`. `check-release-version.ps1` gates the tag against the workspace version (`0.4.0`) and the MSIX identity (`0.4.0.0`).

---

## Command-line arguments

| Argument | Effect |
| :--- | :--- |
| *(none)* | Acquire the single-instance lock, repair a pending journal, load settings and run the dock. |
| `--settings` | Open only the GPUI Settings window; exit when it closes. |
| `--restore` | Replay the taskbar/flyout recovery journal and exit. |
| `--extract-icons` | Short-lived icon extractor worker driven over stdin/stdout. |
| `--software-render` | Force software rendering for this launch instead of the saved preference. |
| `--update*` / `action=update*` | Update-toast activation arguments handled by the in-app updater. |

There are no file-manager or panel launch modes.

---

## Configuration and data

Preferences live in the shared folder `%LOCALAPPDATA%\OpenTaskbar\`:

| Path | Contents |
| :--- | :--- |
| `settings.json` | Preferences, dock geometry, pins, widget IDs, profiles (PascalCase schema). |
| `opentaskbar.log` | Diagnostics; rotated copy `opentaskbar.log.old`. |
| `Themes\` | Installed theme packs (uppercase `T`), including icon/widget pack JSON. |
| `secrets\` | DPAPI-protected provider credentials (`*.bin`). |
| `rust-cache\icon-cache\` | Extracted icon cache. |
| `taskbar-state.txt`, `flyout-positions.txt` | Recovery journals (with `*.tmp` staging files). |
| `OpenTaskbarWatchdog.exe` | Architecture-matched watchdog copy used by release metadata. |

Unknown settings keys are preserved on save, invalid JSON is backed up before defaults are restored, and writes are serialized with a named lock and GUID temp files. Other local files include `render-mode.json`, `country-city-cache.json`, `wireless-battery-cache.json` and the `%TEMP%\OpenTaskbar\update` staging folder. No secrets are written to the log or to `.otbackup` archives. See [PRIVACY.md](PRIVACY.md).

Settings groups: **General** (language, startup, updates), **Appearance** (themes, surfaces, radius, fonts), **Dock layout** (edges, width/height/gap, magnification, auto-hide), **Behavior** (hotkeys, focus mode, desktop integration), **Widgets** (catalog, density, per-widget settings), **Background apps** (tray pins) and **Profiles**. Advanced `DockSettings.extra` switches include `enable_live_tray_host` (default off), `native_flyout_reposition` (default on) and `enable_vertical_dock` (default on).

---

## Workspace layout

| Crate | Responsibility |
| :--- | :--- |
| `crates/opentaskbar-core` | Settings schema, i18n (14 catalogs), shared paths, logging, single-instance lock, deterministic logic. |
| `crates/opentaskbar-shell` | Taskbar journal, AppBar reservation, flyout repositioning, session hooks, watchdog arming. |
| `crates/opentaskbar-app` | GPUI dock and Settings UI, widget catalog, Win32 platform adapters, icon cache and packaging-time rendering resources. |

The root also holds `Fonts/`, `Assets/` (including `Assets/Flags/`), `themes/`, `packaging/msix/`, `scripts/`, `tests/reference/locales/` (frozen locale snapshots) and `tools/watchdog/`. CI is defined in `.github/workflows/`: `ci.yml` runs fmt, clippy, tests and a release build on every push/PR; `rust-ci.yml` adds watchdog/x86 and locale gates; `release.yml` packages only these Rust artifacts with SLSA attestation and an optional VirusTotal scan.

Reliability design and verification evidence are documented in [docs/windows-taskbar-model.md](docs/windows-taskbar-model.md) and [docs/windows-reliability-audit.md](docs/windows-reliability-audit.md).

---

## License

Distributed under the MIT License. See [LICENSE](LICENSE) for details.
