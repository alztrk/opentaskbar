# Theme Packs

A theme pack restyles OpenTaskbar with colors, fonts and widget tokens. The six bundled themes keep the original Windows and application icons; custom themes may add an optional icon pack. On first launch the Rust app installs the bundled themes from the repository's `themes/` folder into the user theme folder.

## Where themes live

```
%LOCALAPPDATA%\OpenTaskbar\Themes\
└── my-theme\
    ├── theme.json          ← name, author, description, icon policy
    ├── icons\
    │   ├── pack.json       ← optional icon pack
    │   ├── firefox.png     ← matched to firefox.exe without a mapping
    │   └── images\…
    └── widgets\
        └── pack.json       ← optional widget design tokens
```

Both packs are optional. Select a theme in **Settings → Appearance → Theme pack**; **Open folder** opens `Themes`, and **Reload** rereads the selected theme after you edit its files. A starter pack is in [theme-template](theme-template); the shipped packs are in [themes](../themes): Catppuccin, Gruvbox, Nord, Prism, Solarized and Tokyo Night. All six preserve original app, Start and Recycle Bin icons even if older installations still contain icon files.

## `theme.json`

```json
{ "name": "My Theme", "author": "You", "description": "Optional.", "preserveOriginalIcons": true }
```

`preserveOriginalIcons: true` keeps native Windows/app icons even when an `icons/pack.json` exists. The bundled themes set this automatically. A custom theme can omit the flag to use its icon pack.

## Optional icon pack: `icons\pack.json`

```jsonc
{
  "icons": {
    "firefox.exe": "images/browser.png",
    "Microsoft.WindowsTerminal": "images/terminal.png",
    "@start": "images/start.png",
    "@recyclebin": "images/trash.png",
    "@recyclebin-full": "images/trash-full.png"
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

Comments and trailing commas are allowed. The first matching key wins:

| Key | Example | Matches |
| :--- | :--- | :--- |
| AppUserModelID | `Microsoft.WindowsTerminal_8wekyb3d8bbwe!App` | One packaged/Store app |
| Package name | `Microsoft.WindowsTerminal` | The ID part before `_` |
| Full path | `C:\Program Files\App\app.exe` | One installation |
| File name | `firefox.exe` | Any executable with that name |
| Name | `firefox` | Same, without the extension |
| Special item | `@start`, `@recyclebin`, `@recyclebin-full` | Start button, empty/full Recycle Bin |

Keys are case-insensitive. Values are relative to the `icons` folder; a path that escapes that folder is ignored. An image named after the executable (`firefox.png` in `icons\` or a nested folder) is used without a mapping. Supported formats: PNG, ICO, JPG and BMP; a square 128–256 px PNG works best.

Apps without a pack image keep their own icon. With a fallback `shape`, that icon is drawn on a plate:

| Field | Values | Default |
| :--- | :--- | :--- |
| `shape` | `None`, `Leaf`, `Petal`, `Rounded`, `Circle` | `None` |
| `sharpCorner` | `TopLeft`, `TopRight`, `BottomLeft`, `BottomRight` | `TopRight` |
| `cornerRadius` | `0`–`0.5`, a fraction of the icon size | `0.5` |
| `background` | `auto` (pale tint of the icon) or a color such as `#FFFFFF` | `auto` |
| `iconScale` | `0.2`–`1.0` | `1.0` |

`Leaf` keeps one corner square and rounds the other three; `Petal` keeps `sharpCorner` and its opposite corner square.

## Widget pack: `widgets\pack.json`

A widget pack replaces **design tokens** used by the dock, widgets, menus, Settings and Dynamic Island.

```jsonc
{
  "tokens": {                       // both themes
    "WidgetAccent": "#FF1FC2F0",
    "WidgetAccentFill": { "stops": ["#FF5AD9FF", "#FF1595D3"], "direction": "vertical" },
    "WidgetRadius": 16,
    "DockSheenRadius": "21,21,0,0",
    "InterfaceFont": "Inter"
  },
  "dark":  { "DockMaterial": { "stops": ["#FF0E2233", "#FF050E17"] } },
  "light": { "DockMaterial": { "stops": ["#FFEAF7FD", "#FFCFE6F3"] } }
}
```

`tokens` applies to both themes; `dark` and `light` override it. A value's type follows the token it replaces:

| Token type | Value | Examples |
| :--- | :--- | :--- |
| Brush | `"#AARRGGBB"`, or `{ "stops": [2+ colors], "direction": "vertical|horizontal|diagonal" }` | `WidgetAccent`, `TextPrimary`, `DockMaterial`, `WidgetCard` |
| Radius / thickness | Number, `"x,y"` or `"left,top,right,bottom"` | `WidgetRadius`, `DockRadius`, `DockSheenRadius` |
| Size | Number | `WidgetHeight`, `TextSizeWidgetValue` |
| Font | Font family name | `InterfaceFont` |

The Rust resolver converts these to typed values with a **256-token limit**; unknown tokens and wrongly typed values are skipped and reported in `%LOCALAPPDATA%\OpenTaskbar\opentaskbar.log` instead of aborting theme load. Brushes and colors update live. Sizes, radii and fonts are read when a view is first built, so they take effect after a restart.

The dock/menus/widget glass take their source colors from the theme; their material (Glass / Solid / True black / None) and opacity are user settings in Settings → Appearance and always win. The Dynamic Island uses `DynamicIslandMaterial`; its custom background color and opacity in Settings take precedence, and **Use theme color** restores theme control. The in-app theme editor saves overrides under `settings.json` → `extra.ThemeTokens` and applies them on startup. A `.otbackup` archive can include the active theme's JSON files.

## Security boundaries

- Theme and icon paths must stay inside the theme folder. Absolute paths, `..` traversal, reserved Windows names and archive entries outside staging are rejected by the icon resolver and backup importer.
- Theme files are data only: JSON design tokens and image files. No scripts or executables are loaded from a theme pack, and credentials/secrets are never stored in theme JSON.
- Image decoding is bounded; oversized or unreadable images are skipped rather than crashing the dock.

Custom icon packs should contain only images you have permission to distribute; application logos may be trademarks. Bundled themes do not ship replacement app icons.
