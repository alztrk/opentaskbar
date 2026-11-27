# WS-H handoff notes for the lead

## §C3 locale contract — delivered

- 14 Rust catalogs at `crates/opentaskbar-core/locales/*.json` now have **1406 keys each, identical key sets** (verified by `scripts/check-locale-parity.ps1` and `tests/i18n_parity.rs`).
- The final key-contract sweep removed five retired AI-grouping keys and added 27 window-picker, magnification, Fjord, Wi-Fi, search, alignment, preview, focus, density and AI-ring keys (24 newly added; `WidgetDensityCompact`, `WidgetDensityFull` and `AiUsageRingLabel` already shipped) with real translations in all 14 languages.
- 32 additional feature keys cover the C3 prefixes: `SpeedTestDownload/Upload/Jitter/Result/Running`, `FocusBreakTitle/Start/Skip/Countdown/Complete`, `FocusSession`, `RadioWifi/Bluetooth/On/Off`, `FullBackupInspect/Contents`, `ThemeEditorTitle/Save/Reset/Token/Advanced/Autosave`, `SeekTo/Label/Duration/Unavailable`, `HardwareUnavailable`, `StartMenuOpen/Close/Toggle`, `ProfileHiddenApps`.

## For WS-C: replace English literals with `loc::t("...")`

WS-C owns the widget code, so I did not touch it. The keys above are ready to consume; the exact spellings are the authoritative names. Priorities:

1. Window picker / magnification / Fjord → `WindowPickerTitle`, `WindowPickerPrevious`, `WindowPickerNext`, `WindowPickerPage`, `MagnificationExpandsDock`, `MagnificationExpandsDockDescription`, `FjordFiles`, `FjordFilesOpen`, `FjordBarShow`, `FjordBarHide`.
2. Wi-Fi flyout → `WifiScan`, `WifiConnect`, `WifiDisconnect`, `WifiPassword`, `WifiNoNetworks`, `WifiConnectFailed`.
3. Speed-test flyout phases/results → `SpeedTestTestingPing`, `SpeedTestTestingDownload`, `SpeedTestTestingUpload` (pre-existing) plus `SpeedTestDownload`, `SpeedTestUpload`, `SpeedTestJitter`, `SpeedTestResult`, `SpeedTestRunning`.
4. Focus break phase/session → `FocusBreakTitle`, `FocusBreakStart`, `FocusBreakSkip`, `FocusBreakCountdown` (`{0}` seconds), `FocusBreakComplete`, `FocusSession` (`{0}` number).
5. Media seek/cover → `SeekTo` (`{0}`), `SeekLabel`, `SeekDuration`, `SeekUnavailable`.
6. Quick-settings radios → `RadioWifi`, `RadioBluetooth`, `RadioOn`, `RadioOff` alongside the pre-existing `RadioToggle`, `RadioConfirmToggle`, `RadioSettings`.
7. Full backup / theme editor / hardware gating / Start menu / profile hidden apps → the corresponding keys listed above.

All new keys are translated in every catalog. A missing key resolves to the key name and never fails the build.

## Merge / union requests

- `opentaskbar-core/src/lib.rs`: I added exactly one line, `pub mod i18n;`. This is lead-owned — take the union, don't drop it (i18n.rs will not compile otherwise).
- `Cargo.toml`: added `ApplicationModel` and `Devices_Radios` to the shared `windows` crate features per the WS-B note. These are valid `windows` 0.61 features (`windows-sys` has neither), and `cargo check --workspace` is clean with them.
- `opentaskbar-core/src/i18n.rs` + `locales/**` are new files in my ownership. I copied the baseline catalogs from the main repo working tree (1071 keys) because this worktree's base commit predates them; the main repo had them untracked. No other WS file was touched.

## Tests — scope caveat

`crates/opentaskbar-core/tests/*.rs` are **self-contained reference tests** for the deterministic reference logic. Cross-WS platform code (`widgets`, `platform`, `ui`, live `settings` validation) is absent from this worktree's baseline, so the tests intentionally do not bind to private or not-yet-existing APIs. When merging WS-A…G, consider re-pointing these tests at the real implementations and deleting the local helper functions:

| Test file | Intended live owner |
|---|---|
| `opentaskbar_core_tests.rs` | WS-A `settings.rs` (contract mirror of 110 keys + `DockProfile` flatten) |
| `ai_usage_logic.rs` | WS-G `widgets/telemetry.rs` (JWT/quota/groups) |
| `full_backup.rs` | WS-E `.otbackup` UI + WS-A settings |
| `wireless_battery_vendors.rs` | WS-G `platform/wireless_battery.rs` |
| `media_timeline.rs` | WS-C `widgets/media_ext.rs` |
| `shell_recovery.rs` | WS-B `shell/journal.rs`, `watchdog.rs`, `shell_shortcuts_ex.rs` |
| `dock_layout_extra.rs` | WS-D `dock/mod.rs` |
| `audio_mixer.rs` | WS-C/WS-G CoreAudio mixer |
| `i18n_parity.rs`, `telemetry_parity.rs` | WS-H (final contract gate) |

## Verification in this worktree

`cargo check --workspace`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (102 tests) and `scripts/check-locale-parity.ps1` all pass. No merge or rebase was performed.
