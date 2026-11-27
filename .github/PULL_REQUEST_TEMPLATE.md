## Description

<!-- Please write your pull request title and description in English. -->
<!-- Provide a clear and concise summary of the changes introduced by this pull request. -->

## Related Issue

<!-- If applicable, link to the issue this pull request fixes or relates to: -->
<!-- e.g. Fixes #123, Relates to #456 -->

## Type of Change

<!-- Please check the options that are relevant. -->

- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature (non-breaking change which adds functionality)
- [ ] Performance improvement
- [ ] Refactoring (code structure improvement with no observable behavior change)
- [ ] UI / Design enhancement
- [ ] Localization / Translation
- [ ] Documentation update

## How Has This Been Tested?

<!-- Describe the tests and verification steps you ran to verify your changes. -->

- [ ] Automated tests: `cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
- [ ] Locale / release gates when relevant: `scripts/check-locale-parity.ps1`, `scripts/check-release-version.ps1`
- [ ] Manual verification on Windows 10 / Windows 11
- [ ] Multi-monitor verification (if applicable)
- [ ] Different DPI scale factors tested (100%, 125%, 150%) (if applicable)

**Manual Test Details:**
<!-- e.g. Opened Quick Settings, toggled media playback, verified that the dock adapts properly. -->

## Checklist

- [ ] My code follows the project's coding standards and conventions.
- [ ] All commit messages, comments, and PR descriptions are written in **English**.
- [ ] I have performed a self-review of my own code.
- [ ] New and existing tests pass locally with my changes.
- [ ] No secrets, tokens, personal data, or unrelated changes are included.
