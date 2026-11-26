# Localization

OpenTaskbar stores the selected UI language as a BCP 47 culture tag, such as `tr-TR`, `en-US`, or `ja-JP`. The language catalog reads embedded JSON files from `Services/Localization/Languages`. The file name is the culture tag and each JSON property is a localization key.

On first launch, the app selects a translation matching the Windows UI language. Regional variants keep their culture-specific date and number formatting while using the matching language translation. If the Windows UI language is not available, the app starts in English. A missing translation in another language uses the English entry; a missing English key raises an explicit error.

## Add a language

1. Copy `en-US.json` to a file named with the new culture tag, such as `fr-FR.json`.
2. Translate the values while keeping every symbolic key and any format placeholders such as `{0}` unchanged. Keys are stable identifiers and must not contain translated text.
3. The project embeds JSON files in this folder automatically. The language picker uses the culture's native name, so no language enum, switch statement, or UI control needs editing.
4. Keep error and status results as localization keys in services and resolve them in the UI. Translate launcher labels, errors, and status messages too. If a key is not ready yet, English is shown until its translation is added.

## Existing fixed text

User-facing labels and errors belong in the locale JSON files. Product names, keyboard shortcuts, file formats, protocol values, operating-system identifiers, and source data such as city names are not translated UI copy.

## Rust port catalogs

The Rust port embeds its own copies of the 14 catalogs at `Rust-Port/crates/opentaskbar-core/locales/*.json` and exposes them through `opentaskbar_core::i18n` (`t`, `tr`, `t_fmt`, `keys`, `key_count`). Lookup order is exact culture → another culture with the same two-letter language → `en-US` → the key name itself. A missing key returns the key name and is **not** a build error, so a widget added ahead of its translations still renders.

Each Rust catalog currently contains **1342 keys**: the 999 C# keys plus 343 Rust-port additions (dynamic island, calendar, fonts, widget features, the five Antigravity keys, 207 newer UI-localization keys, 3 keys replacing the last English literals and 3 searchable-location keys). The five C#-surplus keys that were previously missing are now present in every language:

- `WidgetAiUsageAntigravity`, `WidgetAiUsageAntigravityDescription`
- `AiUsageGroupGemini`, `AiUsageGroupClaudeGpt`, `AiUsageAntigravityGroupsLabel`

The 32 additional feature keys cover `SpeedTest*`, `FocusBreak*`, `Radio*`, `FullBackup*`, `ThemeEditor*`, `Seek*`, `HardwareUnavailable`, `StartMenu*` and `ProfileHiddenApps`. The 207 UI-localization keys added later cover Dynamic Island detail rows (`WidgetDetail*`, `TrayAppsRunning*`), Settings/theme controls (`ThemeToken*`, `Brush*`, `Dock*Label`, `Unit*`) and widget labels (`AiUsage*`, `Battery*`, `WirelessBattery*`, `Weather*`, `Focus*`, `SpeedTest*`). All of them are translated in every language (the en-US values of `Idle`, `Cancelled` and `Expired` intentionally equal their keys so the fallback checks keep working); **key-set parity is mandatory** regardless of value language.

A merge once replaced nine tr-TR values with en-US or ASCII-only text (`FocusBreakTitle`, `FocusBreakStart`, `FocusBreakSkip`, `FocusBreakCountdown`, `FocusBreakComplete`, `FocusSession`, `RadioOn`, `RadioOff`, `DockBottomGap`); they are genuine Turkish again. `tests/i18n_parity.rs` guards those values and asserts that every catalog has 0 empty values and 0 lost placeholders. Placeholder order may legitimately differ by language (`TerminatePrompt`), so only the placeholder multiset is compared. The 14 catalogs and all numeric gates remained at **1336 keys** at the time of that fix; no keys were added or removed by it.

Three further keys — `PlacementInWidgets` (Start button placement option), `OpenLocation` (Jump List display-name fallback) and `GpuEngine` (unknown PDH counter label) — replaced the last user-visible English literals in `ui/settings.rs`, `platform/jumplist.rs` and `widgets/telemetry.rs`. They are translated in all 14 catalogs and raised every catalog and every numeric gate to **1339 keys**. Widget names/categories already resolve through `WidgetCategory*` and the existing widget-name keys (`FolderStacks`, `AppGroups`, `ShowDesktop`), so no extra category keys were needed. `SETTINGS_ROWS` in `ui/settings_search.rs` is test-only; the UI-facing `SettingsSearchEntry` catalog is fully localized. The en-US `Idle`/`Cancelled`/`Expired` key-echo rule is unchanged.

`DropdownSearchPlaceholder` (search input hint), `DropdownNoResults` (empty filtered list) and `WeatherLocationUsed` (location was auto-detected) localize the new searchable country/city pickers and geolocation status. They are translated in all 14 catalogs and raised every catalog and every numeric gate to **1342 keys**; filtering folds ASCII `I` separately from Turkish dotted `İ` and dotless `ı`, which is a UI behaviour change, not a locale change.

### Checking parity

```powershell
# Source-level check (no toolchain needed):
.\scripts\check-locale-parity.ps1

# Embedded-catalog check as part of the Rust test suite:
cd Rust-Port; cargo test --workspace
```

`check-locale-parity.ps1` asserts that all 14 C# catalogs and all 14 Rust catalogs each have an identical key set, that no C# key is missing from Rust, and that every required new key exists in every language. `tests/i18n_parity.rs` repeats the embedded-catalog assertions inside `cargo test`, so the contract is enforced in CI on every branch.

When adding a key: add it to all 14 Rust files with the same spelling, keep `{0}`/`{1}` placeholders intact, add it to the `C3_NEW_KEYS` list in `tests/i18n_parity.rs` and the `$required` list in the parity script, and bump every numeric key-count gate (`i18n.rs`, `tests/i18n_parity.rs`, `tests/telemetry_parity.rs`, `scripts/check-locale-parity.ps1`) together so no gate is left behind.
