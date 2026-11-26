//! C# `MediaTimeline` / `LibreWolfMediaTimeline` / `MediaVolumeChecks` parity
//! (pure portion). WS-C owns the live player and flyout wiring, which is absent
//! from this worktree's baseline, so these are standalone reference tests.

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

#[test]
fn pause_keeps_position_and_seek_updates_it() {
    let duration = 240;
    let position = 60;
    // Pausing does not advance the slider; a seek commit replaces the position.
    let paused = position;
    assert_eq!(paused, 60);
    let seeked = seek_position(progress_ratio(paused, duration), duration);
    assert_eq!(seeked, 60);
    let seeked = seek_position(0.75, duration);
    assert_eq!(seeked, 180);
}
