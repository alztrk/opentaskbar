# Theme Packs

A theme pack restyles OpenTaskbar with colors, fonts and widget styles. Icon and widget packs are optional; bundled themes keep the original Windows and app icons.

## Where themes live

```
%LOCALAPPDATA%\OpenTaskbar\Themes\
└── my-theme\
    ├── theme.json          ← name, author, description
    ├── icons\
    │   ├── pack.json       ← optional icon pack
    │   └── example.png     ← matched to example.exe without a mapping
    └── widgets\
        └── pack.json       ← optional widget pack
```

Pick a theme in **Settings → Appearance → Theme → Theme pack**. **Open folder** opens the Themes folder; **Reload** re-reads the theme after you edit its files.

`theme.json`:

```json
{ "name": "My Theme", "author": "You", "description": "Optional.", "preserveOriginalIcons": true }
```

`preserveOriginalIcons: true` keeps native icons even when an icon pack exists. Bundled themes set it; custom themes can omit it to use an icon pack.

## Icon pack: icons\pack.json

```jsonc
{
  "icons": {
    "example.exe": "icons/browser.png",
    "Microsoft.WindowsTerminal": "icons/terminal.png",
    "@start": "icons/start.png",
    "@recyclebin": "icons/trash.png",
    "@recyclebin-full": "icons/trash-full.png"
  },
  "fallback": { "shape": "Petal", "cornerRadius": 0.42, "iconScale": 0.62 }
}
```

Comments and trailing commas are allowed. Keys are case-insensitive and the first match wins. Values are paths relative to `icons\`; paths leading outside it are ignored.

| Key | Example | Matches |
| :--- | :--- | :--- |
| AppUserModelID | `Microsoft.WindowsTerminal_8wekyb3d8bbwe!App` | One Store or packaged app |
| Package name | `Microsoft.WindowsTerminal` | The ID part before `_` |
| Full path | `C:\Program Files\App\app.exe` | One install |
| File name | `example.exe` | Any `example.exe` |
| Name | `example` | Same, without the extension |
| Special item | `@start`, `@recyclebin`, `@recyclebin-full` | Start button, empty/full Recycle Bin |

An image named after the executable — `example.png` for `example.exe` — works without a mapping when placed in `icons\` or `icons\icons\`. Formats: PNG, ICO, JPG, BMP; a 128–256 px square PNG works best.

Apps with no image keep their own icon. A `fallback.shape` draws that icon on a plate so the dock looks consistent:

| Field | Values | Default |
| :--- | :--- | :--- |
| `shape` | `None`, `Leaf`, `Petal`, `Rounded`, `Circle` | `None` |
| `sharpCorner` | `TopLeft`, `TopRight`, `BottomLeft`, `BottomRight` | `TopRight` |
| `cornerRadius` | `0`–`0.5` of the icon size | `0.5` |
| `background` | `auto` (pale icon tint) or a color like `#FFFFFF` | `auto` |
| `iconScale` | `0.2`–`1.0` | `0.64` |

`Leaf` leaves one corner square; `Petal` keeps `sharpCorner` and the opposite corner square.

## Widget pack: widgets\pack.json

A widget pack replaces design tokens — named colors, brushes, radii, sizes and fonts used by the dock, widgets, menus and Settings.

```jsonc
{
  "tokens": {                       // both themes
    "WidgetAccent": "#FF1FC2F0",
    "WidgetRadius": 16,
    "InterfaceFont": "Segoe UI Variable Display"
  },
  "dark":  { "DockMaterial": { "stops": ["#FF0E2233", "#FF050E17"] } },
  "light": { "DockMaterial": { "stops": ["#FFEAF7FD", "#FFCFE6F3"] } }
}
```

A value's type follows the token it replaces:

| Token type | Value | Examples |
| :--- | :--- | :--- |
| Brush | `"#AARRGGBB"` or `{ "stops": [colors], "direction": "vertical\|horizontal\|diagonal" }` | `WidgetAccent`, `DockMaterial` |
| Radius / thickness | A number, `"x,y"` or `"left,top,right,bottom"` | `WidgetRadius`, `DockRadius` |
| Size | A number | `WidgetHeight`, `TextSizeWidgetValue` |
| Font | Font family name | `InterfaceFont` |

Brushes update immediately; sizes, radii and fonts are read when a view is built and take effect after a restart. Unknown or wrongly typed tokens are skipped and logged to `%LOCALAPPDATA%\OpenTaskbar\opentaskbar.log`.

Glass colors come from the theme; opacity and the Glass/Solid/None mode come from Settings → Appearance and always win. Dynamic Island uses `DynamicIslandMaterial`, with its own Settings color/opacity taking precedence (**Use theme color** follows the theme again).

## Licensing

Use only images you may distribute; app logos can be trademarks. Bundled themes ship no replacement app icons.

## Rust port

The Rust app reads the same folder and files, so themes work in both builds. `preserveOriginalIcons` and the icon-matching order above behave the same. The theme editor is off by default (`enable_theme_editor`); Settings can still list, import and export themes.
