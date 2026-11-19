# Changelog

## Unreleased

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
