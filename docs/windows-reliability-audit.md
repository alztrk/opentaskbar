# Windows reliability and UX verification matrix

> Audit plan, **not** a claim that these cases were reproduced or fixed. Run on real Windows 10/11 machines or VMs and keep native taskbar restoration available while testing.

## Code to review

All paths are Rust-only, from the workspace root:

- `crates/opentaskbar-app/src/platform/win32.rs` — monitor enumeration/selection, physical-pixel/DIP conversion, negative virtual-screen origins, missing-display fallback.
- `crates/opentaskbar-app/src/main.rs` — dock event loop, `WM_DISPLAYCHANGE`/work-area messages, auto-hide, shell state reconciliation.
- `crates/opentaskbar-shell/src/{taskbar,appbar,journal,session}.rs` — taskbar hide/restore, app-bar reservations, journal parsing, single shell owner.
- `crates/opentaskbar-app/src/platform/win32_ex.rs` — current-desktop check (failure currently shows windows).
- `crates/opentaskbar-app/src/dock/{layout,mod}.rs` — overflow, wheel scrolling, magnification, geometry.
- `crates/opentaskbar-shell/src/watchdog.rs`, `tools/watchdog/watchdog.c` — architecture-matched watchdog embedded via `build.rs` (x64 `0x8664`, x86 `0x014C`).

## P0: never leave the user without a taskbar

| ID | Scenario | Expected outcome |
|---|---|---|
| REC-01 | Force-terminate while native taskbar is hidden | Native taskbar recoverable on next startup; document any delay |
| REC-02 | Restart Explorer while replacement is active | No permanent overlap or both taskbars missing |
| REC-03 | Start twice / rapid exit and restart | One owner of taskbar replacement and app-bar reservations |
| REC-04 | Suspend, hibernate, lock/unlock, RDP reconnect | Dock returns without orphaned reservations or stuck hidden state |
| REC-05 | Update fails, elevation denied, executable locked | Existing EXE remains launchable; recovery still possible |
| REC-06 | Cold boot with startup enabled; Explorer absent, slow or recreates its taskbar | Dock usable, bounded retries, app-bar restored, taskbar not wrong |
| REC-07 | Move or replace the portable EXE, then sign in again | Startup points to the running EXE, never a stale copy |
| REC-08 | Explorer restarts without or with delayed shell notification | Reconciliation detects the new taskbar, checks process identity, no duplicate registration |
| REC-09 | Explorer creates/shows a secondary taskbar | It is hidden promptly and stays hidden |
| REC-10 | Journal missing, malformed, stale or unwritable | Recovery prefers a visible taskbar, reports failure, never discards uncertain state |

## P1: monitors, DPI, fullscreen

| ID | Scenario | Expected outcome |
|---|---|---|
| MON-01 | 1–3 monitors, mixed resolutions, portrait | One correctly placed dock per display, no duplicate reservations |
| MON-02 | Secondary left/above primary (negative coordinates) | Correct hit testing, placement and work-area bounds |
| MON-03 | 100/125/150/200% DPI mixed, primary changes | Physical and DIP coordinates agree; no scaling jump |
| MON-04 | Unplug/replug display dock, change primary | Old windows/app-bars disposed, new ones created |
| MON-05 | Disable the target monitor while running | Fallback to a connected monitor; Settings still opens |
| MON-06 | Native taskbar at top/left/right or auto-hide | Reservation does not assume a bottom strip |
| MON-07 | Fullscreen on one monitor, apps on another | Only the affected dock hides; Alt+Tab restores it |
| MON-08 | Borderless vs maximized, HDR/refresh changes | No permanent false fullscreen suppression |
| MON-09 | Move one app's windows across monitors | Current-monitor filtering picks the expected window |
| MON-10 | Virtual desktop / window moved between desktops | Current-desktop filter follows preference; COM failure is logged |
| MON-11 | Display scale or work area changes while dock is open | Repositions on `WM_DPICHANGED`/`WM_DISPLAYCHANGE` |
| MON-12 | Explorer restarts during monitor re-enumeration | No stale handles reused; dock recovers when a display returns |

## P1: interaction, accessibility, visual design

| ID | Scenario | Expected outcome |
|---|---|---|
| UX-01 | 0/10/30/60+ pinned or running items | No inaccessible icon; overflow keyboard-controllable |
| UX-02 | Wheel over icon, empty dock, widget, overflow | predictable behavior, no conflicting actions |
| UX-03 | Menu/thumbnail/flyout open with auto-hide on | Dock stays visible until all flyouts close |
| UX-04 | 1366x768, 1080p, 4K; 100–200% text scaling | No clipped rows, unreadable captions or offscreen dialogs |
| UX-05 | Keyboard only, screen reader, high contrast, reduced motion | Understandable focus order, labels and states |
| UX-06 | RTL language and long translations | No overlap or truncation of critical controls |
| UX-07 | First launch, settings reset, invalid JSON | User understands what changed and can recover |
| UX-08 | Compare screenshots across bundled themes | Consistent spacing, contrast, icon sizing, active/hover states |

## P1/P2: performance, security, delivery

- **PERF-01:** Record idle/active CPU, private bytes, handles, GDI objects, GPU usage and frame time at 10/30/60 windows, 1–3 displays and 1h/8h uptime. No Rust baseline script exists yet — capture a fresh baseline with Windows tools and record OS build, EXE architecture, topology and widget set.
- **PERF-02:** Profile window tracking, thumbnails, image decoding, widget timers and allocations before optimizing. Old managed-build memory figures do not apply to the Rust EXE.
- **SEC-01:** Theme ZIP traversal, duplicate entries, reserved Windows names, size limits, import/export symmetry.
- **SEC-02:** Update asset selection, SHA-256 verification, signing, atomic replacement, backup/rollback.
- **SEC-03:** Shell command arguments, special-character paths, log privacy, permission boundaries.
- **REL-01:** Reproducible release, checksums, pinned Actions, Windows test reporting. `check-release-version.ps1` gates tag ↔ Cargo `0.4.0` ↔ MSIX `0.4.0.0`; `package-rust-release.ps1` records watchdog architecture/PE machine/SHA-256 and verifies `checksums.txt` before SLSA attestation.
- **ARCH-01:** Reduce coupling between GPUI dock, Settings and the shell taskbar session in small tested changes.

## Cross-cutting regression controls

Recurring failures (issues #79, #85, #86, #95, #126–128, #131, #152) fall into three classes:

1. **External lifecycle races:** Explorer, taskbar windows, display enumeration and startup registration change independently. Treat missing or delayed shell objects as normal; revalidate handles at use; make reconciliation idempotent; bound retries and report exhaustion.
2. **Layout feedback loops:** width, scrolling, magnification and placement trigger one another. Keep one geometry source of truth, avoid changing reservations during transient drag/slider updates, and test threshold boundaries with deterministic inputs.
3. **Simulation vs Windows:** logic tests prove transitions, not Explorer timing or real AppBar behavior. Keep them fast and record live results separately with OS build, architecture, topology, DPI, steps and logs.

A passing source-level check is not live compatibility evidence: a simulated `TaskbarCreated` does not prove Explorer-restart recovery, and a single-monitor run does not prove secondary-taskbar handling. Source-level recovery checks live in `crates/opentaskbar-core/tests/shell_recovery.rs`, `production_seams.rs` and `crates/opentaskbar-shell/src/journal.rs`.

## Compatibility evidence

Attach a compatibility record to each release review:

| Environment | Required evidence |
|---|---|
| Minimum supported Windows build, x64 | Startup, taskbar hide/restore, Explorer restart, updater smoke |
| Current Windows release, x64 | Same shell checks plus Start/Search anchoring and window previews |
| Current Windows release, x86 if shipped | Win32 handle checks and watchdog recovery |
| Two displays, mixed DPI, negative origin | Taskbar visibility, placement, disconnect/reconnect |
| Insider/preview build, when available | Exploratory AppBar check; record failures separately |

Record OS build and app commit. Mark unrun cases **not run**, never passed from simulation or static review.

For each case record: OS build, topology, DPI, target monitor, taskbar edge, steps, expected/actual result, screenshot/video, logs and measurements. Implement P0 fixes before broad UI refactors.

## Rust port: source checks vs live evidence

| Risk class | Source-level check | Live evidence still required |
|---|---|---|
| Lifecycle races | `shell_recovery.rs` — journal parsing, retry backoff, watchdog args, modifier release, 3-attempt restore | Explorer restart, `TaskbarCreated`, watchdog heartbeat, flyout repositioning |
| Layout loops | `dock_layout_extra.rs` — spring, falloff, overlap guard, gap/height units, group order | Vertical dock, DPI changes, multi-monitor edges, fullscreen overlays |
| Simulation gaps | `full_backup.rs`, `wireless_battery_vendors.rs`, `media_timeline.rs`, `audio_mixer.rs`, `ai_usage_logic.rs`, `i18n_parity.rs`, `telemetry_parity.rs` | HID reports, WinRT radio toggles, CoreAudio, LibreWolf UIA, OAuth/quota refreshes |

## Rust live smoke list

Run every row on a real Windows session before release sign-off. Record OS build, commit, EXE architecture, topology, DPI, result and logs; **not run** is the only valid initial status. Logs: `%LOCALAPPDATA%\OpenTaskbar\opentaskbar.log` (`opentaskbar.log.old`); journals from the same folder.

| # | Case | Steps | Pass condition | Status |
|---|---|---|---|---|
| S1 | Start/flyout anchoring | Open/close Start 10×, then Search/Control Center; restart Explorer mid-sequence | One generation per open; no stale close; correct anchor on both monitors | not run |
| S2 | Auto-hide and overlays | Hover dock edge, Task View/Alt-Tab, fullscreen app, switch back | Reveals on entry; overlays never stick the dock hidden/visible | not run |
| S3 | Watchdog, clean exit (x64 + x86) | Force-kill each EXE, then close normally; confirm watchdog PE matches architecture | Restores once on kill, never on clean exit, one heartbeat, no WOW64 fallback in x86 package | not run |
| S4 | Radio toggles | Toggle Wi-Fi/Bluetooth with airplane mode on and off | UI matches OS, no UI-thread stall | not run |
| S5 | Multi-monitor, mirrored dock | Move dock, disconnect/reconnect secondary, set negative origin | Live topology seen; no orphan dock; mirror follows `target_display` | not run |
| S6 | Night Light and media | Toggle Night Light; play/pause/seek track and LibreWolf tab | Registry payload decoded; stale samples rejected; seek clamped | not run |
| S7 | Full backup | Export, inspect, import on temp profile; edit manifest `Version` to `2`; craft `../` and `.exe` entries | v1 round-trips; other versions rejected; traversal/`.exe` never leaves staging | not run |
| S8 | Updater, single instance | Run portable updater on Unicode path containing `%`; launch second dock | Path intact, no BOM, atomic replace; second instance exits via named mutex | not run |

Reliability flags and their defaults are in `README.md` (`enable_live_tray_host=false`, `native_flyout_reposition=true`). Release artifacts use generated `checksums.txt`, a schemaVersion 2 `release-manifest.json`, `actions/attest` SLSA provenance and an optional VirusTotal scan.
