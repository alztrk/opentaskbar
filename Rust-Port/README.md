# OpenTaskbar — Rust port

A clean-sheet Rust + GPUI implementation of the OpenTaskbar floating dock and widget shell for Windows 10/11. It targets the same `settings.json` schema, locale catalogs, theme packs and recovery journal as the C# build, so switching between builds never loses user data.

> Status: in active parity work. Features marked **flag** are shipped behind a setting in `DockSettings.extra` and default to the safe value. The C# build remains the reference implementation.

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

The script installs the Rust target if needed, builds `--release`, and writes a single `OpenTaskbar-Rust-<arch>.exe` into `release/`. It has no sidecar DLLs; the icon worker is the same EXE launched with `--extract-icons`.

### From source

```powershell
cd Rust-Port
cargo build --release
cargo test --workspace
```

Requires Rust 1.95+ (edition 2024) and the MSVC toolchain on Windows.

### MSIX

```powershell
.\scripts\publish-rust-msix.ps1 -Runtime win-x64 -Version 0.3.2.0 [-SignForLocalTest]
```

Uses `packaging/msix/AppxManifest.Rust.xml` and the shared visual assets in `packaging/msix/Assets`. The MSIX identity is `Alicanztrk.OpenTaskbar.Rust`; capabilities match the C# package (`runFullTrust`, `radios`).

## Command-line arguments

| Argument | Effect |
| :--- | :--- |
| *(none)* | Acquires the single-instance lock, restores a pending journal, loads settings and runs the dock. |
| `--settings` | Opens only the GPUI Settings window; exits when it closes. |
| `--restore` | Replays the flyout/taskbar recovery journal and exits. |
| `--extract-icons` | Short-lived icon extractor worker driven over stdin/stdout. Used by the tray/dock; safe to run manually with stdin closed. |

## Settings

Settings live at `%LOCALAPPDATA%\OpenTaskBar-Rust\settings.json` and use PascalCase keys identical to C#. Unknown keys are preserved through `#[serde(flatten)] extra`, and invalid JSON is backed up to `settings.json.invalid-<timestamp>` before defaults are restored. Per-profile data (hidden apps, widget order, dock order, widths, theme and hotkeys) is likewise preserved on round-trip.

Common groups: **General** (language, startup, updates), **Appearance** (theme packs, surfaces, radius, fonts), **Dock layout** (edge, width/height/gap units, magnification, auto-hide), **Behavior** (hotkeys, focus mode, desktop integration), **Widgets** (catalog, density, per-widget settings), **Background apps** (tray pins and live icon strip), **Profiles**.

The C# `IsValid()` rules are mirrored in the Rust validation path: hotkeys must differ, percentages stay in 10..100, wireless polling stays in 15..300 seconds, and island order/placement must be consistent.

## Feature list (parity notes)

- **Dock:** horizontal and vertical edges, mirrored dock, secondary light strip, fisheye magnification with a critically damped spring and overlap guard, auto-hide with shell-overlay filtering, live drag/drop, folder stacks, app groups, Aero Peek, tray strip, Start/search/task view buttons.
- **Widgets:** AI usage (8 providers **including Antigravity** — no longer C#-only), weather with forecast and °F, media with seek/cover/per-app volume and LibreWolf timeline recovery, network speed test, focus sessions with break phase, audio mixer, Bluetooth/wireless battery, storage, keyboard layout, system stats with GPU meters, countdown/stopwatch with centiseconds, quick settings with real Wi-Fi/Bluetooth toggles.
- **Flyouts:** settings window with searchable rows, theme editor, full backup (`.otbackup`), hotkey recording, country/city picker, AI account sign-in, dynamic island with expanded details.
- **Reliability:** taskbar replacement session with retry/verify, flyout position journal with generation counter, watchdog heartbeat, overlay-aware auto-hide, per-app audio session matching, Trusted release pipeline (checksums, SLSA attestation, optional VirusTotal scan).

### Feature flags

Riskier integrations default to the safe behaviour and can be enabled from `DockSettings.extra`:

| Flag | Default | Safe fallback |
| :--- | :--- | :--- |
| `enable_live_tray_host` | `false` | Registry discovery only; native tray untouched. |
| `native_flyout_reposition` | `false` | Journal read only; legacy position logic. |
| `enable_vendor_battery_probes` | `false` | Generic HID capacity reading. |
| `enable_theme_editor` | `false` | List + import/export only. |
| `enable_vertical_dock` | `true` | Bottom edge if hit-testing fails. |
| `enable_ai_oauth` | `true` | "Sign in" state when no token is present. |

## Localization

14 cultures ship as embedded JSON: `de-DE`, `en-US`, `es-ES`, `fr-FR`, `it-IT`, `ja-JP`, `ko-KR`, `nl-NL`, `pl-PL`, `pt-BR`, `ru-RU`, `tr-TR`, `uk-UA`, `zh-CN`. Every catalog has the **same 1108 keys** — the 999 C# keys plus Rust-port additions (DI/calendar/font, new widget features and the 5 Antigravity keys). `opentaskbar_core::i18n::{t, tr, t_fmt}` resolve with exact → same-language → `en-US` → key-name fallback; a missing key never crashes. Key parity is enforced by `scripts/check-locale-parity.ps1` and `tests/i18n_parity.rs`. See [docs/localization.md](../docs/localization.md).

## Tests and CI

```powershell
cd Rust-Port
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`tests/` ports the deterministic parts of the C# checks: settings key parity, AI usage/JWT/quota logic, `.otbackup` ZIP round-trip, wireless battery vendor parsing, media timeline/seek, shell recovery/journal, dock layout math, audio mixer scoring and locale completeness. Cross-WS platform code is intentionally not referenced; each file notes which workstream owns the live implementation.

`rust-ci.yml` runs on every branch; `codeql.yml` analyses both C# and Rust; `dependabot.yml` tracks NuGet, Cargo and Actions.

## Privacy

No analytics or telemetry. Network access is opt-in per feature (weather, AI usage, portable update checks). Credentials stay local. See [PRIVACY.md](../PRIVACY.md).

## License

MIT — see [LICENSE](../LICENSE).
