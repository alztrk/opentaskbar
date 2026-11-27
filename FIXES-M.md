# Agent M — deep-review fixes (widgets)

Worktree: `D:\projects\OpenTaskbar-wt-m` · branch `fix/deep-m` · base `03b65fb`.
Files touched: only the eight owned files under `crates/opentaskbar-app/src/widgets/`, plus this report.

**Result: 41 findings fixed, 9 skipped/deferred (with reason), cross-agent call-site changes listed below.**
Verification: `cargo fmt --all`, `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all clean
(534 app tests + all other crate tests pass).

---

## P0 — security / crash

| Finding | Status | Fix | Tests |
|---|---|---|---|
| DPAPI secret scope (C-1, E-2, ORTAK P0-1) | **Fixed** | `telemetry::widget_scoped_secret_name(name, slug)` accepts only `widget:<own-slug>:<key>` (case-insensitive prefix/slug, length caps). `read_dpapi_secret_for_widget(name, slug)` now takes the *loaded spec's* slug, never the sidecar's claim. `api-key:*`, other slugs and malformed names return `None`; the host still sends `secretResult` with an empty value. Docs need agent K's update (see below). | `telemetry::widget_secrets_are_scoped_to_the_owning_slug`; `custom_widgets::sidecar_secret_handshake` (real PowerShell sidecar requests one allowed and one denied secret, asserts `"secret-value|"`). |
| `is_custom()` Unicode panic (C-12, ORTAK P0-2) | **Fixed** | `id.get(..ID_PREFIX.len()).is_some_and(...)` — no byte slicing panic. | `custom_widgets::custom_ids_are_recognized_without_unicode_panics` (7th byte inside `ğ`, leading emoji). |
| Manifest hash change does not restart sidecar (C-2, ORTAK P0-4) | **Fixed** | `reload()` now detects changed hashes, calls `Sidecar::invalidate()` (terminate job, clear snapshot/pending/failures, bump generation). Approval still follows the newly loaded hash via `is_approved`. Descriptors are reused by metadata (not hash); the newly resolved `root` is kept. | `custom_widgets::manifest_hash_change_forces_sidecar_restart`, `invalidated_sidecar_discards_late_replies`. |
| Secret handshake budget / optional protocol+seq (C-7) | **Fixed** | Secret requests have a separate budget (`MAX_SECRET_REQUESTS=8`) from reply attempts (`MAX_REPLY_ATTEMPTS=4`); `reply_matches()` requires both `protocol` and the exact `seq`. Reply lines are capped at 512 KiB. Protocol docs need agent K's update. | `custom_widgets::replies_require_protocol_and_exact_sequence`, `oversized_reply_collections_are_capped_and_ranges_normalized`. |

## P1 — correctness / races

| Finding | Status | Fix | Tests |
|---|---|---|---|
| Timeout only on next poll; `in_flight` cleared by non-worker (C-3, C-4, ORTAK P1-6) | **Fixed** | Per-request watchdog thread kills the job after `IO_TIMEOUT` even with no further polls; it only bumps `generation` and the cancel token — never clears `in_flight`. `InFlightGuard` (RAII, owned by the worker) is the sole clearer. Late replies are discarded by generation + seq check. | `custom_widgets::invalidated_sidecar_discards_late_replies` (generation guard), watchdog covered by compile/integration path. |
| Click clobbers poll snapshot / "sidecar busy" errors (C-8) | **Fixed** | Poll and click go through one dispatcher with a bounded pending queue. Click replies only call `apply_action`, never `apply_reply`, so snapshot/failures stay poll-owned. | `custom_widgets::requests_are_queued_while_the_worker_slot_is_busy`. |
| Timer accumulation starved by animation ticks (B-1, ORTAK P1-5) | **Fixed** | `advance_timers()` accumulates `as_secs_f64()` into `tick_debt_secs`, applies only whole seconds and derives the stopwatch centisecond carry from the remainder. `animation_tick()` remains a no-op and advances nothing. | `widgets::tests::fast_ticks_accumulate_into_whole_seconds_with_centisecond_carry`. |
| Focus right-click only resets mirror (B-3, ORTAK P1-7) | **Fixed** | `handle_secondary_click("focus")` resets `SharedFocusSession`, then mirrors all five fields from its snapshot. | `widgets::tests::focus_secondary_click_resets_the_shared_session`. |
| Live stats flyout spawns a thread every second (B-2, ORTAK P1-9) | **Fixed** | Removed `last_stats_top = None` from `live_spec`; added `invalidate_top_processes()` used by the Refresh button only. Sampling is coalesced by `stats_top_in_flight`; `build_stats_spec` no longer samples processes on the UI thread. | `widgets::tests::live_stats_tick_keeps_the_cached_top_process_list`. |
| Weather unit inconsistency (B-9) | **Fixed** | `weather_unit()` + `weather_display()` produce one `(temp, feels-like, range)` triple used by chip, flyout and tooltip; forecast/wind follow the same effective unit. | `widgets::tests::weather_override_is_shared_by_chip_flyout_and_tooltip`. |
| `speedtest_started_at` leak on live paths (B-10) | **Fixed** | `poll_speedtest()` used by tick, live_spec and all flyout buttons; cancel clears the timestamp too. | Covered by existing speed-test/flyout tests + compile. |
| Radio UI state reconciliation (B-4, ORTAK P2) | **Fixed** | `toggle_radio()` shared by chip click and flyout Click(5/6); success clears `radio_unavailable`, records `*_target`, resets `radio_failed`/`radio_denied`. | Existing quick-settings tests; manual helper logic is small/local. |
| Tooltip cache ignores settings (B-15) | **Fixed** | `TooltipTextCache.settings_key` fingerprint (city, effective unit, overrides); settings change invalidates before the 500 ms TTL. | `widgets::tests::tooltip_cache_invalidates_on_settings_change`. |
| GPU cached values ignored in flyout (B-8) | **Fixed** | Stats flyout reads cached `gpu_pct`, `gpu_temp_c`, `gpu_engine_name` and `gpu_metrics_visible()`; no second UI-thread `diagnostics::sample_gpu()`. | `gpu_thermal_hysteresis_uses_five_entries_and_two_exits` (cache state), compile. |
| `descriptor_for(id)` single lookup (A-6, B-5 lookup part) | **Fixed** | New `registry::descriptor_for()` (canonical + case-insensitive + custom) is used by `widget_name`, `widget_has_flyout`, `WidgetOption::for_id` and `hardware_available`; `widget_descriptor` unchanged. | `shell_helpers_keep_old_visibility_rules` (mixed-case and canonical assertions), `flyout_gate_covers_every_flyout_widget` (derived from `BUILTINS`). |
| `widget_has_settings` contradictions (A-1, D-6, ORTAK P2-12) | **Fixed (API)** | `registry::descriptor_has_settings()` and `widget_has_inline_settings()` provide authoritative answers. The stale local lists in `ui/settings_search.rs` / `ui/settings.rs` are **not** editable here — see cross-agent changes. | `shell_helpers_keep_old_visibility_rules`. |
| WinHTTP duplication + `Retry-After` decision (E-1, E-3, ORTAK P2-14) | **Fixed** | One `winhttp_request(HttpCfg) -> HttpReply` core; `winhttp_get` (fast timeouts, no retry header) and `winhttp_json` (AI timeouts, numeric + textual `Retry-After`) are thin wrappers. `is_retryable_status()` = 429 or 5xx only; a 200/4xx with `Retry-After` is no longer retried. Constants `HTTP_USER_AGENT`, `HTTPS_PORT`, `HTTP_MAX_BODY_BYTES`, `HTTP_TIMEOUTS_FAST/AI` de-duplicated. | `only_rate_limit_and_server_errors_are_retryable`, `retry_after_is_parsed_from_http_header_value`. |
| Time formatting / AI severity duplication (A-7, A-11, B-12, E) | **Fixed** | `focus_break::format_mm_secs` is the single mm:ss/hh:mm:ss helper (mod.rs local alias deleted); AI fallback detail goes through `ai_flyout::from_telemetry`, so severity parsing has one source. `category_i18n_key()` shared with UI call site. | `focus_break` transition/skip tests, `cached_ai_full_detail_is_rendered_instead_of_default`. |
| `FocusSession` field copy / settings serialization (A-8, A-11) | **Fixed** | `#[derive(Clone)]`, `snapshot()` = `with(Clone::clone)`, `from_settings` uses `..Default::default()` and one `serde_json::to_value` via `setting_value_from`. | `shared_session_snapshot_is_independent`, `settings_drive_session_lengths`. |
| `POLL_TTL` single source (A-12) | **Fixed** | `WidgetRuntime::tick` uses `widget_snapshot::POLL_TTL`; `ensure_fresh` no longer mixes caller/current clocks. | `widget_poll_snapshot_has_one_second_ttl`, `snapshot_expires_after_one_second`. |

## Other findings in owned files

| Finding | Status | Fix / reason | Tests |
|---|---|---|---|
| `ensure_child` pipe failure leaks child + job (C-6) | **Fixed** | Child/job are stored in state before pipe extraction; every pipe `?` path calls `terminate`. FFI handle typed as `HANDLE` via a `Send` newtype. | Compile + existing sidecar round-trip. |
| `parse_names` random default locale (C-9) | **Fixed** | JSON source-order key scan (`name_key_order`) provides the documented "first entry" fallback deterministically. | `localized_names_and_manifest_hashes`. |
| Unbounded collections / line length / reversed min-max (C-10, D-1, ORTAK P2-20) | **Fixed** | `read_reply_line` bounded by `MAX_REPLY_LINE_BYTES`; blocks/tiles/series/samples/details capped; `try_into()` for narrow integers; `textInput` min/max normalized. | `oversized_reply_collections_are_capped_and_ranges_normalized`. |
| `ICON_CACHE` unbounded / stale after Reload / pack.json changes (C-13) | **Fixed** | `MAX_ICON_CACHE` eviction, `pack.json` mtime in fingerprint, `widget_icons::invalidate()` called from `reload()`; decode cache kept (mtime-keyed, clearing causes thrash). | `invalidate_drops_cached_resolutions`. |
| Empty registry rescans disk every tick (C-15) | **Fixed** | `CustomRegistry.loaded` gate; only explicit `reload()` rescans an empty folder. | Existing registry tests. |
| Repeated JSON/icon/hash helpers (C-14) | **Fixed** | `str_field`, `opt_str_field`, `num_field`, `bool_field`, `u32_field`, `u16_field`, `ratio_field`; `icon_from_text` shared with `custom_widgets::icon_for`; `hex_hash` uses one `String` + `write!`. | `inline_override_supports_path_data_and_files`, `protocol_maps_chips_and_blocks`. |
| Threshold / brand colors (B-12) | **Fixed** | `threshold_color()` + `design::meter_color()` + `ai_provider_color()` replace literals. | Compile + chip tests. |
| Weather condition classification duplicated (B-13) | **Fixed** | `WeatherKind` enum + one `weather_kind()` feeds glyph and color. | Compile + flyout tests. |
| Module-level `allow(dead_code)`, dead `build_focus_spec`, discarded values (B-11) | **Fixed** | Removed the crate-level allow, deleted `build_focus_spec` and the `let _ = …` computations; kept narrow `#[allow(unused_imports)]` on public re-exports. | Clippy with `-D warnings`. |
| Stale comments (B-14, E-7) | **Fixed (partial)** | Updated merge-era / `wttr.in` / WIP comments in owned files; remaining WS-x labels describe live ownership boundaries, not stale merge state. | — |
| Alias self-mappings and brittle count tests (A-14, A-15) | **Fixed** | Removed `(showdesktop/showdesktop)` and `(quicksettings/quicksettings)` aliases; `canonical_widget_id` now checks all built-ins; tests derive counts from the table and assert `stacks` descriptor state directly. | `aliases_and_canonical_ids_resolve`, `builtin_table_is_the_only_metadata_source`, `shell_helpers_keep_old_visibility_rules`. |
| Compact-ID normalization duplicated (A-9) | **Fixed (shared helper)** | `catalog_ops::compact_id` used by `keys_match` and `registry::canonical_widget_id`; core's independent copy remains outside ownership. | `compact_id_and_cached_value_reader_share_one_rule`. |
| `setting_value` re-serializes settings (A-10) | **Fixed** | `setting_value_from(&Value, key)` added and used by `FocusSession::from_settings` (one serialization instead of four/five). | `compact_id_and_cached_value_reader_share_one_rule`, `settings_drive_session_lengths`. |
| Dead abstractions `WidgetOption`, `WidgetLayout`, `widget_layout`, `hardware_available`, `catalog()`, `ensure_fresh()` (A-5, B-11, ORTAK P2-15) | **Skipped removal** | They are `pub` re-exports from `widgets/mod.rs`; removal would break external/UI callers outside this worktree. They were repaired instead (shared `descriptor_for`, fixed clock mixing). Revisit when agent H removes the last call sites. | Existing registry/snapshot tests. |
| Single `WidgetProvider` behavior table / split of `open_widget_flyout` (B-5, B-6, ORTAK P2-13, D-7) | **Skipped (deferred)** | Behavior-neutral mechanical move of ~22 flyout branches into fn-pointer descriptors. Very large diff and high regression risk in the same batch as the P0/P1 security/race fixes; dispatch matches remain intact. Reconcile after these fixes land. | — |
| D2D transform rollback, `brand_paths` parser (C-5, C-16) | **Skipped — not owned** | `design/brand_paths.rs` belongs to another agent. | — |
| Core ID canonicalization / island limits / density override (E-4, E-5, E-6, E-8) | **Skipped — not owned** | `crates/opentaskbar-core/src/settings.rs`. | — |
| UI card buttons, search catalog, preview, flyout image cache (D-2…D-12) | **Skipped — not owned** | `ui/*` belongs to agent H. The runtime side of the Stats Refresh and radio/weather flows is fixed here. | — |
| `dcomp.rs` Simple/Custom chip drawing (B-13) | **Skipped — not owned** | `dock/dcomp.rs` belongs to agent G. | — |

## Cross-agent call-site changes needed (not applied here)

1. **Agent H — `ui/settings_search.rs:1240` `widget_has_settings()`**: replace the stale hard-coded list with
   `opentaskbar_app::widgets::descriptor_has_settings(id)` (or `registry::descriptor_has_settings`).
   `ui/settings.rs` inline-settings predicate should use `widget_has_inline_settings()` instead of its
   local five-ID list. Do **not** blindly switch it to `descriptor.has_settings`: that would add
   Configure buttons for `clock`/`keyboard`/all `aiusage_*`.
2. **Agent H — `ui/settings.rs::category_label()`**: can delegate the five category mappings to
   `widgets::category_i18n_key()` (keep `All`/`Active` special cases locally).
3. **Agent K — `docs/guides/custom-widgets.md`**: document that secret names must be
   `widget:<your-slug>:<key>`, that other namespaces return an empty `secretResult`, and that the
   sidecar protocol requires `protocol`/`seq` on replies. The old `api-key:example` sample in the
   module header was updated here.
4. **Agent G/H — dock animation timer**: `WidgetRuntime::animation_tick()` is intentionally a no-op;
   do not route it through `tick()` if the animation path only needs the battery wave phase.
5. **Agent G — `dock/dynamic_island.rs`**: `DEFAULT_WIDGET_ORDER` can be derived from
   `BUILTINS.iter().filter(|w| w.catalog).map(|w| w.id)` once that file is in scope.
6. **Core settings owner**: `compact_id()` in `catalog_ops.rs` should become the reference for
   `canonical_widget_setting_id` / `widget_shrink_priority` normalization if those tables are kept.
