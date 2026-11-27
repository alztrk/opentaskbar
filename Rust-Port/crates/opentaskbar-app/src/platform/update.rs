//! GitHub release lookup and in-place updates (C# `UpdateChecker.cs` parity).
//!
//! Release metadata and the binary itself are fetched with WinHTTP. Installation is
//! gated on a GitHub `sha256:` asset digest, file size, and the PE `MZ` magic before a
//! self-deleting `apply-update.cmd` replaces the running executable.

use std::fs;
use std::io::Write;
use std::mem::size_of;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};

pub const REPOSITORY: &str = "alztrk/opentaskbar";
pub const RELEASES_PAGE: &str = "https://github.com/alztrk/opentaskbar/releases/latest";
/// Single source of truth for the shipped version (also shown in Settings ΓåÆ About).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const MIN_BINARY_BYTES: u64 = 1_000_000;
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

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn version_parts(text: &str) -> Option<[u64; 4]> {
    let clean = text.trim().trim_start_matches(['v', 'V']);
    let clean = clean.split(['-', '+']).next().unwrap_or(clean);
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
fn http_get<F: FnMut(&[u8])>(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    mut sink: F,
) -> Option<u32> {
    let agent = wide(&format!("OpenTaskbar/{CURRENT_VERSION}"));
    let host_w = wide(host);
    let verb_w = wide("GET");
    let path_w = wide(path);
    unsafe {
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        // Release binaries are ~10+ MB; a slow connection must not abort the
        // download after 20 seconds. Resolve/connect stay short, send/receive
        // allow the full five-minute download budget.
        WinHttpSetTimeouts(session, 5_000, 5_000, 300_000, 300_000);
        let connect = WinHttpConnect(session, host_w.as_ptr(), 443, 0);
        if connect.is_null() {
            WinHttpCloseHandle(session);
            return None;
        }
        let request = WinHttpOpenRequest(
            connect,
            verb_w.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            return None;
        }
        if !headers.is_empty() {
            let header_text = headers
                .iter()
                .map(|(key, value)| format!("{key}: {value}\r\n"))
                .collect::<String>();
            let header_wide: Vec<u16> = header_text.encode_utf16().collect();
            WinHttpAddRequestHeaders(
                request,
                header_wide.as_ptr(),
                header_wide.len() as u32,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            );
        }
        let sent = WinHttpSendRequest(request, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) != 0
            && WinHttpReceiveResponse(request, std::ptr::null_mut()) != 0;
        let result = if sent {
            let mut status = 0u32;
            let mut status_size = size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                request,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut status_size,
                std::ptr::null_mut(),
            );
            loop {
                let mut available = 0u32;
                if WinHttpQueryDataAvailable(request, &mut available) == 0 || available == 0 {
                    break;
                }
                let mut buffer = vec![0u8; available as usize];
                let mut read = 0u32;
                if WinHttpReadData(request, buffer.as_mut_ptr().cast(), available, &mut read) == 0
                    || read == 0
                {
                    break;
                }
                buffer.truncate(read as usize);
                sink(&buffer);
            }
            Some(status)
        } else {
            None
        };
        WinHttpCloseHandle(request);
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);
        result
    }
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

/// Packaged-build guard used by `find_newer()` (also callable directly by WS-8).
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

fn update_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("OpenTaskbar")
        .join("update")
}

fn split_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, _) = rest.split_once('/')?;
    Some((host, &rest[host.len()..]))
}

/// Downloads, verifies and schedules the replacement. Returns an error key on failure.
pub fn download_and_apply(release: &Release) -> Result<(), &'static str> {
    // The batch script retries the copy until the running dock releases the
    // executable. Refusing while the dock is alive meant an update could never
    // be applied from the Settings UI, so we schedule it and let the caller
    // exit normally (see `exit_for_update`).
    let target = std::env::current_exe().map_err(|_| "ErrorUpdatePath")?;
    let file_name = target.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if file_name.eq_ignore_ascii_case("dotnet.exe") {
        return Err("ErrorUpdateDeveloperMode");
    }
    // A read-only install directory forces the elevated updater path.
    let requires_elevation = target
        .parent()
        .is_none_or(|parent| !is_directory_writable(parent));
    let digest = match valid_digest(release.sha256.as_deref()) {
        true => release.sha256.as_ref().unwrap().clone(),
        false => return Err("ErrorUpdateExecutableInvalid"),
    };
    let (host, path) = split_url(release.download_url.as_deref().unwrap_or(""))
        .ok_or("ErrorUpdateBinaryMissing")?;

    let dir = update_dir();
    fs::create_dir_all(&dir).map_err(|_| "ErrorUpdateFailed")?;
    let part = dir.join("OpenTaskbar.exe.part");
    let temp_exe = dir.join("OpenTaskbar.exe");
    let _ = fs::remove_file(&part);

    let mut file = fs::File::create(&part).map_err(|_| "ErrorUpdateFailed")?;
    let mut write_failed = false;
    let status = http_get(
        host,
        path,
        &[("User-Agent", &format!("OpenTaskbar/{CURRENT_VERSION}"))],
        |chunk| {
            if file.write_all(chunk).is_err() {
                write_failed = true;
            }
        },
    )
    .ok_or("ErrorUpdateHttp")?;
    drop(file);
    if write_failed {
        let _ = fs::remove_file(&part);
        return Err("ErrorUpdateFailed");
    }
    if !(200..300).contains(&status) {
        let _ = fs::remove_file(&part);
        return Err("ErrorUpdateHttp");
    }

    let bytes = fs::read(&part).map_err(|_| "ErrorUpdateFailed")?;
    if (bytes.len() as u64) < MIN_BINARY_BYTES {
        let _ = fs::remove_file(&part);
        return Err("ErrorUpdateSizeInvalid");
    }
    if release
        .size
        .is_some_and(|expected| expected > 0 && expected != bytes.len() as u64)
    {
        let _ = fs::remove_file(&part);
        return Err("ErrorUpdateSizeMismatch");
    }
    if bytes[0] != b'M' || bytes[1] != b'Z' {
        let _ = fs::remove_file(&part);
        return Err("ErrorUpdateExecutableInvalid");
    }
    let expected = digest
        .get(7..)
        .and_then(hex_to_bytes)
        .ok_or("ErrorUpdateExecutableInvalid")?;
    let actual = Sha256::digest(&bytes);
    if !constant_time_eq(actual.as_slice(), &expected) {
        let _ = fs::remove_file(&part);
        return Err("ErrorUpdateExecutableInvalid");
    }

    let _ = fs::remove_file(&temp_exe);
    fs::rename(&part, &temp_exe).map_err(|_| "ErrorUpdateFailed")?;

    if requires_elevation {
        if !launch_updater_elevated(&temp_exe, &target) {
            return Err("ErrorUpdateFailed");
        }
    } else if !launch_updater_and_restart(&temp_exe, &target) {
        return Err("ErrorUpdateFailed");
    }
    Ok(())
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
    fn parses_newer_release_and_selects_arch_asset() {
        let json = r#"{
          "tag_name":"v0.4.0",
          "html_url":"https://github.com/alztrk/opentaskbar/releases/tag/v0.4.0",
          "assets":[
            {"name":"OpenTaskbar-x86.exe","browser_download_url":"https://x/86.exe","size":123,"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            {"name":"OpenTaskbar-x64.exe","browser_download_url":"https://x/64.exe","size":456,"digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
          ]
        }"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.version, "0.4.0");
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

// ---- C# update-checker parity (WS-F) ----

pub const USER_AGENT: &str = "OpenTaskbar";

/// GitHub releases page (WS-F helper; `RELEASES_PAGE` is the canonical constant).
pub fn releases_page() -> String {
    format!("https://github.com/{REPOSITORY}/releases/latest")
}

/// Semver-like version with the same normalization as C# `Version` (missing parts become 0).
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

/// C# `Release` view: the same release with a parsed [`Version`] and an `i64` size.
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

pub type ReleaseWithVersion = DetailedRelease;

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

/// Checks GitHub; `checked == false` means the service could not be reached.
pub fn find_newer_detailed() -> (Option<DetailedRelease>, bool) {
    if !supports_in_app_updates() {
        return (None, true);
    }
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let (status, body) = match http_get_detailed(&url) {
        Ok(result) => result,
        Err(_) => return (None, false),
    };
    if !(200..300).contains(&status) {
        return (None, false);
    }
    (
        parse_release_detailed(&String::from_utf8_lossy(&body), Version::current(), None),
        true,
    )
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
    if target
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("dotnet.exe"))
    {
        return UpdateOutcome::failure("ErrorUpdateDeveloperMode", None);
    }
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

/// Rejects control characters, quotes and newlines in batch paths (C# `EscapeBatchValue`).
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

/// C# `GenerateUpdaterScript`: no BOM (a BOM corrupts `@echo off` on line 1).
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

/// Minimal synchronous HTTPS GET via WinHTTP (status code + body). No extra dependencies.
fn http_get_detailed(url: &str) -> Result<(u16, Vec<u8>), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinHttp::{
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
            WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
            WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
            WinHttpSendRequest, WinHttpSetTimeouts,
        };
        let (scheme, rest) = url.split_once("://").ok_or("invalid URL")?;
        let secure = scheme.eq_ignore_ascii_case("https");
        let (authority, resource) = rest
            .split_once('/')
            .map(|(host, path)| (host, format!("/{path}")))
            .unwrap_or((rest, "/".to_owned()));
        let host_port: Vec<&str> = authority.split(':').collect();
        let host = host_port[0];
        let port = host_port
            .get(1)
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(if secure { 443 } else { 80 });
        let agent: Vec<u16> = format!("{USER_AGENT}/{}\0", Version::current())
            .encode_utf16()
            .collect();
        let host_wide: Vec<u16> = format!("{host}\0").encode_utf16().collect();
        let resource_wide: Vec<u16> = format!("{resource}\0").encode_utf16().collect();
        // SAFETY: every handle is checked for null and closed exactly once on every path.
        unsafe {
            let session = WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if session.is_null() {
                return Err("WinHttpOpen failed".into());
            }
            // Five-minute send/receive budget for full release binaries.
            WinHttpSetTimeouts(session, 5_000, 5_000, 300_000, 300_000);
            let connection = WinHttpConnect(session, host_wide.as_ptr(), port, 0);
            if connection.is_null() {
                WinHttpCloseHandle(session);
                return Err("WinHttpConnect failed".into());
            }
            let flags = if secure { WINHTTP_FLAG_SECURE } else { 0 };
            let request = WinHttpOpenRequest(
                connection,
                "GET\0".encode_utf16().collect::<Vec<_>>().as_ptr(),
                resource_wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                flags,
            );
            if request.is_null() {
                WinHttpCloseHandle(connection);
                WinHttpCloseHandle(session);
                return Err("WinHttpOpenRequest failed".into());
            }
            let ok = WinHttpSendRequest(
                request,
                "Accept: application/vnd.github+json\0"
                    .encode_utf16()
                    .collect::<Vec<_>>()
                    .as_ptr(),
                0,
                std::ptr::null_mut(),
                0,
                0,
                0,
            );
            if ok == 0 || WinHttpReceiveResponse(request, std::ptr::null_mut()) == 0 {
                WinHttpCloseHandle(request);
                WinHttpCloseHandle(connection);
                WinHttpCloseHandle(session);
                return Err("WinHttp request failed".into());
            }
            let mut status: u32 = 0;
            let mut length = size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                request,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut length,
                std::ptr::null_mut(),
            );
            let mut body = Vec::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let mut read = 0u32;
                if WinHttpReadData(
                    request,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                ) == 0
                    || read == 0
                {
                    break;
                }
                body.extend_from_slice(&buffer[..read as usize]);
            }
            WinHttpCloseHandle(request);
            WinHttpCloseHandle(connection);
            WinHttpCloseHandle(session);
            Ok((status as u16, body))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err("HTTPS is only available on Windows".into())
    }
}

/// Compact SHA-256 (FIPS 180-4), used to verify release asset digests offline.
pub fn sha256(data: &[u8]) -> Vec<u8> {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w[..16].iter_mut().enumerate() {
            *word = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    h.iter().flat_map(|word| word.to_be_bytes()).collect()
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
