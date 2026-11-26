//! GitHub release lookup and in-place updates (C# `UpdateChecker.cs` parity).
//!
//! Release metadata and the binary itself are fetched with WinHTTP. Installation is
//! gated on a GitHub `sha256:` asset digest, file size, and the PE `MZ` magic before a
//! self-deleting `apply-update.cmd` replaces the running executable.

use std::fs;
use std::io::Write;
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use opentaskbar_core::single_instance::{MUTEX_NAME, SingleInstance};
use serde_json::Value;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

pub const REPOSITORY: &str = "alztrk/opentaskbar";
pub const RELEASES_PAGE: &str = "https://github.com/alztrk/opentaskbar/releases/latest";
/// Single source of truth for the shipped version (also shown in Settings → About).
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
        WinHttpSetTimeouts(session, 5_000, 5_000, 15_000, 20_000);
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
pub fn find_newer() -> Result<Option<Release>, ()> {
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
    if status != 200 {
        return Err(());
    }
    let json = String::from_utf8(body).map_err(|_| ())?;
    Ok(parse_release(&json))
}

fn valid_digest(digest: Option<&str>) -> bool {
    match digest {
        Some(value) => {
            value.len() == 71
                && value[..7].eq_ignore_ascii_case("sha256:")
                && value[7..].bytes().all(|b| b.is_ascii_hexdigit())
        }
        None => false,
    }
}

fn escape_batch(value: &str) -> Result<String, &'static str> {
    if value.trim().is_empty() {
        return Err("ErrorUpdatePath");
    }
    if value
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\0')
    {
        return Err("ErrorUpdatePath");
    }
    Ok(value.replace('%', "%%"))
}

fn updater_script(source: &str, target: &str) -> Result<String, &'static str> {
    Ok([
        "@echo off".to_owned(),
        "chcp 65001 >nul".to_owned(),
        format!("set \"SRC={}\"", escape_batch(source)?),
        format!("set \"DST={}\"", escape_batch(target)?),
        "set ATTEMPTS=0".to_owned(),
        ":wait_loop".to_owned(),
        "timeout /t 1 /nobreak >nul".to_owned(),
        "copy /y \"%SRC%\" \"%DST%\" >nul 2>&1".to_owned(),
        "if not errorlevel 1 if exist \"%DST%\" goto launch".to_owned(),
        "set /a ATTEMPTS+=1".to_owned(),
        "if %ATTEMPTS% lss 30 goto wait_loop".to_owned(),
        "goto cleanup".to_owned(),
        String::new(),
        ":launch".to_owned(),
        "del /f /q \"%SRC%\" >nul 2>&1".to_owned(),
        "start \"\" \"%DST%\"".to_owned(),
        String::new(),
        ":cleanup".to_owned(),
        "del /f /q \"%~f0\" >nul 2>&1".to_owned(),
        String::new(),
    ]
    .join("\r\n"))
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
    // The batch script copies over the executable. Windows cannot replace a mapped image while the
    // dock is running, so refuse early instead of reporting a success that never takes effect.
    if SingleInstance::acquire(MUTEX_NAME).is_none() {
        return Err("ErrorUpdateDockRunning");
    }
    let target = std::env::current_exe().map_err(|_| "ErrorUpdatePath")?;
    let file_name = target.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if file_name.eq_ignore_ascii_case("dotnet.exe") {
        return Err("ErrorUpdateDeveloperMode");
    }
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
    let status = http_get(
        host,
        path,
        &[("User-Agent", &format!("OpenTaskbar/{CURRENT_VERSION}"))],
        |chunk| {
            let _ = file.write_all(chunk);
        },
    )
    .ok_or("ErrorUpdateHttp")?;
    drop(file);
    if status != 200 {
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
    let expected = hex_to_bytes(&digest[7..]).ok_or("ErrorUpdateExecutableInvalid")?;
    let actual = Sha256::digest(&bytes);
    if actual.as_slice() != expected.as_slice() {
        let _ = fs::remove_file(&part);
        return Err("ErrorUpdateExecutableInvalid");
    }

    let _ = fs::remove_file(&temp_exe);
    fs::rename(&part, &temp_exe).map_err(|_| "ErrorUpdateFailed")?;

    let script_path = dir.join("apply-update.cmd");
    let script = updater_script(&temp_exe.to_string_lossy(), &target.to_string_lossy())?;
    // UTF-8 without BOM: a BOM breaks `@echo off` on the first line of a .cmd file.
    fs::write(&script_path, script).map_err(|_| "ErrorUpdateFailed")?;

    let working_dir = target.parent().unwrap_or(Path::new("."));
    Command::new("cmd.exe")
        .args(["/c", &script_path.to_string_lossy()])
        .current_dir(working_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| "ErrorUpdateFailed")?;
    Ok(())
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
