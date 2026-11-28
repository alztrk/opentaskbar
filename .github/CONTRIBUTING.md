# Contributing to OpenTaskbar

Thank you for your interest in contributing to OpenTaskbar!

## Language Policy

- **All issues, pull requests, commit messages, code comments, and documentation must be written in English.**
- Localization files under `crates/opentaskbar-core/locales/` naturally contain translated user-facing text, but metadata, keys, and pull request descriptions discussing those translations must still be in English.

## Getting Started

### Prerequisites
- Windows 10 (version 19041 or higher) or Windows 11, x64 or x86
- Rust stable toolchain (`rustup`); the minimum version is declared in `Cargo.toml`
- PowerShell 7+ for the build/packaging scripts

### Building the Project
```pwsh
cargo build --workspace
```

To produce a portable EXE and verify it:
```pwsh
.\scripts\publish-portable.ps1 -Runtime win-x64
.\scripts\check-portable.ps1 -ExecutableName OpenTaskbar-x64.exe
```

### Running Tests
The deterministic behavior, contract and i18n checks live in the Rust workspace:
```pwsh
cargo test --workspace
```

Before submitting a pull request, also run the same gates as CI:
```pwsh
cargo check --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Locale and release changes have dedicated gates:
```pwsh
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-locale-parity.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-release-version.ps1 -Tag vX.Y.Z-dev
```

Ensure all checks pass before submitting a pull request. Live Windows shell behavior still needs a manual smoke run; see `docs/development/windows-reliability-audit.md` and `docs/development/windows-taskbar-model.md`.

## Development Guidelines

1. **Focused Changes:** Keep pull requests focused on a single topic. Avoid scope creep or bundling unrelated changes.
2. **Preserve Compatibility:** Do not introduce breaking configuration or storage format changes without migration safety. User data lives in the shared `%LOCALAPPDATA%\OpenTaskbar\` folder.
3. **Coding Standards:**
   - Follow existing architecture: `opentaskbar-core` (settings/i18n/paths), `opentaskbar-shell` (shell journal, taskbar, watchdog) and `opentaskbar-app` (GPUI UI and Win32 platform layer).
   - Use idiomatic Rust with workspace lint settings; keep platform P/Invoke code isolated in the app crate.
   - Write clear, meaningful comments in English explaining *why*, not just *what*.
   - Never commit credentials, personal data, or build artifacts.
4. **Localization:** Keep all 14 locale catalogs in sync; `scripts/check-locale-parity.ps1` and `crates/opentaskbar-core/tests/i18n_parity.rs` enforce key parity.

## Submitting a Pull Request

1. Fork the repository and create a branch for your fix or feature.
2. Verify that `cargo fmt`, `cargo clippy` and `cargo test --workspace` succeed, plus the portable EXE check when packaging changes.
3. Fill out the pull request template completely in **English**.
4. `master` is a protected branch: every change must go through a pull request with at least one approving review and a passing `Build & Test (Windows x64)` check. Do not push directly to `master`.
5. Keep your branch current before requesting review (`git fetch origin && git rebase origin/master`) so the branch is mergeable and CI runs against the latest base.
6. Once a pull request is merged, its head branch is deleted automatically (the repository has `delete_branch_on_merge` enabled). Do not leave merged branches behind: prune remote-tracking refs with `git fetch --prune` and remove local ones with `git branch -d <branch>`.
