//! One-second TTL snapshot for expensive per-chip inputs.
//!
//! `WidgetRuntime::tick` is called on every dock frame (up to 144 Hz). Storage and keyboard
//! chips used to call `sample_drives()` / `GetKeyboardLayout` directly from `chip_spec`,
//! producing roughly 300 disk / locale queries per second on an idle dock. This module keeps a
//! single immutable snapshot that `tick` refreshes behind the existing `last_poll` (1 s) gate;
//! every chip renderer reads only that cached copy.

use std::time::{Duration, Instant};

use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

use super::telemetry::{DriveSpace, active_keyboard_code, sample_drives};

/// Sensor polling cadence shared with `WidgetRuntime::last_poll`.
pub const POLL_TTL: Duration = Duration::from_secs(1);

/// C# `StorageWidget`: `DriveType.Fixed && IsReady`. Filtering happens once per snapshot, never
/// inside a chip renderer (which runs at VSync frequency).
fn fixed_ready(drives: &[DriveSpace]) -> Vec<DriveSpace> {
    const DRIVE_FIXED: u32 = 3;
    drives
        .iter()
        .filter(|d| {
            let root: Vec<u16> = format!("{}:\\\0", d.letter).encode_utf16().collect();
            unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED }
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct WidgetPollSnapshot {
    /// All sampled drives (used by the dynamic width calculation).
    pub drives: Vec<DriveSpace>,
    /// Fixed + ready drives only (used by the chip, tooltip and flyout).
    pub fixed_ready_drives: Vec<DriveSpace>,
    pub keyboard_code: String,
    sampled_at: Option<Instant>,
}

impl WidgetPollSnapshot {
    /// Builds a snapshot with explicit data (used by unit tests and by the runtime bootstrap).
    pub fn with_samples(drives: Vec<DriveSpace>, keyboard_code: String, sampled_at: Instant) -> Self {
        let fixed_ready_drives = fixed_ready(&drives);
        Self {
            drives,
            fixed_ready_drives,
            keyboard_code,
            sampled_at: Some(sampled_at),
        }
    }

    /// True when the cached values are younger than `POLL_TTL`.
    pub fn is_fresh(&self, now: Instant) -> bool {
        self.sampled_at
            .is_some_and(|at| now.duration_since(at) < POLL_TTL)
    }

    /// Refreshes all snapshot fields from the real providers and stamps them atomically.
    pub fn refresh(&mut self) {
        let drives = sample_drives();
        let keyboard_code = active_keyboard_code();
        *self = Self::with_samples(drives, keyboard_code, Instant::now());
    }

    /// Fixed + ready drives for chip renderers (zero per-frame disk/drive-type queries).
    pub fn fixed_drives(&self) -> &[DriveSpace] {
        &self.fixed_ready_drives
    }

    /// Refreshes only when stale. Safe to call from a chip renderer (no work on a cache hit).
    pub fn ensure_fresh(&mut self, now: Instant) {
        if !self.is_fresh(now) {
            self.refresh();
        }
    }

    pub fn sampled_at(&self) -> Option<Instant> {
        self.sampled_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_expires_after_one_second() {
        let now = Instant::now();
        let snap = WidgetPollSnapshot::with_samples(Vec::new(), "TR".to_owned(), now);
        assert!(snap.is_fresh(now));
        assert!(snap.is_fresh(now + Duration::from_millis(999)));
        assert!(!snap.is_fresh(now + Duration::from_secs(1)));
    }

    #[test]
    fn empty_snapshot_is_never_fresh() {
        assert!(!WidgetPollSnapshot::default().is_fresh(Instant::now()));
    }

    #[test]
    fn refresh_stamps_the_snapshot() {
        let mut snap = WidgetPollSnapshot::default();
        snap.refresh();
        assert!(snap.is_fresh(Instant::now()));
        assert!(snap.sampled_at().is_some());
    }
}
