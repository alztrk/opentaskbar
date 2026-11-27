# Security Policy

## Supported Versions

Only the current release line of the Rust workspace receives security fixes.

| Version | Supported |
| ------- | --------- |
| 0.4.x   | :white_check_mark: |
| < 0.3.5 | :x: |

## Reporting a Vulnerability

The OpenTaskbar project takes security and user privacy seriously. If you discover a security vulnerability, please report it privately rather than opening a public issue.

### Preferred Reporting Method

Use **[GitHub Private Vulnerability Reporting](https://github.com/alztrk/opentaskbar/security/advisories/new)**:

1. Navigate to the **Security** tab of the repository.
2. Click **Report a vulnerability** under Security Advisories.
3. Provide a detailed summary, reproduction steps and potential impact.

### What to Include in Your Report

- A clear description of the vulnerability and its potential impact.
- Step-by-step instructions or a proof of concept.
- The OpenTaskbar version, Windows build number and CPU architecture.
- Any mitigation or patch suggestions you have.

Please do not include live credentials, tokens or personal data in a report.

### Response Timeline

- **Initial response:** we aim to acknowledge receipt within 48 hours.
- **Triage and status updates:** we will keep you informed of validation progress and the intended remediation timeline.
- **Coordinated disclosure:** we ask maintainers and reporters to allow reasonable time to address the issue before public disclosure.

## Scope and Hardening Notes

- The application is Rust-only: `crates/opentaskbar-core` (settings, paths, i18n), `crates/opentaskbar-shell` (taskbar journal, AppBar, watchdog) and `crates/opentaskbar-app` (GPUI UI and Win32 layer). Reports about the root workspace and these crates are in scope.
- Provider credentials live only in local CLI/keyring stores or DPAPI-protected files under `%LOCALAPPDATA%\OpenTaskbar\secrets\`. They are never logged, passed on a command line or exported in backups.
- Approved custom widgets can only request DPAPI-encrypted secrets in their own `widget:<slug>:<key>` namespace, and only for keys declared in their manifest; requests for other names are refused. Re-approving an edited manifest restarts the sidecar with the new code.
- Theme/backup imports reject path traversal, absolute paths and unexpected entries; widget token overrides are capped and typed.
- Release artifacts (`OpenTaskbar-v0.3.5-win-{x64,x86}.exe/.zip`) are covered by generated `checksums.txt`, a schemaVersion 2 `release-manifest.json`, SLSA provenance via `actions/attest` and an optional report-only VirusTotal scan. Only downloads from the official [Releases page](https://github.com/alztrk/opentaskbar/releases) are supported.

General bug reports that are not security-sensitive belong in the [issue tracker](https://github.com/alztrk/opentaskbar/issues).
