# FIXES-H — Deep Agent H (UI / widget helpers)

Worktree: `D:\projects\OpenTaskbar-wt-h` (branch `fix/deep-h`).
Only files inside the assigned ownership were modified (plus this report).

Verification (all green): `cargo fmt --all`, `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
(531 passed + 2 pre-existing/ignored in app; all other crates pass).

## Findings

| # | Finding | Status | Fix + tests |
|---|---------|--------|-------------|
| 1 | Raw UIA vtable slots shifted (`uia_raw.rs`) | **fixed** | Corrected SDK method order: `AutomationVtbl` `GetRootElement`=5, `ElementFromHandle`=6, `CreatePropertyCondition`=23; `ElementVtbl` `FindAll`=6, `GetCurrentPattern`=16, `CurrentName`=23; `RangeValueVtbl` `CurrentValue`=4, `CurrentIsReadOnly`=5, `CurrentMaximum`=6, `CurrentMinimum`=7. Added compile-time `offset_of!` and vtbl-size asserts plus a `#[ignore]`d real-COM smoke test (`cargo test -p opentaskbar-app uia_raw::tests::com_smoke -- --ignored`). Test: `vtable_slots_match_sdk_method_order` (runs on every build). Typed `windows` `Win32_UI_Accessibility` API was not used because `Cargo.toml` is outside ownership. |
| 2 | Two parallel text-input engines; unprotected `clamp`; different char limits | **fixed** | `text_input.rs` now owns `MAX_TEXT_CHARS`, `is_numeric_char`, `push_char`, `normalize_numeric`. Both GPUI and C2 flyout engines call them; flyout gains `delete`. Reversed/non-finite bounds are normalized instead of panicking; duplicate `.`/`-` are rejected. Tests: `reversed_and_non_finite_bounds_never_panic`, `both_engines_share_the_unicode_character_limit`, `numeric_helpers_reject_duplicate_sign_and_separator`. |
| 3 | Speedtest `JoinHandle` leak; spawn failure hangs state | **fixed** | Cancelled handles go to a `reaper` Vec joined in `poll()` only after `is_finished()`; spawn error sets `Failed`; worker panic sets `Failed`; removed dead palette local. Tests: `cancelled_handle_is_reaped_by_poll`, `panicked_worker_marks_the_state_failed`. |
| 4 | AI quota parsing in three places; dead parser; severity divergence | **fixed** | Removed production-dead `ai_flyout::parse_full_detail` and its test; `parse_reset_countdown` delegates countdown formatting to `telemetry::countdown_from_unix`; `from_telemetry` includes `retry_after_secs` in severity; removed `dashboard` dead local. Tests: `parses_reset_countdown_hours_and_minutes` (real 1h/1m/expired vectors), `retry_after_alone_raises_adapter_severity`, updated flyout mapping test. |
| 5 | `searchable_dropdown` cache invalidated only by `options.len()` | **fixed** | Cache key is now a content fingerprint (label/flag/glyph/selected); one `clamp_scroll_top` helper used by keyboard and wheel; active row read from owner via `SearchableDropdownHandlers::active` instead of a render-time snapshot. Tests: `option_fingerprint_detects_same_length_replacement`, `scroll_top_uses_one_bound_for_keyboard_and_wheel`. |
| 6 | `SessionRegistry::release()` left stale rollback; NaN clamp | **fixed** | `release()` drops the rollback; `VolumeRollback::begin` uses `clamp_unit`; `finish_volume_change` returns `None` when the session is gone. Tests: `rollback_is_dropped_when_a_session_is_released`, `non_finite_requested_volume_becomes_zero`. |
| 7 | Three dropdown builders duplicated (~250 lines) | **fixed** | Extracted `dropdown_option_row` and `dropdown_popover`; `dropdown_labeled`, `icon_action_menu` and the virtualized searchable row now share them. Extracted `step_index(.., Wrap|Clamp)` used by all three navigation call sites (menu keeps wrap, dropdowns keep clamp). Test: `index_stepping_preserves_wrap_and_clamp_policies`. |
| 8 | Stale "after merge"/"no call site" comments and `allow(dead_code)` | **fixed** | Removed redundant allows from C2 `LiveEvent`/`FlyoutBlock` variants, `show_flyout`, `show_flyout_live`, `FlyoutEdge`, `ButtonVariant::Primary`, native menu APIs; kept allows only where genuinely unused, with accurate comments. Comment-only change. |
| 9 | Two modal Win32 loops; class re-registered; wrong-window keys; PWSTR leak | **fixed** | Shared `register_popup_class` (once per thread, tolerates `ERROR_CLASS_ALREADY_EXISTS`) and `modal_loop`/`ModalStep` in `ui/mod.rs`, used by `prompt.rs` and `menu.rs`. Prompt Enter/Escape only for its own hwnd/edit control. `result_path` frees `PWSTR` on every path. Comment-only/robustness; no dedicated Win32 test. |
| 10 | `exe_stem` / session-id duplication and inconsistent casing | **fixed** | `diagnostics::exe_stem` and `diagnostics::session_id` are `pub(crate)` and used by `media_ext`; `.exe` stripping is case-insensitive for `.Exe` too. Test: `ws4_session_adapter_maps_real_core_type`, existing `exe_stem_strips_path_and_extension`. |
| 11 | Five time-formatting copies | **fixed (owned scope)** | `media_ext::format_time` delegates to `format_clock`; `seek_ratio` shares `progress_ratio` clamping (kept the literal `if duration == 0` guard that an external core integration test pins). Tests: `media_time_formatting`, `clock_formatting_round_trips`, `seek_ratio_clamps`. `focus_break.rs` / `widgets/mod.rs` copies are outside ownership — see skip #24. |
| 12 | Tests assert source text / localized literal instead of behaviour | **fixed** | Removed all `include_str!` assertions from `flyout.rs` and `settings_search.rs`. Added `resolve_live_outcome` pure helper and behavioural tests for tick/close and calendar header ordering from the real culture setting; search test asserts an alias yields no hits; speedtest labels compared with `loc::t`. |
| 13 | `MediaTimeline::update` overwrites `sample` with stale echo | **fixed** | Added `last_rejected_position`; stale echo no longer becomes the comparison baseline; `reset()`/`confirm_seek()` clear all anchor fields. Test: `confirm_seek_rejects_stale_smtc_echo` extended over 3 stale ticks plus real progress. |
| 14 | Flyout cover image decoded from disk every repaint | **fixed** | `FlyoutD2d` caches target-owned `ID2D1Bitmap` by path (failed loads cached as `None`); layout and paint share `image_size_dips`. No new unit test (D2D target required); covered by existing paint tests compiling. |
| 15 | `widget_has_settings` duplicated stale list | **fixed** | Now delegates to `widgets::WidgetOption::for_id(..).has_settings`, covering aliases and custom widgets. Test: `search_matches_ws_a_field_names_and_visible_labels` checks every catalog widget against the registry. |
| 16 | `widget_glyph` inline match list | **fixed (owned scope)** | Converted to one `WIDGET_GLYPHS` const table with prefix/fallback helpers; behavioural test verifies every catalog widget resolves to a non-fallback single glyph. Adding a registry `glyph` field is skipped — see #26. |
| 17 | Custom widget card buttons hard-coded English | **fixed** | Folder/Edit/Remove use existing `OpenFolder`/`Edit`/`WidgetRemove` keys; Approve/Approval required use `localized_action(key, fallback)` so translations activate when keys are added without changing current text. |
| 18 | Custom preview ignored selected icon / ratio / details | **fixed** | `WidgetChipSpec::Custom` now renders `WidgetIcon::Glyph` via `icon_glyph_owned`, `Asset` via GPUI `img`, `PathData` fallback glyph, a `mini_bar` for `ratio` and the first detail row. |
| 19 | Card button style chain duplicated three times | **fixed** | Added `card_button` and `compact_card_button` in `components.rs`; all three Settings widget-card button blocks use them. |
| 20 | D-1 reversed min/max panic (verified earlier finding) | **fixed** | Covered by #2 (both engines route through `normalize_numeric`). Test: `reversed_and_non_finite_bounds_never_panic`. |
| 21 | D-8/D-11 source-text tests in owned files | **fixed** | Covered by #12; no `include_str!` assertion remains in the owned UI/widget files. |
| 22 | WinHTTP wrapper merge (`telemetry.rs`) | **skipped** | `widgets/telemetry.rs` is outside this agent's ownership; no source touched. |
| 23 | `is_custom` Unicode byte-slice panic (`custom_widgets.rs`) | **skipped** | `widgets/custom_widgets.rs` is outside ownership. |
| 24 | Time-formatting copies in `focus_break.rs` / `widgets/mod.rs` | **skipped** | Files outside ownership; only the two `media_ext.rs` copies were unified (#11). |
| 25 | Dead `let _ = (has_data, &pal);` in `widgets/mod.rs` | **skipped** | `widgets/mod.rs` outside ownership; the analogous `let _ = &dashboard` in `ai_flyout.rs` was removed (#4). |
| 26 | Registry `WIDGET_GLYPHS`/`WidgetDescriptor.glyph` field | **skipped** | `registry.rs` outside ownership; local table + parity test used instead (#16). |
| 27 | Source-text tests in `dock/tray.rs` / `dock/mod.rs` | **skipped** | Files outside ownership; their `include_str!` assertions remain. |

## Notes

- The UIA smoke test is deliberately `#[ignore]`d: it needs an interactive desktop and a live
  `CUIAutomation` service. The permanent `offset_of!` test guards the ABI offsets on every build.
- `media_ext::seek_ratio` keeps the `if duration == 0` literal because an out-of-ownership
  integration test (`crates/opentaskbar-core/tests/media_timeline.rs`) pins that source text;
  the actual clamp now flows through the shared `progress_ratio`.
- Fixed: **21** (items 1–21, including owned-scope partials). Skipped: **6** (items 22–27).
