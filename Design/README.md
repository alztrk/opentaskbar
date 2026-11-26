# OpenTaskbar Design System

OpenTaskbar uses a lightweight, vector-driven design system implemented in pure WPF XAML without third-party UI dependencies.

---

## Design Tokens

### Layout & Dimensions

| Token | Value | Description |
| :--- | :--- | :--- |
| `DockWindowHeight` | `98 DIP` | Initial and preview window height; replacement mode derives its height from the shelf and headroom |
| `DockHeadroom` | `24 DIP` | Transparent top headroom included inside the scroll viewport for magnification and launch bounce; excluded from the desktop reservation |
| `DockMinimumShelfHeight` | `74 DIP` | Minimum shelf shared by the replacement window and dock layout; accommodates icon indicators, widget controls, padding, borders, and scrolling even when Explorer's taskbar is shorter |
| Shelf | 48 / 58 / 66 DIP | Dock shelf height for Compact / Medium / Large (`DockMetrics.For`) |
| `DockCellSize` | `52 DIP` | Fixed width of each application cell; never compressed to fit content |
| `DockCellHeight` | `52 DIP` | Height of an individual dock application item cell |
| `DockIconSize` | 28 / 34 / 42 DIP | Icon display size per dock size (`42 DIP` base) |
| `DockHoverScale` | `1.50x` | Peak magnification scale when slots can expand; automatically capped (e.g. ~`1.27x` at Large) on constrained/full-width docks so adjacent icons keep a >= 4 DIP gap |
| `DockRadius` | 16 / 19 / 22 DIP | Default surface corner radius per dock size (`20 DIP` token, user-customizable `0–32 DIP`); screen edge spacing (`3 / 5 / 6 DIP`) is computed automatically from the active size and scale profile |
| `ControlRadius` | `12 DIP` | Standard control and button corner radius |
| `PopupRadius` | `12 DIP` | Context menu and window preview corner radius |
| `PreviewGap` | `18 DIP` | Base vertical elevation gap between dock icons and thumbnail preview cards; dynamically augmented via `WindowThumbnailPreview.ExtraGapDips` when magnification is enabled |
| `StatusDotSize` | `4 DIP / 5 DIP` | Running and active application indicator dot diameter |
| `DockResizeGripWidth` | `8 DIP` | Invisible pointer target on each outside border; cursor indicates resizing |

### Typography

- **Font Family:** `Segoe UI` across all UI surfaces for native Windows consistency, legibility, and full Unicode/Turkish diacritic support.
- **Sizes:**
  - Body and menu items: `13 DIP`
  - Secondary labels, badges, and Jump List category headers: `12 DIP`
  - Titles and section headings: `15 DIP`
  - Widget values: `18 DIP`; timed values: `20 DIP`; detailed time: `32 DIP`

---

## Visual Materials

- **Settings Glass:** Native Desktop Acrylic (`DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW`) on Windows 11 build 22621+. `WindowChrome` provides the custom 48 DIP caption and native resize border. Text and icons remain opaque. Each of the four surfaces (dock, menus, widgets, Settings) has its own Glass/Solid/None mode and 0–100% opacity, defaulting to Glass at 90%; "None" also removes the rim, sheen and shadow and disables DWM acrylic. No layered-window opacity or software blur is used. Unsupported systems and high-contrast mode use an opaque settings surface.
- **Dock Glass:** The dock keeps its layered window for the rounded pill, transparent headroom and click-through gaps, so its glass look comes from the translucent `DockMaterial` fill, rim and ambient shadow rather than a DWM backdrop (a layered window cannot host one). Hardware acceleration is the default so magnification and flyouts stay fluid; Settings → Behavior → Hardware acceleration (or `--software-render`) switches to the lower-memory software renderer at the next start.
- **Compact Settings Layout:** One caption, a 200 DIP navigation column, clean full-height page views without an Apply footer, and adaptive 3-column widget catalog cards (`MinItemWidth = 230 DIP`). Settings are grouped into quiet slate section cards whose rows are separated by hairline dividers instead of separate boxes; segment hosts and sliders use a shared inset track. Auto-save applies all changes immediately in real-time. Short auto-hide options form a vertical list; uniform choice groups share aligned edges. The default `980 × 680 DIP` window (`Min 580 × 440`, `Max 1280 × 920`) fits general settings and the catalog cleanly; smaller sizes scroll to the final option. Custom maximize sizing follows the current monitor work area and DPI.
- **Preference Feedback:** All settings (general switches, choices, themes, sizes, scales, widget selections, and widget-specific options) auto-save immediately or with brief input debouncing. Per-surface material changes preview immediately and persist. Configurable widget cards feature a top-right chevron button to expand or collapse their dedicated settings section inline with direct auto-scroll.

### "Aurora" visual language

One accent gradient, teal `#5EEAD4` → sky `#38BDF8` (→ indigo `#818CF8` on the Start mark), carries every active state. Surfaces stay neutral so application icons keep their own colors.

The default look takes its colors from the user's Windows personalization settings: `AccentPalette` (RGBA light→dark ramp) and `DWM\AccentColor` supply the accent used only for interactive controls — primary buttons, switches, focus rings, selection marks and meter fills. Chrome surfaces stay neutral and follow the Windows Settings app: a tinted dark backdrop, a distinct navigation column and slightly elevated cards (`#1B2029` / `#171C26` / `#252B37` in dark, `#F3F3F3` / `#EEEEEE` / `#FFFFFF` in light). `WindowsPalette` generates the brushes and `ThemeManager` layers them below any explicitly selected theme pack, so a pack keeps control of the keys it defines. A hidden HWND listens for `WM_SETTINGCHANGE("ImmersiveColorSet")`, `WM_THEMECHANGED`, `WM_SYSCOLORCHANGE` and `WM_DWMCOLORIZATIONCOLORCHANGED`, so changing the Windows accent updates the app live. The static `Dark.xaml` / `Light.xaml` files remain as the fallback when no Windows accent is available.

- **Graphite Glass:** Three-stop dock material (`#CC222833` → `#D9161A22` → `#E60F1218` in Dark), its alpha set by the dock's own surface opacity. Corner radius 16 / 19 / 22 DIP per size.
- **Sheen:** A 1 DIP `DockHighlight` line on the top border, fading out before the rounded corners (`DockSheen`, bottom dock only).
- **Ambient Shadow:** `BlurRadius="30"`, `ShadowDepth="5"`, `Color="#05070B"`, `Opacity="0.5"`. Outside border hit targets remain invisible and show the resize cursor.
- **Hover Light:** A radial `IconHoverGlow` pool under the hovered icon instead of a hover box.
- **Status Indicators:**
  - Running application: Muted 4 DIP circular dot (`#94A3B8`) or 8 × 3 DIP capsule (`IndicatorBrush`).
  - Active (foreground) application: Bright illuminated 4 DIP dot (`#38BDF8`) or 16 × 3 DIP capsule in the accent gradient.
  - Alert / notification: Upper-right red badge (`#EF4444`).
- **Control Island & Widgets:** The optional catalog includes clock, date, battery, memory, system-drive space, day progress, stopwatch, countdown, real-time network speed (download and upload), and Bluetooth device battery level widgets, plus the control island (`WidgetIsland`, radius 14) with media, system meters, focus timer, audio, quick settings, virtual desktops, tray apps, weather, and calendar widgets. `Design/WidgetControls.xaml` and `Design/Widgets.xaml` own widget styles and tokens, while `Views/Widgets/` and `Widgets/` implement widget views and coordinators.
- **Flyouts:** Stacks, calendar, weather, background apps and the launcher share the menu glass (`MenuMaterial`, radius 14–18) and the `FlyoutButton` row style. They open away from the docked edge.
- **Icons:** Vector outline paths based on the Lucide icon set (24x24 coordinate box, 1.6 DIP stroke width, rounded caps) for context menus, widgets, and dock controls. Compact time actions and window captions use built-in Segoe MDL2 Assets glyphs.

---

## Interaction & Motion

### Continuous Fisheye Magnification
- Implemented via the pure layout helper `Services/App/DockFisheyeLayout.cs` and `Views/Windows/MainWindow.Magnification.cs`.
- Pointer movement calculates a $C^1$-smooth raised-cosine bell curve from invariant resting centers captured in `DockHost` coordinates:
  $$f(d) = \frac{1 + \cos(\pi \cdot d / R)}{2}, \quad S(d) = 1.0 + (S_{\text{eff}} - 1.0) \cdot f(d)$$
- Influence radius scales with the active cell size (`cell * 2.8`, min `110 DIP`; tightened to `cell * 2.1`, min `95 DIP` on constrained docks).
- Frame-rate independent spring physics (`stiffness = 280`, `damping = 30`) drives both growth and settling smoothly without oscillation.
- When monitor space allows, button slots (`SlotExpansionFactor = 0.95`) and the floating `DockSurface` expand synchronously so icons, active indicator dots, tooltips, and click targets move together with zero overlap. When the dock is full-width, scrolling, or width-locked, `ComputeEffectiveMaxScale` smoothly caps peak scale to guarantee at least `4 DIP` clearance between adjacent icons.
- Items scale anchored at `IconScaleOrigin` (`(0.5, 1.0)` on bottom edge, `(0.0, 0.5)` on left, `(1.0, 0.5)` on right) with a subtle headroom-clamped lift (`<= 1.0 DIP`). Hovering widgets, resize thumbs, or edge scroll chevrons smoothly settle magnification so interactive controls never shift under the pointer.

### Multi-Window Live Thumbnails
- Interactive live DWM thumbnails are created using `DwmRegisterThumbnail`.
- Thumbnails fade and scale in from 95% over 160 ms with cubic ease-out.
- The preview strip floats `PreviewGap` (`18 DIP`) plus `DockFisheyeLayout.ComputePreviewClearance` above the dock when magnification is active so cards never touch magnified icons.
- Each preview card features the application icon, window title, close button, and live window preview.

### Smart Auto-Hide (IntelliHide)
- **Disabled:** Dock remains persistently visible.
- **Smart:** Dock stays visible on the desktop and behind non-overlapping windows. When an active or maximized window overlaps the bottom dock region, the dock smoothly translates down off-screen.
- **Always:** Dock remains hidden below the screen edge by default.
- **Edge Reveal:** Moving the mouse into the bottom 3 DIP trigger area instantly reveals the dock. Mouse leave triggers a 400 ms debounce before sliding down with a 220 ms cubic ease-in-out transition; reveal uses 220 ms cubic ease-out. With animations off both are instant. While hidden, only the edge strip receives input; the reserved-area backdrop is collapsed so windows below stay clickable.
- **Full-screen detection:** Captionless windows that cover the monitor (games, video, F11 browsers) hide the dock. Maximized windows with a caption do not, because with auto-hide they also cover the whole monitor.

### Dock sizing and overflow

- Drag either dock edge to resize the visible surface. The opposite edge stays anchored during the drag.
- Application icons remain 42 DIP inside fixed 52 DIP cells when the dock narrows or widgets are added. The minimum visible surface is one cell plus its frame insets, 86 DIP with the default tokens.
- Overflow uses a hidden horizontal scroll viewport. Shift/Ctrl plus wheel moves horizontally, including high-resolution wheel deltas; middle-button dragging pans the content. No scrollbar row is reserved. `Views/DockScrollViewer.cs` expands the viewport upward and offsets its content by the same headroom. `Design/DockControls.xaml` keeps that offset inside the scroll presenter, preserving horizontal clipping at the resize gutters.
- Native mode queries the approved AppBar rectangle to measure space already reserved below it, then reserves only the missing shelf height. Hover headroom remains transparent above the shelf and does not reduce the desktop work area. Repeated position notifications do not accumulate another reservation.
- Invisible hit targets cover the outside left/right borders. They do not consume content padding or display extra lines. Resizing and middle-button panning lock auto-hide until the gesture ends.
- The dock width is bounded by the active monitor; cell and icon sizes remain fixed.

### Launch Bounce
- Clicking an application icon triggers a 420 ms damped vertical launch bounce animation (-12 DIP peak) on its translate transform to provide tactile feedback while the application opens.

---

## Layouts and Widgets

- **Floating:** The dock hugs its content and follows the Left/Center/Right alignment. Widgets sit after the applications inside the same surface.
- **Full width:** The surface spans the monitor with a 6 DIP side inset. Applications stay left and widgets are pinned to the right edge. Alignment is disabled.
- **Sizing:** Widgets keep their natural width.
- **Widget card** (`WidgetCardBorder` / `WidgetCardButton`): 46 DIP high, 12 DIP radius, transparent resting surface with a soft, eased 140/180 ms hover fade (`SurfaceHover`, driven by an animated overlay/`SurfaceHoverColor` token) and 3 DIP horizontal spacing. No instant background swaps.
- **Widget Height Uniformity Rule:** All dock widgets must strictly have a uniform height governed by the single dynamic `{DynamicResource WidgetHeight}` resource (46 DIP design height; `MainWindow.ApplySizeProfile` sets it to `DockSizeProfile.WidgetCardHeight`, 34–46 DIP, so compact docks get real-size crisp cards instead of a fractional bitmap scale). Every top-level widget container must use `WidgetCardBorder` or `WidgetCardButton` and must never specify a hardcoded or one-off `Height` (such as `Height = 32`). This invariant guarantees that all dock widgets align symmetrically and flush within the dock shelf across all configurations.
- **Colors:**
  - `WidgetAccent` (sky blue): meter values and the on state.
  - `WidgetAccentFill` (blue gradient): the battery tile and the volume track.
  - `WidgetWarning`: meters at 75% and above.
  - `WidgetFocusAccent` (red): meters at 90% and above, the focus phase, and low battery.
  - `WidgetOnStroke`: the outline of active Wi-Fi/Bluetooth pills.
- **Meters:** `RingMeter` draws a 3 DIP ring clockwise from 12 o'clock with a small caption (CPU/RAM) above the value.
- **Focus timer:** The card outline turns red while focusing and blue during a break. A finished phase plays the system sound and pulses the card three times (skipped when animations are off).
- **Typography:** Widget titles are 12 DIP semibold, captions 11 DIP, the clock 15 DIP, and the focus countdown 20 DIP with tabular numerals.

---

## Project Structure

The design system is modular: every token category, icon family and control family lives in its own XAML file. `App.xaml` merges them in dependency order (tokens → palette → icons → controls); `ThemeManager` swaps the palette and appends theme-pack overrides last.

- `Design/Tokens/`: `Typography.xaml`, `Spacing.xaml`, `Radii.xaml`, `Layout.xaml`, `Motion.xaml` — every shared token, one category per file. Theme packs override these by key.
- `Design/Palettes/Dark.xaml` & `Light.xaml`: Theme color palettes and material brushes swapped dynamically by `ThemeManager`.
- `Design/Icons/`: `Navigation.xaml`, `Actions.xaml`, `Status.xaml`, `Media.xaml`, `Brand.xaml`, `GeometryTemplate.xaml` — all vector geometries, grouped by meaning, plus the shared geometry `DataTemplate`.
- `Design/Controls/Tooltip.xaml` & `ScrollBar.xaml`: Implicit base-control styles.
- `Design/DockControls.xaml`: Dock buttons (`DockButton`, `StartDockButton` with aligned 6 DIP bottom baseline), resize thumbs, and scroll viewer/indicator templates.
- `Design/ContextMenus.xaml`: Menu item templates, Jump List headers and the destructive action style.
- `Design/SettingsControls.xaml`: Settings window controls.
- `Design/WidgetControls.xaml` & `Design/Widgets.xaml`: Widget card/button/pill/slider styles with full-surface `HoverLayer` alignment.

### Settings navigation

The 200 DIP sidebar uses 44 DIP minimum-height tabs with an 18 DIP Segoe MDL2 icon column and a subtle right divider. Selection is marked by a slim accent bar and a subtle neutral surface; hover and keyboard focus have separate treatments. Native TabControl keyboard/selection behavior is retained. Decorative navigation glyphs are non-focusable template children; the actual tab header supplies its accessible name.

## Settings verification

Release compilation and temporary-file checks cover legacy opacity defaults, finite/range validation, saved values, pin preservation, live material changes, unchanged text opacity, preview rollback, and Apply state. WPF renders were inspected in dark/light, Turkish/English, default and minimum sizes. A native Windows 11 probe confirmed Acrylic type 3, maximize bounds within the monitor work area, and restore/minimize states; a cropped screen capture verified the actual custom window. Pointer dragging, resize grips, keyboard shortcuts, multiple-monitor DPI transitions, and Windows transparency/power-policy variations still require interactive validation. Computer Use was not used.

References: [Microsoft WindowChrome](https://learn.microsoft.com/en-us/dotnet/api/system.windows.shell.windowchrome), [Microsoft DWM system backdrop types](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type).
