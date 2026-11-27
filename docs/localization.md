# Localization

OpenTaskbar identifies languages with BCP 47 culture tags such as `tr-TR`, `en-US` or `ja-JP`. Each catalog is a JSON file named after its tag; every property is a stable key.

The first launch follows the Windows UI language, falling back to English. A missing translation uses the English entry; a missing key resolves to the key name and never fails the build.

## Add a language

1. Copy `en-US.json` to a file named with the new culture tag, e.g. `fr-FR.json`.
2. Translate values only. Keep every key and placeholder such as `{0}` unchanged.
3. The file is embedded automatically; the picker shows the culture's native name, so no code change is needed.
4. Resolve status and error strings through localization keys in code. Untranslated keys display English until translated.

Product names, shortcuts, file formats, protocol values, OS identifiers and source data such as city names are not translated.

## Rust catalogs

The Rust app embeds 14 catalogs at `crates/opentaskbar-core/locales/*.json` (1373 keys each) through `opentaskbar_core::i18n` (`t`, `tr`, `t_fmt`, `keys`). Lookup order: exact culture → same two-letter language → `en-US` → the key name itself.

## Checking parity

```powershell
.\scripts\check-locale-parity.ps1
cargo test --workspace
```

Both check that all catalogs have identical key sets, the documented key count and every required contract key.

When adding a key: add it to all 14 catalogs with the same spelling, keep placeholders intact, add it to `C3_NEW_KEYS` in `tests/i18n_parity.rs` and `$required` in the parity script, then bump every numeric key-count gate together.
