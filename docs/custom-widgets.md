# Custom widgets

Users can add their own dock widgets without rebuilding OpenTaskbar. A custom
widget is a folder in `%LOCALAPPDATA%\OpenTaskbar\Widgets\<slug>\` containing a
`widget.json` manifest and a small program. OpenTaskbar launches that program as
a **sidecar process** and talks to it over newline-delimited JSON on
stdin/stdout.

```
%LOCALAPPDATA%\OpenTaskbar\Widgets\
└── usd-try\
    ├── widget.json
    ├── widget.ps1      (any executable: Python, PowerShell, Node, Rust, …)
    └── icon.png        (optional, used when no theme/icon override wins)
```

## `widget.json`

```json
{
  "name": "USD/TRY",
  "category": "Time & Weather",
  "width": 130,
  "pollSeconds": 300,
  "exec": ["powershell.exe", "-NoProfile", "-File", "widget.ps1"]
}
```

| Field | Default | Notes |
|---|---|---|
| `name` | required | 1–128 chars, shown in the Settings catalog |
| `category` | `Productivity` | one of the five built-in categories |
| `width` | `120` | clamped to 48–512 DIP |
| `pollSeconds` | `60` | clamped to 5–86400 |
| `exec` | required | string (split on whitespace) or array; relative paths resolve against the widget folder |

The persisted widget id is `custom:<slug>` (e.g. `custom:usd-try`). The slug is
the folder name and may only contain ASCII letters, digits, `_` and `-`.

## Protocol

One JSON object per line. The host always initiates:

```text
host   → {"cmd":"poll"}
widget → {"chip":{"label":"USD","value":"42,15","ratio":0.6,"icon":"M0 0 L10 10 Z"},
          "flyout":{"title":"USD/TRY","width":280,
                    "blocks":[{"kind":"stat","label":"Kur","value":"42,15"},
                              {"kind":"progress","label":"Dolu","ratio":0.6,"value":"60%"}]}}
host   → {"cmd":"click","id":7}
widget → {"action":"openUrl","target":"https://example.com"}
```

`click` is sent when the user activates a flyout block that has an `id`. The
only host-side action today is `openUrl`; anything else is ignored.

### Chip fields

| Field | Type | Meaning |
|---|---|---|
| `label` | string | small caption under the value |
| `value` | string | main chip text |
| `ratio` | number (0–1) | optional progress bar at the chip bottom |
| `icon` | string | `M…` StreamGeometry path data, or a PNG path |

### Flyout blocks

`stat` (`label`, `value`), `note` (`text`), `hero` (`value`, `caption`),
`progress` (`label`, `value`, `ratio`), `metric` (`tiles: [[label, value], …]`),
`button` (`id`, `label`). Unknown kinds are skipped; a malformed reply never
crashes the dock.

## Example (PowerShell)

```powershell
while (($line = [Console]::In.ReadLine()) -ne $null) {
    $msg = $line | ConvertFrom-Json
    if ($msg.cmd -eq 'poll') {
        $rate = (Invoke-RestMethod 'https://api.frankfurter.app/latest?from=USD&to=TRY').rates.TRY
        @{chip=@{label='USD'; value=[string]$rate};
          flyout=@{title='USD/TRY'; blocks=@(@{kind='stat'; label='Kur'; value=[string]$rate})}} |
            ConvertTo-Json -Compress -Depth 6
    }
}
```

## Icons

Resolution order, first hit wins:

1. Per-widget override from Settings → Widgets → the card's **Icons** button
   (stored as `CustomWidgetIcons` in `settings.json`); the value may also be
   inline `M…` path data.
2. Active theme's `Themes\<theme>\widgets\icons\pack.json`:
   `{"icons":{"widget:usd-try":"icons/usd.png"},
     "pathData":{"widget:groups":"M3 3 L21 3 L21 21 L3 21 Z"}}`
3. `<widget folder>\icon.png`.
4. Default Fluent glyph.

Paths are confined to the pack directory and `..`/absolute mappings are
rejected. PNGs are capped at 256×256 and decoded off the UI thread with a cache.

## Lifecycle and safety

- Sidecars are spawned lazily, killed on crash or a 10 s stalled reply, and
  restarted on the next poll. All I/O runs on worker threads; the dock render
  loop only reads the last cached snapshot.
- A sidecar is **code you chose to install**. It inherits your user session and
  can make any network request its script performs. Only install widgets from
  sources you trust; OpenTaskbar does not sandbox the sidecar in this version.
- The host itself never parses `source.kind`, credentials or JSON paths — the
  widget owns its data source. Credentials should be kept in the OS credential
  store or a user file, not in `widget.json`.

After adding or editing a folder, press **Reload** on the Widgets settings page.
