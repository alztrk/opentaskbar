# Privacy Policy — OpenTaskbar

Last updated: 2026-11-22

OpenTaskbar is an open-source desktop dock and taskbar customization utility for Windows. Your privacy is respected by default.

## Data Collection

- **No telemetry or analytics:** OpenTaskbar does not collect, sell, or transmit personal data, usage analytics, crash telemetry, or tracking identifiers to the developer or third parties.
- **Local storage only:** Your preferences, pinned items, theme packs, profiles, and logs are stored locally on your device under `%LOCALAPPDATA%\OpenTaskbar` (or `%LOCALAPPDATA%\OpenTaskBar-Rust` for the Rust port). The Rust port preserves unknown `settings.json` keys and backs up unreadable files before restoring defaults.

## Optional Network Requests

OpenTaskbar only makes outbound network connections when you enable or trigger specific features:

- **Weather widget:** Queries the public Open-Meteo API with your configured city name (or optional Windows location lookup) to display current weather and forecast data.
- **AI usage widgets (optional):** Only when explicitly enabled by you, reads local provider credentials or queries the corresponding provider's API directly from your machine to display your own quota. Credentials are never sent to the developer. Only the providers you configure are queried, and all provider labels are local UI strings.
- **Portable update checks (GitHub builds only):** Non-Store portable builds may query the public GitHub Releases API to check for newer releases. Microsoft Store builds rely exclusively on the Microsoft Store for updates.

## Permissions

OpenTaskbar runs as a full-trust desktop application (`runFullTrust`) solely to manage the Windows taskbar/AppBar work area, display open application windows, host system tray icons, and render widgets such as audio, media, battery, network speed, and quick settings.

## Contact

For questions or privacy requests, please open an issue on the project repository:
https://github.com/alztrk/opentaskbar/issues
