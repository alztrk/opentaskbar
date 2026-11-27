# FIXES-K — deep review findings (docs & resources)

All work was done in worktree `D:\projects\OpenTaskbar-wt-k` on branch
`fix/deep-k`. Only owned text files were touched; no Rust source, scripts,
`.github`, packaging, Cargo files or tests were modified. No build was needed
(docs-only changes).

## Findings from the report

| # | Finding | Status | What changed / reason |
| :--- | :--- | :--- | :--- |
| 1 | DPAPI secret scope example | **Fixed** | `docs/guides/custom-widgets.md` now uses `widget:<slug>:<key>` (example `widget:usd-try:token`), requires the key to be declared in the widget's manifest allowlist and states that other names are refused. `PRIVACY.md` and `SECURITY.md` gained the same own-namespace guarantee. Written as target behavior; code agents implement the scope check. |
| 2 | Hash change / sidecar restart promise | **Fixed** | `custom-widgets.md` now says re-approving the new hash restarts the sidecar so the new code runs; `PRIVACY.md` mirrors it. Written as target behavior; no "old process may linger" caveat left. |
| 3 | Timeout / Job Object overclaim | **Fixed** | `custom-widgets.md` now says the kill-on-close job guarantees children die when the app exits, a timed-out poll is detected on the next poll, and a stopped widget's job is not torn down until app exit. Doc-only correction (not in the parallel code-fix list). |
| 4 | `protocol`/`seq` optionality | **Fixed** | `custom-widgets.md` now says mismatched *present* values are ignored while replies omitting the fields are accepted. Doc-only correction. |
| 5 | `name` default "first entry" | **Fixed** | Removed the "first entry" claim; documented exact culture → two-letter language → arbitrary remaining entry, with advice to always include `en-US`/`en` for a predictable name. Doc-only correction. |
| 6 | min/max crash claim | **Fixed** | `custom-widgets.md` now states reversed `min`/`max` are normalized (swapped) before use, so invalid bounds cannot crash the dock. Written as target behavior; code agents normalize the limits. |
| 7 | Custom widget icon resolution order | **Fixed** | Both `custom-widgets.md` and `themes.md` describe the full chain (Settings override → theme `widgets/icons/pack.json` → widget `icon.png` → default glyph), explicitly keyed by slug without the `custom:` prefix, and state that both theme mapping and the bundled icon are honored. Written as target behavior; code agents fix `icon_for()` slug extraction. |
| 8 | Nonexistent feature flags | **Fixed** | Removed `enable_vendor_battery_probes`, `enable_theme_editor` and `enable_ai_oauth` from the README flag table and from `themes.md`; added a note that theme editor, vendor battery reading and AI sign-in are always available. Only the three flags actually read remain. |
| 9 | Theme `fallback` ranges | **Fixed** | `themes.md` table now shows `cornerRadius` `0`–`1.0` (default `0.5`) and `iconScale` `0.1`–`1.0` (default `0.64`), matching the code clamps. |
| 10 | Nested icon folders | **Fixed** | Folder diagram labels `icons\images\…` as usable only via a `pack.json` mapping; the prose now limits unmapped files to `icons\` / `icons\icons\`. |
| 11 | `.otbackup` theme contents | **Fixed** | `themes.md`, `PRIVACY.md` and audit case S7 now say archives contain settings and pins only; theme packs install separately to `Themes\` and are not archived. Doc-only correction (not in the parallel code-fix list). |
| 12 | CHANGELOG workspace layout | **Fixed** | `CHANGELOG.md` now names `resources/fonts/`, `resources/assets/`, `resources/themes/` instead of the nonexistent top-level `Fonts/`, `Assets/`, `themes/`; "only the root workspace" softened to "root workspace first" with `docs/`, `resources/`, `packaging/`, `tests/`, `tools/` listed. |
| 13 | Token problems "logged" | **Fixed** | Removed the claim that skipped tokens are written to `opentaskbar.log`; `themes.md` now says unknown/wrongly typed values are skipped without aborting theme load. Doc-only correction (not in the parallel code-fix list). |
| 14 | "both builds" language | **Fixed** | `themes.md` technical note now says the single Rust build reads these folders directly and the theme editor is always available; no stale reference-build implication. |
| 15 | Theme template missing icons pack | **Fixed** | Added `docs/guides/theme-template/widgets/icons/pack.json` with commented `widget:<slug>` / `pathData` entries and expanded the folder diagram and template note in `themes.md`. |
| 16 | Icon priority order | **Fixed** | Moved the "Special name" row to the top of the matching table in `themes.md` and clarified that `@` keys are checked before all others; technical note updated accordingly. |
| 17 | Third-party theme licenses | **Fixed** | Added `resources/themes/LICENSES.md` with MIT attribution for the five bundled palettes (Catppuccin Community, morhetz/sainnhe, Arctic Ice Studio, Ethan Schoonover, enkia) plus a Prism note, linked from "Sharing themes". Root `LICENSE` left untouched. |
| 18 | README widget list / update default | **Fixed** | Kept the 22-widget gate but changed the list to clearly non-exhaustive examples and added the missing widgets/providers; privacy text now states update checks are on by default in portable builds and can be turned off in Settings. |
| 19 | Relative `icon` hint path | **Fixed** | `custom-widgets.md` now says PNG hints must be absolute because relative paths resolve against the host's working directory, and that a widget-folder `icon.png` needs no hint. Doc-only correction. |

## Other checks

- **Stale `.local` scratch paths/versions — skipped.** `.local/` is gitignored
  and is not present in the worktree, so there is nothing trackable to change
  or commit; no document links to it. Left untouched to avoid editing
  out-of-worktree user files.
- **Out-of-ownership source examples — skipped.** `crates/.../custom_widgets.rs`
  module comment still shows `api-key:example`, and
  `crates/opentaskbar-shell/src/flyout_writer.rs` still says
  `native_flyout_reposition` defaults to `false`; both are source-side and
  belong to code agents.
- **Numeric gates — verified unchanged:** 14 languages, 1373 Rust keys /
  1046 frozen reference keys, 22 catalog widgets, version `0.4.0` /
  MSIX `0.4.0.0`.
- **Relative links — verified:** every relative Markdown link in the touched
  files resolves (including the new `LICENSES.md` links `../../LICENSE` and
  `prism/LICENSE`).
- **Build/test — not run:** only Markdown/JSON text files were changed.
