# Languages

OpenTaskbar comes in 14 languages. On first start it uses the same language as Windows; if there's no translation, it uses English. If a single word is missing from a translation, that word appears in English instead — nothing breaks.

## Change the language

Open **Settings → Appearance** and pick a language.

## Add or improve a language

Translations are simple text files named after the language and region, for example `fr-FR.json`.

1. Copy `en-US.json` and rename it to the new language's tag, such as `fr-FR.json`.
2. Translate the text on the right side of each line. Leave the names on the left unchanged.
3. Leave markers like `{0}` exactly as they are. They're replaced with a number or name while the app runs.
4. Save the file. It's picked up automatically — no coding needed, and no menu to edit.

Untranslated words show up in English until you translate them. Don't translate product names, keyboard shortcuts, file types, Windows settings names or data that comes from Windows, such as city names.

<details>
<summary><strong>Technical details</strong></summary>

Supported cultures: `de-DE`, `en-US`, `es-ES`, `fr-FR`, `it-IT`, `ja-JP`, `ko-KR`, `nl-NL`, `pl-PL`, `pt-BR`, `ru-RU`, `tr-TR`, `uk-UA`, `zh-CN`. Each catalog is a BCP 47 culture tag with stable JSON keys; the 14 catalogs are embedded from `crates/opentaskbar-core/locales/*.json` (1373 keys each) through `opentaskbar_core::i18n` (`t`, `tr`, `t_fmt`, `keys`, `key_count`). Lookup order: exact culture → same two-letter language → `en-US` → the key name itself. A missing key returns the key name and is not a build error.

### Key counts and frozen reference

| Family | Location | Keys per catalog |
| :--- | :--- | :--- |
| Rust UI catalogs | `crates/opentaskbar-core/locales/*.json` | **1373** |
| Frozen reference snapshots | `tests/reference/locales/*.json` | **1046** |

The frozen snapshots are kept only as a key-parity reference for the shared settings-era catalogs; they are not loaded at runtime. Both families must have identical key sets across all 14 languages. Rust catalogs carry the additional feature keys (Dynamic Island, calendar/fonts, widget features, UI localization, window picker/magnification, OLED/display care and searchable location labels); the 50 removed panel-only keys are absent from both families.

### Add or change a key

1. Add the key to all 14 Rust catalogs with exactly the same spelling. Keep format placeholders such as `{0}` / `{1}` unchanged; only their multiset is compared because their order can legitimately differ by language.
2. Use real translations, never empty values. Product names, keyboard shortcuts, file formats, protocol values and source data such as city names are not translated UI copy.
3. Resolve user-facing strings through `opentaskbar_core::i18n::{t, tr, t_fmt}` in the app crate; do not leave English literals in the UI.
4. Add the key to the `$required` list in `scripts/check-locale-parity.ps1` and, for contract keys, to the `C3_NEW_KEYS` list in `crates/opentaskbar-core/tests/i18n_parity.rs`.
5. Bump every numeric gate together (`crates/opentaskbar-core/src/i18n.rs`, `crates/opentaskbar-core/tests/i18n_parity.rs`, `crates/opentaskbar-core/tests/telemetry_parity.rs` and `scripts/check-locale-parity.ps1`) so no gate is left behind.

The parity tests also guard the nine restored Turkish labels (`FocusBreakTitle/Start/Skip/Countdown/Complete`, `FocusSession`, `RadioOn`, `RadioOff`, `DockBottomGap`) against en-US or ASCII-only regressions, and assert zero empty values and zero lost placeholders in every catalog. The en-US values of `Idle`, `Cancelled` and `Expired` intentionally equal their keys so the fallback rule keeps working.

### Checking parity

Check that every catalog has the same keys with:

```powershell
.\scripts\check-locale-parity.ps1
cargo test --workspace
```

From the workspace root, `powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-locale-parity.ps1` is the source-level check (no toolchain required); `cargo test --workspace` repeats it against the embedded catalogs. Both assert identical key sets, the documented key counts and every required contract key, so parity is enforced in CI on every branch.

</details>
