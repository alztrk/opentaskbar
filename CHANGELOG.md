# Changelog

## Unreleased

### Rust port — test/CI/docs/packaging parity

- Removed five retired AI-provider grouping keys and added 27 window-picker, magnification, Fjord, Wi-Fi, search, alignment, preview, focus, density and AI-ring keys (24 newly added; three already shipped in every catalog) to all 14 Rust catalogs. The C# catalogs now hold **994 keys** and every Rust catalog and numeric gate sits at **1406 keys** (40 Fjord/Wi-Fi/preview labels added for the ported UI).
- Added 32 new feature keys to all 14 Rust catalogs and enforced identical key sets with `scripts/check-locale-parity.ps1` and `tests/i18n_parity.rs`.
- Merged 202 new UI localization keys (Dynamic Island details, Settings/theme controls and widget labels) with the five widget/start-layout keys from the other branch, all with real translations in 14 Rust catalogs, raising every catalog from 1129 to **1336 keys**, and raised the numeric parity gates to match.
- Restored the nine tr-TR labels that the merge overwrote with en-US or ASCII-only text (`FocusBreakTitle/Start/Skip/Countdown/Complete`, `FocusSession`, `RadioOn`, `RadioOff`, `DockBottomGap`) and added parity tests for translation regressions, empty values and placeholder retention. The key count stayed **1336** at that point in all 14 catalogs and every gate.
- Replaced the remaining user-visible English literals with locale keys: update toasts (`UpdateAvailableTitle`/`UpdateToastBody`/`UpdateDownload`), Start placement labels (`PlacementStart`/`PlacementEnd`/`PlacementHidden` plus the new `PlacementInWidgets`), the Jump List fallback (`OpenLocation`) and the unknown GPU counter label (`GpuEngine`). Widget names/categories now resolve through existing `WidgetCategory*`/widget-name keys. The three new keys are translated in all 14 catalogs and raise every catalog and numeric gate to **1339 keys**; the en-US `Idle`/`Cancelled`/`Expired` fallback rule is unchanged.
- Made the Weather country/city pickers searchable (case-insensitive, Turkish `ı/İ`-aware filtering, arrow/Enter/Escape navigation and an empty-result message) and persisted `"City, Country"` immediately on selection. Added `platform::country_city::geolocate()` (Windows capability consent + WinRT permission, coordinate reverse geocoding with offline-catalog fallback) which silently auto-fills an empty saved location on Settings open and logs the result. Three new locale keys (`DropdownSearchPlaceholder`, `DropdownNoResults`, `WeatherLocationUsed`) raise every catalog and every numeric gate to **1342 keys**.
- Ported the deterministic C# checks into Rust integration tests: settings key parity, AI usage/JWT/quota/groups, `.otbackup` ZIP round-trip, wireless battery vendors, media timeline/seek, shell recovery, dock layout, audio mixer and locale completeness.
- Linked the reference checks to production code and core APIs, removed self-validating assertions, and added production-seam tests for radio, Night Light, watchdog, monitors, auto-hide, updater bytes and the single-instance mutex (cross-WS APIs that are not landed yet are marked `#[ignore]` with a note).
- Rust CI now runs on every branch and also on C# locale changes; CodeQL analyses Rust on every branch/PR (C# stays master-only); Dependabot tracks Cargo.
- Added Rust portable/MSIX packaging scripts and merged Rust artifacts into checksums, release manifest, SLSA attestation and optional VirusTotal scanning.
- Expanded `Rust-Port/README.md` and updated localization/theme/reliability/privacy docs for the Rust port.

- Placed Rust Background Apps pins in a compact strip beside the Background Apps button, with overflow available in the flyout. Added a short dock closing animation that respects Windows and OpenTaskbar animation settings.

- Fixed the Rust Background Apps list opening hidden utility windows or launching helper executables. Opening from either the row or its menu now restores an app window when possible and uses the dock's launch resolver otherwise.

- Fixed Rust dock launches for Microsoft Store apps and desktop apps tracked through helper processes, such as Steam. Steam game shortcuts can also be dropped onto the dock and opened as Windows Internet shortcuts.

- Redesigned the Rust port's Settings and Background Apps panels with coordinated light and dark themes, responsive Settings navigation, and inline dock pin and window close controls.

- Read playback progress from the matching LibreWolf tab when Windows omits the timeline and the browser exposes a verifiable player position and duration. Show an unavailable-duration status when neither source has a usable timeline.

- Added transition and error states to Wi-Fi and Bluetooth controls, waited for Windows to confirm radio changes (#161), and placed direct Night Light control and Windows quick settings shortcuts side by side in the dock.

## 0.3.3 - 2026-11-22

- Added pinning of selected background tray icons directly onto the dock, with stable identities across restarts (#114).
- Completed compact density mode support across dock widgets and live Settings previews (#115), and removed compact-dock widget blur with adaptive WidgetHeight and crisp text rendering (#116).
- Added a clean-room multi-protocol wireless and Bluetooth battery engine (SteelSeries, ATK/VXE, Logitech HID++, Razer, XInput) with charging and stale-reading indicators (#117).
- Added AI compact layouts and AiUsageRing filtering (#118).
- Added Settings account sign-in with DPAPI API key storage and GitHub CLI Copilot credential detection (#119).
- Added the Cursor quota widget with browser PKCE sign-in and compact bar layout (#120).
- Fixed the dock scrolling back to the start after a deferred resize triggered by scrolling itself (#122).
- Updated the release pipeline to build x64 and x86 artifacts, generate checksums and a release manifest, and publish signed build-provenance attestations.

## Unreleased

- Simplified the system metrics widget and its flyouts, and removed the duplicate Show Desktop widget in favor of the taskbar edge strip.

- Fixed the media widget timeline freezing or jumping when players repeat an unchanged position or omit its timestamp. The volume slider resolves window identities used by apps such as LibreWolf, prefers the active audio session, and disables itself when the match is ambiguous.

- Reveal the desktop when a file is dragged over the Show Desktop strip. Drop directly onto the strip to copy, or hold Shift to move (#148).
- Add an application pin command to the dock menu and preserve Store app identities and icons, including AppsFolder entries that lack a readable executable (#147).
- Update an existing Windows startup entry when a newer portable copy is opened from a different location or the registered executable no longer exists (#152).
- Keep app grouping separate from the live reordering preview when dragging an app over a group (#130).
- Stop the charging battery animation after a short pulse and import pinned apps off the UI thread at startup (#128).
- Avoid restarting icon magnification resets and replacing hover bitmaps while the icons settle (#125, #127).

- Fixed the Audio widget showing generic speaker and microphone labels on x86 when Windows provides the device names.
- Added runtime detection and measured dock anchoring for alternate Windows Search flyouts, with the existing Search host and native Win+S available as fallbacks.

- Enabled hardware rendering by default for fluid magnification and flyout animations. Added Settings → Behavior → Hardware acceleration (restart required) to switch back to software rendering, which measured ~100 MB lower private memory (81–86 MB vs 180–190 MB) at the cost of animation fluidity. `--software-render` remains available as a command-line override.
- Fixed `scripts/measure-baseline.ps1` failing to find the dock HWND because `ShowInTaskbar=false` leaves `Process.MainWindowHandle` at zero; it now enumerates the layered window instead.

- Added Aero Peek when hovering over window previews (respecting Windows' Enable Peek setting) and middle-click on preview cards to close windows.
- Added a one-click Focus Mode toggle in the dock/tray context menu and Preferences that hides notification badges and widgets, pauses widget polling, and disables animations without overwriting saved settings.
- Added combined `.otbackup` archives for exporting and importing settings, pinned apps, and the active custom theme, with backup validation and optional theme restoration.
- Added custom keyboard shortcut recording with conflict detection and per-shortcut reset for dock focus and launcher/search.
- Added live search in the Settings title bar to filter pages and jump directly to matching setting rows with brief highlighting.
- Split the icon spacing setting into independent horizontal (left+right) and vertical (top+bottom) padding controls.
- Added full dock profiles for switching, creating, renaming, and deleting profiles, with widgets, order, width, theme, hotkeys, and auto-hide saved per profile.
- Fixed the media widget volume slider changing the system/master output volume instead of the playing app's volume. The widget now resolves the WASAPI audio session for the current media session (via its AppUserModelId) and controls that app's per-app volume and mute. When no matching app session can be found, the volume control is disabled rather than falling back to the master endpoint, so the headset/system volume is never changed unexpectedly.
- Settings UI consistency pass: unified input metrics (34 DIP controls, one border/radius vocabulary), removed per-row padding overrides, replaced the auto-hide radio list and the side-by-side backup/theme-pack button rows with dropdowns, and gave the sidebar a deliberate header/nav hierarchy. Scrollbars are now thin rounded pills. Widget catalog hover paints only the outer card surface (inner preview no longer changes color or carries top corner radii).

## 0.3.0 - 2026-11-20

- Fixed the dock staying visible over fullscreen browser videos. Fullscreen is now re-evaluated from a 500 ms scan of all top-level windows on the dock's monitor, so resizing an already-foreground window (how browsers enter fullscreen) hides the dock. Focus popups such as Chromium's Esc bubble no longer bring it back, and secondary docks follow the same state. The scan only counts real app windows with a taskbar button, so monitor-sized shell surfaces (desktop, IME, GPU overlays) cannot hide the dock at startup. This also fixes the dock disappearing right after launch when `TextInputHost` (the Windows login/IME surface) was misdetected as a fullscreen app.

- The default palette now follows Windows Settings > Personalization > Colors: the Windows accent (from `AccentPalette`/`DWM\AccentColor`) is used only for interactive controls such as primary buttons, switches, focus rings and selection marks, while backgrounds and cards use neutral Windows Settings-like surfaces. Windows accent changes apply live; explicitly selected theme packs still override these generated colors. Static palettes remain as fallback.
- Rebuilt the Settings components with a shadcn/ui-inspired vocabulary: muted inset tracks, rounded-md controls, ghost/outline/primary button hierarchy, elevated thumb switches and sliders, a real focus-ring treatment, and card sections with rounded row hover.
- Redesigned the Settings page and its controls: grouped section cards with hairline dividers, shared inset tracks for segmented controls and sliders, filled accent states for switches and buttons, and a cleaner sidebar hierarchy.
- Replaced the system-dependent Segoe UI interface font with embedded Inter (Regular/Medium/SemiBold/Bold under `Fonts/`, SIL OFL license included). Segoe MDL2 Assets remains the icon font, so glyphs are unchanged.

- Fixed running indicators and notification badges disappearing from pinned apps. They now use the full window projection (including pinned apps) while the running-app strip still excludes pins to avoid duplicate buttons. Inactive running apps show the low-opacity dot again and the foreground app the bright one.

- Fixed the Start menu staying misaligned after OpenTaskbar exits. The saved "original" position could already be a displaced rect left by an earlier session; the full-screen Start canvas now records the work-area origin as its native position, so restore and watchdog put it back where Windows expects it. While the dock is active, the flyout is still anchored to the dock's actual Start button wherever the user placed it.

- Fixed the Windows Start menu opening on the left when the native taskbar is centered: anchored positioning now only follows the dock's Start button for a left-aligned taskbar, while a centered taskbar keeps the flyout centered on the work area (relevant for full-width docks where the Start button sits at the far left).

- Redesigned the Settings page and its controls: sections now group rows into quiet slate cards with hairline dividers, segmented controls and sliders share a consistent inset track, switches and buttons use filled accent states, and the sidebar/divider hierarchy is cleaner. The default palette shifts to a Tailwind-inspired slate ramp with a sky accent in both themes.
- Stabilized the fisheye peak so an identical hover no longer blooms to different sizes: the expansion budget is sampled only from resting geometry, spring goals are refreshed whenever the cap changes (even with a stationary pointer), and influence radius now interpolates with the same expansion fraction instead of switching abruptly at a 0.5 DIP threshold.

- Removed hover magnification (fisheye) from the native shell buttons (Start, Search, Task View, tray and Recycle Bin) and stopped neighbouring icons from magnifying while the pointer is over them. Application icons keep the fisheye effect.

- Fixed app icons looking clipped on the right and bottom (notably LibreWolf). The icon image is now centered in its dock cell and its drawn DIP size is capped to that cell, so snapped pixel sizes can no longer overflow one edge.

- Replaced the single Surface opacity slider with independent per-surface materials for the dock, menus, widgets and Settings. Each surface now has Glass / Solid / None with its own 0–100% opacity and live preview; None removes the fill, rim, sheen and shadow (and DWM acrylic for Settings, which stays opaque due to native window composition). Existing `SurfaceOpacity` values migrate to all four surfaces on load.

- Gave the permanent Search, Task View and tray buttons a quiet rounded surface with hover/press feedback via a dedicated `ShellDockButton` style, so they remain readable next to the deliberately transparent app icons and widgets without changing the normal app-button baseline.

- Removed the `--preview` launch mode. The dock now always runs in replacement mode with full journaling and watchdog protection.
- Fixed logoff teardown leaving the dock broken when another application vetoes the shutdown: restore is deferred to application exit, with the watchdog still armed in the meantime.
- Added a staleness check so a leftover taskbar journal cannot overwrite a taskbar state the user changed since, and made the managed watchdog require three consecutive missed heartbeats before treating the dock as hung.

- Hardened crash recovery: the watchdog now keeps the taskbar journal until the taskbar is verified visible, re-broadcasts `TaskbarCreated` so tray icons return to Explorer, masks corrupted state values, and preserves flyout journals whose window handles were recycled. Clean-exit signalling is delayed until both restore and journal deletion succeed.
- Fixed restore dropping the "always on top" taskbar bit when only auto-hide matched, and recovery declaring success while secondary taskbars were still hidden.
- Fixed pending Settings edits being lost on application shutdown, startup heal consuming a running instance's journal before the singleton check, and watchdog armament starting after the taskbar had already been hidden.
- Made watchdog binary extraction atomic and guarded the heartbeat callback against disposal races.

- Fixed Start, Search and Control Center staying anchored to the dock after OpenTaskbar exits. Their original positions are remembered before the first move, restored on normal shutdown, and journaled to `flyout-positions.txt` so the native watchdog (or managed recovery) can restore them after a hard kill; pending anchoring passes are cancelled during exit.
- Fixed tray-only and background apps appearing as normal running apps in the dock. Window tracking now follows the Windows shell's taskbar rules (owned opt-in, non-activatable helpers, `ITaskbarList::DeleteTab`, cloaked and hidden immersive frames) and reacts to cloak/uncloak changes. Running-app projection is now shared by the primary dock, secondary docks, badges and indicators, so hidden or missing apps stay hidden everywhere.
- Promoted Task View, Tray and Show Desktop to permanent dock shell buttons. Task View is a single icon with a right-click menu (previous/next/new desktop), Tray keeps its rich flyout in a fixed button, and Show Desktop is a slim trailing strip (Win+D). Each has a Settings toggle; legacy `Desktops`/`Tray` widget entries migrate into the new button state.
- Moved Search out of the widget strip into a dedicated Start-adjacent dock button (`MainWindow.xaml`, `MainWindow.xaml.cs`) sized to the dock cell with an optically matched glyph, and added a `ShowSearchButton` Settings toggle. Legacy `Search` widget entries in `settings.json` migrate to the new button state.

- Overhauled dock fisheye magnification (`Services/App/DockFisheyeLayout.cs`, `MainWindow.Magnification.cs`) with C1-smooth raised-cosine falloff, critically-damped spring physics, synchronized slot and floating dock-frame expansion, overlap-free scale capping on constrained/scrolling docks, `StartDockButton` baseline alignment, and dynamic thumbnail preview clearance.
- Added application groups (`MainWindow.Groups.cs`) with context-menu grouping, drag-and-drop membership, and group member count badges.
- Moved dock scroll chevrons (`‹` / `›`) and directional gradient scrims to the outer dock resize edges (`DockSurface`), inset cleanly inside `DockFrame` bounds and synced to the active inner corner radius.
- Added Bluetooth Battery widget with device-type icons and live Windows PnP active-node filtering (`System.Devices.Present` and `LastArrivalDate`) so stale disconnected PnP nodes never override current battery readings.
- Removed the manual Screen edge spacing setting in favor of automatic profile-based spacing from `DockMetrics.For(size, scale)`, while keeping legacy `settings.json` files backward-compatible.
- Updated Settings widget catalog to display inline category headings with live widget previews, and fixed widget/flyout hover highlight templates (`WidgetCardButton`, `WidgetPill`, `FlyoutButton`, `WidgetButton`) so hover fills the full outer button surface matching pressed state.
- Replaced managed watchdog runtime overhead with a lightweight native C Win32 watchdog (`tools/watchdog/watchdog.c`) that restores the native taskbar and exits immediately, with incremental `build.ps1` skipping rebuilds when `watchdog.c` is unchanged.
- Added graceful fallback taskbar dimensions when starting while Explorer's taskbar is already hidden, and preserved desktop work area when switching dock screen edges.
- Rebuilt the UI as a modular design system: tokens (`Design/Tokens/{Typography,Spacing,Radii,Layout,Motion}.xaml`), vector icons (`Design/Icons/{Navigation,Actions,Status,Media,Brand,GeometryTemplate}.xaml`) and base controls (`Design/Controls/{Tooltip,ScrollBar}.xaml`) now live in one file per concern and are merged in dependency order by `App.xaml`. Theme-pack overrides keep working by key.
- Removed the unused parallel `UI/UILibrary.xaml` design system and consolidated the shipped UI entirely under `Design/*`; unified the duplicated dock/widget context menus and de-duplicated shared icon geometries and hover-animation tokens.

## 0.2.11 - 2026-11-19

- Added GitHub Copilot, DeepSeek, and OpenRouter quota and balance providers to the AI usage widget, including credential discovery, status rings, and settings toggles (#58, #59, #60).
- Hosted notification area to open apps' native tray context menus from the Background apps flyout (#57).
- Improved dock responsiveness by moving Recycle Bin queries, network speed sampling, application launches, and icon extraction off the UI thread (#61).

## 0.2.8 - 2026-11-18

- Added an optional AI Usage widget for Claude Code and Codex, with read-only CLI sign-in, one quota ring per provider (session and weekly), a pace hint, reset countdowns, a selectable ring window, and translations in all 14 languages.
- Added an active dock-integrated Search capsule widget with embedded text input, live watermark, clear button, and direct two-way synchronization with Windows Search (`SearchHost.exe`).
- Forwarded `Enter`, `Up`, `Down`, and `Escape` keyboard navigation from dock search input directly to Windows Search.
- Positioned Windows Search flyout dynamically centered above the Search widget trigger button and suppressed rogue Explorer taskbar search boxes.
- Removed legacy built-in Spotlight popup and `Alt+Space` hotkey in favor of native Windows Search integration.
- Redesigned Audio widget with output and input device pickers and a comprehensive per-app volume mixer enumerating all active audio sessions across endpoints.
- Elevated all dock widget flyout popups with comfortable spacing above the dock.
- Removed runtime `HeapHardLimit` and extreme memory conservation to eliminate 0x80131506 crashes under heavy layout rendering.
- Added idle-triggered memory trimming and non-concurrent workstation GC to reduce RAM footprint.
- Disabled `PublishReadyToRun` to drastically reduce build times and single-file publish artifact sizes.
- Added right-click context menu with widget removal support directly on dock widget items.
- Fixed native taskbar recovery journaling, verified repeated state restore, and hardened recovery across crashes.
- Fixed application-specific jump list tasks (e.g. LibreWolf private window) launching their shell link target and arguments directly.
- Fixed Windows Settings opening after lock transitions with synthetic Win chord release guarantees.
- Fixed secondary dock shelf measurement in multi-monitor setups.
- Added `--headless` test mode for pure logic checks.

## 0.2.7 - 2026-11-17

- Fixed auto-hide lock when clicking the Clock/Date widget by auto-closing flyouts on cursor departure, preventing activation foreground theft, and eliminating counter desynchronization.
- Added toggle in theme settings to keep original application icons while preserving theme colors and widget styling.
- Balanced application icon artwork size (34 DIP) with widgets while retaining full-size click targets.
- Hardened in-place update installation with mandatory release asset SHA-256 digest verification.
- Isolated settings file writes with unique temporary files to prevent collision or corruption.
- Guarded weather forecast against truncated or mismatched daily provider arrays.
- Added multi-monitor display selection and secondary dock app and widget scoping.
- Added automated CI and release workflows, English contribution guidelines, and issue/PR templates.
- Added adaptive responsive grid layout to the Widgets Settings catalog, automatically scaling column count to available window width.
- Ensured immediate dock width recalculation and shrink when applications or widgets are closed or removed.
- Placed newly launched applications dynamically alongside running apps, preserving trailing widget placement on the dock.
- Added right-click removal support directly on dock widget items.
- Optimized dock fisheye magnification hit-testing with cached target coordinates.
- Optimized HTTP client lifetimes and theme icon image caching.
- Restored native Windows 11 notification center launching with automatic `ShellExperienceHost` recovery.
- Bundled six theme packs and added custom theme and icon pack support.
- Added culture-based language preferences and localized launcher/widget formatting.

## 0.2.6 - 2026-11-17

- Added automated in-place update downloader and restarter; updates download silently in the background and replace the executable without browser redirects.
- Replaced intrusive dock status messages with native Windows Action Center toast notifications for update alerts and system feedback.
- Added Task Manager launch command to the dock context menu with system path resolution and dedicated vector icon.
- Added full support for Microsoft Store and packaged application icons in running dock entries and `ApplicationFrameHost` instances.
- Fixed dock layout delay so closed applications immediately shrink the dock without leaving blank placeholder cells.

## 0.2.5 - 2026-11-17

- Added custom, theme-integrated `ComboBox` and `ComboBoxItem` styles matching OpenTaskbar's acrylic glass design tokens.
- Expanded Weather flyout with humidity, wind speed, today's high/low temperatures, and dynamic condition-based background gradients.
- Added a 3-day weather forecast with day titles, condition icons, localized status descriptions, and high/low temperatures.
- Widened Settings window default bounds (`980x680`) and content metrics (`SettingsReadingWidth` to 960 DIP, `SettingsPageInset` to 32,20,24,20) for optimal 3-column widget card wrapping.
- Refined vertical scrollbars with a slender 6 DIP footprint and minimal thumb margin.

## 0.2.4 - 2026-11-16

- Refactored and modernized project architecture: partitioned windows into `Views/Windows/`, popups into `Views/Popups/`, and system services into `Services/Interop/`, `Services/Settings/`, and `Services/App/`.
- Decomposed `MainWindow` into focused partial classes for magnification, drag-and-drop, and resizing.
- Renamed data model item to `DockWidgetItem` to eliminate ambiguous name collision with widget UI base class.
- Added downward-pointing chevron buttons on configurable widget cards in Settings to expand and collapse widget-specific configuration drawers with automatic scrolling.
- Replaced the manual Apply footer with a full real-time auto-save system across all preferences and widget settings.
- Added graphics card usage, graphics card temperature and processor temperature meters to System meters, each with its own toggle.
- Added country and city selection picker for weather forecasts.
- Added automatic detection and hiding of the dock when full-screen browser videos, games, or borderless windows are active.
- Added fluent open/close and tab transition animations to the Settings window with bounded min/max dimensions.
- Highlighted the foreground application independently of keyboard focus, clearing its background when minimized or another app becomes active.
- Added normal close and confirmed process termination to background tray apps.
- Updated README documentation with authentic desktop hero showcase and visual feature gallery.

## 0.2.3 - 2026-11-16

- Kept application icons at 42 DIP inside fixed 52 DIP cells when resizing or adding widgets.
- Allowed the dock to narrow to one visible application cell, deriving its minimum width from the frame insets.
- Removed the visible horizontal scrollbar; Shift/Ctrl plus wheel and middle-button dragging scroll overflowing content.
- Replaced decorative resize grips with invisible hit targets at the outside left and right borders.
- Kept auto-hide locked during resizing and middle-button panning.

## 0.2.2 - 2026-11-16

- Kept magnified and bouncing icons inside the dock's scroll viewport while preserving horizontal clipping.
- Reserved the missing shelf height above Explorer's existing bottom space, without adding hover headroom to the desktop reservation.
- Updated the reservation when auto-hide changes and prevented recursive Shell positioning updates.
- Kept maximized captioned windows distinct from full-screen applications and released the extra reservation on exit.

## 0.2.1 - 2026-11-16

- Replaced text-heavy widget catalog rows with a wrapping gallery of real dock widget previews.
- Install widgets by selecting their preview; keep visibility and removal controls on installed items.
- Refresh local preview data while the widget settings page is visible, without running timer actions.

## 0.2.0 - 2026-11-16

- Refined dark and light dock surfaces, quieter resize handles, and softer shadows.
- Added an optional widget catalog with install, remove, show, hide, and localized search.
- Added clock, date, battery, memory, system-drive space, day progress, stopwatch, and countdown widgets.
- Added compact time tiles and shared details panels for duration input and reset.
- Improved widget catalog readability and themed input, focus, and read-only states.
- Standardized application icon bounds and added two-sided dock resizing with horizontal scrolling.
- Used the Windows UI language for initial preferences while preserving saved language choices.
- Added deterministic behavior checks and rendered WPF component checks.

## 0.1.0 - 2026-11-15

- Initial portable Windows x64 release.
