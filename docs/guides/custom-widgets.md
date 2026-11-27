# Custom widgets

Users can add their own dock widgets without rebuilding OpenTaskbar. A custom
widget is a folder in `%LOCALAPPDATA%\OpenTaskbar\Widgets\<slug>\` containing a
`widget.json` manifest and a small program. OpenTaskbar launches that program as
a **sidecar process** and speaks newline-delimited JSON on stdin/stdout.

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
  "name": { "en-US": "USD/TRY", "tr-TR": "USD/TRY" },
  "category": "Time & Weather",
  "width": 130,
  "pollSeconds": 300,
  "exec": ["powershell.exe", "-NoProfile", "-File", "widget.ps1"]
}
```

`name` may be a plain string or a locale → name object (exact culture, then
two-letter language, then the first entry). Other fields:

| Field | Default | Notes |
|---|---|---|
| `category` | `Productivity` | one of the five built-in categories |
| `width` | `120` | clamped to 48–512 DIP |
| `pollSeconds` | `60` | clamped to 5–86400 |
| `exec` | required | string (split on whitespace) or array; bare program names resolve through PATH, explicit relative paths against the widget folder |

The persisted id is `custom:<slug>` (e.g. `custom:usd-try`). The slug may only
contain ASCII letters, digits, `_` and `-`.

## Approval and trust

A newly discovered widget is **not started** until you press **Approve** on its
Settings card. Approval pins the SHA-256 of `widget.json` in
`CustomWidgetConsent` inside `settings.json`; editing the manifest invalidates
the approval and you must approve again. The card also offers **Folder**,
**Edit** (opens the manifest in Notepad) and **Remove** actions, plus the
current error status when a sidecar fails.

Sidecars are placed in a kill-on-close Job Object so child processes do not
survive a timeout or crash. stderr is captured into `opentaskbar.log` with a
`[widget:<id>]` prefix. Failed polls retry with capped exponential backoff
(1 → 2 → 4 … 60 s).

## Protocol

Every request has a protocol version and a sequence number; replies with a
different version or sequence are ignored. Version 1:

```text
host   -> {"cmd":"poll","protocol":1,"seq":4}
widget -> {"protocol":1,"seq":4,"chip":{"label":"USD","value":"42,15","ratio":0.6},
           "flyout":{"title":"USD/TRY","width":280,
                     "details":[["Kur","42,15"]],
                     "blocks":[{"kind":"stat","label":"Kur","value":"42,15"}]}}
host   -> {"cmd":"click","id":7,"protocol":1,"seq":5}
widget -> {"protocol":1,"seq":5,"action":"openUrl","target":"https://..."}
```

### Chip fields

`label` (caption), `value` (main text), `ratio` (optional 0–1 progress bar),
`icon` (`M…` StreamGeometry path data or a PNG path).

### Flyout blocks

| `kind` | Fields |
|---|---|
| `stat` | `label`, `value` |
| `note` | `text` |
| `hero` | `value`, `caption` |
| `progress` | `label`, `value`, `ratio` |
| `metric` | `tiles: [[label, value], …]` |
| `button` | `id`, `label` |
| `chart` | `label`, `series: [{name, samples: [f32, …]}]` |
| `image` | `path`, `glyph`, `size` |
| `iconBar` | `buttons: [{id, label, glyph, active}]` |
| `seek` | `id`, `label`, `position`, `duration`, `ratio` |
| `textInput` | `id`, `label`, `value`, `numeric`, `min`, `max` |

`flyout.details` is an array of `[label, value]` rows shown in the Dynamic
Island expanded panel. Unknown block kinds are skipped; a malformed reply
never crashes the dock.

### Actions

`openUrl` / `launch` (`target`), `copy` (`text`), `notify` (`title`, `body`),
`toggleSetting` (`key`, a boolean settings key). Returned from a `click`
reply; anything else is ignored.

### Secrets

A widget can ask the host for a DPAPI-encrypted secret without putting it on a
command line:

```text
widget -> {"cmd":"secret","name":"api-key:example","seq":9,"protocol":1}
host   -> {"cmd":"secretResult","name":"api-key:example","value":"...","seq":9,"protocol":1}
```

The value is read from `%LOCALAPPDATA%\OpenTaskbar\secrets\<name>.bin` with the
same DPAPI + application entropy as the AI providers and is never logged. Use
the existing secret store UI/API to place the value there first.

## Example (PowerShell)

```powershell
while (($line = [Console]::In.ReadLine()) -ne $null) {
    $msg = $line | ConvertFrom-Json
    if ($msg.cmd -eq 'poll') {
        $rate = (Invoke-RestMethod 'https://api.frankfurter.app/latest?from=USD&to=TRY').rates.TRY
        @{protocol=1; seq=$msg.seq;
          chip=@{label='USD'; value=[string]$rate};
          flyout=@{title='USD/TRY'; details=@(@('Kur',[string]$rate));
                   blocks=@(@{kind='stat'; label='Kur'; value=[string]$rate})}} |
            ConvertTo-Json -Compress -Depth 8
    }
}
```

## Icons

Resolution order, first hit wins:

1. Per-widget override from Settings → Widgets → the **Icons** button
   (stored as `CustomWidgetIcons` in `settings.json`; inline `M…` path data works too).
2. Active theme's `Themes\<theme>\widgets\icons\pack.json`:
   `{"icons":{"widget:usd-try":"icons/usd.png"},
     "pathData":{"widget:groups":"M3 3 L21 3 L21 21 L3 21 Z"}}`
3. `<widget folder>\icon.png`.
4. Default Fluent glyph.

Paths are confined to the pack directory and `..`/absolute mappings are
rejected. PNGs are capped at 256×256 and decoded off the UI thread with a cache.

After adding or editing a folder, press **Reload** on the Widgets settings page.
