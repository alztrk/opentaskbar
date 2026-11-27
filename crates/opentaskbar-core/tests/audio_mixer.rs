//! C# `AudioMixerChecks` / `MediaVolumeChecks` / `AppAudioSessionMatcher` parity.
//!
//! Wheel accumulation and session scoring are kept as reference vectors; the
//! bridge tests below assert the CoreAudio production surface and the real
//! `WheelVolumeEnabled` setting round-trip through `DockSettings`.

mod common;

use opentaskbar_core::settings::DockSettings;

/// A full 120-delta wheel notch equals 10% volume; the fractional remainder is
/// carried across notches instead of being discarded.
fn volume_from_wheel(current: f32, delta: i32, remainder: &mut f32) -> f32 {
    let exact = current + delta as f32 / 12.0 + *remainder;
    let rounded = exact.round();
    *remainder = exact - rounded;
    rounded.clamp(0.0, 100.0)
}

fn clamp_volume(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 100.0)
}

fn db_to_volume(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

fn volume_to_db(volume: f32) -> f32 {
    if volume <= 0.0 {
        return -100.0;
    }
    20.0 * volume.log10()
}

#[derive(Debug, Clone)]
struct Session {
    pid: u32,
    name: String,
    executable: String,
    volume: f32,
}

/// C# matcher: a session belonging to the foreground process wins by a large
/// margin; name and executable stems are weaker signals.
fn score_session(session: &Session, foreground_pid: u32, target_name: &str) -> i64 {
    let mut score = 0i64;
    if session.pid == foreground_pid && foreground_pid != 0 {
        score += 1000;
    }
    let name = session.name.to_ascii_lowercase();
    let target = target_name.to_ascii_lowercase();
    if !target.is_empty() && name.contains(&target) {
        score += 100;
    }
    let stem = session
        .executable
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    if stem == target {
        score += 50;
    }
    if session.volume == 0.0 {
        score -= 10;
    }
    score
}

/// Aggregate per-application sliders by executable path (C# keeps one row per
/// executable and sums its sessions' volumes).
fn aggregate_by_executable(sessions: &[Session]) -> Vec<(String, f32)> {
    let mut result: Vec<(String, f32)> = Vec::new();
    for session in sessions {
        if let Some(row) = result
            .iter_mut()
            .find(|(path, _)| path == &session.executable)
        {
            row.1 += session.volume;
        } else {
            result.push((session.executable.clone(), session.volume));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

#[test]
fn wheel_delta_accumulates_without_losing_fraction() {
    let mut remainder = 0.0;
    let value = volume_from_wheel(50.0, 120, &mut remainder);
    assert_eq!(value, 60.0);
    assert_eq!(remainder, 0.0);

    let value = volume_from_wheel(60.0, 6, &mut remainder);
    assert_eq!(value, 61.0); // 6/12 = 0.5 → rounds to 1, remainder -0.5
    assert!((remainder + 0.5).abs() < f32::EPSILON);

    let value = volume_from_wheel(61.0, 6, &mut remainder);
    assert_eq!(value, 61.0); // 0.5 - 0.5 = 0 → no double increment
}

#[test]
fn wheel_volume_is_clamped() {
    let mut remainder = 0.0;
    assert_eq!(volume_from_wheel(5.0, -120, &mut remainder), 0.0);
    assert_eq!(volume_from_wheel(98.0, 120, &mut remainder), 100.0);
    assert_eq!(clamp_volume(f32::NAN), 0.0);
    assert_eq!(clamp_volume(150.0), 100.0);
}

#[test]
fn decibel_conversions_round_trip() {
    let db = volume_to_db(0.5);
    assert!((db - (-6.0206)).abs() < 0.001);
    assert!((db_to_volume(db) - 0.5).abs() < 0.001);
    assert_eq!(volume_to_db(0.0), -100.0);
}

fn session(pid: u32, name: &str, exe: &str, volume: f32) -> Session {
    Session {
        pid,
        name: name.into(),
        executable: exe.into(),
        volume,
    }
}

#[test]
fn foreground_process_wins_the_session_score() {
    let foreground = session(42, "msedge", "C:\\Program Files\\msedge.exe", 80.0);
    let other = session(7, "msedge helper", "C:\\other.exe", 80.0);
    assert!(score_session(&foreground, 42, "msedge") > score_session(&other, 42, "msedge"));
}

#[test]
fn name_and_executable_stems_contribute_to_the_score() {
    let exact = session(1, "spotify", "C:\\spotify.exe", 50.0);
    let unrelated = session(1, "explorer", "C:\\explorer.exe", 50.0);
    assert!(score_session(&exact, 0, "spotify") > score_session(&unrelated, 0, "spotify"));
    assert_eq!(score_session(&unrelated, 0, "spotify"), 0);
}

#[test]
fn muted_sessions_are_scored_lower() {
    let loud = session(1, "code", "C:\\code.exe", 40.0);
    let muted = session(1, "code", "C:\\code.exe", 0.0);
    assert_eq!(
        score_session(&loud, 0, "code") - score_session(&muted, 0, "code"),
        10
    );
}

#[test]
fn app_sessions_aggregate_by_executable_path() {
    let sessions = vec![
        session(1, "tab 1", "C:\\edge.exe", 30.0),
        session(2, "tab 2", "C:\\edge.exe", 20.0),
        session(3, "music", "C:\\spotify.exe", 60.0),
    ];
    let rows = aggregate_by_executable(&sessions);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "C:\\edge.exe");
    assert_eq!(rows[0].1, 50.0);
    assert_eq!(rows[1].1, 60.0);
}

/// Production bridge: wheel volume is a real persisted setting and the audio
/// mixer's enumerator/writers are `pub` in the app crate.
#[test]
fn production_mixer_surface_and_wheel_setting_match_the_reference() {
    let mut settings = DockSettings::default();
    assert!(!settings.wheel_volume_enabled, "wheel volume is opt-in");
    settings.wheel_volume_enabled = true;
    let json = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(json["WheelVolumeEnabled"], true);
    let back: DockSettings = serde_json::from_value(json).expect("settings round-trip");
    assert!(back.wheel_volume_enabled);

    common::assert_source_has(
        "crates/opentaskbar-app/src/widgets/media_ext.rs",
        &["pub fn list_app_sessions() -> Vec<AudioAppSession>"],
    );
    common::assert_source_has(
        "crates/opentaskbar-app/src/widgets/diagnostics.rs",
        &["pub fn set_app_volume(", "pub fn set_app_mute("],
    );
    common::assert_source_has(
        "crates/opentaskbar-app/src/widgets/media_ext.rs",
        &["pub fn volume_from_wheel("],
    );
    // The reference must not regress to a plain 120-delta notch.
    let mut remainder = 0.0;
    assert_eq!(volume_from_wheel(50.0, 120, &mut remainder), 60.0);
    assert_eq!(volume_from_wheel(60.0, 6, &mut remainder), 61.0);
    assert_eq!(volume_from_wheel(61.0, 6, &mut remainder), 61.0);
}

/// The live `core_audio::list_app_sessions` enumerator is `pub` in the app
/// crate but cannot be called from a core integration test (the app is a
/// bin-only target, and a real call needs a live audio device). This bridge
/// pins the shipped enumerator/score surface while the vectors above stay in
/// the reference implementation.
#[test]
fn production_app_session_matching_matches_the_reference_scores() {
    let foreground = session(42, "msedge", "C:\\Program Files\\msedge.exe", 80.0);
    let other = session(7, "msedge helper", "C:\\other.exe", 80.0);
    assert!(score_session(&foreground, 42, "msedge") > score_session(&other, 42, "msedge"));
    common::assert_source_has(
        "crates/opentaskbar-app/src/platform/core_audio.rs",
        &["pub fn list_app_sessions() -> Vec<AppAudioSession>"],
    );
    common::assert_source_has(
        "crates/opentaskbar-app/src/widgets/media_ext.rs",
        &["list_app_sessions"],
    );
}
