# Deep Agent I — fix log

Worktree: `D:\projects\OpenTaskbar-wt-i` (branch `fix/deep-i`).
Only `crates/opentaskbar-core/**` and `scripts/check-locale-parity.ps1` were touched. No locale JSON values were edited.

## Fixed

| Finding | Fix | Regression tests |
| --- | --- | --- |
| `[Orta]` `custom:` IDs not canonicalized | `canonical_widget_setting_id` now always emits `custom:<slug>` (prefix lowercased, slug case preserved) through a shared `custom_widget_slug` validator; returns `String` so no borrowed-prefix allocation hack is needed. Placements, density overrides, island orders, widget lists and dock tokens all use it. | `tests/settings_compat.rs::mixed_case_custom_ids_canonicalize_through_every_collection`; `settings::tests::save_canonicalizes_repairable_settings_before_validating`; `i18n` unaffected. |
| `[Orta]` Island limits used `CATALOG_WIDGET_COUNT` | Added `MAX_ISLAND_PLACEMENTS`, `MAX_ISLAND_ORDER_IDS`, `MAX_ISLAND_MONITOR_ORDERS` (64/64/32); `CATALOG_WIDGET_COUNT` derived as `SUPPORTED_WIDGET_IDS.len() - 2` and used only for reference parity. Validation comment now matches the real rule (custom IDs are valid). | `settings::tests::island_limits_are_not_tied_to_the_catalog_card_count`. |
| `[Orta]` `dynamic_island_widget_orders` not canonicalized | New `canonicalize_island_orders` normalizes order values with `normalize_widget_ids` while keeping monitor keys untouched; called from `canonicalize_widget_keys` (load and save). Updated the existing round-trip expectation from lowercase to canonical IDs. | `settings::tests::dynamic_island_settings_round_trip_with_reference_keys`; `save_canonicalizes_repairable_settings_before_validating`. |
| `[Düşük]` Several "compact ID" filters | Single `compact_id` helper (ASCII alnum, lowercase, documented Unicode policy) used by retired-provider detection, alias matching and shrink priority. `ordinal_ignore_case_key` delegates to `culture_ignore_case_key(value, false)`. Historic `stats`/`keyboard` aliases kept explicitly. | `settings_compat.rs::widget_density_override_and_shrink_priority_are_exposed`, `legacy_lowercase_catalog_ids_are_canonicalized_on_load`, `widget_map_keys_are_canonicalized_for_both_builds`. |
| `[Orta]` Density lookup assumed canonical keys | `resolve_widget_density` canonicalizes both the requested ID and each stored key before comparing, so in-memory `stats` finds a saved `SystemStats` override. | `settings_compat.rs::density_overrides_resolve_through_legacy_id_spellings`. |
| `[Düşük]` Backup timestamp collision | Invalid-settings backups use milliseconds plus an atomic sequence suffix; original file still kept. | `settings::tests::invalid_backups_never_overwrite_each_other`. |
| `[Düşük]` Custom-ID test gap | Mixed-case `CUSTOM:Usd-Try` now covered through canonicalization, placements, island orders, density, profiles, save → load → validate and direct `set_widget_enabled`. | `settings_compat.rs::mixed_case_custom_ids_canonicalize_through_every_collection`. |
| `[Yüksek]` Disabled hotkey strings rejected by `validate()` | `HotkeyBinding::is_valid_custom_string` now accepts the exact `is_disabled_hotkey` set (`Esc`, `Escape`, `Backspace`, `Disabled`, empty/whitespace); `resolve` returns `None` for all of them instead of falling back to the preset, eliminating false `HotkeysConflict`. | `hotkey::tests::every_disabled_spelling_validates_and_resolves_to_no_binding`; `settings::tests::disabled_hotkey_values_survive_load_validate_and_save_round_trip`. |
| `[Orta]` Comma-separated legacy `AiUsageProviders` | Extracted `parse_flag_names_text`, shared by serde and `ai_bits`; `"Claude, Codex"` now yields both bits during legacy migration. Also saturates oversized numeric flags instead of truncating to a valid bit. | `settings_compat.rs::comma_separated_ai_provider_flags_survive_legacy_migration`; `settings::tests::oversized_flag_numbers_do_not_truncate_into_valid_bits`. |
| `[Orta]` `save_to` validated before canonicalizing | `write_settings_to` clones → `canonicalize_widget_keys()` → `validate()` → serialize, so repairable in-memory spellings are written canonically instead of returning `InvalidData`. | `settings::tests::save_canonicalizes_repairable_settings_before_validating`. |
| `[Orta]` `apply_full_profile` wiped `theme_pack` | `theme_pack` is now applied only when the profile explicitly contains one, like the other `Option` fields. | `settings_compat.rs::profile_without_a_theme_pack_preserves_the_current_pack`. |
| `[Orta]` `log::resolve_message` rescanned inserted arguments | Shared single-pass `i18n::substitute_placeholders` used by both `log::resolve_message` and `i18n::t_fmt` (via new `t_fmt_culture`). | `log::tests::resolve_message_is_single_pass_like_i18n_t_fmt`; `i18n::tests::substitution_does_not_rescan_inserted_arguments`. |
| `[Orta]` i18n unit test raced with language mutation | `format_replaces_placeholders` uses explicit `DEFAULT_LANGUAGE` via `t_fmt_culture`; language-mutating test restores the default with a `Drop` guard. Catalog/global-lock poisoning now recovers consistently instead of mixing `expect` and `.ok()?`. | existing `i18n::tests::format_replaces_placeholders`, `set_language_normalizes_and_rejects_unknown_tags`. |
| `[Orta]` English-left user-visible locale values | Added an active quality gate: 44 high-visibility labels that are already translated in all 13 non-English catalogs must stay different from en-US, plus per-language "en-US-identical value" budgets that catch mass English overwrites without mass-editing catalogs. Same gate added to `scripts/check-locale-parity.ps1`. Numeric gates unchanged: 14 × 1373 Rust keys / 14 × 1046 reference keys. | `tests/i18n_parity.rs::high_visibility_labels_stay_translated_in_every_language`, `no_catalog_regresses_to_more_english_identical_values`. |
| `[Düşük]` `LEGACY_RUST_LOG_FILE_NAME` / `log::init` redirect | Removed the silent path rewrite; `init` honors the caller's path and reports a duplicate init to stderr. The legacy constant is documented as an alias of the current name. | `log::tests::rust_log_rotation_uses_the_shared_backup_name` (custom/relocated `.old` rotation added). |

## Partially fixed

| Finding | What changed | What remains |
| --- | --- | --- |
| `[Orta]` `apply_full_profile` legacy guard | `theme_pack` preservation fixed (see above). | Extending the legacy guard to `position`/`size`/`auto_hide`, or making those `DockProfile` fields `Option`, changes the public profile schema consumed by `crates/opentaskbar-app` — outside this worktree's ownership. Skipped; no test added for it. |

## Skipped

| Finding | Reason |
| --- | --- |
| `[Düşük]` Second ID dictionary / reversed `ShrinkPriority` table | Aligning `widget_shrink_priority` with the app registry requires editing `crates/opentaskbar-app` (out of ownership); the function has no production caller in core, so changing its numbers alone would not fix the divergence. |
| `[Düşük]` Core integration tests call local algorithm copies / source-text bridges instead of production seams | Making those tests call production requires exporting seams from `opentaskbar-app`/`opentaskbar-shell` (out of ownership). Existing tests and source-contract bridges were left intact. |
| `[Not]` `stats_meter_bits` `extra["StatsMeters"]` branch is unreachable | Dead defensive branch behind a custom deserializer; removing it has no behavioral effect and is not a report finding. |
| `[Not]` App-side `custom:` registry lookup | Core now always writes canonical `custom:<slug>`; the app registry lookup lives in `crates/opentaskbar-app` and cannot be changed here. |

## Verification

- `cargo fmt --all` — pass
- `cargo check --workspace --all-targets` — pass
- `cargo clippy --workspace --all-targets -- -D warnings` — pass
- `cargo test --workspace` — pass (519 + 58 + 8 + 9 + 10 + 10 + 9 + 10 + 12 + 13 + 28 + 11 + 5 + 5 + 11 + 80 tests; 1 pre-existing ignored)
- `powershell -File scripts/check-locale-parity.ps1` and `pwsh -File ... -Quiet` — pass
