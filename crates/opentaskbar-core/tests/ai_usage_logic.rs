//! C# `AiUsageChecks` parity.
//!
//! The deterministic logic (JWT expiry, quota/pace/reset and severity) is kept
//! as reference vectors. The bridge test below compares those vectors against
//! the real provider flags and serde representation in
//! `opentaskbar-core::settings`.

mod common;

use opentaskbar_core::settings::{DockSettings, ai_providers};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let a = data[i] as u32;
        let b = data.get(i + 1).copied().unwrap_or(0) as u32;
        let c = data.get(i + 2).copied().unwrap_or(0) as u32;
        let n = (a << 16) | (b << 8) | c;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        if i + 1 < data.len() {
            out.push(B64[((n >> 6) & 63) as usize] as char);
        }
        if i + 2 < data.len() {
            out.push(B64[(n & 63) as usize] as char);
        }
        i += 3;
    }
    out
}

fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for ch in input.bytes() {
        if ch == b'=' {
            break;
        }
        let value = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn make_token(exp: i64, extra: &str) -> String {
    let header = b64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = format!(r#"{{"exp":{exp}{extra}}}"#);
    format!("{}.{}.signature", header, b64url_encode(payload.as_bytes()))
}

/// Expiry epoch seconds from a JWT, without verifying the signature (the C#
/// helper only needs the timestamp to decide whether to refresh).
fn jwt_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("exp")?.as_i64()
}

fn is_expired(token: &str, now: i64) -> bool {
    jwt_expiry(token).is_none_or(|exp| now >= exp)
}

fn remaining_percent(limit: f64, used: f64) -> f64 {
    if limit <= 0.0 {
        return 0.0;
    }
    ((limit - used) / limit * 100.0).clamp(0.0, 100.0)
}

fn pace_per_hour(used: f64, elapsed_secs: u64) -> f64 {
    if elapsed_secs == 0 {
        return 0.0;
    }
    used * 3600.0 / elapsed_secs as f64
}

fn reset_due(last_reset: i64, now: i64, window_secs: i64) -> bool {
    window_secs > 0 && now.saturating_sub(last_reset) >= window_secs
}

fn severity(remaining_pct: f64) -> &'static str {
    if remaining_pct >= 50.0 {
        "ok"
    } else if remaining_pct >= 20.0 {
        "warning"
    } else {
        "critical"
    }
}

#[test]
fn jwt_expiry_is_parsed_from_the_payload() {
    let token = make_token(1_900_000_000, "");
    assert_eq!(jwt_expiry(&token), Some(1_900_000_000));
}

#[test]
fn jwt_expiry_handles_extra_claims_and_base64url_padding() {
    let token = make_token(1_234_567_890, r#","plan":"pro","seat":1"#);
    assert_eq!(jwt_expiry(&token), Some(1_234_567_890));
}

#[test]
fn malformed_and_claimless_jwt_tokens_have_no_expiry() {
    assert_eq!(jwt_expiry("not-a-jwt"), None);
    assert_eq!(jwt_expiry("hdr.@@@.sig"), None);
    let no_exp = format!(
        "{}.{}.sig",
        b64url_encode(b"{}"),
        b64url_encode(br#"{"iat":1}"#)
    );
    assert_eq!(jwt_expiry(&no_exp), None);
}

#[test]
fn expired_token_is_detected_at_the_boundary() {
    let token = make_token(1000, "");
    assert!(!is_expired(&token, 999));
    assert!(is_expired(&token, 1000));
    assert!(is_expired(&token, 1001));
    // Unparseable tokens are treated as expired so the widget prompts sign-in.
    assert!(is_expired("garbage", 0));
}

#[test]
fn quota_percentage_is_clamped() {
    assert_eq!(remaining_percent(100.0, 25.0), 75.0);
    assert_eq!(remaining_percent(100.0, 150.0), 0.0);
    assert_eq!(remaining_percent(0.0, 0.0), 0.0);
    assert_eq!(remaining_percent(100.0, -10.0), 100.0);
}

#[test]
fn pace_and_reset_windows_are_deterministic() {
    assert_eq!(pace_per_hour(10.0, 600), 60.0);
    assert_eq!(pace_per_hour(10.0, 0), 0.0);
    assert!(!reset_due(1000, 1500, 600));
    assert!(reset_due(1000, 1600, 600));
    assert!(!reset_due(1000, 1600, 0), "zero window never resets");
}

#[test]
fn severity_tracks_remaining_quota() {
    assert_eq!(severity(100.0), "ok");
    assert_eq!(severity(50.0), "ok");
    assert_eq!(severity(49.9), "warning");
    assert_eq!(severity(20.0), "warning");
    assert_eq!(severity(19.9), "critical");
    assert_eq!(severity(0.0), "critical");
}

/// Production bridge: the provider bits are real core constants and must agree
/// with the C# flag-string serialization.
#[test]
fn production_provider_flags_are_consistent() {
    assert_eq!(
        ai_providers::ALL,
        ai_providers::WIDGETS
            .iter()
            .fold(0, |acc, (bit, _)| acc | bit)
    );
    assert_eq!(ai_providers::WIDGETS.len(), 7);

    let settings = DockSettings {
        ai_usage_providers: ai_providers::CLAUDE | ai_providers::OPEN_CODE,
        ..DockSettings::default()
    };
    let json = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(json["AiUsageProviders"], "Claude, OpenCode");
    let back: DockSettings = serde_json::from_value(json).expect("settings round-trip");
    assert_eq!(
        back.ai_usage_providers,
        ai_providers::CLAUDE | ai_providers::OPEN_CODE
    );

    common::assert_source_has(
        "crates/opentaskbar-core/src/settings.rs",
        &["parse_flag_names"],
    );
}
