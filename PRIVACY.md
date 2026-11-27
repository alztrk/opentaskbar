# Privacy Policy — OpenTaskbar

Last updated: 2026-11-27

OpenTaskbar is an open-source desktop dock and taskbar utility for Windows, built entirely in Rust. Privacy-respecting defaults are the rule: nothing is collected, sold or transmitted to the developer.

## Local storage only

Preferences, pinned apps, widget settings, profiles, themes and diagnostics stay on your device in the shared folder `%LOCALAPPDATA%\OpenTaskbar\`:

- `settings.json` preferences and pins; unknown keys are preserved on save and unreadable files are backed up before defaults are restored.
- `opentaskbar.log` diagnostics (rotated to `opentaskbar.log.old`).
- `Themes\` theme packs, `rust-cache\icon-cache\` extracted icons, and local caches such as `country-city-cache.json` and `wireless-battery-cache.json`.
- `secrets\*.bin` encrypted provider credentials, protected with Windows DPAPI and app-specific entropy.

There is no analytics, telemetry, usage tracking, crash reporting or advertising identifier. No secret, API key, token or password is printed to the log, passed on a process command line, or included in a `.otbackup` archive (which contains only settings, pins and theme files).

## Optional network requests

OpenTaskbar only makes outbound HTTPS requests when a relevant feature is enabled or used:

- **Weather widget — Open-Meteo:** city-name geocoding and forecast data are fetched from the public `geocoding-api.open-meteo.com` / `api.open-meteo.com` services. No API key is used. Your configured city and chosen units are sent as query parameters.
- **Location lookup — Windows location service and BigDataCloud:** if you allow location access, Windows may provide coordinates; reverse geocoding can call the public `api.bigdatacloud.net` endpoint to turn those coordinates into a city name, with an offline catalog fallback. Location is used only to pre-fill an empty weather city. Denying the Windows capability keeps the feature off.
- **AI usage widgets — provider APIs:** only when you enable a provider does the app read its locally discovered credential (CLI config/credentials files, the `gh` CLI keyring for Copilot, or DPAPI-encrypted files in `secrets\`) and query that provider's own API from your machine to display your quota. Credentials are never sent to the developer, never logged and never copied into backups. Providers you have not configured are not queried; a missing token simply leaves the widget in its sign-in state.
- **Update checks — GitHub Releases API (portable builds only):** the portable build may query `api.github.com/repos/alztrk/opentaskbar/releases/latest` for version information and, when you install an update, downloads the matching architecture asset over HTTPS with digest verification. Disable updates in Settings to stop this check. The updater stages files under `%TEMP%\OpenTaskbar\update`. Microsoft Store builds rely exclusively on the Microsoft Store for updates.
- **Net Speed test (user-triggered only):** the speed-test button in the Net Speed widget opens parallel HTTP streams to `speed.cloudflare.com` to measure download throughput and connection latency. No account or credential is used; nothing is uploaded.
- **Custom widgets (optional):** if you add a custom widget, OpenTaskbar runs the program in that widget's folder as a separate process (sidecar). Any network request it makes is defined by your own script, not by OpenTaskbar; the host never sends your widget data anywhere. Sidecars are not sandboxed in this version, so only install widgets from sources you trust. See [docs/custom-widgets.md](docs/custom-widgets.md).

No other network endpoints are contacted by the dock itself. A custom widget's sidecar is code you chose to install and is outside the dock's request policy.

## Permissions

OpenTaskbar runs as a full-trust desktop application (`runFullTrust`) to replace/hide the Explorer taskbar and reserve an AppBar work area, enumerate and activate windows, render a layered dock above them, support Start/Search/Control Center anchoring, and read local device state for widgets such as audio, media, battery, network speed and radios. Location access is opt-in through Windows. Radio toggles and Night Light use Windows APIs on your machine only.

## Contact

For questions or privacy requests, open an issue on the project repository:

https://github.com/alztrk/opentaskbar/issues
