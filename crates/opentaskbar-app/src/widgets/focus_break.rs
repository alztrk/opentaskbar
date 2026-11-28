//! Focus / Break session engine — `FocusWidget.cs` parity.
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

/// `FocusWidget.Phase`.
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

/// Duration of the reference pulse animation applied after a phase transition.
pub const PULSE_MS: u64 = 800;

/// Core session state. Pure apart from `Instant` (which is `Clone`), so it is easy to test and share.
#[derive(Clone, Debug)]
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
    /// Number of the currently running/next session (1-based), matching reference UI.
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
    /// Reads `FocusMinutes`, `FocusBreakMinutes`, `FocusSessions` from settings without
    /// serializing the full `DockSettings` struct on the UI hot path.
    pub fn from_settings(settings: &DockSettings) -> Self {
        let extra_i64 = |key: &str| -> Option<i64> {
            settings
                .extra
                .get(key)
                .and_then(|v| v.as_i64())
                .or_else(|| {
                    settings
                        .extra
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(key))
                        .and_then(|(_, v)| v.as_i64())
                })
        };
        let focus = extra_i64("FocusMinutes")
            .unwrap_or_else(|| i64::from(settings.focus_minutes))
            .clamp(1, 180) as u64;
        let break_mins = extra_i64("FocusBreakMinutes")
            .unwrap_or_else(|| i64::from(settings.focus_break_minutes))
            .clamp(0, 60) as u64;
        let target = extra_i64("FocusSessions")
            .or_else(|| {
                (settings.focus_sessions == 4)
                    .then(|| extra_i64("FocusBreakSessions"))
                    .flatten()
            })
            .unwrap_or_else(|| i64::from(settings.focus_sessions))
            .clamp(1, 20) as u64;
        Self {
            remaining_secs: focus * 60,
            focus_minutes: focus,
            break_minutes: break_mins,
            sessions_target: target,
            ..Self::default()
        }
    }

    /// Synchronizes duration & target settings into this session whenever it is idle so
    /// edits made in Settings take effect immediately without clobbering an active timer.
    pub fn sync_from_settings(&mut self, settings: &DockSettings) {
        if self.running {
            return;
        }
        let fresh = Self::from_settings(settings);
        let at_start = self.phase == FocusPhase::Focus
            && self.remaining_secs == self.focus_minutes.saturating_mul(60);
        let break_at_start = self.phase == FocusPhase::Break
            && self.remaining_secs == self.break_minutes.saturating_mul(60);
        self.focus_minutes = fresh.focus_minutes;
        self.break_minutes = fresh.break_minutes;
        self.sessions_target = fresh.sessions_target;
        if at_start {
            self.remaining_secs = self.focus_minutes.saturating_mul(60);
        } else if break_at_start {
            self.remaining_secs = self.break_minutes.saturating_mul(60);
        }
        self.session_number = (self.sessions_completed % self.sessions_target.max(1)) + 1;
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

    /// Advances by `secs`; may cross one or more phase boundaries. Leftover time is carried
    /// into the next phase instead of being discarded, so a long stall still advances the
    /// session correctly. Returns the first meaningful event (a `PhaseEnded` chime is never
    /// overwritten by a later `CycleCompleted`) so the UI reacts once per frame.
    pub fn tick(&mut self, secs: u64) -> FocusEvent {
        if !self.running || secs == 0 {
            return FocusEvent::None;
        }
        let mut left = secs;
        let mut event = FocusEvent::None;
        while left > 0 {
            let consumed = self.remaining_secs.min(left);
            self.remaining_secs = self.remaining_secs.saturating_sub(consumed);
            left -= consumed;
            if self.remaining_secs > 0 {
                break;
            }
            match self.phase {
                FocusPhase::Focus => {
                    self.focus_minutes_total =
                        self.focus_minutes_total.saturating_add(self.focus_minutes);
                    self.sessions_completed = self.sessions_completed.saturating_add(1);
                    self.phase = FocusPhase::Break;
                    self.remaining_secs = self.break_minutes.saturating_mul(60);
                    self.last_transition = Some(Instant::now());
                    if event == FocusEvent::None {
                        event = FocusEvent::PhaseEnded(FocusPhase::Focus);
                    }
                    if self.break_minutes == 0 {
                        // Zero-length break: carry the overflow straight into the next focus phase.
                        continue;
                    }
                }
                FocusPhase::Break => {
                    self.phase = FocusPhase::Focus;
                    self.remaining_secs = self.focus_minutes.saturating_mul(60);
                    self.last_transition = Some(Instant::now());
                    self.session_number =
                        (self.sessions_completed % self.sessions_target.max(1)) + 1;
                    if event == FocusEvent::None {
                        event = FocusEvent::CycleCompleted;
                    }
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
        self.session_number = (self.sessions_completed % self.sessions_target.max(1)) + 1;
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
        let was_running = self.running;
        self.running = true;
        let _ = self.tick(self.remaining_secs.max(1));
        self.running = was_running;
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
        self.with(Clone::clone).unwrap_or_default()
    }
}

/// `SystemSounds.Asterisk.Play()` → native Win32 `MessageBeep(MB_ICONASTERISK)`.
pub fn play_break_chime() {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBeep(u_type: u32) -> i32;
        }
        // SAFETY: `MessageBeep(MB_ICONASTERISK)` is a stateless Win32 sound call.
        unsafe {
            let _ = MessageBeep(0x0000_0040);
        }
    }
}

/// Builds the redesigned live focus flyout with localized phase/session status and presets.
pub fn build_focus_flyout(session: &FocusSession, is_dark: bool) -> FlyoutSpec {
    let ratio = session.progress_ratio();
    let is_break = session.phase == FocusPhase::Break;
    let phase_label = loc::t(if is_break { "Break" } else { "Focus" });
    let phase_mins = if is_break {
        session.break_minutes
    } else {
        session.focus_minutes
    };
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
    let pal = design::palette(is_dark);
    let phase_color = if is_break {
        pal.success
    } else {
        pal.widget_accent
    };
    let phase_glyph = if is_break { 0xE708 } else { 0xE7C8 };

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
            FlyoutBlock::Hero {
                value: format_mm_secs(session.remaining_secs),
                caption: Some(format!("{} · {}", session.phase.label(), total_label)),
                centered: false,
                large: true,
                right_meta: None,
            },
            FlyoutBlock::ProgressBar {
                label: phase_label,
                value_text: super::format_percent((ratio * 100.0).round() as u32),
                ratio,
                tone: if is_break {
                    BarTone::Level
                } else {
                    BarTone::Accent
                },
            },
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

/// Single source for the `mm:ss` / `hh:mm:ss` countdown display used by runtime chips,
/// tooltips and the focus flyout.
pub fn format_mm_secs(total: u64) -> String {
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
    fn phase_overflow_is_carried_into_the_next_phase() {
        let mut s = FocusSession {
            focus_minutes: 25,
            break_minutes: 5,
            remaining_secs: 25 * 60,
            running: true,
            ..Default::default()
        };
        // 25 min focus + 30 s into the 5 min break; the old code dropped the 30 s.
        assert_eq!(
            s.tick(25 * 60 + 30),
            FocusEvent::PhaseEnded(FocusPhase::Focus)
        );
        assert_eq!(s.phase, FocusPhase::Break);
        assert_eq!(s.remaining_secs, 5 * 60 - 30);
        // A zero-length break carries the overflow into the next focus phase and keeps the chime.
        let mut s = FocusSession {
            focus_minutes: 1,
            break_minutes: 0,
            remaining_secs: 60,
            running: true,
            ..Default::default()
        };
        assert_eq!(s.tick(90), FocusEvent::PhaseEnded(FocusPhase::Focus));
        assert_eq!(s.phase, FocusPhase::Focus);
        assert_eq!(s.remaining_secs, 30);
        assert_eq!(s.session_number, 2);
    }

    #[test]
    fn skip_phase_works_while_paused() {
        let mut s = FocusSession {
            remaining_secs: 60,
            running: false,
            ..Default::default()
        };
        s.skip_phase();
        assert_eq!(s.phase, FocusPhase::Break);
        assert!(!s.running, "skip must not resume a paused session");
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

    #[test]
    fn sync_from_settings_updates_when_idle_and_preserves_running() {
        let mut s = FocusSession::default();
        let mut settings = DockSettings::default();
        settings
            .extra
            .insert("FocusMinutes".into(), serde_json::json!(40));
        settings
            .extra
            .insert("FocusBreakMinutes".into(), serde_json::json!(8));
        settings
            .extra
            .insert("FocusSessions".into(), serde_json::json!(5));
        s.sync_from_settings(&settings);
        assert_eq!(s.focus_minutes, 40);
        assert_eq!(s.break_minutes, 8);
        assert_eq!(s.sessions_target, 5);
        assert_eq!(s.remaining_secs, 40 * 60);

        s.running = true;
        s.remaining_secs = 123;
        settings
            .extra
            .insert("FocusMinutes".into(), serde_json::json!(15));
        s.sync_from_settings(&settings);
        assert_eq!(s.focus_minutes, 40);
        assert_eq!(s.remaining_secs, 123);
    }

    #[test]
    fn build_focus_flyout_respects_theme_and_removes_duplicates() {
        let s = FocusSession::default();
        let dark = build_focus_flyout(&s, true);
        let light = build_focus_flyout(&s, false);
        let stat_rows = dark
            .blocks
            .iter()
            .filter(|b| matches!(b, FlyoutBlock::StatRow { .. }))
            .count();
        assert_eq!(stat_rows, 1, "duplicated focus stat rows must be removed");
        assert_eq!(dark.blocks.len(), light.blocks.len());
    }
}
