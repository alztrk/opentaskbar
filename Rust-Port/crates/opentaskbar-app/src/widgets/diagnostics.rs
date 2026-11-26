//! Diagnostics for per-app volume control and session lifetime (C# `CoreAudio.cs`
//! `AppSessionVolumeHandle` / `AppAudioSession.Dispose` parity).
//!
//! A mixer slider holds a COM audio session for as long as it is visible. This module models
//! the lifetime with an explicit `release()` and an optimistic `VolumeRollback` that restores
//! the previous level when a write fails (session disconnected / process exited).

use std::collections::HashMap;

use crate::platform::core_audio::{
    AppAudioSession, AppAudioSessionState, AudioEvent, AudioEventKind, find_app_session,
};

/// A single optimistic volume change with rollback support.
#[derive(Clone, Debug)]
pub struct VolumeRollback {
    pub session_key: String,
    pub previous_volume: f32,
    pub previous_mute: bool,
    pub requested_volume: f32,
    committed: bool,
}

impl VolumeRollback {
    pub fn begin(session_key: &str, previous_volume: f32, previous_mute: bool, requested: f32) -> Self {
        Self {
            session_key: session_key.to_owned(),
            previous_volume,
            previous_mute,
            requested_volume: requested.clamp(0.0, 1.0),
            committed: false,
        }
    }

    /// Commits the change; returns the value the UI should show.
    pub fn commit(&mut self, write_ok: bool) -> f32 {
        self.committed = write_ok;
        if write_ok {
            self.requested_volume
        } else {
            self.previous_volume
        }
    }

    /// Restores the original level if the write failed (or the session vanished).
    pub fn rollback(&self) -> Option<(f32, bool)> {
        (!self.committed).then_some((self.previous_volume, self.previous_mute))
    }

    pub fn needs_rollback(&self) -> bool {
        !self.committed
    }
}

/// Maps mixer slider IDs to released session handles.
#[derive(Default)]
pub struct SessionRegistry {
    sessions: HashMap<u32, AppAudioSession>,
    rollbacks: HashMap<u32, VolumeRollback>,
}

impl SessionRegistry {
    pub fn attach(&mut self, slider_id: u32, session: AppAudioSession) {
        self.release(slider_id);
        self.sessions.insert(slider_id, session);
        self.rollbacks.remove(&slider_id);
    }

    /// Releases the COM session held by a slider. Idempotent (C# `Dispose` guard).
    pub fn release(&mut self, slider_id: u32) -> bool {
        self.sessions.remove(&slider_id).is_some()
    }

    pub fn release_all(&mut self) {
        self.sessions.clear();
        self.rollbacks.clear();
    }

    pub fn get(&self, slider_id: u32) -> Option<&AppAudioSession> {
        self.sessions.get(&slider_id)
    }

    /// Starts an optimistic volume change for a session.
    pub fn begin_volume_change(&mut self, slider_id: u32, requested: f32) -> Option<VolumeRollback> {
        let session = self.sessions.get(&slider_id)?;
        let rollback = VolumeRollback::begin(
            &session.name,
            session.volume,
            session.muted,
            requested,
        );
        self.rollbacks.insert(slider_id, rollback.clone());
        Some(rollback)
    }

    /// Completes a volume change: on failure the original level is restored in the snapshot.
    pub fn finish_volume_change(&mut self, slider_id: u32, write_ok: bool) -> Option<f32> {
        let mut rollback = self.rollbacks.remove(&slider_id)?;
        let shown = rollback.commit(write_ok);
        if !write_ok {
            if let Some(session) = self.sessions.get_mut(&slider_id) {
                session.volume = rollback.previous_volume;
                session.muted = rollback.previous_mute;
            }
        } else if let Some(session) = self.sessions.get_mut(&slider_id) {
            session.volume = shown;
        }
        Some(shown)
    }

    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    /// Drops sessions whose audio event says the endpoint/app went away.
    pub fn on_events(&mut self, events: &[AudioEvent]) -> usize {
        let mut removed = 0usize;
        for event in events {
            if !matches!(
                event.kind,
                AudioEventKind::DefaultDeviceChanged
                    | AudioEventKind::DeviceRemoved
                    | AudioEventKind::DeviceStateChanged
            ) {
                continue;
            }
            let stale: Vec<u32> = self
                .sessions
                .iter()
                .filter(|(_, s)| s.state == AppAudioSessionState::Unavailable)
                .map(|(id, _)| *id)
                .collect();
            for id in stale {
                self.release(id);
                removed += 1;
            }
        }
        removed
    }
}

/// Picks the unambiguous session for a media source, then releases all rejected sessions.
pub fn select_and_release(
    sessions: Vec<AppAudioSession>,
    source_id: &str,
) -> Option<AppAudioSession> {
    let selected = find_app_session(&sessions, source_id);
    // In Rust, dropping the unused `AppAudioSession` values releases their handles.
    drop(sessions);
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(pid: u32, name: &str, volume: f32) -> AppAudioSession {
        AppAudioSession {
            process_id: pid,
            name: name.to_owned(),
            app_user_model_id: None,
            window_app_user_model_id: None,
            state: AppAudioSessionState::Active,
            volume,
            muted: false,
        }
    }

    #[test]
    fn volume_rollback_on_failure() {
        let mut r = VolumeRollback::begin("app", 0.3, false, 0.8);
        assert_eq!(r.commit(false), 0.3);
        assert_eq!(r.rollback(), Some((0.3, false)));
        let mut ok = VolumeRollback::begin("app", 0.3, false, 0.8);
        assert_eq!(ok.commit(true), 0.8);
        assert!(ok.rollback().is_none());
    }

    #[test]
    fn registry_releases_sessions_idempotently() {
        let mut reg = SessionRegistry::default();
        reg.attach(1, session(10, "chrome", 0.5));
        assert_eq!(reg.active_count(), 1);
        assert!(reg.release(1));
        assert!(!reg.release(1));
        assert_eq!(reg.active_count(), 0);
    }

    #[test]
    fn registry_failed_write_restores_snapshot() {
        let mut reg = SessionRegistry::default();
        reg.attach(7, session(10, "spotify", 0.4));
        let rollback = reg.begin_volume_change(7, 0.9).unwrap();
        assert_eq!(rollback.previous_volume, 0.4);
        assert_eq!(reg.finish_volume_change(7, false), Some(0.4));
        assert_eq!(reg.get(7).unwrap().volume, 0.4);
    }

    #[test]
    fn media_source_selection_drops_rejected_sessions() {
        let picked = select_and_release(
            vec![session(1, "chrome", 0.5), session(2, "spotify", 0.7)],
            "spotify",
        );
        assert_eq!(picked.unwrap().process_id, 2);
    }

    #[test]
    fn endpoint_events_release_unavailable_sessions() {
        let mut reg = SessionRegistry::default();
        let mut s = session(10, "app", 0.5);
        s.state = AppAudioSessionState::Unavailable;
        reg.attach(2, s);
        let events = vec![AudioEvent {
            kind: AudioEventKind::DefaultDeviceChanged,
            device_id: String::new(),
        }];
        assert_eq!(reg.on_events(&events), 1);
        assert_eq!(reg.active_count(), 0);
    }
}
