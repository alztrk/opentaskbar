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

Each Rust catalog currently contains **1331 keys**: the 999 C# keys plus 332 Rust-port additions (dynamic island, calendar, fonts, widget features, the five Antigravity keys and 202 newer UI-localization keys). The five C#-surplus keys that were previously missing are now present in every language:

- `WidgetAiUsageAntigravity`, `WidgetAiUsageAntigravityDescription`
- `AiUsageGroupGemini`, `AiUsageGroupClaudeGpt`, `AiUsageAntigravityGroupsLabel`

The 32 additional feature keys cover `SpeedTest*`, `FocusBreak*`, `Radio*`, `FullBackup*`, `ThemeEditor*`, `Seek*`, `HardwareUnavailable`, `StartMenu*` and `ProfileHiddenApps`. The 202 UI-localization keys added later cover Dynamic Island detail rows (`WidgetDetail*`, `TrayAppsRunning*`), Settings/theme controls (`ThemeToken*`, `Brush*`, `Dock*Label`, `Unit*`) and widget labels (`AiUsage*`, `Battery*`, `WirelessBattery*`, `Weather*`, `Focus*`, `SpeedTest*`). All of them are translated in every language (the en-US values of `Idle`, `Cancelled` and `Expired` intentionally equal their keys so the fallback checks keep working); **key-set parity is mandatory** regardless of value language.

### Checking parity

```powershell
# Source-level check (no toolchain needed):
.\scripts\check-locale-parity.ps1

# Embedded-catalog check as part of the Rust test suite:
cd Rust-Port; cargo test --workspace
```

`check-locale-parity.ps1` asserts that all 14 C# catalogs and all 14 Rust catalogs each have an identical key set, that no C# key is missing from Rust, and that every required new key exists in every language. `tests/i18n_parity.rs` repeats the embedded-catalog assertions inside `cargo test`, so the contract is enforced in CI on every branch.

When adding a key: add it to all 14 Rust files with the same spelling, keep `{0}`/`{1}` placeholders intact, add it to the `C3_NEW_KEYS` list in `tests/i18n_parity.rs` and the `$required` list in the parity script, and bump every numeric key-count gate (`i18n.rs`, `tests/i18n_parity.rs`, `tests/telemetry_parity.rs`, `scripts/check-locale-parity.ps1`) together so no gate is left behind.
