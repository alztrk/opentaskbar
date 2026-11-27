# Localization

OpenTaskbar stores the selected UI language as a BCP 47 culture tag (for example `tr-TR`, `en-US`, `ja-JP`). The 14 embedded catalogs live in `crates/opentaskbar-core/locales/<culture>.json`; each JSON property is a localization key.

Supported cultures: `de-DE`, `en-US`, `es-ES`, `fr-FR`, `it-IT`, `ja-JP`, `ko-KR`, `nl-NL`, `pl-PL`, `pt-BR`, `ru-RU`, `tr-TR`, `uk-UA`, `zh-CN`.

On first launch the Rust app follows the Windows UI language when a matching translation exists; otherwise it uses English. The lookup order in `opentaskbar_core::i18n` is exact culture → another culture with the same two-letter language → `en-US` → the key name. A missing key renders as its key name and is never a build error, so a new UI string can land before its translations. The public API is `t(key)`, `tr(culture, key)` and `t_fmt(key, args)`; `keys()` and `key_count(culture)` are used by the parity gates.

## Key counts and frozen reference

| Family | Location | Keys per catalog |
| :--- | :--- | :--- |
| Rust UI catalogs | `crates/opentaskbar-core/locales/*.json` | **1373** |
| Frozen reference snapshots | `tests/reference/locales/*.json` | **1046** |

The frozen snapshots are kept only as a key-parity reference for the shared settings-era catalogs; they are not loaded at runtime. Both families must have identical key sets across all 14 languages. Rust catalogs carry the additional feature keys (Dynamic Island, calendar/fonts, widget features, UI localization, window picker/magnification, OLED/display care and searchable location labels); the 50 removed panel-only keys are absent from both families.

## Add or change a key

1. Add the key to all 14 Rust catalogs with exactly the same spelling. Keep format placeholders such as `{0}` / `{1}` unchanged; only their multiset is compared because their order can legitimately differ by language.
2. Use real translations, never empty values. Product names, keyboard shortcuts, file formats, protocol values and source data such as city names are not translated UI copy.
3. Resolve user-facing strings through `opentaskbar_core::i18n::{t, tr, t_fmt}` in the app crate; do not leave English literals in the UI.
4. Add the key to the `$required` list in `scripts/check-locale-parity.ps1` and, for contract keys, to the `C3_NEW_KEYS` list in `crates/opentaskbar-core/tests/i18n_parity.rs`.
5. Bump every numeric gate together (`crates/opentaskbar-core/src/i18n.rs`, `crates/opentaskbar-core/tests/i18n_parity.rs`, `crates/opentaskbar-core/tests/telemetry_parity.rs` and `scripts/check-locale-parity.ps1`) so no gate is left behind.

The parity tests also guard the nine restored Turkish labels (`FocusBreakTitle/Start/Skip/Countdown/Complete`, `FocusSession`, `RadioOn`, `RadioOff`, `DockBottomGap`) against en-US or ASCII-only regressions, and assert zero empty values and zero lost placeholders in every catalog. The en-US values of `Idle`, `Cancelled` and `Expired` intentionally equal their keys so the fallback rule keeps working.

## Checking parity

```powershell
# From the workspace root; source-level check, no toolchain required:
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-locale-parity.ps1

# Embedded-catalog check as part of the Rust test suite:
cargo test --workspace
```

`check-locale-parity.ps1` asserts 14 × 1046 frozen reference keys, 14 × 1373 Rust keys, identical key sets per family and every required contract key. `crates/opentaskbar-core/tests/i18n_parity.rs` repeats the embedded-catalog assertions inside `cargo test`, so parity is enforced in CI on every branch.
