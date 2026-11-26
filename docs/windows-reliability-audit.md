# Windows reliability and UX verification matrix

> Status: audit plan, **not** a claim that these cases have been reproduced or fixed. Run on real Windows 10/11 machines or VMs. Keep native taskbar restoration available during testing.

## Source-grounded review targets

- `Services/Interop/DisplayService.cs`: monitor enumeration, target selection, DPI conversion of screen coordinates, fallback when a selected display disappears.
- `Views/Windows/SecondaryDockWindow.cs`: secondary monitor app bars, bottom-edge work-area reservation, foreground fullscreen suppression, grouping windows by executable.
- `Views/Windows/MainWindow.xaml.cs`: `WM_DISPLAYCHANGE` and `SPI_SETWORKAREA` trigger secondary dock updates.
- `Services/Interop/TaskbarReplacementSession.cs`: display/DPI messages, native taskbar hide/restore, crash-recovery journal and app-bar reservations.
- `Services/Interop/VirtualDesktops.cs`: current-desktop COM check; failure currently falls back to showing windows.
- `Views/DockScrollViewer.cs`: horizontal overflow, hidden scrollbar, wheel scrolling and synchronous `UpdateLayout()` during content measurement.
- `Services/App/DockAutoHideController.cs`: reveal/hide guards, flyout and foreground transitions.

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
| MON-03 | Mixed 100%, 125%, 150%, 200% DPI; move primary target | Physical pixel and WPF DIP coordinates agree; no offset or scaling jump |
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
  - Partial baseline captured on 2026-11-22 (Windows 11 build 26300, 16 cores, single 2560x1440 display, x64 self-contained publish, user's full widget set): steady-state working set 60–120 MB after idle trimming, private bytes 180–190 MB, ~31–35 threads, ~1300 handles. Idle CPU ~22% of one core, fisheye sweep ~36% of one core. Short private-byte spikes to ~275 MB occur on periodic widget/CLI polling and settle after GC. `scripts/measure-baseline.ps1 -Label <name>` produces the raw CSV and `summary.json` for A/B comparisons; `scripts/compare-perf.ps1` diffs two runs. Still open: 10/30/60-window scaling, multi-display, 1h/8h uptime, GDI objects and GPU usage.
- PERF-02: Profile `UpdateLayout()`, window tracking refreshes, thumbnail previews, image decoding, widget timers and allocations; optimize only after measurement.
  - Existing icon/flag/album-art decode caps and the bounded `RunningAppIconCache` already cover the obvious allocation sources; no single dominant leak was found in the idle/sweep baseline. The ~180 MB private footprint is mostly .NET/WPF runtime and widget visuals, not an app-level cache. Any further reduction (trimming unused assemblies, compacting GC) needs an A/B measurement before adoption.
- SEC-01: Theme ZIP traversal, duplicate entries, reserved Windows names, size limits and import/export symmetry.
- SEC-02: Update executable asset selection, SHA-256 verification, signing policy, atomic replacement, backup and rollback; coordinate with separate updater PR.
- SEC-03: Review shell command arguments, paths with special characters, log privacy and permission boundaries.
- REL-01: Release pipeline reproducibility, artifact checksums, GitHub Actions pinning and Windows test reporting.
- ARCH-01: Reduce coupling in `MainWindow`, `SettingsWindow`, `TaskbarReplacementSession` through small independently tested changes.

## Cross-cutting regression controls

The recurring failures in issues #79, #85, #86, #95, #126, #127, #128, #131 and #152 fall into three related classes:

1. **External lifecycle races:** Explorer, its taskbar windows, display enumeration, and startup registration change independently. Treat missing, replaced, delayed, or unresponsive shell objects as normal states. Revalidate window handles and process identity at the point of use; make reconciliation idempotent; bound retries and expose exhaustion in diagnostics.
2. **Feedback loops in layout:** measured width, scrolling, magnification, and window placement can trigger one another. Keep one source of truth for geometry, avoid changing native reservations during transient drag/slider updates, and test threshold boundaries (including a one-pixel overflow) with deterministic inputs.
3. **Validation gaps between simulation and Windows:** logic tests can prove transitions and calculations but cannot prove Explorer timing, shell broadcasts, actual AppBar behavior, or multi-monitor placement. Keep simulated checks fast, and separately record live Windows results with OS build, process architecture, display topology, DPI, exact steps, and logs.

Do not infer live compatibility from a passing source-level check. In particular, a simulated `TaskbarCreated` event does not prove Explorer restart recovery, and a single-monitor run does not prove secondary-taskbar handling. The current source-level recovery checks live in `tests/OpenTaskbar.Tests/ShellRecoveryChecks.cs`, `TaskbarRecoveryChecks.cs`, and `TaskbarWindowChecks.cs`; keep them aligned with the state machine and preserve separate live checks for the shell boundary.

## Windows compatibility evidence

For every release candidate, attach a small compatibility record to the release review:

| Environment | Required evidence |
|---|---|
| Minimum supported Windows build, x64 | Startup, taskbar hide/restore, Explorer restart, and updater smoke checks |
| Current supported Windows release, x64 | The same shell checks plus Start/Search anchoring and window previews |
| Current supported Windows release, x86 if shipped | Native handle/P/Invoke checks and watchdog recovery |
| Two displays with mixed DPI and a negative virtual-screen coordinate | Primary and secondary taskbar visibility, placement, disconnect/reconnect recovery |
| Insider/preview Windows build, when available | Exploratory taskbar/AppBar check; record failures separately from supported-release claims |

Record the exact OS build and app commit for each run. If a live test cannot be performed, mark it **not run**; do not mark it passed based on a simulation or static review. Windows shell behavior is outside this application’s release control, so long-term support requires a recurring compatibility check rather than an assumption that one tested build covers future Windows updates.

## Exit criteria and evidence

For each case record: OS build, display topology, DPI, target monitor, taskbar edge, exact steps, expected/actual result, screenshot/video, logs, CPU/memory/handle measurements when applicable, and regression test reference. Do not mark any case passing based on static review alone. Implement P0 fixes before broad UI refactors; split fixes into narrowly scoped PRs.

## Rust port parity controls

The Rust port applies the same three regression classes above through its own deterministic test files under `Rust-Port/crates/opentaskbar-core/tests/`:

| Risk class | Rust source-level check | Live Windows evidence still required |
|---|---|---|
| External lifecycle races (Explorer, shell broadcasts, taskbar journal) | `shell_recovery.rs` — journal line parsing, retry backoff, watchdog args, `finally` modifier release, 3-attempt restore verification | Explorer restart recovery, `TaskbarCreated` reconcile, watchdog heartbeat, native flyout repositioning |
| Layout feedback loops (magnification, gap/height units, reorder hysteresis) | `dock_layout_extra.rs` — critically damped spring, raised-cosine falloff, overlap guard, gap/height resolution, pinned-group ordering | Vertical dock hit-testing, DPI changes, multi-monitor edges, fullscreen overlay filtering |
| Validation gaps between simulation and Windows | `full_backup.rs`, `wireless_battery_vendors.rs`, `media_timeline.rs`, `audio_mixer.rs`, `ai_usage_logic.rs`, `opentaskbar_core_tests.rs`, `i18n_parity.rs`, `telemetry_parity.rs` | Real HID reports, WinRT radio toggles, CoreAudio session matching, LibreWolf UIA, provider OAuth and quota refreshes |

The same caveat applies: a passing Rust source-level check is **not** live compatibility evidence. Keep the "Windows compatibility evidence" table above for both builds, and record the Rust worktree commit and OS build separately.

Reliability-related flags and their safe defaults are documented in `Rust-Port/README.md`. In particular `enable_live_tray_host=false` and `native_flyout_reposition=false` keep the native shell untouched until a live Windows run has validated the replacement path. Release-level reproducibility is enforced by `checksums.txt`, `release-manifest.json`, `actions/attest` SLSA provenance and the optional VirusTotal step in `.github/workflows/release.yml`, which now covers the Rust artifacts as well as the C# ones.
