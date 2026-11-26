//! Focus / Break session engine — C# `FocusWidget.cs` parity.
//!
//! Adds the Break phase, Asterisk chime + visual pulse, session numbering and the
//! `FocusMinutes` / `FocusBreakMinutes` / `FocusSessions` settings. `SharedFocusSession` lets the
//! dock chip, flyout and island render the same state without duplicated timers.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentaskbar_core::i18n as loc;
use opentaskbar_core::settings::DockSettings;

use crate::design;
use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

use super::catalog_ops::{setting_i64, setting_value};

/// C# `FocusWidget.Phase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusPhase {
    Focus,
    Break,
}

impl FocusPhase {
    pub fn label(self) -> String {
        match self {
            Self::Focus => loc::t("Focus"),
            Self::Break => loc::t("Break"),
        }
    }
}

/// Emitted by `tick` so the caller can play the chime / trigger the pulse animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusEvent {
    None,
    /// A phase finished; the caller should call `play_break_chime()` and pulse the chip.
    PhaseEnded(FocusPhase),
    /// A full focus+break cycle completed (session counter incremented).
    CycleCompleted,
}

/// Duration of the C# pulse animation applied after a phase transition.
pub const PULSE_MS: u64 = 800;

/// Core session state. Pure apart from `Instant`, so it is easy to test and share.
#[derive(Debug)]
pub struct FocusSession {
    pub phase: FocusPhase,
    pub running: bool,
    pub remaining_secs: u64,
    pub focus_minutes: u64,
    pub break_minutes: u64,
    pub sessions_target: u64,
    pub sessions_completed: u64,
    pub focus_minutes_total: u64,
    pub last_transition: Option<Instant>,
    /// Number of the currently running/next session (1-based), matching C# UI.
    pub session_number: u64,
}

impl Default for FocusSession {
    fn default() -> Self {
        Self {
            phase: FocusPhase::Focus,
            running: false,
            remaining_secs: 25 * 60,
            focus_minutes: 25,
            break_minutes: 5,
            sessions_target: 4,
            sessions_completed: 0,
            focus_minutes_total: 0,
            last_transition: None,
            session_number: 1,
        }
    }
}

impl FocusSession {
    /// Reads `FocusMinutes`, `FocusBreakMinutes`, `FocusSessions` from settings (WS-A typed fields
    /// or `extra` — both are visible via the serialized-settings helper).
    pub fn from_settings(settings: &DockSettings) -> Self {
        let focus = setting_i64(settings, "FocusMinutes", 25).clamp(1, 180) as u64;
        let break_mins = setting_i64(settings, "FocusBreakMinutes", 5).clamp(0, 60) as u64;
        let target = setting_i64(settings, "FocusSessions", 4).clamp(1, 20) as u64;
        // Legacy alias used by some C# profiles before the rename.
        let target = if setting_value(settings, "FocusSessions").is_none() {
            setting_i64(settings, "FocusBreakSessions", target as i64).clamp(1, 20) as u64
        } else {
            target
        };
        Self {
            phase: FocusPhase::Focus,
            running: false,
            remaining_secs: focus * 60,
            focus_minutes: focus,
            break_minutes: break_mins,
            sessions_target: target,
            sessions_completed: 0,
            focus_minutes_total: 0,
            last_transition: None,
            session_number: 1,
        }
    }

    pub fn phase_duration_secs(&self) -> u64 {
        match self.phase {
            FocusPhase::Focus => self.focus_minutes.saturating_mul(60),
            FocusPhase::Break => self.break_minutes.saturating_mul(60),
        }
        .max(1)
    }

    pub fn progress_ratio(&self) -> f32 {
        (self.remaining_secs as f32 / self.phase_duration_secs() as f32).clamp(0.0, 1.0)
    }

    pub fn pulse_active(&self, now: Instant) -> bool {
        self.last_transition
            .is_some_and(|at| now.duration_since(at) < Duration::from_millis(PULSE_MS))
    }

    /// Advances by `secs`; may cross one or more phase boundaries. Returns the last meaningful
    /// event so the UI can react once per frame.
    pub fn tick(&mut self, secs: u64) -> FocusEvent {
        if !self.running || secs == 0 {
            return FocusEvent::None;
        }
        self.remaining_secs = self.remaining_secs.saturating_sub(secs);
        let mut event = FocusEvent::None;
        while self.remaining_secs == 0 {
            match self.phase {
                FocusPhase::Focus => {
                    self.focus_minutes_total =
                        self.focus_minutes_total.saturating_add(self.focus_minutes);
                    self.sessions_completed = self.sessions_completed.saturating_add(1);
                    self.phase = FocusPhase::Break;
                    self.remaining_secs = self.break_minutes.saturating_mul(60);
                    self.last_transition = Some(Instant::now());
                    event = FocusEvent::PhaseEnded(FocusPhase::Focus);
                    if self.break_minutes == 0 {
                        continue;
                    }
                    break;
                }
                FocusPhase::Break => {
                    self.phase = FocusPhase::Focus;
                    self.remaining_secs = self.focus_minutes.saturating_mul(60);
                    self.last_transition = Some(Instant::now());
                    self.session_number = (self.sessions_completed % self.sessions_target) + 1;
                    event = FocusEvent::CycleCompleted;
                    break;
                }
            }
        }
        event
    }

    pub fn toggle(&mut self) {
        if !self.running && self.remaining_secs == 0 {
            self.remaining_secs = self.phase_duration_secs();
        }
        self.running = !self.running;
        self.last_transition = Some(Instant::now());
    }

    pub fn reset(&mut self) {
        self.running = false;
        self.phase = FocusPhase::Focus;
        self.remaining_secs = self.focus_minutes.saturating_mul(60);
        self.session_number = (self.sessions_completed % self.sessions_target) + 1;
        self.last_transition = None;
    }

    pub fn set_focus_minutes(&mut self, mins: u64) {
        self.focus_minutes = mins.clamp(1, 180);
        if self.phase == FocusPhase::Focus {
            self.remaining_secs = self.focus_minutes * 60;
            self.running = false;
        }
    }

    pub fn skip_phase(&mut self) {
        let _ = self.tick(self.remaining_secs.max(1));
    }

    pub fn session_label(&self) -> String {
        loc::t_fmt(
            "FocusSessionProgress",
            &[
                &self.session_number.to_string(),
                &self.sessions_target.to_string(),
            ],
        )
    }

    pub fn status_text(&self) -> String {
        format!("{} · {}", self.phase.label(), self.session_label())
    }
}

/// Shared handle used by the dock chip, widget flyout and dynamic island.
#[derive(Clone, Default)]
pub struct SharedFocusSession(Arc<Mutex<FocusSession>>);

impl SharedFocusSession {
    pub fn new(session: FocusSession) -> Self {
        Self(Arc::new(Mutex::new(session)))
    }

    pub fn with<R>(&self, f: impl FnOnce(&FocusSession) -> R) -> Option<R> {
        self.0.lock().ok().map(|g| f(&g))
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut FocusSession) -> R) -> Option<R> {
        self.0.lock().ok().map(|mut g| f(&mut g))
    }

    pub fn snapshot(&self) -> FocusSession {
        // FocusSession is not Clone (Instant is Clone, so we clone field-by-field).
        self.with(|s| FocusSession {
            phase: s.phase,
            running: s.running,
            remaining_secs: s.remaining_secs,
            focus_minutes: s.focus_minutes,
            break_minutes: s.break_minutes,
            sessions_target: s.sessions_target,
            sessions_completed: s.sessions_completed,
            focus_minutes_total: s.focus_minutes_total,
            last_transition: s.last_transition,
            session_number: s.session_number,
        })
        .unwrap_or_default()
    }
}

/// C# `SystemSounds.Asterisk.Play()` → `MessageBeep(MB_ICONASTERISK)`.
pub fn play_break_chime() {
    // C# `SystemSounds.Asterisk.Play()` equivalent without a console window.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[console]::beep(880,120)",
            ])
            .creation_flags(0x0800_0000)
            .spawn();
    }
}

/// Builds the redesigned live focus flyout with localized phase/session status and presets.
pub fn build_focus_flyout(session: &FocusSession) -> FlyoutSpec {
    let ratio = session.progress_ratio();
    let is_break = session.phase == FocusPhase::Break;
    let phase_label = loc::t(if is_break { "Break" } else { "Focus" });
    let phase_mins = if is_break {
        session.break_minutes
    } else {
        session.focus_minutes
    };
    // WS-E: keep the localized per-phase summary keys alongside the redesigned glyph row.
    let total_label = match session.phase {
        FocusPhase::Focus => {
            loc::t_fmt("FocusMinutesSummary", &[&session.focus_minutes.to_string()])
        }
        FocusPhase::Break => {
            loc::t_fmt("BreakMinutesSummary", &[&session.break_minutes.to_string()])
        }
    };
    let duration_label = loc::t_fmt("MinutesShort", &[&phase_mins.to_string()]);
    let session_progress = format!("{}/{}", session.session_number, session.sessions_target);
    let session_badge = loc::t_fmt("FocusSession", &[&session_progress]);
    let completed_sessions = loc::t_fmt(
        "FocusSessionsCount",
        &[&session.sessions_completed.to_string()],
    );
    let completed_mins = loc::t_fmt("MinutesShort", &[&session.focus_minutes_total.to_string()]);
    let pal = design::DARK;
    let phase_color = if is_break {
        pal.success
    } else {
        pal.widget_accent
    };
    let phase_glyph = if is_break { 0xE708 } else { 0xE916 };

    FlyoutSpec {
        title: String::new(),
        subtitle: None,
        width_dips: 316.0,
        blocks: vec![
            FlyoutBlock::GlyphStatRow {
                glyph: phase_glyph,
                glyph_color: phase_color,
                label: phase_label.clone(),
                detail: Some(duration_label),
                value: session_badge,
            },
            // WS-E: `FocusSessionProgress` via `session_label()`.
            FlyoutBlock::StatRow {
                label: loc::t("Focus"),
                value: session.session_label(),
            },
            FlyoutBlock::Hero {
                value: format_mm_ss(session.remaining_secs),
                caption: Some(format!("{} · {}", session.phase.label(), total_label)),
                centered: false,
                large: true,
                right_meta: None,
            },
            FlyoutBlock::ProgressBar {
                label: phase_label,
                value_text: format!("{:.0}%", ratio * 100.0),
                ratio,
                tone: if is_break {
                    BarTone::Level
                } else {
                    BarTone::Accent
                },
            },
            FlyoutBlock::StatRow {
                label: loc::t("FocusSessionCount"),
                value: format!("{completed_sessions} · {completed_mins}"),
            },
            // WS-E: combined completion summary keys.
            FlyoutBlock::StatRow {
                label: loc::t("FocusCompleted"),
                value: loc::t_fmt(
                    "FocusCompletedSummary",
                    &[
                        &session.sessions_completed.to_string(),
                        &session.focus_minutes_total.to_string(),
                    ],
                ),
            },
            FlyoutBlock::Separator,
            FlyoutBlock::ButtonBar(vec![
                (
                    15,
                    loc::t_fmt("MinutesShort", &["15"]),
                    session.focus_minutes == 15,
                ),
                (
                    25,
                    loc::t_fmt("MinutesShort", &["25"]),
                    session.focus_minutes == 25,
                ),
                (
                    50,
                    loc::t_fmt("MinutesShort", &["50"]),
                    session.focus_minutes == 50,
                ),
            ]),
            FlyoutBlock::ButtonBar(vec![
                (4, loc::t("FocusSkip"), false),
                (2, loc::t("FocusReset"), false),
                (
                    1,
                    loc::t(if session.running {
                        "FocusPause"
                    } else {
                        "FocusStart"
                    }),
                    true,
                ),
            ]),
        ],
    }
}

fn format_mm_secs(total: u64) -> String {
    if total >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            total / 3600,
            (total % 3600) / 60,
            total % 60
        )
    } else {
        format!("{:02}:{:02}", total / 60, total % 60)
    }
}

// Local alias used by the flyout builder.
fn format_mm_ss(total: u64) -> String {
    format_mm_secs(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_phase_transitions_to_break_and_back() {
        let mut s = FocusSession {
            focus_minutes: 25,
            break_minutes: 5,
            remaining_secs: 25 * 60,
            running: true,
            ..Default::default()
        };
        assert_eq!(s.tick(25 * 60), FocusEvent::PhaseEnded(FocusPhase::Focus));
        assert_eq!(s.phase, FocusPhase::Break);
        assert_eq!(s.remaining_secs, 5 * 60);
        assert_eq!(s.sessions_completed, 1);
        assert_eq!(s.tick(5 * 60), FocusEvent::CycleCompleted);
        assert_eq!(s.phase, FocusPhase::Focus);
        assert_eq!(s.session_number, 2);
    }

    #[test]
    fn settings_drive_session_lengths() {
        let mut settings = DockSettings::default();
        settings
            .extra
            .insert("FocusMinutes".into(), serde_json::json!(50));
        settings
            .extra
            .insert("FocusBreakMinutes".into(), serde_json::json!(10));
        settings
            .extra
            .insert("FocusSessions".into(), serde_json::json!(6));
        let s = FocusSession::from_settings(&settings);
        assert_eq!(s.focus_minutes, 50);
        assert_eq!(s.break_minutes, 10);
        assert_eq!(s.sessions_target, 6);
        assert_eq!(s.remaining_secs, 50 * 60);
    }

    #[test]
    fn reset_restores_focus_phase() {
        let mut s = FocusSession {
            running: true,
            phase: FocusPhase::Break,
            ..Default::default()
        };
        s.reset();
        assert_eq!(s.phase, FocusPhase::Focus);
        assert!(!s.running);
        assert_eq!(s.remaining_secs, s.focus_minutes * 60);
    }

    #[test]
    fn shared_session_snapshot_is_independent() {
        let shared = SharedFocusSession::default();
        shared.with_mut(|s| s.sessions_completed = 3);
        let snap = shared.snapshot();
        assert_eq!(snap.sessions_completed, 3);
        shared.with_mut(|s| s.sessions_completed = 4);
        assert_eq!(snap.sessions_completed, 3);
    }
}
