//! Full AI quota flyout — C# `AiUsageWidget.BuildFlyout` + `AiUsageService` parity.
//!
//! Adds the spend header, Usage breakdown, reset countdown, plan/status chips, severity dot and
//! a live refresh action on top of the existing window bars.
//!
//! The full provider payload is obtained through `widgets::telemetry::ai_fetch_full`, cached in
//! `WidgetRuntime::ai_full` and passed to [`build_ai_flyout`] on every render.

use std::time::{SystemTime, UNIX_EPOCH};

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::DockSettings;

use crate::design::{self, Rgb};
use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

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

/// Rich provider detail assembled from the full fetch plus the cached chip snapshot.
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
}

/// Adapts the cached `telemetry::AiFullDetail` (the result of `ai_fetch_full`) into the flyout's
/// presentation model. The flyout must always show the fetched spend / breakdown / reset / plan /
/// limit / credit data instead of an empty `AiFullDetail::default()`.
pub fn from_telemetry(full: &super::telemetry::AiFullDetail) -> AiFullDetail {
    let now = now_epoch();
    let reset_countdown = full
        .reset_unix_ms
        .filter(|ms| *ms > 0)
        .and_then(|ms| super::telemetry::countdown_from_unix(ms / 1000, now))
        .or_else(|| {
            (!full.reset_text.is_empty() && full.reset_text != "—")
                .then(|| full.reset_text.clone())
        });
    let peak = std::iter::once(full.used)
        .chain(full.breakdown.iter().map(|s| s.used))
        .fold(0.0f32, f32::max);
    let mut spend = Vec::new();
    if let Some(s) = &full.spend {
        let symbol = match s.currency.as_str() {
            "USD" => "$",
            "EUR" => "€",
            "TRY" | "TL" => "₺",
            _ => "",
        };
        let money = |v: f64| -> String {
            if symbol.is_empty() {
                format!("{v:.2} {}", s.currency)
            } else {
                format!("{symbol}{v:.2}")
            }
        };
        spend.push((loc::t("AiUsageExtra"), money(s.used)));
        if let Some(limit) = s.limit {
            spend.push((loc::t("AiUsageQuota"), money(limit)));
        }
    } else if let Some(credits) = full.credits {
        spend.push((loc::t("AiUsageRemainingCredits"), format!("{credits:.2}")));
    }
    AiFullDetail {
        plan: full.plan.clone(),
        spend,
        breakdown: full
            .breakdown
            .iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| (s.name.clone(), format!("{:.0}%", s.used * 100.0)))
            .collect(),
        reset_countdown,
        unlimited_credits: full.unlimited_credits,
        limit_reached: full.limit_reached,
        severity: severity_for_usage(peak, full.limit_reached),
        retry_after_secs: full.retry_after_secs.map(u64::from),
    }
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
        return Some(loc::t("Expired"));
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
    _settings: &DockSettings,
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
        loc::t("AiUsageAccountConnected")
    } else if snap.has_cred {
        loc::t("AiUsageConfigured")
    } else {
        loc::t("AiUsageStatusSignIn")
    };
    blocks.push(FlyoutBlock::StatusHeader {
        title: name.clone(),
        status_label,
        status_color: detail.severity.color(&pal),
        action_id: Some(1),
        action_label: Some(loc::t("Open")),
    });

    // Plan chip (BadgeRow gives the rounded pill C# uses for subscription tier).
    if let Some(plan) = detail.plan.as_ref().or(snap.plan.as_ref()) {
        blocks.push(FlyoutBlock::BadgeRow {
            id: 20,
            code: if detail.unlimited_credits {
                "∞".to_owned()
            } else {
                loc::t("AiUsagePlan")
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
                loc::t("AiUsageRemainingCredits")
            } else {
                loc::t("AiUsageAvailableBalance")
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
            label: loc::t("AiUsageResetsInLabel"),
            value: countdown.clone(),
        });
    }
    if let Some(retry) = detail.retry_after_secs {
        blocks.push(FlyoutBlock::Note(loc::t_fmt(
            "AiUsageRetryAfter",
            &[&retry.to_string()],
        )));
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
            label: loc::t("AiUsageBreakdown"),
            value: String::new(),
        });
        for (label, value) in &detail.breakdown {
            blocks.push(FlyoutBlock::StatRow {
                label: label.clone(),
                value: value.clone(),
            });
        }
    }

    if detail.limit_reached {
        blocks.push(FlyoutBlock::Note(loc::t("AiUsageLimitReached")));
    }
    if !has_data {
        let hint = ai.map_or("provider CLI", |m| m.signin_hint);
        blocks.push(FlyoutBlock::Note(loc::t_fmt("AiUsageSignInHint", &[hint])));
    }
    blocks.push(FlyoutBlock::Separator);
    blocks.push(FlyoutBlock::ButtonBar(vec![
        (2, loc::t("AiUsageRefresh"), false),
        (3, loc::t("AiUsageFullRefresh"), false),
        (1, loc::t("AiUsageOpenDashboard"), true),
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
    let severity = severity_for_usage(
        if limit_reached { 1.0 } else { 0.0 },
        limit_reached || retry_after_secs.is_some(),
    );
    AiFullDetail {
        plan,
        spend,
        breakdown,
        reset_countdown,
        unlimited_credits: unlimited,
        limit_reached,
        severity,
        retry_after_secs,
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
    fn severity_thresholds_match_csharp() {
        assert_eq!(severity_for_usage(0.0, false), AiSeverity::None);
        assert_eq!(severity_for_usage(0.2, false), AiSeverity::Info);
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
    fn fetched_full_detail_is_mapped_into_flyout_blocks() {
        let full = crate::widgets::telemetry::AiFullDetail {
            provider_id: "aiusage_claude".to_owned(),
            plan: Some("Max 20x".to_owned()),
            used: 0.81,
            label: "7d".to_owned(),
            reset_text: "11-30 12:00".to_owned(),
            reset_unix_ms: Some(1_893_456_000_000),
            spend: Some(crate::widgets::telemetry::AiSpend {
                used: 12.34,
                limit: Some(50.0),
                currency: "USD".to_owned(),
                used_01: Some(0.25),
            }),
            breakdown: vec![crate::widgets::telemetry::AiUsageShare {
                name: "Opus".to_owned(),
                used: 0.31,
            }],
            limit_reached: false,
            retry_after_secs: Some(12),
            credits: None,
            unlimited_credits: false,
            error: None,
        };
        let d = from_telemetry(&full);
        assert_eq!(d.plan.as_deref(), Some("Max 20x"));
        assert_eq!(d.spend.len(), 2);
        assert_eq!(d.breakdown[0].0, "Opus");
        assert_eq!(d.retry_after_secs, Some(12));
        assert_eq!(d.severity, AiSeverity::Info);

        let snap = crate::widgets::telemetry::AiUsageSnapshot::default();
        let s = DockSettings::default();
        let spec = build_ai_flyout("aiusage_claude", &snap, &d, &s, true);
        assert!(spec.blocks.iter().any(|b| matches!(
            b,
            FlyoutBlock::StatRow { label, .. } if label == "Opus"
        )));
    }

    #[test]
    fn provider_catalog_has_seven_named_providers() {
        assert_eq!(AI_PROVIDERS.len(), 7);
        let ids: Vec<&str> = AI_PROVIDERS.iter().map(|p| p.id).collect();
        assert_eq!(
            ids,
            vec![
                "aiusage_claude",
                "aiusage_codex",
                "aiusage_opencode",
                "aiusage_copilot",
                "aiusage_deepseek",
                "aiusage_openrouter",
                "aiusage_cursor"
            ]
        );
        assert!(AI_PROVIDERS.iter().any(|p| p.id == "aiusage_cursor"));
        assert!(AI_PROVIDERS.iter().any(|p| p.id == "aiusage_claude"));
    }
}
