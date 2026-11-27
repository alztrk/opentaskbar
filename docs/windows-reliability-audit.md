# Windows reliability and UX verification matrix

> Status: audit plan, **not** a claim that these cases have been reproduced or fixed. Run on real Windows 10/11 machines or VMs. Keep native taskbar restoration available during testing.

## Source-grounded review targets

All implementation paths are Rust-only; review them from the repository/workspace root.

- `crates/opentaskbar-app/src/platform/win32.rs`: `enumerate_monitors`, monitor selection, physical-pixel/DIP conversion, negative virtual-screen origins and fallback when a selected display disappears.
- `crates/opentaskbar-app/src/main.rs`: startup modes (`--settings`, `--restore`, `--extract-icons`, `--update*`, default dock), single-instance lock, journal repair before launch, watchdog arming and panic-triggered restore.
- `crates/opentaskbar-shell/src/taskbar.rs` and `appbar.rs`: native taskbar hide/restore, crash-recovery journal and app-bar reservations.
- `crates/opentaskbar-shell/src/journal.rs` and `session.rs`: journal line parsing, recovery retries and single-owner shell session.
- `crates/opentaskbar-app/src/platform/win32_ex.rs`: current-desktop check; failure currently falls back to showing windows.
- `crates/opentaskbar-app/src/dock/layout.rs` and `dock/mod.rs`: horizontal overflow, wheel scrolling, magnification and geometry measurement.
- `crates/opentaskbar-shell/src/watchdog.rs` and `tools/watchdog/watchdog.c`: architecture-matched native watchdog embedded via `build.rs` (x64 `0x8664`, x86 `0x014C`).

## P0: do not leave the user without a taskbar

| ID | Scenario | Expected outcome |
|---|---|---|
| REC-01 | Force-terminate OpenTaskbar while native taskbar is hidden | Native taskbar is recoverable on next startup/recovery path; document any unavoidable delay |
| REC-02 | Restart Explorer while replacement is active | Native and replacement taskbars do not permanently overlap or both disappear |
| REC-03 | Start twice / rapidly exit and restart | Only one owner of taskbar replacement and app-bar reservations |
| REC-04 | Suspend, hibernate, lock/unlock, RDP reconnect | Dock returns without orphaned reservations or permanent hidden state |
| REC-05 | Update attempt fails, elevation denied, executable locked | Existing executable remains launchable; taskbar recovery remains possible |
| REC-06 | Cold boot with Windows startup enabled; Explorer is absent, slow, or recreates its taskbar after OpenTaskbar starts | Dock remains usable, retries are bounded, AppBar registration is restored, and the native taskbar does not remain visible or hidden incorrectly |
| REC-07 | Move or replace the portable executable, then sign in again | Startup registration points to the running supported executable; a stale path never silently starts an older copy |
| REC-08 | Explorer restarts while replacement is active and delays or omits a shell notification | Reconciliation detects the new taskbar window, validates its process identity, and restores the reservation without duplicate registration |
| REC-09 | Explorer creates or shows a secondary taskbar after the initial hide | The secondary taskbar is hidden promptly and remains hidden after delayed creation |
| REC-10 | Recovery journal is missing, malformed, stale, or cannot be written | Recovery prefers a visible native taskbar and reports the failed operation; it never silently discards uncertain recovery state |

## P1: monitor topology, DPI and fullscreen

| ID | Scenario | Expected outcome |
|---|---|---|
| MON-01 | 1, 2 and 3 monitors, mixed resolutions and portrait orientation | One correctly positioned dock per enabled display; no duplicate reservations |
| MON-02 | Secondary monitor left/above primary (negative virtual-screen coordinates) | Correct hit testing, placement, and work-area bounds |
| MON-03 | Mixed 100%, 125%, 150%, 200% DPI; move primary target | Physical pixel and device-independent layout coordinates agree; no offset or scaling jump |
| MON-04 | Unplug/replug HDMI/DP/USB-C dock; change primary monitor | Old secondary windows/app-bars disposed; new ones created on valid handles |
| MON-05 | Disable selected target monitor while app runs | Fallback to a connected monitor; user can still open Settings |
| MON-06 | Explorer taskbar at top/left/right or auto-hide | Reservation does not assume the bottom strip is always available |
| MON-07 | Fullscreen game/video on one monitor, normal apps on another | Suppress only the affected monitor's dock; restore on Alt+Tab |
| MON-08 | Borderless fullscreen vs maximized window; HDR and refresh-rate changes | No incorrect permanent fullscreen suppression |
| MON-09 | Move one app's windows across monitors | Current-monitor filtering and click/activation choose the expected window |
| MON-10 | Change virtual desktop or move a window between desktops | Current-desktop filter follows preference; COM failure is visible in diagnostics |
| MON-11 | Change Windows display scale or work area while dock is open | Correct repositioning after `WM_DPICHANGED`, `WM_DISPLAYCHANGE` and work-area notifications |
| MON-12 | Explorer restarts while a secondary monitor is disconnected or being re-enumerated | No stale monitor or window handles are reused; the dock recovers when a valid display returns |

## P1: interaction, accessibility and visual design

| ID | Scenario | Expected outcome |
|---|---|---|
| UX-01 | 0, 10, 30, 60+ running/pinned items | No inaccessible icon; overflow discoverable and controllable by keyboard |
| UX-02 | Wheel over app icon, empty dock, widget and overflow area | Predictable behavior; scrolling and volume/window actions do not conflict |
| UX-03 | Open context menu, thumbnail, widget flyout with auto-hide enabled | Dock remains visible while interacting, hides after all flyouts close |
| UX-04 | 1366x768, 1080p, 4K; 100%-200% text scaling | No clipped Settings rows, unreadable captions or offscreen dialogs |
| UX-05 | Keyboard only, screen reader, high contrast, reduced motion | Focus order, labels and control states remain understandable |
| UX-06 | RTL language and long translations | No overlap/truncation of critical controls; localization fallback is intentional |
| UX-07 | First launch, settings reset, invalid settings JSON | User understands what changed and can recover preferences |
| UX-08 | Compare dock and Settings screenshots across bundled themes | Consistent spacing, contrast, icon sizing, active/hover indicators |

## P1/P2: performance, security, delivery

- PERF-01: Record idle and active CPU, private bytes, handles, GDI objects, GPU usage, UI frame time at 10/30/60 windows, 1/2/3 displays and 1h/8h uptime.
  - The retired reference baseline workflow (`scripts/measure-baseline.ps1`) and its numbers no longer apply to the Rust binary. There is no Rust benchmark/baseline script yet; capture a fresh baseline with Windows tools on a real session and record OS build, EXE architecture, display topology and widget set with each run. Still open: every Rust baseline point above.
- PERF-02: Profile window tracking refreshes, thumbnail previews, image decoding, widget timers and allocations; optimize only after measurement.
  - Existing icon/flag/album-art decode caps and the bounded icon cache cover the obvious allocation sources. The historical ~180 MB private-byte figure came from the retired managed build and must not be attributed to the Rust binary; measure before drawing conclusions or adopting further changes.
- SEC-01: Theme ZIP traversal, duplicate entries, reserved Windows names, size limits and import/export symmetry.
- SEC-02: Update executable asset selection, SHA-256 verification, signing policy, atomic replacement, backup and rollback; coordinate with separate updater PR.
- SEC-03: Review shell command arguments, paths with special characters, log privacy and permission boundaries.
- REL-01: Release pipeline reproducibility, artifact checksums, GitHub Actions pinning and Windows test reporting. The release gate `scripts/check-release-version.ps1` checks tag ↔ Cargo workspace `0.4.0` ↔ Rust MSIX `0.4.0.0` before any build; `package-rust-release.ps1` records each artifact's watchdog architecture/PE machine/SHA-256 and verifies every `checksums.txt` row against disk before SLSA attestation.
- ARCH-01: Reduce coupling between the GPUI dock, on-demand Settings UI and shell taskbar session in `crates` through small independently tested changes.

## Cross-cutting regression controls

The recurring failures in issues #79, #85, #86, #95, #126, #127, #128, #131 and #152 fall into three related classes:

1. **External lifecycle races:** Explorer, its taskbar windows, display enumeration, and startup registration change independently. Treat missing, replaced, delayed, or unresponsive shell objects as normal states. Revalidate window handles and process identity at the point of use; make reconciliation idempotent; bound retries and expose exhaustion in diagnostics.
2. **Feedback loops in layout:** measured width, scrolling, magnification, and window placement can trigger one another. Keep one source of truth for geometry, avoid changing native reservations during transient drag/slider updates, and test threshold boundaries (including a one-pixel overflow) with deterministic inputs.
3. **Validation gaps between simulation and Windows:** logic tests can prove transitions and calculations but cannot prove Explorer timing, shell broadcasts, actual AppBar behavior, or multi-monitor placement. Keep simulated checks fast, and separately record live Windows results with OS build, process architecture, display topology, DPI, exact steps, and logs.

Do not infer live compatibility from a passing source-level check. In particular, a simulated `TaskbarCreated` event does not prove Explorer restart recovery, and a single-monitor run does not prove secondary-taskbar handling. The current source-level recovery checks live in `crates/opentaskbar-core/tests/shell_recovery.rs`, `production_seams.rs` and `crates/opentaskbar-shell/src/journal.rs`; keep them aligned with the state machine and preserve separate live checks for the shell boundary.

## Windows compatibility evidence

For every release candidate, attach a small compatibility record to the release review:

| Environment | Required evidence |
|---|---|
| Minimum supported Windows build, x64 | Startup, taskbar hide/restore, Explorer restart, and updater smoke checks |
| Current supported Windows release, x64 | The same shell checks plus Start/Search anchoring and window previews |
| Current supported Windows release, x86 if shipped | Native Win32 handle checks and watchdog recovery |
| Two displays with mixed DPI and a negative virtual-screen coordinate | Primary and secondary taskbar visibility, placement, disconnect/reconnect recovery |
| Insider/preview Windows build, when available | Exploratory taskbar/AppBar check; record failures separately from supported-release claims |

Record the exact OS build and app commit for each run. If a live test cannot be performed, mark it **not run**; do not mark it passed based on a simulation or static review. Windows shell behavior is outside this application’s release control, so long-term support requires a recurring compatibility check rather than an assumption that one tested build covers future Windows updates.

## Exit criteria and evidence

For each case record: OS build, display topology, DPI, target monitor, taskbar edge, exact steps, expected/actual result, screenshot/video, logs, CPU/memory/handle measurements when applicable, and regression test reference. Do not mark any case passing based on static review alone. Implement P0 fixes before broad UI refactors; split fixes into narrowly scoped PRs.

Evidence record template (copy per case into the release review):

```text
Case:            S3 (watchdog and clean exit)
Commit:          <short sha>
OS build:        <e.g. Windows 11 10.0.26100.xxxx>
EXE arch:        x64 | x86
Displays / DPI:  <count, layout, per-display scale>
Steps:           <exact actions, including CLI flags used>
Expected:        <pass condition from the table>
Actual:          pass | fail | not run
Artifacts:       <screenshot/video path, %LOCALAPPDATA%\OpenTaskbar\opentaskbar.log excerpt>
Measurements:    <CPU / private bytes / handles if applicable>
Automated link:  <test file or issue reference>
```

The same template applies to the Rust smoke list below; append the watchdog PE machine (`0x8664` / `0x014C`) for S3.

## Rust implementation parity controls

The Rust workspace applies the same three regression classes above through deterministic test files under `crates/opentaskbar-core/tests/`:

| Risk class | Rust source-level check | Live Windows evidence still required |
|---|---|---|
| External lifecycle races (Explorer, shell broadcasts, taskbar journal) | `shell_recovery.rs` — journal line parsing, retry backoff, watchdog args, `finally` modifier release, 3-attempt restore verification | Explorer restart recovery, `TaskbarCreated` reconcile, watchdog heartbeat, native flyout repositioning |
| Layout feedback loops (magnification, gap/height units, reorder hysteresis) | `dock_layout_extra.rs` — critically damped spring, raised-cosine falloff, overlap guard, gap/height resolution, pinned-group ordering | Vertical dock hit-testing, DPI changes, multi-monitor edges, fullscreen overlay filtering |
| Validation gaps between simulation and Windows | `full_backup.rs`, `wireless_battery_vendors.rs`, `media_timeline.rs`, `audio_mixer.rs`, `ai_usage_logic.rs`, `opentaskbar_core_tests.rs`, `i18n_parity.rs`, `telemetry_parity.rs` | Real HID reports, WinRT radio toggles, CoreAudio session matching, LibreWolf UIA, provider OAuth and quota refreshes |

The same caveat applies: a passing Rust source-level check is **not** live compatibility evidence. Keep the "Windows compatibility evidence" table above for the Rust build, and record the commit and OS build for each run.

### Rust live smoke list

Run every row on a real Windows session before signing off a release candidate. Record OS build, commit, EXE architecture, display topology, DPI, actual result and logs; **not run** is the only valid initial status. All diagnostics and journals live in the single shared data folder: `%LOCALAPPDATA%\OpenTaskbar\opentaskbar.log` (rotated copy `opentaskbar.log.old`), `taskbar-state.txt` and `flyout-positions.txt`. There is no second data folder.

| # | Smoke case | Steps | Pass condition | Status |
| :--- | :--- | :--- | :--- | :--- |
| S1 | Start/flyout anchoring | Open and close Start 10× in quick succession, then Search and Control Center; restart Explorer mid-sequence. | One generation per open; no stale close cancels a newer open; flyout follows the anchor on both monitors. | not run |
| S2 | Auto-hide and shell overlays | Hover the dock edge, open Task View/Alt-Tab, enter a fullscreen app, switch back. | Dock reveals on pointer entry; overlays never leave the dock hidden or stuck visible. | not run |
| S3 | Watchdog and clean exit (x64 + x86) | For each architecture, kill `OpenTaskBar.exe` with Task Manager (`taskkill /F`), then close it normally. Confirm the embedded PE and the app-directory `OpenTaskbarWatchdog.exe` match the EXE architecture (x86 = 0x014C, x64 = 0x8664). | Watchdog restores the shell once on kill; no restore after a clean exit; one process and one heartbeat event throughout; no WOW64 fallback binary in the x86 package. | not run |
| S4 | Radio toggles | Toggle Wi-Fi and Bluetooth from Quick Settings with airplane mode on and off. | UI state matches the OS after each click; no UI-thread stall or crash from async completion. | not run |
| S5 | Multi-monitor and mirrored dock | Move the dock between displays, disconnect/reconnect the secondary display, set a negative virtual-screen origin. | `enumerate_monitors` sees the live topology; no orphan dock, mirrored dock follows `target_display`. | not run |
| S6 | Night Light and media | Toggle Night Light; play/pause/seek a track and a LibreWolf YouTube tab. | Registry payload decodes to the new state; stale samples are rejected; seek and rate changes stay clamped. | not run |
| S7 | Full backup | Export, inspect, then import on a temp profile; edit the manifest `Version` to `2` and retry; craft `../` and `.exe` theme entries. | Version 1 round-trips settings/pins/theme; other versions are rejected; traversal/`.exe` entries never leave staging. | not run |
| S8 | Updater and single instance | Run the portable updater with a Unicode path containing `%`; launch a second dock process. | Script keeps the path intact, has no BOM, and replaces atomically; second instance exits via the named mutex. | not run |

Reliability-related flags and their real defaults are documented in `README.md`. `enable_live_tray_host=false` keeps the native tray untouched; `native_flyout_reposition=true` is now the default because Start/Search/Control Center repositioning is generation-cancelled and journal-backed — set it to `false` to fall back to journal-read-only positioning if a live regression appears. Release-level reproducibility is enforced by generated `checksums.txt` (no stale rows are tracked in the repository), a schemaVersion 2 `release-manifest.json`, `actions/attest` SLSA provenance and the optional VirusTotal step in `.github/workflows/release.yml`, all covering the Rust-only artifacts.
