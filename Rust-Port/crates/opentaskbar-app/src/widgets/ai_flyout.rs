//! Full AI quota flyout — C# `AiUsageWidget.BuildFlyout` + `AiUsageService` parity.
//!
//! Adds the spend header, Usage breakdown, reset countdown, plan/status chips, severity dot and
//! a live refresh action on top of the existing window bars. Antigravity is the 8th provider and
//! uses the `AntigravityGroups` setting to split its usage into Gemini / Claude / Argon pools.
//!
//! The full provider payload is obtained through the C4 API `widgets::telemetry::ai_fetch_full`.

use std::time::{SystemTime, UNIX_EPOCH};

use opentaskbar_core::settings::DockSettings;

use crate::design::{self, Rgb};
use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

use super::catalog_ops::setting_value;
use super::telemetry::{AiUsageSnapshot, ai_provider_meta};

/// C# `AiUsageService.Severity` — drives the status dot color in the flyout header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum AiSeverity {
    #[default]
    None,
    Info,
    Warning,
    Critical,
}

impl AiSeverity {
    pub fn color(self, pal: &design::Palette) -> Rgb {
        match self {
            Self::Critical => pal.danger,
            Self::Warning => pal.warning,
            _ => pal.widget_accent,
        }
    }
}

/// One Antigravity pool / group row (C# `AiUsageGroup`).
#[derive(Clone, Debug, PartialEq)]
pub struct AiPoolGroup {
    pub name: String,
    pub label: String,
    pub used: f32,
    pub reset_text: String,
}

/// Rich provider detail assembled from the C4 full fetch plus the cached chip snapshot.
#[derive(Clone, Debug, Default)]
pub struct AiFullDetail {
    pub plan: Option<String>,
    pub spend: Vec<(String, String)>,
    pub breakdown: Vec<(String, String)>,
    pub reset_countdown: Option<String>,
    pub unlimited_credits: bool,
    pub limit_reached: bool,
    pub severity: AiSeverity,
    pub retry_after_secs: Option<u64>,
    pub groups: Vec<AiPoolGroup>,
}

/// Parses an ISO-8601 reset timestamp into a concise local countdown relative to `now_epoch`.
/// Returns `None` for absent/unparseable values so callers can fall back to chip data.
pub fn parse_reset_countdown(iso: &str, now_epoch: u64) -> Option<String> {
    // `2026-11-26T16:30:00Z` / `2026-11-26T16:30:00+03:00`
    let date = iso.get(0..10)?;
    let mut parts = date.split('-');
    let (Ok(y), Ok(m), Ok(d)) = (
        parts.next()?.parse::<i64>(),
        parts.next()?.parse::<u32>(),
        parts.next()?.parse::<u32>(),
    ) else {
        return None;
    };
    let time = iso.get(11..19).unwrap_or("00:00:00");
    let mut tp = time.split(':');
    let h = tp.next()?.parse::<i64>().ok()?;
    let mi = tp.next()?.parse::<i64>().ok()?;
    let se = tp
        .next()
        .and_then(|s| s.get(0..2))
        .unwrap_or("00")
        .parse::<i64>()
        .ok()?;

    // Days from civil date (Howard Hinnant's algorithm) → epoch seconds.
    let y_adj = y - if m <= 2 { 1 } else { 0 };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut ts = days * 86_400 + h * 3600 + mi * 60 + se;
    if let (Some(suffix), Some(sign_pos)) = (
        iso.get(19..),
        iso.get(19..).and_then(|s| s.find(['+', '-'])),
    ) {
        let sign = suffix.as_bytes()[sign_pos];
        let off = &suffix[sign_pos + 1..];
        let mut op = off.split(':');
        let oh = op.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let om = op.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let delta = oh * 3600 + om * 60;
        ts = if sign == b'-' { ts + delta } else { ts - delta };
    }
    let remaining = ts.saturating_sub(now_epoch as i64);
    if remaining <= 0 {
        return Some("Expired".to_owned());
    }
    let (days_left, rem) = (remaining / 86_400, remaining % 86_400);
    let (hours, rem) = (rem / 3600, rem % 3600);
    let mins = rem / 60;
    if days_left > 0 {
        Some(format!("{days_left}d {hours}h"))
    } else if hours > 0 {
        Some(format!("{hours}h {mins}m"))
    } else {
        Some(format!("{mins}m"))
    }
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Reads the `AntigravityGroups` setting. C# stores either a `string[]` of group names or an
/// array of `{ name, pool }` objects; both map to the same normalized group list.
pub fn antigravity_groups(settings: &DockSettings) -> Vec<String> {
    let defaults = ["Gemini", "Claude", "Argon"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let Some(v) = setting_value(settings, "AntigravityGroups") else {
        return defaults;
    };
    let arr = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(m) => m.values().cloned().collect(),
        // The merged core settings type serializes this flag set as `"All"` / `"Gemini, ..."`;
        // older JSON files may store the raw bitmask as a number.
        serde_json::Value::String(s) if s.eq_ignore_ascii_case("All") => return defaults,
        serde_json::Value::Number(n) => {
            use opentaskbar_core::settings::antigravity_groups as groups;
            let bits = n.as_u64().unwrap_or(0) as u32;
            let mut named = Vec::new();
            if bits & groups::GEMINI != 0 {
                named.push("Gemini".to_owned());
            }
            if bits & groups::CLAUDE != 0 {
                named.push("Claude".to_owned());
            }
            if bits & groups::ARGON != 0 {
                named.push("Argon".to_owned());
            }
            return if named.is_empty() { defaults } else { named };
        }
        other => return vec![other.as_str().unwrap_or("Gemini").to_owned()],
    };
    let groups: Vec<String> = arr
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => o
                .get("name")
                .or_else(|| o.get("group"))
                .or_else(|| o.get("label"))
                .and_then(|s| s.as_str())
                .map(str::to_owned),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect();
    if groups.is_empty() { defaults } else { groups }
}

/// Parses an Antigravity pool/group usage JSON object into normalized pool rows. Accepts both
/// `{"groups":[...]}` and a bare array, and maps `usedPercent` / `utilization` / `used_percent`.
pub fn parse_antigravity_pools(body: &str) -> Vec<AiPoolGroup> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let array = value
        .get("groups")
        .or_else(|| value.get("pools"))
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())
        .cloned()
        .unwrap_or_default();
    let now = now_epoch();
    array
        .iter()
        .map(|entry| {
            let name = entry
                .get("name")
                .or_else(|| entry.get("group"))
                .or_else(|| entry.get("pool"))
                .and_then(|v| v.as_str())
                .unwrap_or("Pool")
                .to_owned();
            let used = entry
                .get("usedPercent")
                .or_else(|| entry.get("utilization"))
                .or_else(|| entry.get("used_percent"))
                .and_then(|v| v.as_f64())
                .map(|n| (if n > 1.0 { n / 100.0 } else { n }).clamp(0.0, 1.0) as f32)
                .unwrap_or(0.0);
            let label = entry
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("5h")
                .to_owned();
            let reset_text = entry
                .get("resetsAt")
                .or_else(|| entry.get("resets_at"))
                .and_then(|v| v.as_str())
                .and_then(|iso| parse_reset_countdown(iso, now))
                .unwrap_or_else(|| format!("{:.0}%", used * 100.0));
            AiPoolGroup {
                name,
                label,
                used,
                reset_text,
            }
        })
        .collect()
}

/// Derives severity from a peak utilization value (C# `AiUsageService.SeverityFor`).
pub fn severity_for_usage(peak: f32, limit_reached: bool) -> AiSeverity {
    if limit_reached || peak >= 0.98 {
        AiSeverity::Critical
    } else if peak >= 0.85 {
        AiSeverity::Warning
    } else if peak > 0.0 {
        AiSeverity::Info
    } else {
        AiSeverity::None
    }
}

/// Calls the C4 full-fetch API for a provider. Fire-and-forget: WS-G owns the payload shape and
/// updates the shared `AiUsageCache`; the flyout re-reads the cache through `build_ai_flyout`.
pub fn refresh_full(provider_id: &'static str) {
    super::telemetry::ai_fetch_full(provider_id);
}

/// Builds the rich flyout blocks for one AI provider (C# `AiUsageWidget.BuildFlyout`).
pub fn build_ai_flyout(
    id: &str,
    snap: &AiUsageSnapshot,
    detail: &AiFullDetail,
    settings: &DockSettings,
    is_dark: bool,
) -> FlyoutSpec {
    let pal = design::palette(is_dark);
    let ai = ai_provider_meta(id);
    let name = ai.map_or(id, |m| m.name).to_owned();
    let dashboard = ai.map_or("", |m| m.dashboard_url).to_owned();
    let has_data = snap.balance_text.is_some() || !snap.windows.is_empty();

    let mut blocks = Vec::new();
    // Status dot + plan chip in a single C# `SectionHeader`-style row.
    let status_label = if let Some(ref plan) = detail.plan {
        plan.clone()
    } else if let Some(ref plan) = snap.plan {
        plan.clone()
    } else if has_data {
        "Connected".to_owned()
    } else if snap.has_cred {
        "Configured".to_owned()
    } else {
        "Sign in".to_owned()
    };
    blocks.push(FlyoutBlock::StatusHeader {
        title: name.clone(),
        status_label,
        status_color: detail.severity.color(&pal),
        action_id: Some(1),
        action_label: Some("Open".to_owned()),
    });

    // Plan chip (BadgeRow gives the rounded pill C# uses for subscription tier).
    if let Some(plan) = detail.plan.as_ref().or(snap.plan.as_ref()) {
        blocks.push(FlyoutBlock::BadgeRow {
            id: 20,
            code: if detail.unlimited_credits {
                "∞".to_owned()
            } else {
                "Plan".to_owned()
            },
            title: plan.clone(),
            active: false,
        });
    }

    // Spend header.
    if let Some(bal) = &snap.balance_text {
        blocks.push(FlyoutBlock::Hero {
            value: bal.clone(),
            caption: Some(if id.eq_ignore_ascii_case("aiusage_openrouter") {
                "Remaining credits".to_owned()
            } else {
                "Available balance".to_owned()
            }),
            centered: false,
            large: false,
            right_meta: None,
        });
    }
    for (label, value) in &detail.spend {
        blocks.push(FlyoutBlock::StatRow {
            label: label.clone(),
            value: value.clone(),
        });
    }

    // Reset countdown chip.
    if let Some(ref countdown) = detail.reset_countdown {
        blocks.push(FlyoutBlock::StatRow {
            label: "Resets in".to_owned(),
            value: countdown.clone(),
        });
    }
    if let Some(retry) = detail.retry_after_secs {
        blocks.push(FlyoutBlock::Note(format!("Retry after {retry}s")));
    }

    // Usage window bars from the cached chip snapshot.
    for w in &snap.windows {
        blocks.push(FlyoutBlock::ProgressBar {
            label: w.label.clone(),
            value_text: format!("{:.0}% · {}", w.used * 100.0, w.reset_text),
            ratio: w.used,
            tone: BarTone::Usage,
        });
    }

    // Spend breakdown table.
    if !detail.breakdown.is_empty() {
        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::StatRow {
            label: "Breakdown".to_owned(),
            value: String::new(),
        });
        for (label, value) in &detail.breakdown {
            blocks.push(FlyoutBlock::StatRow {
                label: label.clone(),
                value: value.clone(),
            });
        }
    }

    // Antigravity group/pool split (C# `AiUsageAntigravityGroupsLabel`).
    if id.eq_ignore_ascii_case("aiusage_antigravity") {
        let groups = antigravity_groups(settings);
        if !groups.is_empty() {
            blocks.push(FlyoutBlock::Separator);
            for group in &groups {
                let pool = detail
                    .groups
                    .iter()
                    .find(|g| g.name.eq_ignore_ascii_case(group));
                blocks.push(FlyoutBlock::ProgressBar {
                    label: group.clone(),
                    value_text: pool.map_or_else(
                        || "—".to_owned(),
                        |g| format!("{:.0}% · {}", g.used * 100.0, g.reset_text),
                    ),
                    ratio: pool.map_or(0.0, |g| g.used),
                    tone: BarTone::Usage,
                });
            }
        }
    }

    if detail.limit_reached {
        blocks.push(FlyoutBlock::Note("Limit reached".to_owned()));
    }
    if !has_data {
        let hint = ai.map_or("provider CLI", |m| m.signin_hint);
        blocks.push(FlyoutBlock::Note(format!(
            "Sign in via `{hint}` or save an API key to load live usage."
        )));
    }
    blocks.push(FlyoutBlock::Separator);
    blocks.push(FlyoutBlock::ButtonBar(vec![
        (2, "Refresh".to_owned(), false),
        (3, "Full Refresh".to_owned(), false),
        (1, "Open Dashboard".to_owned(), true),
    ]));
    let _ = &dashboard; // dashboard URL is consumed by the caller's Click(1) handler.
    FlyoutSpec {
        title: String::new(),
        subtitle: None,
        width_dips: 292.0,
        blocks,
    }
}

/// Converts raw provider JSON (from `ai_fetch_full`) into the rich detail layout. Pure function.
pub fn parse_full_detail(body: &str) -> AiFullDetail {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(body) else {
        return AiFullDetail::default();
    };
    let get_str = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| root.get(*k).and_then(|v| v.as_str()))
            .map(str::to_owned)
    };
    let now = now_epoch();
    let plan = get_str(&["plan", "subscriptionType", "tier"]);
    let unlimited = root
        .get("unlimitedCredits")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit_reached = root
        .get("limitReached")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reset_countdown = get_str(&["resetsAt", "resets_at", "resetAt"])
        .and_then(|iso| parse_reset_countdown(&iso, now));
    let retry_after_secs = root
        .get("retryAfter")
        .or_else(|| root.get("retry_after"))
        .and_then(|v| v.as_u64());

    let read_pairs = |key: &str| -> Vec<(String, String)> {
        root.get(key)
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        let val = match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Number(n) => n.to_string(),
                            _ => return None,
                        };
                        Some((k.clone(), val))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let spend = read_pairs("spend");
    let breakdown = read_pairs("breakdown");
    let groups = parse_antigravity_pools(body);
    let peak = groups.iter().map(|g| g.used).fold(0.0f32, f32::max);
    let severity = severity_for_usage(peak, limit_reached);
    AiFullDetail {
        plan,
        spend,
        breakdown,
        reset_countdown,
        unlimited_credits: unlimited,
        limit_reached,
        severity,
        retry_after_secs,
        groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::telemetry::AI_PROVIDERS;

    #[test]
    fn parses_reset_countdown_hours_and_minutes() {
        // 2030-01-01T00:00:00Z is safely in the future relative to the test clock.
        let target = 1_893_456_000u64;
        assert_eq!(
            parse_reset_countdown("2030-01-01T00:00:00Z", target),
            Some("Expired".to_owned())
        );
        assert!(
            parse_reset_countdown("2030-01-01T00:00:00Z", 0)
                .unwrap()
                .contains('h')
        );
    }

    #[test]
    fn antigravity_pool_parsing_and_severity() {
        let body = r#"{"groups":[{"name":"Gemini","label":"5h","usedPercent":92,"resetsAt":"2030-01-01T00:00:00Z"},{"name":"Claude","label":"7d","used_percent":0.4}]}"#;
        let groups = parse_antigravity_pools(body);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "Gemini");
        assert!((groups[0].used - 0.92).abs() < 0.001);
        assert_eq!(severity_for_usage(0.92, false), AiSeverity::Warning);
        assert_eq!(severity_for_usage(1.0, true), AiSeverity::Critical);
    }

    #[test]
    fn full_detail_parses_spend_and_plan() {
        let body = r#"{"plan":"Pro","spend":{"today":"$1.20","month":"$9.80"},"breakdown":{"requests":"142"},"unlimitedCredits":false,"limitReached":true,"retryAfter":30}"#;
        let d = parse_full_detail(body);
        assert_eq!(d.plan.as_deref(), Some("Pro"));
        assert_eq!(d.spend.len(), 2);
        assert!(d.limit_reached);
        assert_eq!(d.retry_after_secs, Some(30));
        assert_eq!(d.severity, AiSeverity::Critical);
    }

    #[test]
    fn antigravity_groups_setting_defaults() {
        let s = DockSettings::default();
        assert_eq!(antigravity_groups(&s), vec!["Gemini", "Claude", "Argon"]);
    }

    #[test]
    fn provider_count_is_eight() {
        assert_eq!(AI_PROVIDERS.len(), 8);
        assert!(AI_PROVIDERS.iter().any(|p| p.id == "aiusage_antigravity"));
    }
}
