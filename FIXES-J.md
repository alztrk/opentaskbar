# Agent J — fix disposition (deep review: shell, scripts, CI)

All line references below are to the original report
`.notes/code-review/deep-agent-j-shell-ci.md`. Work was done only in the
owned files. Verification in this worktree: `cargo fmt --all`,
`cargo check --workspace --all-targets`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace` — all pass. The
watchdog C source was additionally compiled with `tools/watchdog/build.ps1
-Architecture x64 -NoFallback` (PE machine 0x8664 verified); all touched
PowerShell files parse, all touched YAML parses, the MSIX XML parses with
14 `<Resource Language>` entries, and `check-release-version.ps1 -Tag v0.4.0`
passes.

## Priority fixes

### [High] Manual "Restore taskbar" permanently disables watchdog — FIXED
- `taskbar::restore_from_journal()` no longer calls
  `watchdog::notify_clean_exit()`. The watchdog stays armed for the rest of
  the session; the dock's live restore action cannot disarm crash recovery.
- Shell-side verified-only gate added in `watchdog.rs`: `RESTORE_VERIFIED`
  plus `set_restore_verified()`. `notify_clean_exit()` is a no-op until the
  real shutdown path (`taskbar::end_replacement`) publishes a verified
  restore, and `arm()` resets the gate for every new session. The result of
  `restore_from_journal()` is kept in `LAST_RESTORE_VERIFIED` because app
  call sites use the function as a statement (return type must stay `()`).
- Tests: `watchdog::tests::clean_exit_signal_waits_for_verified_restore`
  (early call leaves slot armed, later verified call consumes it),
  `clean_exit_stops_heartbeat_before_signalling` (unchanged).
- App-side follow-up (NOT made, app crate out of scope): delete
  `crates/opentaskbar-app/src/main.rs:238` — the early
  `watchdog::notify_clean_exit()` is now harmless but redundant and logs a
  "deferred" message. If the UI should re-establish the AppBar reservation
  after a manual restore, call `begin_replacement(...)` from the two live
  call sites (`dock/mod.rs:6541`, `ui/settings.rs:9009`); the shell no longer
  re-arms itself because it was never disarmed.

### [High] Clean-exit signalled before verified restore — FIXED
- `end_replacement()` now does restore → publish verification → signal →
  clear authorization. A failed restore (missing tray, re-hide, exhausted
  attempts) leaves the watchdog armed.
- The existing `main.rs` ordering is safe as-is thanks to the gate, so no
  app-side change is required for correctness.
- Tests: gate test above plus existing
  `session::tests::restore_verification_needs_two_consecutive_clean_passes`.

### [High] Hook flag set before `SetWinEventHook` succeeded — FIXED
- `session.rs` splits installation (`install_win_event_hook`) from the
  message pump. `HOOKS_INSTALLED` is set only after a non-null hook handle,
  the handle is retained for `UnhookWinEvent`, and a spawn failure resets
  `HOOK_INSTALL_PENDING` so a later call can retry. Real state is queryable
  via `are_shell_hooks_installed()`.
- Test-only `FORCE_HOOK_FAILURE` seam added;
  `shell_hook_install_failure_keeps_the_flag_false_and_retryable` verifies
  failure leaves the flag false.
- No app-side change required.

## Remaining new findings

| # | Finding | Status | What changed / why |
|---|---------|--------|--------------------|
| 4 | Flyout meta parser divergence | FIXED | `flyout_writer::read()` now delegates to `flyout_journal::read_from_with_meta()` through an injectable `read_from(path)`; extended `generation=N seq=M` lines restore correctly. New test `recovery_reader_accepts_extended_generation_lines`. |
| 5 | Dead position API in `flyout_journal` | SKIPPED (removal) | Out-of-scope `crates/opentaskbar-core/tests/shell_recovery.rs` pins the exact `pub fn` names by `include_str!`; deleting the APIs would break that crate's tests. Module docs were corrected to describe the real roles (shared parser, tag journal, optional meta records). Follow-up: core-owner should convert those source-text bridges to behavioral tests, then remove the unused wrappers. |
| 6 | Dead AppBar callback / reentrancy gate | FIXED | `APPBAR_CALLBACK_MSG` removed; `uCallbackMessage` stays 0 since no Rust WndProc handles `ABN_*`. `NotificationScope` retained as a documented defensive fence, not a claimed live dispatcher. |
| 7 | C watchdog deleted journal after one positive visibility pass | FIXED | `RestoreNativeTaskbars` now requires two consecutive visible observations 60 ms apart before deleting either journal; mirrors Rust's `RESTORE_REQUIRED_CLEAN_PASSES = 2`. |
| 8 | `IsWindow == 0` deleted journals (C + Rust) | FIXED | C returns a three-state `FlyoutRestoreStatus` and only deletes on `FLYOUT_JOURNAL_CLEAN`; Rust `classify_disposition` uses `Restored`/`Deferred`/`ForeignOwned` and keeps the journal for deferred or foreign-owned handles. Test `journal_is_kept_for_late_or_foreign_owned_windows`. |
| 9 | `(DWORD)-1` sentinel aliases `WAIT_FAILED` | FIXED | Sentinels removed; wait result is classified by handle identity, with `WAIT_FAILED` / `WAIT_ABANDONED_*` treated as fail-closed (restore). C argument parser now validates decimal PID and 16/32-hex token; Rust parser shares the same rule (tests updated with matching vectors). |
| 10 | Double publish from `release/v*` branch pushes | FIXED | Branch trigger removed; tag-only or `workflow_dispatch`. `concurrency.group` is keyed by tag name. `release_info` no longer derives a tag from a branch. |
| 11 | MSIX never built in CI | FIXED | New `msix` job in `release.yml` builds x64 and x86 unsigned Store-ready packages with `publish-rust-msix.ps1`, uploads them as artifacts, and attaches them to the GitHub Release (including the expected-assets check). |
| 12 | MSIX declared only 2 of 14 languages | FIXED | Manifest now declares all 14 catalogs; new `rust-ci.yml` step compares manifest `<Resource Language>` values against the on-disk locale catalogs so future drift fails CI. |
| 13 | `watchdogSha256` referenced unpublished files | FIXED | The exact `OpenTaskbarWatchdog-{x64,x86}.exe` PEs are now release assets, included in `checksums.txt`, attestation inputs and expected-assets verification. |
| 14 | `dependency-review` path filter missed member crates | FIXED | Path filter removed; the security gate runs on every PR to `master`. |
| 15 | Fourth hardcoded version / unusable local PR command | FIXED | `check-release-version.ps1` now derives the expected version from Cargo workspace (tag ↔ Cargo ↔ MSIX only). PR template uses `-Tag vX.Y.Z`; CONTRIBUTING placeholder updated. Out-of-scope README/docs still show literal `v0.4.0`. |
| 16 | Journal write errors silently swallowed | FIXED | `flyout_writer::persist`, both `flyout_journal` writers, `write_tag` and `journal::write_state` log warnings with path, operation and error; temporary files are cleaned on failed atomic replace. `begin_replacement` logs a failed taskbar journal write instead of discarding it. |
| 17 | Inconsistent action pinning | FIXED | `rust-ci.yml` checkout/rust-cache and `ci.yml` rust-cache now use immutable SHAs with version comments, matching the other workflows. |
| 18 | Duplicate fmt/clippy/test/build gates | FIXED | `ci.yml` remains the single required gate on every push/PR; the duplicate `build` job was removed from `rust-ci.yml`, which keeps only watchdog-arch and locale/MSIX checks. |
| 19 | Stale bot exempted all real issues | FIXED | `exempt-issue-labels` reduced to `pinned,security`. |
| 20 | Locale parity "14 languages" tautology | SKIPPED (script) | `scripts/check-locale-parity.ps1` is owned by agent I, so it was not edited. A CI-side manifest↔catalog check was added in `rust-ci.yml` to close the MSIX half of the gap. Agent I: replace `@($langs).Count -ne 14` with a filesystem count and collect `Get-Catalog` throws into `$failures`. |
| 21 | App-dir watchdog chosen without verification | FIXED | `select_watchdog_path` accepts the app-dir PE only when its bytes equal the embedded `build.rs` output; a stale/wrong-arch file falls back to the byte-exact `%LOCALAPPDATA%` copy. Test extended with a mismatching candidate. |
| 22 | `build.rs` host assumption / PE offset overflow | FIXED | `pe_machine` uses `checked_add(4/6)` and fails closed on corrupt `e_lfanew`; build script asserts a Windows host with a clear message instead of a vague PowerShell spawn error. |
| 23 | Stale comments / language policy violations | FIXED | `tracker.rs` multi-worktree comment replaced; all Turkish doc comments in `flyout_journal.rs` translated; `session::reset_budgets` no longer takes ignored arguments; bug report example is `0.4.0`. (`flyout_reposition.rs` "Başlat" is a localized UI string, intentionally kept.) |
| 24 | Source-text tests / weak shell tests | SKIPPED (core tests) | Out-of-scope `crates/opentaskbar-core/tests/{shell_recovery,production_seams}.rs` still assert source text; converting them is a core-owner follow-up. In owned shell code: arg-parser vectors mirrored between Rust and C, misleading `journals_are_atomic...` test renamed and replaced with a real generation/generation-path test, extended-line parser test and hook-failure seam test added. |

## Previously reported items (verification pass, items 1–5)

All SKIPPED: they were cross-layer confirmations of other agents' findings,
not actionable changes inside my ownership. Where a shell counterpart was
actionable it is listed above (dead APIs → item 5, source-text tests → item
24, stale comments → item 23, error-path cleanup → item 16).

## Other notes

- The checked-in `tools/watchdog/OpenTaskbarWatchdog.exe` is read-only and was
  not regenerated; `build.ps1` still rebuilds the architecture-specific PE
  into `OUT_DIR` for every Rust build. Its `-Force` argument keeps the
  script's "up-to-date, skip" branch dormant for Cargo builds (intentional;
  it remains useful for direct CodeQL/manual invocations).
- `crates/opentaskbar-core/tests/i18n_parity.rs` / `check-locale-parity.ps1`
  still duplicate hard-coded key counts (1046/1373). No owned file can remove
  that third source without coordination with agent I / the core owner.
- App-side message-ID collision noted in the report (`WM_APP_MENU_CANDIDATE`
  vs `WM_APP_ICONS`) remains an app-crate follow-up and was not touched.
