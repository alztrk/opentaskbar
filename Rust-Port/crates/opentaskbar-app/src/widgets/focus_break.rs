//! Focus / Break session engine — C# `FocusWidget.cs` parity.
//!
//! Adds the Break phase, Asterisk chime + visual pulse, session numbering and the
//! `FocusMinutes` / `FocusBreakMinutes` / `FocusSessions` settings. `SharedFocusSession` lets the
//! dock chip, flyout and island render the same state without duplicated timers.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentaskbar_core::settings::DockSettings;

use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

use super::catalog_ops::{setting_i64, setting_value};

/// C# `FocusWidget.Phase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusPhase {
    Focus,
    Break,
}

impl FocusPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Focus => "Focus",
            Self::Break => "Break",
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
                    self.focus_minutes_total = self.focus_minutes_total.saturating_add(self.focus_minutes);
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
        format!("Session {}/{}", self.session_number, self.sessions_target)
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

/// Builds the live focus flyout including Break phase info, session number and presets.
pub fn build_focus_flyout(session: &FocusSession) -> FlyoutSpec {
    let ratio = session.progress_ratio();
    let total_label = match session.phase {
        FocusPhase::Focus => format!("{}m focus", session.focus_minutes),
        FocusPhase::Break => format!("{}m break", session.break_minutes),
    };
    FlyoutSpec {
        title: String::new(),
        subtitle: None,
        width_dips: 316.0,
        blocks: vec![
            FlyoutBlock::StatRow {
                label: "Focus".to_owned(),
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
                label: session.phase.label().to_owned(),
                value_text: format!("{:.0}%", ratio * 100.0),
                ratio,
                tone: if session.phase == FocusPhase::Break {
                    BarTone::Level
                } else {
                    BarTone::Accent
                },
            },
            FlyoutBlock::StatRow {
                label: "Completed".to_owned(),
                value: format!(
                    "{} sessions · {}m",
                    session.sessions_completed, session.focus_minutes_total
                ),
            },
            FlyoutBlock::ButtonBar(vec![
                (15, "15m".to_owned(), session.focus_minutes == 15),
                (25, "25m".to_owned(), session.focus_minutes == 25),
                (50, "50m".to_owned(), session.focus_minutes == 50),
            ]),
            FlyoutBlock::ButtonBar(vec![
                (4, "Skip".to_owned(), false),
                (2, "Reset".to_owned(), false),
                (1, if session.running { "Pause" } else { "Start" }.to_owned(), true),
            ]),
        ],
    }
}

fn format_mm_secs(total: u64) -> String {
    if total >= 3600 {
        format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
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
        let mut s = FocusSession::default();
        s.focus_minutes = 25;
        s.break_minutes = 5;
        s.remaining_secs = 25 * 60;
        s.running = true;
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
        settings.extra.insert("FocusMinutes".into(), serde_json::json!(50));
        settings.extra.insert("FocusBreakMinutes".into(), serde_json::json!(10));
        settings.extra.insert("FocusSessions".into(), serde_json::json!(6));
        let s = FocusSession::from_settings(&settings);
        assert_eq!(s.focus_minutes, 50);
        assert_eq!(s.break_minutes, 10);
        assert_eq!(s.sessions_target, 6);
        assert_eq!(s.remaining_secs, 50 * 60);
    }

    #[test]
    fn reset_restores_focus_phase() {
        let mut s = FocusSession::default();
        s.running = true;
        s.phase = FocusPhase::Break;
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
