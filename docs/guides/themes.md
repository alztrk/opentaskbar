# Themes

A theme changes OpenTaskbar's colors, fonts and widget looks. Six themes come with the app: Catppuccin Mocha, Gruvbox Material, Nord, Prism, Solarized and Tokyo Night. They leave your normal Windows and app icons alone — changing icons is optional. On first launch the Rust app installs the bundled themes from the repository's `resources/themes/` folder into `%LOCALAPPDATA%\OpenTaskbar\Themes`.

## Choose a theme

Open **Settings → Appearance → Theme → Theme pack**. **Open folder** takes you to the themes folder; **Reload** applies changes after you edit a theme's files.

Themes live in `%LOCALAPPDATA%\OpenTaskbar\Themes\`. Each theme is a folder:

```
my-theme\
├── theme.json          ← the theme's name and author
├── icons\              ← optional replacement icons
│   ├── pack.json       ← optional icon rules
│   ├── example.png     ← matched to example.exe without a mapping
│   └── images\…        ← nested images, used only when mapped in pack.json
└── widgets\            ← optional color and size changes
    ├── pack.json       ← optional widget design tokens
    └── icons\          ← optional custom-widget icon overrides
        └── pack.json   ← `widget:<slug>` and `pathData` entries
```

Both optional folders are picked up automatically. A starter example — including `widgets\icons\pack.json` — is in [theme-template](theme-template); the shipped packs are in [themes](../../resources/themes). A `theme.json` can set `"preserveOriginalIcons": true` to keep native icons even when an icon pack exists — all six bundled themes do this.

## Change app icons

The easiest way is to put a picture in the theme's `icons` folder and name it after the program's file. For example, a picture called `example.png` is used for a program called `example.exe`. Put it directly in `icons\` or in `icons\icons\`; a deeper folder such as `icons\images\` is used only when a `pack.json` rule points at it.

PNG, ICO, JPG and BMP pictures work. A square PNG around 128–256 pixels looks best. Programs without a picture keep their normal icon.

If you need more control — a different install of the same program, a Windows Store app, or the Start button and Recycle Bin — add a small `pack.json` file in `icons\`:

```jsonc
{
  "icons": {
    "example.exe": "icons/browser.png",
    "firefox.exe": "images/firefox.png",
    "Microsoft.WindowsTerminal": "icons/terminal.png",
    "@start": "icons/start.png",
    "@recyclebin": "icons/trash.png",
    "@recyclebin-full": "icons/trash-full.png"
  },
  "fallback": { "shape": "Petal", "sharpCorner": "TopRight", "cornerRadius": 0.42, "background": "auto", "iconScale": 0.62 }
}
```

The left side says which program to match, the right side is the picture to use. The first rule that matches wins. You can match by:

| Left side | Example | When to use it |
| :--- | :--- | :--- |
| Special name | `@start`, `@recyclebin`, `@recyclebin-full` | Start button, empty/full Recycle Bin |
| AppUserModelID (App ID) | `Microsoft.WindowsTerminal_8wekyb3d8bbwe!App` | One packaged/Store app |
| Package name | `Microsoft.WindowsTerminal` | All versions of that Store app |
| Full path | `C:\Program Files\App\app.exe` | One particular install |
| File name | `example.exe` | Any program with that file name |
| Name | `example` | Same, without `.exe` |

Special `@` keys are checked before all other keys. Capital letters don't matter, and paths point inside the `icons` folder — absolute paths and paths escaping it (including `..`) are ignored. Images named after the executable need no mapping, but must sit directly in `icons\` or in `icons\icons\`.

Programs without a picture can be drawn on a shaped background so the dock looks even. Add a `fallback` section to `pack.json`:

| Field | Values | Default |
| :--- | :--- | :--- |
| `shape` | `None`, `Leaf`, `Petal`, `Rounded`, `Circle` | `None` |
| `sharpCorner` | `TopLeft`, `TopRight`, `BottomLeft`, `BottomRight` | `TopRight` |
| `cornerRadius` | `0`–`1.0` of the icon size | `0.5` |
| `background` | `auto` (a pale tint of the icon) or a color such as `#FFFFFF` | `auto` |
| `iconScale` | `0.1`–`1.0` | `0.64` |

`Leaf` leaves one corner square; `Petal` keeps `sharpCorner` and its opposite corner square. Image decoding is bounded, so oversized or unreadable pictures are skipped rather than crashing the dock.

## Change widget looks

A `pack.json` in the theme's `widgets` folder changes colors, gradients, sizes and fonts. Only list what you want to change; everything else stays as it is.

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

`tokens` applies to both themes; `dark` and `light` override it. Values follow the type of the thing they replace:

| Token type | Value | Examples |
| :--- | :--- | :--- |
| Brush | `#AARRGGBB` or `{ "stops": [colors], "direction": "vertical\|horizontal\|diagonal" }` | `WidgetAccent`, `DockMaterial` |
| Radius / thickness | A number, `"x,y"` or `"left,top,right,bottom"` | `WidgetRadius`, `DockRadius` |
| Size | A number | `WidgetHeight`, `TextSizeWidgetValue` |
| Font | Font family name | `InterfaceFont` |

Colors change right away. Sizes, corners and fonts take effect after OpenTaskbar restarts. Your own choices in **Settings → Appearance** (glass, solid, true black or none, and how see-through it is) always win over the theme. The Dynamic Island uses `DynamicIslandMaterial`; its own color/opacity control in Settings takes precedence, and **Use theme color** follows the theme again. The theme editor is always available in Settings, which can also list, import and export themes. A `.otbackup` archive contains settings and pins only — theme packs are installed separately in `Themes\` and are not part of it.

## Custom widget icons

Custom widgets install their own optional `icon.png`, but a theme can override it. Add `widget:<slug>` keys to `widgets\icons\pack.json`, where `<slug>` is the widget's id without the `custom:` prefix, or an inline `M…` path under `pathData`:

```jsonc
{
  "icons": { "widget:usd-try": "icons/usd.png" },
  "pathData": { "widget:groups": "M3 3 L21 3 L21 21 L3 21 Z" }
}
```

A per-widget override set from **Settings → Widgets → the card's Icons button** (`CustomWidgetIcons` in `settings.json`) wins over the theme; otherwise the theme mapping, the widget's own `icon.png` and finally the default glyph are used. Paths are confined to the pack directory. See [custom-widgets.md](custom-widgets.md) for the full resolution order.

## Sharing themes

Only include pictures you're allowed to share — app logos may be trademarks. The themes that come with OpenTaskbar contain no replacement app icons. Theme and icon files are data only: JSON design tokens and images. No scripts or executables are loaded from a theme pack, no credentials are stored in theme JSON, and archive entries that traverse outside the staging folder or use reserved Windows names are rejected on import. Attribution and license notes for the third-party color palettes used by the bundled themes are in [resources/themes/LICENSES.md](../../resources/themes/LICENSES.md).

<details>
<summary><strong>Technical details</strong></summary>

`theme.json` can set `"preserveOriginalIcons": true` to keep native icons even when an icon pack exists; bundled themes do this. Icon rules are case-insensitive; special `@` keys are checked before all other keys, and the remaining keys are evaluated in the order shown above. Paths outside `icons\` are ignored. Comments and trailing commas are allowed in `pack.json`.

Widget values follow the type of the thing they replace: colors are `#AARRGGBB` or a gradient with `stops` and `direction`; radii and thicknesses are numbers, `"x,y"` or `"left,top,right,bottom"`; sizes are numbers; fonts are family names. The Rust resolver applies a **256-token limit**: unknown or wrongly typed values are skipped without aborting theme load.

The Rust app reads these folders and files directly, so themes work in the single Rust build. `preserveOriginalIcons` and the icon-matching order behave the same everywhere; the theme editor is always available in Settings.

</details>
