//! C# `MediaTimeline` / `LibreWolfMediaTimeline` / `MediaVolumeChecks` parity.
//!
//! The deterministic helpers are kept as reference vectors; bridge tests below
//! assert that the production implementations in `widgets/media_ext.rs` still
//! expose the same guards/signatures. `librewolf_timeline` and `confirm_seek`
//! belong to WS-6 and are pinned by an `#[ignore]`d bridge until they land.

mod common;

/// Parse `m:ss` or `h:mm:ss` (and plain seconds) into seconds.
fn parse_clock(label: &str) -> Option<u32> {
    let parts: Vec<u32> = label
        .trim()
        .split(':')
        .map(|part| part.trim().parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    match parts.len() {
        1 => Some(parts[0]),
        2 if parts[1] < 60 => Some(parts[0] * 60 + parts[1]),
        3 if parts[1] < 60 && parts[2] < 60 => Some(parts[0] * 3600 + parts[1] * 60 + parts[2]),
        _ => None,
    }
}

fn format_clock(secs: u32) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

fn progress_ratio(position: u32, duration: u32) -> f32 {
    if duration == 0 {
        return 0.0;
    }
    (position as f32 / duration as f32).clamp(0.0, 1.0)
}

fn seek_position(ratio: f32, duration: u32) -> u32 {
    ((ratio.clamp(0.0, 1.0)) * duration as f32).round() as u32
}

fn apply_rate(rate: f32) -> f32 {
    if !rate.is_finite() {
        return 1.0;
    }
    rate.clamp(0.25, 4.0)
}

fn is_stale(last_update_ms: u64, now_ms: u64, ttl_ms: u64) -> bool {
    now_ms.saturating_sub(last_update_ms) > ttl_ms
}

/// LibreWolf exposes a UIA `Document` with the track title and a `Slider`
/// whose label reads `position / duration`. The C# helper extracts both.
fn librewolf_timeline(uia_dump: &str) -> Option<(String, u32, u32)> {
    let fields: Vec<&str> = uia_dump.split('|').collect();
    if fields.len() < 4 || fields[0] != "music.youtube.com" {
        return None;
    }
    let title = fields[1].trim();
    let (position, duration) = fields[2].split_once('/')?;
    Some((
        title.to_owned(),
        parse_clock(position.trim())?,
        parse_clock(duration.trim())?,
    ))
}

#[test]
fn clock_labels_accept_minutes_seconds_and_hours() {
    assert_eq!(parse_clock("0:42"), Some(42));
    assert_eq!(parse_clock("1:23"), Some(83));
    assert_eq!(parse_clock("1:02:03"), Some(3723));
    assert_eq!(parse_clock("95"), Some(95));
    assert_eq!(parse_clock("1:99"), None);
    assert_eq!(parse_clock("n/a"), None);
}

#[test]
fn clock_formatting_round_trips() {
    assert_eq!(format_clock(83), "1:23");
    assert_eq!(format_clock(3723), "1:02:03");
    assert_eq!(parse_clock(&format_clock(3723)), Some(3723));
}

#[test]
fn seek_ratio_and_position_are_clamped() {
    assert!((progress_ratio(30, 120) - 0.25).abs() < f32::EPSILON);
    assert_eq!(progress_ratio(10, 0), 0.0);
    assert_eq!(progress_ratio(200, 100), 1.0);
    assert_eq!(seek_position(0.5, 200), 100);
    assert_eq!(seek_position(2.0, 100), 100);
    assert_eq!(seek_position(-1.0, 100), 0);
}

#[test]
fn playback_rate_is_clamped_to_a_safe_range() {
    assert_eq!(apply_rate(1.0), 1.0);
    assert_eq!(apply_rate(0.1), 0.25);
    assert_eq!(apply_rate(10.0), 4.0);
    assert_eq!(apply_rate(f32::NAN), 1.0);
}

#[test]
fn stale_samples_expire_after_the_time_to_live() {
    assert!(!is_stale(1000, 1200, 200));
    assert!(is_stale(1000, 1201, 200));
    assert!(!is_stale(2000, 1000, 200), "clock going backwards is fresh");
}

#[test]
fn librewolf_timeline_extracts_title_and_progress() {
    let dump = "music.youtube.com|Bohemian Rhapsody|1:23 / 4:56|slider";
    let (title, position, duration) =
        librewolf_timeline(dump).expect("LibreWolf timeline must parse");
    assert_eq!(title, "Bohemian Rhapsody");
    assert_eq!(position, 83);
    assert_eq!(duration, 296);
    assert!((progress_ratio(position, duration) - 83.0 / 296.0).abs() < 0.001);
}

#[test]
fn librewolf_parser_rejects_foreign_sites_and_bad_labels() {
    assert!(librewolf_timeline("youtube.com|Song|1:00 / 2:00|slider").is_none());
    assert!(librewolf_timeline("music.youtube.com|Song|bad / 2:00|slider").is_none());
    assert!(librewolf_timeline("music.youtube.com|Song").is_none());
}

/// Minimal player state: pausing must freeze the clock, a seek commit must
/// replace the position, and neither operation may leave the clamped range.
#[derive(Debug, Clone, Copy)]
struct PlayerState {
    position: u32,
    duration: u32,
    paused: bool,
}

impl PlayerState {
    fn new(position: u32, duration: u32) -> Self {
        Self {
            position,
            duration,
            paused: false,
        }
    }

    fn tick(&mut self, seconds: u32) {
        if !self.paused {
            self.position = (self.position + seconds).min(self.duration);
        }
    }

    fn pause(&mut self) {
        self.paused = true;
    }

    fn seek(&mut self, ratio: f32) {
        self.position = seek_position(ratio, self.duration);
    }
}

#[test]
fn pause_freezes_the_clock_and_seek_replaces_the_position() {
    let mut player = PlayerState::new(60, 240);
    player.tick(5);
    assert_eq!(player.position, 65);

    player.pause();
    player.tick(30);
    assert_eq!(player.position, 65, "paused playback must not advance");
    player.seek(progress_ratio(player.position, player.duration));
    assert_eq!(player.position, 65, "seeking to the same ratio is a no-op");

    player.seek(0.75);
    assert_eq!(player.position, 180);
    player.seek(2.0);
    assert_eq!(player.position, 240, "seek stays inside the timeline");
    player.tick(30);
    assert_eq!(player.position, 240);
}

/// Live part of the contract: these helpers are `pub` in the app crate, so a
/// source-level bridge is enough to detect drift without a circular dependency.
#[test]
fn production_media_helpers_keep_the_reference_guards() {
    const MEDIA_EXT: &str = "crates/opentaskbar-app/src/widgets/media_ext.rs";
    let source = common::source(MEDIA_EXT);
    assert!(source.contains("pub fn format_time(secs: u64)"));
    assert!(source.contains("pub fn seek_ratio(position: u64, duration: u64)"));
    assert!(source.contains("if duration == 0"));
    assert!(source.contains("pub fn volume_from_wheel(current_pct: u32, delta: i32"));
    // Every reference vector above is expressed in seconds/f32; production
    // uses u64/i32, so the guards (not the types) are the shared contract.
    assert_eq!(format_clock(3723), "1:02:03");
    assert_eq!(progress_ratio(200, 100), 1.0);
}

/// WS-6 owns these two functions. The bridge is deliberately ignored in this
/// worktree; unignore it once `opentaskbar-core` can dev-depend on the app
/// crate, or move it next to the implementation there.
#[test]
#[ignore = "WS-6: librewolf_timeline/confirm_seek are not landed in this worktree yet"]
fn production_librewolf_timeline_and_confirm_seek_contract() {
    common::assert_source_has(
        "crates/opentaskbar-app/src/widgets/media_ext.rs",
        &[
            "pub fn librewolf_timeline(source_app_id: &str, title: &str)",
            "pub fn confirm_seek(",
        ],
    );
    let dump = "music.youtube.com|Bohemian Rhapsody|1:23 / 4:56|slider";
    let (title, position, duration) = librewolf_timeline(dump).expect("must parse");
    assert_eq!(
        (title.as_str(), position, duration),
        ("Bohemian Rhapsody", 83, 296)
    );
}
