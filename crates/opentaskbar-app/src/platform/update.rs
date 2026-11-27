//! GitHub release lookup and in-place updates (`UpdateChecker.cs` parity).
//!
//! Release metadata and the binary itself are fetched with WinHTTP. Installation is
//! gated on a GitHub `sha256:` asset digest, file size, and the PE `MZ` magic before a
//! self-deleting `apply-update.cmd` replaces the running executable.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::http::{self, Proxy};

pub const REPOSITORY: &str = "alztrk/opentaskbar";
pub const RELEASES_PAGE: &str = "https://github.com/alztrk/opentaskbar/releases/latest";
/// Single source of truth for the shipped version (also shown in Settings → About).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_METADATA_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub url: String,
    pub download_url: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

fn version_parts(text: &str) -> Option<[u64; 4]> {
    let clean = text.trim().trim_start_matches(['v', 'V']);
    let clean = clean.split(['-', '+']).next().unwrap_or(clean);
    // Match `Version::parse`: more than four components is not a valid release.
    if clean.split('.').count() > 4 {
        return None;
    }
    let mut parts = [0u64; 4];
    for (index, part) in clean.split('.').take(4).enumerate() {
        parts[index] = part.parse().ok()?;
    }
    Some(parts)
}

fn is_newer(candidate: &str) -> bool {
    match (version_parts(candidate), version_parts(CURRENT_VERSION)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Streams a GET response into `sink` and returns the HTTP status code.
///
/// Release binaries are ~10+ MB, so send/receive keep the full five-minute
/// budget while resolve/connect stay short. The shared core owns the WinHTTP
/// handles; only the caller-specific policy is configured here.
fn http_get<F: FnMut(&[u8])>(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    sink: F,
) -> Option<u32> {
    let mut cfg = http::HttpCfg::new(format!("OpenTaskbar/{CURRENT_VERSION}"));
    cfg.timeouts = [5_000, 5_000, 300_000, 300_000];
    cfg.headers = headers
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    http::request(&cfg, host, path, "GET", sink)
}

fn select_asset<'a>(assets: &'a [Value], architecture: &str) -> Option<&'a Value> {
    let suffix = format!("-{architecture}.exe");
    let marker = format!("-win-{architecture}.");
    let mut first_exe = None;
    let mut legacy = None;
    let mut matching = None;
    for asset in assets {
        let Some(name) = asset.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.eq_ignore_ascii_case("OpenTaskbar.exe") && legacy.is_none() {
            legacy = Some(asset);
        }
        if !name.to_ascii_lowercase().ends_with(".exe") {
            continue;
        }
        if first_exe.is_none() {
            first_exe = Some(asset);
        }
        if matching.is_none()
            && (name.to_ascii_lowercase().ends_with(&suffix)
                || name.to_ascii_lowercase().contains(&marker))
        {
            matching = Some(asset);
        }
    }
    matching.or(legacy).or(first_exe)
}

/// Parses a GitHub release JSON document and returns it only when it is newer.
pub fn parse_release(json: &str) -> Option<Release> {
    let root: Value = serde_json::from_str(json).ok()?;
    let tag = root
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if !is_newer(&tag) {
        return None;
    }
    let url = root
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or(RELEASES_PAGE)
        .to_owned();
    let architecture = if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "x86"
    };
    let asset = root
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| select_asset(assets, architecture));
    Some(Release {
        version: tag
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or(&tag)
            .to_owned(),
        tag,
        url,
        download_url: asset
            .and_then(|a| a.get("browser_download_url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        size: asset.and_then(|a| a.get("size")).and_then(Value::as_u64),
        sha256: asset
            .and_then(|a| a.get("digest"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// `Ok(None)` means up to date; `Err(())` means GitHub could not be reached.
///
/// Packaged/MSIX builds are intentionally short-circuited: their updates are
/// delivered by the Store, and `GetCurrentPackageFullName` reports success with
/// `ERROR_INSUFFICIENT_BUFFER (122)` on the length query.
pub fn find_newer() -> Result<Option<Release>, ()> {
    find_newer_packaged_guard()
}

/// Packaged-build guard used by `find_newer()`.
pub fn find_newer_packaged_guard() -> Result<Option<Release>, ()> {
    find_newer_with_packaged(!supports_in_app_updates())
}

/// Testable core of the packaged guard: a Store build always reports "no
/// update" without touching the network.
fn find_newer_with_packaged(packaged: bool) -> Result<Option<Release>, ()> {
    if packaged {
        return Ok(None);
    }
    find_newer_github()
}

fn find_newer_github() -> Result<Option<Release>, ()> {
    let mut body = Vec::new();
    let status = http_get(
        "api.github.com",
        &format!("/repos/{REPOSITORY}/releases/latest"),
        &[
            ("User-Agent", &format!("OpenTaskbar/{CURRENT_VERSION}")),
            ("Accept", "application/vnd.github+json"),
        ],
        |chunk| {
            if body.len() < MAX_METADATA_BYTES {
                let remaining = MAX_METADATA_BYTES - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
        },
    )
    .ok_or(())?;
    if !(200..300).contains(&status) {
        return Err(());
    }
    let json = String::from_utf8(body).map_err(|_| ())?;
    Ok(parse_release(&json))
}

fn valid_digest(digest: Option<&str>) -> bool {
    is_valid_sha256_digest(digest)
}

fn escape_batch(value: &str) -> Result<String, &'static str> {
    escape_batch_value(value).map_err(|_| "ErrorUpdatePath")
}

fn updater_script(source: &str, target: &str) -> Result<String, &'static str> {
    generate_updater_script(Path::new(source), Path::new(target))
}

/// `UpdateChecker` uses `%TEMP%\OpenTaskbar\update`, not LOCALAPPDATA. The
/// shared `paths` contract has no updater helper, so this literal stays here.
fn update_dir() -> PathBuf {
    std::env::temp_dir().join("OpenTaskbar").join("update")
}

/// Constant-time digest comparison: the number of compared bytes never depends
/// on where the first mismatch occurs.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn hex_to_bytes(text: &str) -> Option<Vec<u8>> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_dir_matches_the_reference_temp_folder() {
        assert_eq!(
            update_dir(),
            std::env::temp_dir().join("OpenTaskbar").join("update")
        );
        assert_ne!(
            update_dir(),
            opentaskbar_core::paths::shared_app_data_dir().join("update")
        );
    }

    #[test]
    fn parses_newer_release_and_selects_arch_asset() {
        let json = r#"{
          "tag_name":"v0.4.1",
          "html_url":"https://github.com/alztrk/opentaskbar/releases/tag/v0.4.1",
          "assets":[
            {"name":"OpenTaskbar-x86.exe","browser_download_url":"https://x/86.exe","size":123,"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            {"name":"OpenTaskbar-x64.exe","browser_download_url":"https://x/64.exe","size":456,"digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
          ]
        }"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.version, "0.4.1");
        let expected = if cfg!(target_arch = "x86_64") {
            "https://x/64.exe"
        } else {
            "https://x/86.exe"
        };
        assert_eq!(release.download_url.as_deref(), Some(expected));
    }

    #[test]
    fn ignores_older_or_invalid_versions() {
        assert!(parse_release(r#"{"tag_name":"v0.1.0"}"#).is_none());
        assert!(parse_release(r#"{"tag_name":"nightly"}"#).is_none());
    }

    #[test]
    fn rejects_invalid_digests_and_batch_values() {
        assert!(!valid_digest(Some("sha256:abc")));
        assert!(!valid_digest(None));
        assert!(valid_digest(Some(
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        )));
        assert_eq!(
            escape_batch("C:\\path\\100%\\app.exe").unwrap(),
            "C:\\path\\100%%\\app.exe"
        );
        assert!(escape_batch("bad\"quote").is_err());
    }
}

// ---- reference update-checker parity (WS-F) ----

pub const USER_AGENT: &str = "OpenTaskbar";

/// GitHub releases page (WS-F helper; `RELEASES_PAGE` is the canonical constant).
pub fn releases_page() -> String {
    format!("https://github.com/{REPOSITORY}/releases/latest")
}

/// Semver-like version with the same normalization as `Version` (missing parts become 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub revision: u32,
}

impl Version {
    pub fn parse(tag: &str) -> Option<Self> {
        let tag = tag
            .trim()
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or_default();
        let parts: Vec<u32> = tag
            .split('.')
            .map(|part| part.parse::<u32>().ok())
            .collect::<Option<Vec<_>>>()?;
        if parts.is_empty() || parts.len() > 4 {
            return None;
        }
        Some(Self {
            major: parts[0],
            minor: *parts.get(1).unwrap_or(&0),
            build: *parts.get(2).unwrap_or(&0),
            revision: *parts.get(3).unwrap_or(&0),
        })
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or_default()
    }

    pub fn padded(self) -> (u32, u32, u32, u32) {
        (self.major, self.minor, self.build, self.revision)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.revision
        )
    }
}

/// `Release` view: the same release with a parsed [`Version`] and an `i64` size.
///
/// The canonical [`Release`] keeps the String version consumed by the settings UI; use
/// `Release::from(detailed)` (or [`DetailedRelease`]) for the typed WS-F shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailedRelease {
    pub version: Version,
    pub tag: String,
    pub url: String,
    pub download_url: Option<String>,
    pub size: Option<i64>,
    pub sha256: Option<String>,
}

/// Builds the typed release shape from the canonical release used by the UI.
impl From<&Release> for DetailedRelease {
    fn from(release: &Release) -> Self {
        Self {
            version: Version::parse(&release.version).unwrap_or_default(),
            tag: release.tag.clone(),
            url: release.url.clone(),
            download_url: release.download_url.clone(),
            size: release.size.map(|value| value as i64),
            sha256: release.sha256.clone(),
        }
    }
}

impl Release {
    /// Typed view of the canonical String version.
    pub fn version_struct(&self) -> Option<Version> {
        Version::parse(&self.version)
    }

    /// WS-F comparison against a parsed current version.
    pub fn is_newer_than(&self, current: Version) -> bool {
        self.version_struct()
            .is_some_and(|version| version.padded() > current.padded())
    }
}

impl DetailedRelease {
    pub fn is_newer_than(&self, current: Version) -> bool {
        self.version.padded() > current.padded()
    }
}

impl From<DetailedRelease> for Release {
    fn from(value: DetailedRelease) -> Self {
        Self {
            version: value.version.to_string(),
            tag: value.tag,
            url: value.url,
            download_url: value.download_url,
            size: value.size.map(|size| size.max(0) as u64),
            sha256: value.sha256,
        }
    }
}

/// `GetCurrentPackageFullName` status codes used by the packaged guard.
pub const ERROR_SUCCESS_I32: i32 = 0;
/// Returned with a null buffer when the caller *is* packaged: the buffer is too
/// small, but the length query itself succeeded. Treating this as "unpackaged"
/// was the original bug that let Store builds run the GitHub updater.
pub const ERROR_INSUFFICIENT_BUFFER_I32: i32 = 122;
/// `APPMODEL_ERROR_NO_PACKAGE`: the caller is a portable build.
pub const APPMODEL_ERROR_NO_PACKAGE_I32: i32 = 15700;

/// Pure classifier for the `GetCurrentPackageFullName` status code.
pub fn status_means_packaged(status: i32) -> bool {
    status == ERROR_SUCCESS_I32 || status == ERROR_INSUFFICIENT_BUFFER_I32
}

/// Detects an MSIX/Store install via `GetCurrentPackageFullName` (kernel32, dynamically loaded).
pub fn is_packaged() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let kernel = "kernel32.dll\0".encode_utf16().collect::<Vec<_>>();
        let function = b"GetCurrentPackageFullName\0";
        // SAFETY: LoadLibraryW/GetProcAddress are safe with null-terminated literals; the
        // function pointer signature matches GetCurrentPackageFullName(UINT32*, PWSTR).
        unsafe {
            let module = LoadLibraryW(kernel.as_ptr());
            if module.is_null() {
                return false;
            }
            let Some(address) = GetProcAddress(module, function.as_ptr().cast()) else {
                return false;
            };
            let get: unsafe extern "system" fn(*mut u32, *mut u16) -> i32 =
                std::mem::transmute(address);
            let mut length: u32 = 0;
            // With a null buffer a packaged process gets 122 (ERROR_INSUFFICIENT_BUFFER),
            // while a portable process gets 15700 (APPMODEL_ERROR_NO_PACKAGE).
            status_means_packaged(get(&mut length, std::ptr::null_mut()))
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Store-managed installs have no in-app updater.
pub fn supports_in_app_updates() -> bool {
    !is_packaged()
}

fn architecture() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "x86"
    }
}

/// Parses a GitHub `releases/latest` response; returns `None` when not newer.
pub fn parse_release_detailed(
    json: &str,
    current: Version,
    arch: Option<&str>,
) -> Option<DetailedRelease> {
    let root: Value = serde_json::from_str(json).ok()?;
    let tag = root
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let version = Version::parse(&tag)?;
    let url = root
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(releases_page);
    let (download_url, size, sha256) = root
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| select_asset(assets, arch.unwrap_or(architecture())))
        .map(|asset| {
            (
                asset
                    .get("browser_download_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                asset.get("size").and_then(Value::as_i64),
                asset
                    .get("digest")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
        })
        .unwrap_or((None, None, None));
    let release = DetailedRelease {
        version,
        tag,
        url,
        download_url,
        size,
        sha256,
    };
    release.is_newer_than(current).then_some(release)
}

/// Running executable path, with a fallback to the current process module path.
pub fn get_executable_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .filter(|path| path.is_file())
        .or_else(|| {
            #[cfg(windows)]
            {
                use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
                let mut buffer = [0u16; 32768];
                // SAFETY: the buffer is large enough for any Windows path; null handle means self.
                let len = unsafe {
                    GetModuleFileNameW(
                        std::ptr::null_mut(),
                        buffer.as_mut_ptr(),
                        buffer.len() as u32,
                    )
                } as usize;
                if len > 0 && len < buffer.len() {
                    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
                    path.is_file().then_some(path)
                } else {
                    None
                }
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub success: bool,
    pub error_key: Option<&'static str>,
    pub http_status: Option<i32>,
}

impl UpdateOutcome {
    fn failure(error_key: &'static str, http_status: Option<i32>) -> Self {
        Self {
            success: false,
            error_key: Some(error_key),
            http_status,
        }
    }
}

/// Downloads and applies an update. Successful application spawns the updater; call
/// `exit_for_update()` afterwards to release the running executable.
pub fn download_and_apply_update(release: &DetailedRelease) -> UpdateOutcome {
    apply_update(release, false)
}

/// Same as [`download_and_apply_update`] but always launches the copy/restart
/// script through `ShellExecuteW("runas")`, even when the install directory
/// happens to be writable by the current user.
pub fn apply_with_elevation(release: &DetailedRelease) -> UpdateOutcome {
    apply_update(release, true)
}

fn apply_update(release: &DetailedRelease, force_elevation: bool) -> UpdateOutcome {
    if !supports_in_app_updates() {
        return UpdateOutcome::failure("UpdateFailed", None);
    }
    let Some(target) = get_executable_path() else {
        return UpdateOutcome::failure("ErrorUpdatePath", None);
    };
    let Some(url) = release.download_url.as_deref() else {
        let _ = std::process::Command::new("cmd.exe")
            .args(["/c", "start", "", &release.url])
            .spawn();
        return UpdateOutcome::failure("ErrorUpdateBinaryMissing", None);
    };
    if !is_valid_sha256_digest(release.sha256.as_deref()) {
        return UpdateOutcome::failure("ErrorUpdateExecutableInvalid", None);
    }

    let temp_dir = std::env::temp_dir().join("OpenTaskbar").join("update");
    if fs::create_dir_all(&temp_dir).is_err() {
        return UpdateOutcome::failure("ErrorUpdateFailed", None);
    }
    let temp_exe = temp_dir.join("OpenTaskbar.exe");
    let part = temp_dir.join("OpenTaskbar.exe.part");
    let _ = fs::remove_file(&part);

    let (status, bytes) = match http_get_detailed(url) {
        Ok(result) => result,
        Err(_) => return UpdateOutcome::failure("ErrorUpdateFailed", None),
    };
    if !(200..300).contains(&status) {
        return UpdateOutcome::failure("ErrorUpdateHttp", Some(i32::from(status)));
    }
    if bytes.len() < 1_000_000 {
        return UpdateOutcome::failure("ErrorUpdateSizeInvalid", None);
    }
    if release
        .size
        .is_some_and(|expected| expected > 0 && bytes.len() as i64 != expected)
    {
        let _ = fs::remove_file(&part);
        return UpdateOutcome::failure("ErrorUpdateSizeMismatch", None);
    }
    if bytes.len() < 2 || bytes[0] != b'M' || bytes[1] != b'Z' {
        let _ = fs::remove_file(&part);
        return UpdateOutcome::failure("ErrorUpdateExecutableInvalid", None);
    }
    let Some(expected) = release.sha256.as_deref().and_then(|value| value.get(7..)) else {
        return UpdateOutcome::failure("ErrorUpdateExecutableInvalid", None);
    };
    let expected = (0..expected.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&expected[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>();
    if expected
        .map(|expected| !constant_time_eq(&sha256(&bytes), &expected))
        .unwrap_or(true)
    {
        let _ = fs::remove_file(&part);
        return UpdateOutcome::failure("ErrorUpdateExecutableInvalid", None);
    }

    if fs::write(&part, &bytes).is_err() {
        return UpdateOutcome::failure("ErrorUpdateFailed", None);
    }
    let _ = fs::remove_file(&temp_exe);
    if fs::rename(&part, &temp_exe).is_err() {
        return UpdateOutcome::failure("ErrorUpdateFailed", None);
    }
    if !launch_updater(&temp_exe, &target, force_elevation) {
        return UpdateOutcome::failure("ErrorUpdateFailed", None);
    }
    UpdateOutcome {
        success: true,
        error_key: None,
        http_status: None,
    }
}

pub fn is_valid_sha256_digest(digest: Option<&str>) -> bool {
    match digest {
        Some(value) => {
            value.len() == 71
                && value
                    .get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sha256:"))
                && value
                    .get(7..)
                    .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
        }
        None => false,
    }
}

/// Rejects control characters, quotes and newlines in batch paths (`EscapeBatchValue`).
pub fn escape_batch_value(value: &str) -> Result<String, &'static str> {
    if value.trim().is_empty() {
        return Err("Updater path must not be empty.");
    }
    if value
        .chars()
        .any(|c| c.is_control() || matches!(c, '\r' | '\n' | '"' | '\0'))
    {
        return Err("Updater path contains unsupported control or quote characters.");
    }
    Ok(value.replace('%', "%%"))
}

/// `GenerateUpdaterScript`: no BOM (a BOM corrupts `@echo off` on line 1).
pub fn generate_updater_script(temp_exe: &Path, target_exe: &Path) -> Result<String, &'static str> {
    let src = escape_batch_value(temp_exe.to_string_lossy().as_ref())?;
    let dst = escape_batch_value(target_exe.to_string_lossy().as_ref())?;
    Ok([
        "@echo off",
        "chcp 65001 >nul",
        &format!("set \"SRC={src}\""),
        &format!("set \"DST={dst}\""),
        "set ATTEMPTS=0",
        ":wait_loop",
        "timeout /t 1 /nobreak >nul",
        "copy /y \"%SRC%\" \"%DST%\" >nul 2>&1",
        "if not errorlevel 1 if exist \"%DST%\" goto launch",
        "set /a ATTEMPTS+=1",
        "if %ATTEMPTS% lss 30 goto wait_loop",
        "goto cleanup",
        "",
        ":launch",
        "del /f /q \"%SRC%\" >nul 2>&1",
        "start \"\" \"%DST%\"",
        "",
        ":cleanup",
        "del /f /q \"%~f0\" >nul 2>&1",
        "",
    ]
    .join("\r\n"))
}

/// Writes the batch script as plain UTF-8 bytes (no BOM: a BOM breaks `@echo off` on line 1).
pub fn write_updater_script(
    path: &Path,
    temp_exe: &Path,
    target_exe: &Path,
) -> Result<(), &'static str> {
    let script = generate_updater_script(temp_exe, target_exe)?;
    fs::write(path, script).map_err(|_| "ErrorUpdateFailed")
}

fn is_directory_writable(dir: &Path) -> bool {
    let probe = dir.join(format!("_update_test_{}.tmp", std::process::id()));
    let ok = fs::write(&probe, b"test").is_ok();
    let _ = fs::remove_file(probe);
    ok
}

/// Writes the script, spawns it (elevated with `runas` when needed), returns success.
pub fn launch_updater_and_restart(temp_exe: &Path, target: &Path) -> bool {
    launch_updater(temp_exe, target, false)
}

/// Always elevates the updater via `ShellExecuteW("runas")`.
pub fn launch_updater_elevated(temp_exe: &Path, target: &Path) -> bool {
    launch_updater(temp_exe, target, true)
}

fn launch_updater(temp_exe: &Path, target: &Path, force_elevation: bool) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    let Some(script_dir) = temp_exe.parent() else {
        return false;
    };
    let script = script_dir.join("apply-update.cmd");
    if write_updater_script(&script, temp_exe, target).is_err() {
        return false;
    }
    let requires_elevation = force_elevation || !is_directory_writable(parent);
    #[cfg(windows)]
    let spawned = if requires_elevation {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let verb: Vec<u16> = "runas\0".encode_utf16().collect();
        let file: Vec<u16> = "cmd.exe\0".encode_utf16().collect();
        let mut arguments = "/c \"".to_owned();
        arguments.push_str(&script.to_string_lossy());
        arguments.push('"');
        let args: Vec<u16> = format!("{arguments}\0").encode_utf16().collect();
        // SAFETY: all UTF-16 buffers are null-terminated and remain valid during the call.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                args.as_ptr(),
                std::ptr::null(),
                0, // SW_HIDE
            ) as isize
                > 32
        }
    } else {
        std::process::Command::new("cmd.exe")
            .arg("/c")
            .arg(&script)
            .spawn()
            .is_ok()
    };
    #[cfg(not(windows))]
    let spawned = !requires_elevation
        && std::process::Command::new("cmd.exe")
            .arg("/c")
            .arg(&script)
            .spawn()
            .is_ok();

    if spawned {
        let _ = fs::remove_file(temp_exe.with_extension("exe.part"));
    }
    spawned
}

/// Releases the running executable after a successful update launch. WS-B/lead owns graceful
/// shutdown; call this from the app's update handler, not from a background worker thread.
pub fn exit_for_update() -> ! {
    std::process::exit(0);
}

/// Minimal synchronous HTTPS GET via the shared WinHTTP core (status + body).
fn http_get_detailed(url: &str) -> Result<(u16, Vec<u8>), String> {
    #[cfg(windows)]
    {
        // The old caller passed an `Accept` header pointer with length 0, which
        // WinHTTP ignores, so no header is configured here either.
        let mut cfg = http::HttpCfg::new(format!("{USER_AGENT}/{}", Version::current()));
        cfg.proxy = Proxy::Default;
        let (status, body) = http::get_body(&cfg, url).ok_or("WinHttp request failed")?;
        Ok((status as u16, body))
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err("HTTPS is only available on Windows".into())
    }
}

/// SHA-256 of release bytes, used to verify GitHub asset digests.
pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

#[cfg(test)]
mod parity_tests {
    use super::*;

    #[test]
    fn packaged_guard_accepts_insufficient_buffer() {
        assert!(status_means_packaged(ERROR_SUCCESS_I32));
        // A null-buffer length query succeeds with 122 for packaged processes.
        assert!(status_means_packaged(ERROR_INSUFFICIENT_BUFFER_I32));
        assert!(!status_means_packaged(APPMODEL_ERROR_NO_PACKAGE_I32));
        assert!(!status_means_packaged(5));
        assert_eq!(find_newer_with_packaged(true), Ok(None));
    }

    #[test]
    fn digest_comparison_is_exact_and_constant_time() {
        let expected = [0xAAu8, 0xBB, 0xCC, 0xDD];
        assert!(constant_time_eq(&expected, &[0xAA, 0xBB, 0xCC, 0xDD]));
        assert!(!constant_time_eq(&expected, &[0xAA, 0xBB, 0xCC, 0xDE]));
        assert!(!constant_time_eq(&expected, &[0xAA, 0xBB, 0xCC]));
        assert!(!constant_time_eq(&expected, &[]));
    }

    #[test]
    fn version_parsing_and_comparison() {
        assert_eq!(
            Version::parse("v1.2.3-beta.1"),
            Some(Version {
                major: 1,
                minor: 2,
                build: 3,
                revision: 0
            })
        );
        assert_eq!(
            Version::parse("2.0"),
            Some(Version {
                major: 2,
                minor: 0,
                build: 0,
                revision: 0
            })
        );
        assert_eq!(Version::parse("nope"), None);
        // Both comparators must agree that a five-component tag is invalid.
        assert_eq!(version_parts("1.2.3.4.5"), None);
        assert_eq!(Version::parse("1.2.3.4.5"), None);
        assert_eq!(version_parts("v1.3"), Some([1, 3, 0, 0]));
        assert!(Version::parse("1.3.0").unwrap() > Version::parse("1.2.9").unwrap());
    }

    #[test]
    fn parses_release_and_selects_arch_asset() {
        let json = r#"{
            "tag_name":"v9.8.7","html_url":"https://example.com/release",
            "assets":[
                {"name":"OpenTaskbar.exe","browser_download_url":"https://e/legacy","size":1000},
                {"name":"OpenTaskbar-x64.exe","browser_download_url":"https://e/x64","size":2000000,"digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}
            ]
        }"#;
        let release = parse_release_detailed(json, Version::current(), Some("x64")).unwrap();
        assert_eq!(release.download_url.as_deref(), Some("https://e/x64"));
        assert_eq!(
            release.version,
            Version {
                major: 9,
                minor: 8,
                build: 7,
                revision: 0
            }
        );
        assert!(
            parse_release_detailed(
                json,
                Version {
                    major: 99,
                    ..Version::default()
                },
                None
            )
            .is_none()
        );
    }

    #[test]
    fn batch_script_is_safe_and_bom_free() {
        assert!(escape_batch_value("C:\\a\"b").is_err());
        assert_eq!(escape_batch_value("C:\\100%.exe").unwrap(), "C:\\100%%.exe");
        let script = generate_updater_script(
            Path::new("C:\\temp\\new.exe"),
            Path::new("C:\\app\\OpenTaskbar.exe"),
        )
        .unwrap();
        assert!(script.starts_with("@echo off\r\n"));
        assert!(script.contains("set \"SRC=C:\\temp\\new.exe\""));
        assert!(script.contains("goto launch"));
        assert!(is_valid_sha256_digest(Some(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )));
        assert!(!is_valid_sha256_digest(Some("md5:abc")));
    }

    #[test]
    fn sha256_matches_known_vector() {
        let digest = sha256(b"abc");
        assert_eq!(
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn digest_validation_never_panics_on_multibyte_input() {
        // 71 bytes total, but byte 7 lands inside a 4-byte character: get(..7) must return None.
        let split_character = format!("😀{}", "a".repeat(67));
        assert!(!is_valid_sha256_digest(Some(&split_character)));
        assert!(!valid_digest(Some(&split_character)));
        // ASCII prefix, but the 64-byte tail is multi-byte UTF-8, not hex digits.
        let multibyte_tail = format!("sha256:{}", "ç".repeat(32));
        assert_eq!(multibyte_tail.len(), 71);
        assert!(!is_valid_sha256_digest(Some(&multibyte_tail)));
        assert!(!valid_digest(Some(&multibyte_tail)));
        assert!(is_valid_sha256_digest(Some(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )));
    }

    #[test]
    fn write_updater_script_handles_unicode_spaces_and_percent() {
        let dir =
            std::env::temp_dir().join(format!("opentaskbar-update-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let script_path = dir.join("apply-update.cmd");
        let temp_exe =
            Path::new(r"C:\Users\agnes\AppData\Local\Temp\Yeni Güncelleme %1\OpenTaskbar.exe");
        let target_exe = Path::new(r"C:\Program Files\OpenTaskbar %D\OpenTaskbar.exe");
        write_updater_script(&script_path, temp_exe, target_exe).unwrap();
        let bytes = fs::read(&script_path).unwrap();
        let _ = fs::remove_file(&script_path);
        let _ = fs::remove_dir(&dir);
        assert!(
            !bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
            "the .cmd file must not start with a UTF-8 BOM"
        );
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("@echo off\r\n"));
        assert!(text.contains("chcp 65001 >nul"));
        assert!(text.contains(
            r#"set "SRC=C:\Users\agnes\AppData\Local\Temp\Yeni Güncelleme %%1\OpenTaskbar.exe""#
        ));
        assert!(text.contains(r#"set "DST=C:\Program Files\OpenTaskbar %%D\OpenTaskbar.exe""#));
        assert!(text.contains("goto launch"));
    }
}
