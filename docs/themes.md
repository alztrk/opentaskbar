# Theme Packs

A theme pack restyles OpenTaskbar with colors, fonts and widget styles. All six bundled themes **keep original Windows and application icons**. Custom themes can still include an optional icon pack. OpenTaskbar installs bundled themes into your user theme folder on first launch.

## Where themes live

```
%LOCALAPPDATA%\OpenTaskbar\Themes\
└── my-theme\
    ├── theme.json          ← name, author, description
    ├── icons\
    │   ├── pack.json       ← icon pack
    │   ├── firefox.png     ← matched to firefox.exe without a mapping
    │   └── icons\…
    └── widgets\
        └── pack.json       ← widget pack
```

Both packs are optional. Pick a theme in **Settings → Appearance → Theme → Theme pack**. **Open folder** opens the `Themes` folder; **Reload** rereads the selected theme after you change its files.

The built-in library includes Catppuccin Mocha, Gruvbox Material, Nord, Prism, Solarized and Tokyo Night. **All six preserve original app, Start and Recycle Bin icons, even when older installations still contain icon files.** Their widget styles continue to work. A starter custom theme is in [theme-template](theme-template). Bundled themes are available in [themes](../themes).

## theme.json

```json
{ "name": "My Theme", "author": "You", "description": "Optional.", "preserveOriginalIcons": true }
```

Set `preserveOriginalIcons` to `true` to retain native Windows and app icons even if an `icons/pack.json` file exists. All bundled themes do this automatically, including previously installed copies. Custom themes can omit this flag to use an icon pack.

## Optional custom icon pack: icons\pack.json

```jsonc
{
  "icons": {
    "firefox.exe": "icons/browser.png",
    "Microsoft.WindowsTerminal": "icons/terminal.png",
    "@start": "icons/start.png",
    "@recyclebin": "icons/trash.png",
    "@recyclebin-full": "icons/trash-full.png"
  },
  "fallback": {
    "shape": "Petal",
    "sharpCorner": "TopRight",
    "cornerRadius": 0.42,
    "background": "auto",
    "iconScale": 0.62
  }
}
```

Comments and trailing commas are allowed.

### Icon keys

For each app, the first key that matches wins:

| Key | Example | Matches |
| :--- | :--- | :--- |
| AppUserModelID | `Microsoft.WindowsTerminal_8wekyb3d8bbwe!App` | One Store or packaged app |
| Package name | `Microsoft.WindowsTerminal` | The part of the ID before `_` |
| Full path | `C:\Program Files\App\app.exe` | One install of an app |
| File name | `firefox.exe` | Any `firefox.exe` |
| Name | `firefox` | Same, without the extension |
| Special item | `@start`, `@recyclebin`, `@recyclebin-full` | The Start button, the Recycle Bin empty or full |

Keys are not case-sensitive. Values are paths relative to the `icons` folder; a path that leads outside it is ignored.

An image named after the executable, such as `firefox.png` in the `icons` folder or in `icons\icons\`, is used without a mapping.

Supported image formats: PNG, ICO, JPG and BMP. A square PNG of 128 to 256 pixels works best.

### Fallback plate

Apps the pack has no image for keep their own icon. With a fallback `shape`, that icon is drawn on a plate so the whole dock matches the theme:

| Field | Values | Default |
| :--- | :--- | :--- |
| `shape` | `None`, `Leaf`, `Petal`, `Rounded`, `Circle` | `None` |
| `sharpCorner` | `TopLeft`, `TopRight`, `BottomLeft`, `BottomRight` | `TopRight` |
| `cornerRadius` | `0` to `0.5`, a fraction of the icon size | `0.5` |
| `background` | `auto` (a pale tint of the icon) or a color such as `#FFFFFF` | `auto` |
| `iconScale` | `0.2` to `1.0`, the size of the icon on the plate | `0.64` |

`Leaf` keeps one corner square and rounds the other three. `Petal` keeps `sharpCorner` and the opposite corner square, like many Sailfish OS app icons.

## Widget pack: widgets\pack.json

A widget pack replaces **design tokens**: the named resources in [`Design/`](../Design) that the dock, widgets, menus and Settings are drawn with.

```jsonc
{
  "tokens": {                       // both themes
    "WidgetAccent": "#FF1FC2F0",
    "WidgetAccentFill": { "stops": ["#FF5AD9FF", "#FF1595D3"], "direction": "vertical" },
    "WidgetRadius": 16,
    "DockSheenRadius": "21,21,0,0",
    "InterfaceFont": "Segoe UI Variable Display"
  },
  "dark":  { "DockMaterial": { "stops": ["#FF0E2233", "#FF050E17"] } },   // Dark theme only
  "light": { "DockMaterial": { "stops": ["#FFEAF7FD", "#FFCFE6F3"] } }    // Light theme only
}
```

A value's type follows the token it replaces:

| Token type | Value | Examples |
| :--- | :--- | :--- |
| Brush | `"#AARRGGBB"`, or a gradient: `stops` (2 or more colors) and `direction` (`vertical`, `horizontal`, `diagonal`) | `WidgetAccent`, `TextPrimary`, `DockMaterial`, `WidgetCard` |
| Corner radius, thickness | A number, `"x,y"` or `"left,top,right,bottom"` | `WidgetRadius`, `DockRadius`, `WidgetSpacing` |
| Size | A number | `WidgetHeight`, `TextSizeWidgetValue` |
| Font | A font family name | `InterfaceFont` |

Brushes and colors change at once. Sizes, radii and fonts are read when a view is first built, so they take effect after OpenTaskbar restarts. Unknown tokens and values of the wrong type are skipped and written to `%LOCALAPPDATA%\OpenTaskbar\log.txt`.

The dock and menu glass (`DockMaterial`, `MenuMaterial`) take their colors from the theme. Their alpha comes from the per-surface **Surfaces** settings in Settings → Appearance; a theme pack can change the source colors, but the user's Glass/Solid/None mode and opacity always win. The widget panel reuses `MenuMaterial` as its source with an independent style.

Token names are listed in [Design/Tokens/](../Design/Tokens/) (`Typography`, `Spacing`, `Radii`, `Layout`, `Motion`), [Design/Palettes/Dark.xaml](../Design/Palettes/Dark.xaml), and [Design/Widgets.xaml](../Design/Widgets.xaml).

## Licensing

Custom icon packs should only contain images you have permission to distribute. App logos may be trademarks. Bundled themes do not ship replacement app icons.

## Rust port

The Rust port reads the same `%LOCALAPPDATA%\OpenTaskbar\Themes` folder and the same `theme.json` / `icons\pack.json` / `widgets\pack.json` files, so themes are interchangeable between builds. The six bundled themes are installed on first launch. `preserveOriginalIcons` and the icon-matching order described above are honoured by the Rust `platform::icon_pack` path.

Differences worth knowing:

- The Rust widget token resolver converts design tokens to typed values (brush, color, corner radius, thickness, font family) with a 256-token limit; unknown or wrongly typed tokens are skipped instead of aborting theme load.
- Windows accentPalette and `SystemUsesLightTheme` are read live by the Rust palette listener; a change is applied to the dock, menu, widgets and Settings surfaces without a restart.
- The theme editor is behind the `enable_theme_editor` flag (default off). With it off, the Rust Settings window still lists, imports and exports themes but does not expose token editing.
- A full backup (`.otbackup`) includes the active custom theme; importing it can restore settings alone or settings plus the theme. The archive contract is covered by `tests/full_backup.rs`.
