# Changelog

All notable changes to the Rust workspace are recorded here. Versions follow the
`version` in the root `Cargo.toml`; the release tag and the MSIX identity are
gated against it by `scripts/check-release-version.ps1`.

## Unreleased

### Removed features and stale code paths

- Removed the bundled file manager, per-monitor top bar and Wi-Fi panel, including their launch modes, settings toggles, dock menu entries and 50 panel-only locale keys. The Rust catalog gate is now **1375 keys** in each of 14 languages; the frozen reference snapshots remain at 1046 keys.
- Removed the remaining outdated documentation, history and source-path references. Documentation now describes the root workspace first — `Cargo.toml`, `crates/`, `scripts/` — alongside `docs/`, `resources/`, `packaging/`, `tests/` and `tools/`, plus the shared `%LOCALAPPDATA%\OpenTaskbar` data folder.

### Repository and pipeline

- Reverted the 2026-11-27 cargo bump to `windows` 0.62, `sha2` 0.11, `zip` 9 and `roxmltree` 0.21, which left the workspace uncompilable: `windows` 0.62 drops `IAsyncOperation::get`, regates `VARIANT` behind `Win32_System_Ole` and changes `IShellLinkW::GetPath`, while `zip` 9 returns a `Result` from `ZipFile::name`. The migration is a follow-up; the workspace builds against the previous versions again.
- Flattened the Rust workspace into the repository root: `Cargo.toml`, `Cargo.lock`, `crates/` and the merged README now live at the top level; bundled data lives in `resources/fonts/`, `resources/assets/` and `resources/themes/`. CI path filters, packaging scripts, docs and test helpers resolve paths from the new root.
- MSIX packaging uses `packaging/msix/AppxManifest.Rust.xml`; the release workflow builds x64/x86 Rust EXEs and ZIPs only.

### Behavior and UI

- Kept Background Apps pins in a compact strip beside the Background Apps button with overflow in the flyout, and fixed opening a pin so it restores a window instead of launching helper executables.
- Fixed launching Store/packaged apps and helper-process apps such as Steam from the dock; shortcuts can also be dropped onto the dock.
- Refreshed Settings and Background Apps panels with coordinated light/dark styling, responsive navigation and inline pin/window controls.
- Applied active theme and theme-pack tokens, including Dynamic Island background color and opacity controls.
- Read playback progress from a matching LibreWolf tab when Windows omits the media timeline; show an unavailable-duration state when no usable timeline exists.
- Added transition/error states for Wi-Fi and Bluetooth radio toggles and placed Night Light and Windows quick-settings shortcuts together in Quick Settings.
- Simplified the system metrics widget and its flyouts; removed the duplicate Show Desktop widget in favor of the permanent taskbar edge strip.
- Opening Settings twice now raises the window that is already open: the dock reuses a live Settings dialog, and a second `--settings` process exits instead of adding another window.

### Documentation

- Refreshed `README.md`, `PRIVACY.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `docs/**` and `.github/*.md` against the current Rust implementation: CLI modes, 22 catalog widgets, 14 languages / 1375 keys, theme packs, and journal/watchdog recovery.

## 0.3.5 - 2026-11-27

### Rust-only application

- OpenTaskbar is now a Rust-only GPUI application. Source lives in `crates/opentaskbar-core`, `crates/opentaskbar-shell` and `crates/opentaskbar-app` under the repository-root Cargo workspace; portable EXEs, MSIX packages and release ZIPs are all produced from that workspace.
- `ci.yml` is the baseline gate (fmt, clippy, tests, release build); `rust-ci.yml` adds the watchdog/x86 and locale-parity checks. CodeQL analyses the Rust workspace and the native C watchdog (`tools/watchdog/watchdog.c`) on every branch and PR.
- `release.yml` builds only Rust x64/x86 EXEs and ZIPs plus `checksums.txt`, a schemaVersion 2 `release-manifest.json` and `release-notes.md`, with SLSA attestation, optional report-only VirusTotal scanning and the protected `release` environment. There is no performance-baseline workflow; a Rust benchmark suite does not exist yet.
- Dependabot tracks Cargo and GitHub Actions, and dependency review watches the Rust lockfiles.

### Recovery and reliability

- `crates/opentaskbar-shell/build.rs` compiles the watchdog for the exact target architecture, verifies its PE machine (x64 `0x8664`, x86 `0x014C`) and embeds it via `include_bytes!`. The watchdog runs as a separate process with a 1 s heartbeat and restores Explorer's taskbar after three missed 4 s timeouts.
- The taskbar journal records only the supported AppBar state bits, rejects invalid/stale values and is cleared only after the primary and secondary taskbars are visible. `OpenTaskbar.exe --restore` replays the journal manually.
- Auto-hide re-evaluates fullscreen state per monitor from real app windows, filters shell overlays and keeps the dock visible while flyouts are open. Flyout journals preserve generation/identity across Explorer restarts.

### Settings, themes and localization

- All settings are stored in `%LOCALAPPDATA%\OpenTaskbar\settings.json` with a shared PascalCase schema; unknown keys are preserved, invalid JSON is backed up before defaults are restored, and profiles round-trip without data loss.
- Added a fourth per-surface material, True black (OLED), plus independent display-care settings: reduced glow, burn-in shift, low-battery/Night Light triggers, ambient mode and an animation frame cap. The 17 new locale keys are translated into all 14 catalogs.
- All 14 Rust locale catalogs contain **1375 keys** with identical key sets; the frozen reference snapshots under `tests/reference/locales` stay at 1046 keys. `scripts/check-locale-parity.ps1` and `crates/opentaskbar-core/tests/i18n_parity.rs` enforce both counts, required keys, non-empty values and placeholder retention.
- Six bundled theme packs are installed into `%LOCALAPPDATA%\OpenTaskbar\Themes`. Widget packs resolve typed design tokens with a 256-token limit; unknown or wrongly typed values are skipped, and user surface settings always win over theme colors.

### Version gate

- Release tag ↔ Cargo workspace version `0.3.5` ↔ MSIX identity `0.3.5.0` must match before any build. `package-rust-release.ps1` generates the versioned x64/x86 EXE and ZIP names (`OpenTaskbar-v0.3.5-win-{x64,x86}.exe/.zip`), records each watchdog's architecture/PE machine/SHA-256 and verifies every checksum row against disk before attestation.
