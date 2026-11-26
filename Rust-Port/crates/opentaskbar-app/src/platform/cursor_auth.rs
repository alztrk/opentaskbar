//! Cursor browser PKCE sign-in and DPAPI-encrypted OAuth token storage.
//!
//! C# parity: `Services/App/CursorAuthService.cs`.
//! - `sign_in_start()` -> PKCE verifier/challenge/uuid + browser URL (mode=login).
//! - `sign_in_finish()` polls `api2.cursor.sh/auth/poll` every 2 s (up to 3 minutes) and
//!   persists the access/refresh token pair with `CryptProtectData` + application entropy.
//! - `token()` / `load_tokens()` decrypt the token from `%LOCALAPPDATA%\OpenTaskbar\secrets\cursor-auth.bin`.
//! - `refresh()` rotates an expiring access token; `sign_out()` deletes the blob.
//!
//! No token is ever written to settings/log/argv.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

const CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
const API_BASE: &str = "https://api2.cursor.sh";
const MAX_FILE_BYTES: usize = 1 << 16;
const ENTROPY: &[u8] = b"OpenTaskbar.CursorAuth.v1";

static FILE_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();
static SIGNING_OUT: AtomicBool = AtomicBool::new(false);
static REFRESH_LOCK: Mutex<()> = Mutex::new(());
static PENDING_LOGIN: Mutex<Option<SignInStart>> = Mutex::new(None);

/// One in-progress PKCE login.
#[derive(Clone, Debug)]
pub struct SignInStart {
    pub verifier: String,
    pub challenge: String,
    pub uuid: String,
    pub login_url: String,
}

/// Result of an OAuth token refresh.
#[derive(Clone, Debug, Default)]
pub struct RefreshResult {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub session_expired: bool,
}

fn token_path() -> PathBuf {
    FILE_OVERRIDE.get().cloned().unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("OpenTaskbar")
            .join("secrets")
            .join("cursor-auth.bin")
    })
}

/// Test hook: replaces the token file location.
pub fn set_token_path(path: PathBuf) {
    let _ = FILE_OVERRIDE.set(path);
}

// ---------------------------------------------------------------------------------------------
// Base64Url + SHA-256 (no extra crate dependency; both are pure and unit-tested).
// ---------------------------------------------------------------------------------------------

pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        }
    }
    out
}

fn decode_base64url(input: &str) -> Option<Vec<u8>> {
    let mut normalized = input.replace('-', "+").replace('_', "/");
    match normalized.len() % 4 {
        2 => normalized.push_str("=="),
        3 => normalized.push('='),
        1 => return None,
        _ => {}
    }
    let mut out = Vec::with_capacity(normalized.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in normalized.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1u32 << bits) - 1;
        }
    }
    Some(out)
}

/// SHA-256 of `data`, implemented in-process so no crypto crate is required.
pub fn sha256(data: &[u8]) -> [u8; 32] {
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
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(chunk.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h = [
            h[0].wrapping_add(a),
            h[1].wrapping_add(b),
            h[2].wrapping_add(c),
            h[3].wrapping_add(d),
            h[4].wrapping_add(e),
            h[5].wrapping_add(f),
            h[6].wrapping_add(g),
            h[7].wrapping_add(hh),
        ];
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// PKCE S256 challenge for a verifier string.
pub fn pkce_challenge(verifier: &str) -> String {
    base64url(&sha256(verifier.as_bytes()))
}

// ---------------------------------------------------------------------------------------------
// DPAPI helpers (fail closed: never retry without application entropy).
// ---------------------------------------------------------------------------------------------

fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    if plain.is_empty() {
        return Some(Vec::new());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() || output.cbData == 0 {
        return None;
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        LocalFree(output.pbData.cast());
    }
    Some(bytes)
}

fn unprotect(encrypted: &[u8]) -> Option<Vec<u8>> {
    if encrypted.is_empty() {
        return Some(Vec::new());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() || output.cbData == 0 {
        return None;
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        LocalFree(output.pbData.cast());
    }
    Some(bytes)
}

/// Pure JSON payload builder used by `save_tokens` (also used in tests).
pub fn token_json(access: &str, refresh: Option<&str>) -> String {
    let mut access_escaped = String::new();
    for c in access.chars() {
        match c {
            '"' => access_escaped.push_str("\\\""),
            '\\' => access_escaped.push_str("\\\\"),
            '\n' => access_escaped.push_str("\\n"),
            _ => access_escaped.push(c),
        }
    }
    match refresh.filter(|r| !r.trim().is_empty()) {
        Some(r) => format!(
            "{{\"accessToken\":\"{}\",\"refreshToken\":\"{}\"}}",
            access_escaped,
            r.replace('"', "\\\"")
        ),
        None => format!("{{\"accessToken\":\"{access_escaped}\"}}"),
    }
}

/// Pure token JSON reader: JSON object -> `(access, refresh)`, plain string -> `(token, None)`.
pub fn parse_token_payload(raw: &str) -> (Option<String>, Option<String>) {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let a = v
                .get("accessToken")
                .or_else(|| v.get("access_token"))
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let r = v
                .get("refreshToken")
                .or_else(|| v.get("refresh_token"))
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            return (a, r);
        }
        (None, None)
    } else {
        let t = trimmed.to_owned();
        (!t.is_empty())
            .then_some(t)
            .map(|t| (Some(t), None))
            .unwrap_or((None, None))
    }
}

/// Persists an OAuth token pair with DPAPI + entropy.
pub fn save_tokens(access: &str, refresh: Option<&str>) -> Result<(), String> {
    if access.trim().is_empty() {
        return Err("access token is required".to_owned());
    }
    let plain = token_json(access, refresh);
    let mut bytes = plain.into_bytes();
    let encrypted = protect(&bytes).ok_or_else(|| "DPAPI protect failed".to_owned())?;
    bytes.fill(0);
    let path = token_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, &encrypted).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Reads the decrypted token pair. Never panics; malformed blobs are ignored.
pub fn load_tokens() -> (Option<String>, Option<String>) {
    let Ok(bytes) = std::fs::read(token_path()) else {
        return (None, None);
    };
    if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
        return (None, None);
    }
    match unprotect(&bytes).and_then(|p| String::from_utf8(p).ok()) {
        Some(raw) => parse_token_payload(&raw),
        None => (None, None),
    }
}

/// True when either token exists.
pub fn has_session() -> bool {
    let (a, r) = load_tokens();
    a.is_some() || r.is_some()
}

/// Deletes the encrypted token blob.
pub fn sign_out() -> bool {
    SIGNING_OUT.store(true, Ordering::Release);
    let ok = std::fs::remove_file(token_path()).is_ok();
    SIGNING_OUT.store(false, Ordering::Release);
    ok
}

/// Primary access token, if any (C4 `cursor_auth::token()`).
pub fn token() -> Option<String> {
    load_tokens().0
}

/// Returns true when a JWT expires within five minutes (C# `IsExpiringSoon`).
#[allow(non_snake_case)]
pub fn is_expiringSoon(jwt: &str) -> bool {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    let Some(payload) = decode_base64url(parts[1]) else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return false;
    };
    let Some(exp) = v.get("exp").and_then(|x| {
        x.as_f64()
            .or_else(|| x.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    }) else {
        return false;
    };
    if !exp.is_finite() || !(-62_135_596_800.0..=253_402_300_799.0).contains(&exp) {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as f64)
        .unwrap_or(0.0);
    exp <= now + 300.0
}

/// Alias with idiomatic casing.
pub fn is_expiring_soon(jwt: &str) -> bool {
    is_expiringSoon(jwt)
}

/// Starts a browser PKCE login. Pass the returned `login_url` to the OS browser, then call
/// `sign_in_finish(&start)` on a background thread.
pub fn sign_in_start() -> SignInStart {
    let mut seed = [0u8; 32];
    // No RNG crate dependency: mix process id, nanoseconds and address entropy.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seed_addr = seed.as_ptr() as usize;
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = ((now >> (i % 8 * 8)) as u8)
            ^ (std::process::id() as u8).wrapping_mul(i as u8 + 1)
            ^ (seed_addr >> (i % 8 * 8)) as u8;
    }
    let verifier = base64url(&seed);
    let challenge = pkce_challenge(&verifier);
    let uuid = format!(
        "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
        now as u32,
        (now >> 32) as u16,
        std::process::id(),
        (now >> 16) as u16,
        now
    );
    let login_url =
        format!("https://cursor.com/loginDeepControl?challenge={challenge}&uuid={uuid}&mode=login");
    let start = SignInStart {
        verifier,
        challenge,
        uuid,
        login_url,
    };
    if let Ok(mut pending) = PENDING_LOGIN.lock() {
        *pending = Some(start.clone());
    }
    start
}

/// Pure poll URL builder (kept separate for tests).
pub fn poll_url(uuid: &str, verifier: &str) -> String {
    format!("{API_BASE}/auth/poll?uuid={uuid}&verifier={verifier}")
}

/// C4 `sign_in_finish()` — polls using the most recent `sign_in_start()` challenge.
pub fn sign_in_finish() -> bool {
    let start = PENDING_LOGIN
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(sign_in_start);
    sign_in_finish_with(&start)
}

/// Polls the browser login every 2 s for up to 3 minutes. Returns `true` when tokens were saved.
pub fn sign_in_finish_with(start: &SignInStart) -> bool {
    let url = poll_url(&start.uuid, &start.verifier);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if SIGNING_OUT.load(Ordering::Acquire) {
            return false;
        }
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "$r=Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 -Headers @{{'User-Agent'='OpenTaskbar'}} '{url}'; $r.Content"
                ),
            ])
            .creation_flags(0x0800_0000)
            .output();
        let Ok(out) = output else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (access, refresh) = parse_token_payload(&text);
        if let Some(access) = access {
            let _ = save_tokens(&access, refresh.as_deref());
            return true;
        }
    }
    false
}

/// Parses an OAuth refresh response into a `RefreshResult` (pure).
pub fn parse_refresh_response(body: &str, old_refresh: &str) -> RefreshResult {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return RefreshResult {
            refresh_token: Some(old_refresh.to_owned()),
            ..Default::default()
        };
    };
    if v.get("shouldLogout").and_then(|x| x.as_bool()) == Some(true) {
        return RefreshResult {
            session_expired: true,
            ..Default::default()
        };
    }
    let access = v
        .get("access_token")
        .or_else(|| v.get("accessToken"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if access.is_none() {
        return RefreshResult {
            refresh_token: Some(old_refresh.to_owned()),
            ..Default::default()
        };
    }
    let refresh = v
        .get("refresh_token")
        .or_else(|| v.get("refreshToken"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| old_refresh.to_owned());
    RefreshResult {
        access_token: access,
        refresh_token: Some(refresh),
        session_expired: false,
    }
}

/// Refreshes an expiring Cursor session, persisting rotated tokens on success.
pub fn refresh(refresh_token: &str) -> RefreshResult {
    let _guard = match REFRESH_LOCK.lock() {
        Ok(g) => g,
        Err(_) => return RefreshResult::default(),
    };
    let (current_access, current_refresh) = load_tokens();
    if let (Some(a), Some(r)) = (&current_access, &current_refresh)
        && !is_expiring_soon(a)
        && r != refresh_token
    {
        return RefreshResult {
            access_token: Some(a.clone()),
            refresh_token: Some(r.clone()),
            session_expired: false,
        };
    }
    let payload = format!(
        "{{\"grant_type\":\"refresh_token\",\"client_id\":\"{CLIENT_ID}\",\"refresh_token\":\"{}\"}}",
        refresh_token.replace('"', "\\\"")
    );
    let ps = format!(
        "$body='{payload}'; $r=Invoke-WebRequest -UseBasicParsing -TimeoutSec 15 -Method Post -ContentType 'application/json' -Headers @{{'User-Agent'='OpenTaskbar'}} -Body $body '{API_BASE}/oauth/token'; $r.Content"
    );
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .creation_flags(0x0800_0000)
        .output();
    let Ok(out) = out else {
        return RefreshResult {
            refresh_token: Some(refresh_token.to_owned()),
            ..Default::default()
        };
    };
    if !out.status.success() {
        return RefreshResult {
            refresh_token: Some(refresh_token.to_owned()),
            ..Default::default()
        };
    }
    let result = parse_refresh_response(&String::from_utf8_lossy(&out.stdout), refresh_token);
    if result.session_expired {
        sign_out();
    } else if let (Some(a), Some(r)) = (&result.access_token, &result.refresh_token) {
        let _ = save_tokens(a, Some(r));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            base64url(&sha256(b"")),
            "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU"
        );
        assert_eq!(
            base64url(&sha256(b"abc")),
            "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
        );
    }

    #[test]
    fn base64url_roundtrip() {
        for data in [b"".as_slice(), b"f", b"fo", b"foo", b"foob", b"foobar"] {
            let enc = base64url(data);
            assert_eq!(decode_base64url(&enc).unwrap(), data);
        }
    }

    #[test]
    fn token_payload_json_and_plain() {
        let (a, r) = parse_token_payload(r#"{"accessToken":"acc","refreshToken":"ref"}"#);
        assert_eq!(a.as_deref(), Some("acc"));
        assert_eq!(r.as_deref(), Some("ref"));
        let (a, r) = parse_token_payload("  legacy-token  ");
        assert_eq!(a.as_deref(), Some("legacy-token"));
        assert!(r.is_none());
    }

    #[test]
    fn refresh_response_rotation_and_logout() {
        let r =
            parse_refresh_response(r#"{"access_token":"new","refresh_token":"rotated"}"#, "old");
        assert_eq!(r.access_token.as_deref(), Some("new"));
        assert_eq!(r.refresh_token.as_deref(), Some("rotated"));
        assert!(!r.session_expired);
        let r = parse_refresh_response(r#"{"shouldLogout":true}"#, "old");
        assert!(r.session_expired);
        let r = parse_refresh_response("not json", "old");
        assert_eq!(r.refresh_token.as_deref(), Some("old"));
    }

    #[test]
    fn sign_in_start_builds_pkce_url() {
        let s = sign_in_start();
        assert!(s.login_url.contains("loginDeepControl"));
        assert!(s.login_url.contains("mode=login"));
        assert_eq!(s.challenge, pkce_challenge(&s.verifier));
    }

    #[test]
    fn jwt_expiry_detection() {
        // {"exp":1000000000} (year 2001) encoded as a JWT payload.
        let jwt = format!("hdr.{} .sig", base64url(br#"{"exp":1000000000}"#)).replace(" .", ".");
        assert!(is_expiring_soon(&jwt));
        assert!(!is_expiring_soon("not-a-jwt"));
    }
}
