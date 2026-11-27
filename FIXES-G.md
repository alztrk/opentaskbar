# FIXES-G — Deep agent G (design & dock)

Worktree: `D:\projects\OpenTaskbar-wt-g` (branch `fix/deep-g`).
All changes are confined to `crates/opentaskbar-app/src/design/**`,
`crates/opentaskbar-app/src/dock/**` and `crates/opentaskbar-app/examples/**`.

Verification (all green):
`cargo fmt --all`, `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (530 app tests + all other crates, 0 failures).

## Deepened prior findings

| Finding | Status | Fix / reason | Tests |
|---|---|---|---|
| D2D transform restore + `BeginDraw`/`EndDraw` balance | FIXED | Added `design/d2d.rs` with generic `TransformGuard` and `with_surface_draw`. `brand_paths::draw_path_data_logo` now restores the transform on a brush-creation `?`; the three `spatial_glass.rs` surfaces and the four lab-example surface draws call `EndDraw` unconditionally before propagating errors (same pattern as `dcomp::draw_surface`). | `d2d::tests` (end on failure, transform restore), `tokens` malformed path test |
| `Simple`/`Custom` chip duplication | FIXED | Shared `TwoLine` struct + `draw_two_line_chip` closure, with ratio bar extracted into `draw_chip_ratio`. Both variants now share card/icon/two-line text layout. | compile + existing chip tests |
| Stale WS-x comments / `allow(dead_code)` | FIXED | Rewrote module docs in `design/mod.rs`, `design/tokens.rs`, `dock/layout.rs`, `dock/mod.rs`, `dynamic_island.rs`, `window_picker.rs`, `show_desktop.rs`; removed `_unused_rect_check`, the wrong `#[allow(dead_code)]` on `create_widget_text_surface`, and unused `ShowDesktopCorner::new/update`, `PreviewPopup::set_glass_mode`, `preview::rgb`, `DWMWCP_ROUND`. | clippy `-D warnings` |
| Proliferating colour/glyph tables | FIXED | `icons.rs`: one `(fluent, mdl2)` `GLYPHS` table + shared PUA constants used by `MDL2_FALLBACKS`; added `named_glyph`. Dock/lab PUA literals for prev/next, clock, chevrons, close and fallback icon now resolve through `icons::*`. Fixed font families/locales now come from `icons::icon_font_family()`, `fonts::ui_family_list()` / `ui_locale()`. Threshold colours use `palette().warning/text_primary/text_secondary`. | `icons` tests, build |
| `DEFAULT_WIDGET_ORDER` manual list | FIXED | Derived at compile time from `widgets::registry::BUILTINS` filtered by `catalog`; island test asserts the derived order equals the registry order. | `dynamic_island::tests::default_order_is_derived_from_the_registry_catalog` |
| `PathData` consumed check | FIXED | `build_path_geometry` returns `E_INVALIDARG` when `parse_stream_geometry` does not consume all input; malformed icons are no longer half-drawn. | `brand_paths::tests::malformed_path_data_is_rejected_instead_of_drawn_partially` |

## New findings

| Finding | Status | Fix / reason | Tests |
|---|---|---|---|
| [High] `Rgba::from_hex` Unicode panic | FIXED | All slicing now happens on `value.as_bytes()` via `from_utf8` + `from_str_radix` / nibble parsing; non-ASCII bytes return `None` instead of slicing a `&str` mid-character. | `tokens::tests::unicode_and_non_ascii_hex_input_returns_none_without_panicking` |
| [High] CAM surface `BeginDraw` error path | FIXED | Same `with_surface_draw` helper applied to all three `spatial_glass.rs` draws and the four lab-example draws. | `d2d::tests::end_draw_runs_even_when_draw_fails` |
| [High] Dock ignores `ThemeManager` materials | FIXED | `rebuild_theme_surfaces` now reads `ThemeManager::materials(Dock)` (fill, raw editor tokens for border/hover/press/highlight, radii), `background_for_opacity` for the fill mode and `glass_surface_color_for` for pill RGB. Stock-token values keep the legacy dock colours so default appearance is unchanged; theme-pack/editor overrides now reach DComp. | `theme::tests::dock_material_token_helpers...`, build |
| [High] Duplicate Win-key grace timers | FIXED | Removed `win_key_grace_until` and `WINKEY_REVEAL_MS`; single `win_key_reveal_until`, one `arm_win_key_reveal()` gate used by hook, key-down and keyboard navigation, cleared by `reset_transient_ui`, duration from `layout::WIN_KEY_GRACE_MS`. **Behaviour change:** disabling reveal/auto-hide no longer suppresses auto-hide, and session switches no longer leave a stale reveal. | build + existing auto-hide tests |
| [Medium] Island policy duplicated in app | FIXED | Deleted local `clamp_window_width`, `should_show`, `order_widgets`, `move_widget` and `IslandVisibility`; tests now exercise `policy::*` exactly as production does. | `dynamic_island::tests` (clamp/visibility/order/move) |
| [Medium] `sync_rect` marks key after failure | FIXED | `surface_key` is only set when `rebuild_geometry_surfaces` succeeds; failures clear it so the next tick retries. | build |
| [Medium] `preview.rs`/`window_picker.rs` D2D/DIB boilerplate | PARTIAL | Added shared `d2d_color`/`d2d_color_channels` in `design/d2d.rs`; `dcomp::color_rgba`, `preview::d2d_rgba`, `window_picker::d2d_rgba` now delegate to it, and popup text formats use the shared locale/font helpers. Full `D2dFrame`/`with_dib_target` extraction was **skipped**: it is a ~150-line structural rewrite of two live popup renderers with no correctness bug, and is better done as its own change. | build + popup tests |
| [Medium] Lab copy drift | FIXED | Lab tint now uses `clear_crystal_tint_alpha`, dark tint `(8,11,16)`, `edge_glow / 0.88`, sheen stop `0.34` — aligned with `spatial_glass.rs`. The effect wrapper classes stay local because the example is a standalone binary with no app lib to link; module doc says so. | build (example target) |
| [Medium] Asset path tables repeated 3–4× | FIXED | Macro-generated `EMBEDDED_ASSETS` is now the single source for `flag_bytes`, `all_paths`, `theme_json_path`, `widget_pack_path` and `EmbeddedAssets::load`; `BUNDLED_THEME_IDS` is reused by the manifest test. | `fonts` asset tests, build |
| [Medium] Duplicate Lucide/glyph rows | FIXED | `REFRESH_PATH`/`GITHUB_PATH` constants shared by two keys each; MDL2/Fluent glyphs generated from the one `GLYPHS` table. | `icons`/`lucide` tests |
| [Medium] Opacity helpers diverge on non-finite input | FIXED | `clamp_opacity`/`alpha_from_opacity` live in `tokens.rs`; glass and theme delegate, non-finite now consistently means "no surface" (alpha 0). **Behaviour change:** theme `scaled_alpha(NaN)` is now 0 instead of 255. `radius::derive_for` clamps the base once and uses `NineKeys::REFERENCES` as the fallback source. | `tokens::tests::opacity_helpers_share_one_non_finite_rule`, `radius::tests::derive_clamps_the_base_once...` |
| [Medium] Island hover/flyout gate case-sensitive | FIXED | `dock::widget_has_flyout_ci` canonicalizes the id and uses the registry's case-insensitive descriptor lookup before the case-sensitive `widget_has_flyout`. **Behaviour change:** legacy mixed-case/custom ids can open flyouts. | `dock::tests::hover_flyout_gate_accepts_legacy_casing_and_custom_ids` |
| [Medium] Middle-click leaves a dead preview card | FIXED | `close_card_and_reflow(idx)` shared by X-button and middle-click; rebuilds thumbnails for remaining cards. **Behaviour change:** dead card disappears immediately. | build (Win32 UI path) |
| [Medium] Four text-format aliases | FIXED | Aliases removed; names now match real scaled sizes (`fmt_12_b_l`, `fmt_11_b_l`, `fmt_11_b_c`, `fmt_11_l`, `fmt_11_b_r`, `fmt_10_b_l`, `fmt_10_l`, `fmt_9_l`). No duplicate AddRef of the same COM format. | build |
| [Medium] Picker refresh worker not joined | FIXED | `PickerState` keeps a wakeable worker (`recv_timeout`) plus `JoinHandle`; `show()` and `Drop` signal cancellation and join before teardown. | build + picker tests |
| [Low] Two shadowed default-colour tables | FIXED | Removed the `or_insert` reference entries that `default_palette` had already filled (text/accent/focus), leaving one authoritative palette-derived value per key. `windows_palette.rs` remains the separate Windows accent layer by design. | theme tests |
| [Low] Source-text validation tests | FIXED | Replaced `include_str!` assertions in `tray.rs`/`window_picker.rs` with behavioural assertions (`FlyoutOptions::tray()`, menu id ranges, picker enable gate, `request_close_all`). | respective unit tests |
| [Low] `ease_out_cubic` / `38.0` duplicates | FIXED | Local cubic helper removed in favour of `layout::cubic_ease_out`; island chip rect uses `CHIP_H_DIPS`. | island tests |
| [Low] Dead `IslandClickAction::OpenFlyout` | FIXED | Variant and unused `_widget_id` parameter removed; dispatch table keeps Prev/Next/ToggleDetails. | `island_click_dispatch_table` |
| [Low] `apply_window_backdrop_with` partial success | FIXED | Cosmetic DWM attributes now log warnings without flipping the result; success depends only on the applied backdrop. **Behaviour change:** callers no longer get false negatives. | `glass::tests::failed_backdrop_returns_false_but_cosmetic_attributes_are_advisory` |
| [Low] `parse_json_lenient` double state machine | FIXED | One `strip_comments` scanner; second pass only removes trailing commas. `MAX_GRADIENT_STOPS` moved to `tokens.rs`. | widget-pack tests |
| [Low] `TOKEN_COUNT` test name / dead constants | FIXED | Test renamed to `derives_33_tokens_and_wcag_foreground`; unused `DWMWCP_ROUND` and `rgb()` removed. | windows-palette tests |

## Skipped with reasons

- Extending `opentaskbar-core::dynamic_island::order_widgets` to a 5-argument timed-order signature: `core/**` is outside my file ownership. The production call already passes all timed widgets through `DEFAULT_WIDGET_ORDER`, so no behaviour is lost; the local divergent copy was removed instead.
- Making `widgets::registry::widget_has_flyout`/`widget_name` canonicalize ids internally: `widgets/**` is outside my ownership. Fixed at the only owned call site via `widget_has_flyout_ci`.
- Full `D2dFrame`/`with_dib_target` extraction across the two popup renderers: large structural refactor of working Win32/DIB code; only the shared colour conversion and font/locale lookups were unified (see PARTIAL above).
- Table-driven `ChipPainter` split of `create_widget_chip_surface`: explicitly not recommended by the report; only the genuinely duplicated branches were unified.
