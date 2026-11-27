# OpenTaskbar — Rust port

A clean-sheet Rust + GPUI implementation of the OpenTaskbar floating dock and widget shell for Windows 10/11. After the shared-data integration it uses the same `%LOCALAPPDATA%\OpenTaskbar` folder, `settings.json` schema, locale catalogs, theme packs and recovery journal as the C# build; Rust diagnostics use the separate `opentaskbar-rust.log` name in that folder so the two logs never collide.

> Status: in active parity work. Features marked **flag** are controlled by `DockSettings.extra`; each row below states the real default and its fallback. `native_flyout_reposition` is **on by default** now that Start anchoring is verified and journal-backed. The C# build remains the reference implementation.

## Crates

| Crate | Responsibility |
| :--- | :--- |
| `opentaskbar-core` | C#-compatible `settings.json`, hotkeys, structured logging, single-instance lock, and 14 embedded locale catalogs (`src/i18n.rs`, `locales/*.json`). |
| `opentaskbar-shell` | Non-blocking Explorer taskbar hide/restore, recovery journal, watchdog process, running-window tracker. |
| `opentaskbar-app` | GPUI settings window, transparent borderless Win32 dock bridge, fisheye magnification, icon cache with an `--extract-icons` worker, widgets, flyouts, platform adapters. |

## Install

### Portable (recommended for testing)

```powershell
# From the repository root, for the architecture you want:
.\scripts\publish-rust-portable.ps1 -Runtime win-x64 -OutputDir release
.\scripts\check-rust-portable.ps1  -OutputDir release -ExecutableName OpenTaskbar-Rust-x64.exe
```

The script installs the Rust target if needed, builds `--release`, and writes a single `OpenTaskbar-Rust-<arch>.exe` into `release/`. It has no sidecar DLLs; the icon worker is the same EXE launched with `--extract-icons`. `build.rs` compiles `tools/watchdog/watchdog.c` for the target architecture, verifies its PE machine (x64 `0x8664` / x86 `0x014C`) and embeds it with `include_bytes!`; `-NoFallback` makes an x86 build fail rather than embed the checked-in x64 binary. An x86 build needs GCC multilib or the MSVC `VC Tools x86.x64` component. A copy named `OpenTaskbarWatchdog-<arch>.exe` is left beside the portable EXE for release metadata.

### From source

```powershell
cd Rust-Port
cargo build --release
cargo test --workspace
```

Requires Rust 1.95+ (edition 2024) and the MSVC toolchain on Windows.

### MSIX

```powershell
.\scripts\publish-rust-msix.ps1 -Runtime win-x64 [-Version 0.3.5.0] [-SignForLocalTest]
```

Uses `packaging/msix/AppxManifest.Rust.xml` and the shared visual assets in `packaging/msix/Assets`. There is no hardcoded version in the script: it reads `OpenTaskbar.csproj` (currently `0.3.5`) unless `-Version` is supplied, and the manifest template ships identity version `0.3.5.0`. The architecture-matched watchdog is staged as `OpenTaskbarWatchdog.exe` next to `OpenTaskBar.exe` so the packaged app uses the signed in-package copy. The MSIX identity is `Alicanztrk.OpenTaskbar.Rust`; capabilities match the C# package (`runFullTrust`, `radios`).

## Command-line arguments

| Argument | Effect |
| :--- | :--- |
| *(none)* | Acquires the single-instance lock, restores a pending journal, loads settings and runs the dock. |
| `--settings` | Opens only the GPUI Settings window; exits when it closes. |
| `--restore` | Replays the flyout/taskbar recovery journal and exits. |
| `--extract-icons` | Short-lived icon extractor worker driven over stdin/stdout. Used by the tray/dock; safe to run manually with stdin closed. |

## Settings

After the shared-data integration, settings live in the common `%LOCALAPPDATA%\OpenTaskbar\settings.json` folder alongside the C# build (the pre-integration `%LOCALAPPDATA%\OpenTaskBar-Rust\` folder is retired), and use PascalCase keys identical to C#. Rust diagnostics go to `%LOCALAPPDATA%\OpenTaskbar\opentaskbar-rust.log` (rotated copy `opentaskbar-rust.log.old`) so they never overwrite C# `log.txt`. Unknown keys are preserved through Rust `#[serde(flatten)] extra` and C# `[JsonExtensionData]`, and invalid JSON is backed up to `settings.json.invalid-<timestamp>` before defaults are restored. Per-profile data (hidden apps, widget order, dock order, widths, theme and hotkeys) is likewise preserved on round-trip. Concurrent writes are serialized by `Local\OpenTaskbar.Settings.Lock` with a unique GUID temp file; use a full-backup import to migrate preferences from an older separate-folder layout.

Common groups: **General** (language, startup, updates), **Appearance** (theme packs, surfaces, radius, fonts), **Dock layout** (edge, width/height/gap units, magnification, auto-hide), **Behavior** (hotkeys, focus mode, desktop integration), **Widgets** (catalog, density, per-widget settings), **Background apps** (tray pins and live icon strip), **Profiles**.

The C# `IsValid()` rules are mirrored in the Rust validation path: hotkeys must differ, percentages stay in 10..100, wireless polling stays in 15..300 seconds, and island order/placement must be consistent.

## Feature list (parity notes)

- **Dock:** horizontal and vertical edges, mirrored dock, secondary light strip, fisheye magnification with a critically damped spring and overlap guard, auto-hide with shell-overlay filtering, live drag/drop, folder stacks, app groups, Aero Peek, tray strip, Start/search/task view buttons.
- **Widgets:** AI usage (per-provider quota widgets), weather with forecast and °F, media with seek/cover/per-app volume and LibreWolf timeline recovery, network speed test, focus sessions with break phase, audio mixer, Bluetooth/wireless battery, storage, keyboard layout, system stats with GPU meters, countdown/stopwatch with centiseconds, quick settings with real Wi-Fi/Bluetooth toggles.
- **Flyouts:** settings window with searchable rows, theme editor, full backup (`.otbackup`), hotkey recording, country/city picker, AI account sign-in, dynamic island with expanded details.
- **Reliability:** taskbar replacement session with retry/verify, flyout position journal with generation counter, architecture-matched watchdog with heartbeat and clean-exit signalling, overlay-aware auto-hide, per-app audio session matching, trusted release pipeline (checksums, SLSA attestation, optional VirusTotal scan).

### Feature flags

Every flag is read from `DockSettings.extra` (PascalCase aliases are accepted where noted). Defaults below match the code, not an aspirational safe list:

| Flag | Default | Behaviour / fallback |
| :--- | :--- | :--- |
| `enable_live_tray_host` | `false` | Opt-in only: registry discovery and the light strip keep the native tray untouched. |
| `native_flyout_reposition` | **`true`** | Both shell writers follow one switch (`NativeFlyoutReposition` alias accepted). Verified Start anchoring is generation-cancelled; Search/Control Center join the same worker in the final WS-3 integration. `false` keeps journal restore and legacy positioning. |
| `enable_vertical_dock` | `true` | Left/right edges are live; `false` degrades every edge to `Bottom`. |
| `enable_vendor_battery_probes` | `false` (planned WS-4) | Generic HID capacity reading until vendor transports are enabled explicitly. |
| `enable_theme_editor` | `false` (planned WS-7) | List/import/export only until token editing is enabled explicitly. |
| `enable_ai_oauth` | `true` (planned WS-5/WS-7) | Cursor/AI sign-in flows; a missing token stays in the "Sign in" state. |

### Live shell APIs

The final integration wires these production entry points; integration test files under `crates/opentaskbar-core/tests/` pin their contracts even where the owning workstream has not landed yet (those cases are `#[ignore]`d with a note):

- Start/flyout: `open_start_via_syscommand(owner_hwnd)` (non-blocking post), `close_start(hwnd)` with an HWND/token guard, `was_open_at_press()`, and `reposition_flyout(FlyoutKind, anchor, hwnd)` (`reposition_start` remains as the Start wrapper). `mark_opened()` only fires from the uncloaked hook, so a generation is counted once.
- Shell/watchdog: `tracker::enumerate_windows_async(tx)`, `window_rules::has_taskbar_button` (ITaskbarList_Deleted, cloak and extended-style rules), `modifiers::{note_down, note_up, release_all}` with `finally` semantics, and a one-process watchdog with heartbeat/clean-exit verification.
- Devices: `core_audio::list_app_sessions()` plus writable `set_app_volume/mute`, `radio::set(kind, on) -> bool` (completion-event wait, no `GetResults` on the Set operation, fire-and-forget refresh), and `wireless_battery::read_all_with_settings()`.
- Media/Night Light: `media_ext::librewolf_timeline(source_app_id, title)`, `confirm_seek(...)` rejecting stale samples, and `telemetry::{read_night_light, toggle_night_light}` with the encoded registry payload.
- Reliability: `single_instance::SingleInstance::acquire` (named mutex), `DockSettings::validate()` (theme ids, ranges, hotkeys), and `update::write_updater_script` (Unicode-safe, no BOM).

## Localization

14 cultures ship as embedded JSON: `de-DE`, `en-US`, `es-ES`, `fr-FR`, `it-IT`, `ja-JP`, `ko-KR`, `nl-NL`, `pl-PL`, `pt-BR`, `ru-RU`, `tr-TR`, `uk-UA`, `zh-CN`. Every catalog has the **same 1406 keys** — Rust-port additions on top of the 1034 C# keys after the integration union (994 original C# keys plus the 40 Fjord/Wi-Fi/preview labels now shared by both builds): DI/calendar/font, C3 widget features, 207 UI localization keys for island details, Settings/theme and widgets, 27 window-picker/magnification/Fjord/Wi-Fi/settings labels (24 newly added; three already shipped), 3 keys covering the last English-literal fallbacks, 3 keys for the searchable location pickers / geolocation status, and 40 Fjord/Wi-Fi/preview labels (`FjordFiles*`, `Connected`, `Saved`, `WifiNetworks`, `PdfPages`, `SelectPreview`, …). `opentaskbar_core::i18n::{t, tr, t_fmt}` resolve with exact → same-language → `en-US` → key-name fallback; a missing key never crashes. Key parity is enforced by `scripts/check-locale-parity.ps1` and `tests/i18n_parity.rs`, which also guard the nine restored tr-TR focus/radio/spacing labels and assert 0 empty values and 0 lost placeholders in every catalog. See [docs/localization.md](../docs/localization.md).

## Tests and CI

```powershell
cd Rust-Port
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`crates/opentaskbar-core/tests/` ports the deterministic parts of the C# checks: settings key parity, AI usage/JWT/quota logic, `.otbackup` ZIP round-trip, wireless battery vendor parsing, media timeline/seek, shell recovery/journal, dock layout math, audio mixer scoring and locale completeness. Where the production API is a private function in another crate, a bridge test asserts the production source contract instead of duplicating it; cases that need a not-yet-landed cross-WS API are `#[ignore]`d with the owning workstream named in the message. Core-owned behaviour (settings enums/validation, i18n, the single-instance mutex) is tested against the real production functions.

`rust-ci.yml` runs on every branch and also when locale catalogs, watchdog sources, packaging scripts or the Rust MSIX manifest change; its `watchdog-arch` job builds the x86 PE (`0x014C`) with `-NoFallback`, builds the workspace for `i686-pc-windows-msvc` and re-verifies the x86 portable EXE. `codeql.yml` scans Rust on every branch/PR and keeps C# on master; `dependabot.yml` tracks NuGet, Cargo and Actions. `scripts/check-locale-parity.ps1` gates 14 × 1034 C# keys and 14 × 1406 Rust keys, and `scripts/check-release-version.ps1` gates tag ↔ csproj ↔ Cargo (`0.1.0` carve-out) ↔ Rust MSIX before release builds.

## Privacy

No analytics or telemetry. Network access is opt-in per feature (weather, AI usage, portable update checks). Credentials stay local. See [PRIVACY.md](../PRIVACY.md).

## License

MIT — see [LICENSE](../LICENSE).
