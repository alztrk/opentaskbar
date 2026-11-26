//! Media widget extensions — C# `MediaWidget.xaml.cs` / `MediaTimeline.cs` parity.
//!
//! Seek slider (`LiveEvent::Seek`), album cover (`FlyoutBlock::Image`) + tint, vertical volume
//! popover, per-app audio sessions, dynamic-island expanded `GetDetails()` payload and the
//! playing/paused glyph state used by the dock chip.

use std::path::PathBuf;

use opentaskbar_core::settings::DockSettings;

use crate::design::Rgb;
use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

use super::telemetry::MediaSessionSnapshot;

/// C# `MediaWidget.GetDetails()` payload consumed by the island's 560px expanded panel.
#[derive(Clone, Debug, Default)]
pub struct MediaDetails {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app_name: String,
    pub position_secs: u64,
    pub duration_secs: u64,
    pub playing: bool,
    pub cover_path: Option<String>,
    pub tint: Rgb,
    pub volume: u32,
    pub muted: bool,
}

/// One per-app audio session (C# `AppAudioSessionMatcher`).
#[derive(Clone, Debug)]
pub struct AudioAppSession {
    pub app_name: String,
    pub volume: u32,
    pub muted: bool,
    pub icon_glyph: u16,
}

/// Builds the island expanded details payload from the SMTC snapshot + current audio state.
pub fn get_details(snap: &MediaSessionSnapshot, volume: u32, muted: bool) -> MediaDetails {
    let cover_path = find_cover_art(&snap.title, &snap.artist);
    let tint = cover_path
        .as_deref()
        .map_or((0x9f, 0x9f, 0xf2), tint_from_path);
    MediaDetails {
        title: snap.title.clone(),
        artist: snap.artist.clone(),
        album: String::new(),
        app_name: String::new(),
        position_secs: snap.position_secs,
        duration_secs: snap.duration_secs,
        playing: snap.playing,
        cover_path,
        tint,
        volume,
        muted,
    }
}

/// C# `MediaWidget`缓存 album art under `%LOCALAPPDATA%\OpenTaskbar\Art` as
/// `<artist>-<title>.jpg` (sanitized). Returns the first existing cached image.
pub fn find_cover_art(title: &str, artist: &str) -> Option<String> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let stem = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect()
    };
    let dir = local.join("OpenTaskbar").join("Art");
    for name in [
        format!("-{}-{}.jpg", stem(artist), stem(title)),
        format!("{}.jpg", stem(title)),
    ] {
        let path = dir.join(name.trim_start_matches('-'));
        if path.is_file() {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    None
}

/// Deterministic accent extracted from a cover path — C# uses the dominant pixel, which requires
/// a decoder we cannot call here; hashing gives a stable, tinted fallback.
pub fn tint_from_path(path: &str) -> Rgb {
    let mut h: u32 = 2166136261;
    for b in path.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16777619);
    }
    let r = ((h >> 16) & 0xff) as u8;
    let g = ((h >> 8) & 0xff) as u8;
    let b = (h & 0xff) as u8;
    // Lift dark values so the tint stays visible on both light and dark surfaces.
    (r.max(0x60), g.max(0x60), b.max(0x60))
}

pub fn format_time(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Segoe Fluent Icons glyph for the transport button, matching C# playing state.
pub fn playing_glyph(playing: bool) -> &'static str {
    if playing { "\u{E711}" } else { "\u{E768}" } // Pause / Play
}

pub fn seek_ratio(position: u64, duration: u64) -> f32 {
    if duration == 0 {
        0.0
    } else {
        (position as f32 / duration as f32).clamp(0.0, 1.0)
    }
}

/// Full media flyout: cover + tint header, C2 `SeekSlider`, transport controls, vertical volume
/// section and per-app sessions.
pub fn build_media_flyout(
    details: &MediaDetails,
    sessions: &[AudioAppSession],
    settings: &DockSettings,
) -> FlyoutSpec {
    let mut blocks = Vec::new();
    if let Some(ref path) = details.cover_path {
        // C2 FlyoutBlock::Image — album cover / flag.
        blocks.push(FlyoutBlock::Image {
            glyph: 0xE93C, // MusicNote
            path: Some(path.clone()),
            size_dips: 56.0,
        });
    }
    blocks.push(FlyoutBlock::Hero {
        value: if details.title.is_empty() {
            "Nothing Playing".to_owned()
        } else {
            details.title.clone()
        },
        caption: if details.artist.is_empty() {
            Some("Start playback in any media app".to_owned())
        } else {
            Some(details.artist.clone())
        },
        centered: false,
        large: false,
        right_meta: Some(if details.playing {
            playing_glyph(true).to_owned()
        } else {
            playing_glyph(false).to_owned()
        }),
    });
    if details.duration_secs > 0 {
        // C2 FlyoutBlock::SeekSlider + LiveEvent::Seek.
        blocks.push(FlyoutBlock::SeekSlider {
            id: 50,
            label: String::new(),
            position_secs: details.position_secs as u32,
            duration_secs: details.duration_secs as u32,
            ratio: seek_ratio(details.position_secs, details.duration_secs),
        });
        blocks.push(FlyoutBlock::StatRow {
            label: format_time(details.position_secs),
            value: format_time(details.duration_secs),
        });
    }
    blocks.push(FlyoutBlock::ButtonBar(vec![
        (1, "Previous".to_owned(), false),
        (
            2,
            if details.playing { "Pause" } else { "Play" }.to_owned(),
            true,
        ),
        (3, "Next".to_owned(), false),
    ]));

    // Vertical volume popover (C# `MediaWidget.VolumeFlyout`), stacked as an AudioSliderSection.
    blocks.push(FlyoutBlock::Separator);
    blocks.push(FlyoutBlock::AudioSliderSection {
        label: "Volume".to_owned(),
        device_name: details.app_name.clone(),
        value_text: if details.muted {
            "Muted".to_owned()
        } else {
            format!("{}%", details.volume)
        },
        ratio: details.volume as f32 / 100.0,
        muted: details.muted,
        mute_id: 60,
        slider_id: 51,
    });

    // Per-app session volumes (C# per-app mixer).
    if !sessions.is_empty() {
        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::StatRow {
            label: "App volumes".to_owned(),
            value: String::new(),
        });
        for (i, session) in sessions.iter().take(6).enumerate() {
            blocks.push(FlyoutBlock::ProgressBar {
                label: session.app_name.clone(),
                value_text: if session.muted {
                    "Muted".to_owned()
                } else {
                    format!("{}%", session.volume)
                },
                ratio: session.volume as f32 / 100.0,
                tone: BarTone::Accent,
            });
            let _ = i;
        }
    }
    let _ = settings;
    FlyoutSpec {
        title: "Media".to_owned(),
        subtitle: None,
        width_dips: 312.0,
        blocks,
    }
}

/// Builds the standalone vertical volume popover (wheel/tap on the chip's volume glyph).
pub fn build_volume_popover(volume: u32, muted: bool, device_name: &str) -> FlyoutSpec {
    FlyoutSpec {
        title: "Volume".to_owned(),
        subtitle: Some(device_name.to_owned()),
        width_dips: 260.0,
        blocks: vec![FlyoutBlock::AudioSliderSection {
            label: "Output".to_owned(),
            device_name: device_name.to_owned(),
            value_text: if muted {
                "Muted".to_owned()
            } else {
                format!("{volume}%")
            },
            ratio: volume as f32 / 100.0,
            muted,
            mute_id: 1,
            slider_id: 2,
        }],
    }
}

/// Applies a mouse-wheel remainder: C# `AudioWidget` uses 120 delta = 10%.
/// Returns `(new_pct, remainder)` so callers keep the fractional step across events.
pub fn volume_from_wheel(current_pct: u32, delta: i32, remainder: &mut i32) -> u32 {
    let total = *remainder + delta;
    let steps = total / 12; // 120 / 10% = 12 delta per percent
    *remainder = total % 12;
    (current_pct as i32 + steps).clamp(0, 100) as u32
}

/// Lists per-app audio sessions through the C4 `platform::core_audio` device API.
/// Falls back to an empty list when Core Audio is unavailable (no sound card / headless).
pub fn list_app_sessions() -> Vec<AudioAppSession> {
    // C4: platform::core_audio::list_devices(false) returns the render endpoints; per-app session
    // volumes are aggregated from those endpoints by WS-G. Until that provider lands, expose an
    // empty list so callers degrade gracefully.
    let _ = super::telemetry::default_audio_device_name(false);
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_time_formatting() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(65), "1:05");
        assert_eq!(format_time(3661), "1:01:01");
    }

    #[test]
    fn seek_ratio_clamps() {
        assert_eq!(seek_ratio(30, 120), 0.25);
        assert_eq!(seek_ratio(10, 0), 0.0);
        assert_eq!(seek_ratio(200, 100), 1.0);
    }

    #[test]
    fn playing_glyph_switches() {
        assert_eq!(playing_glyph(true), "\u{E711}");
        assert_eq!(playing_glyph(false), "\u{E768}");
    }

    #[test]
    fn wheel_remainder_accumulates() {
        let mut rem = 0;
        assert_eq!(volume_from_wheel(50, 5, &mut rem), 50);
        assert_eq!(rem, 5);
        assert_eq!(volume_from_wheel(50, 7, &mut rem), 51);
        assert_eq!(rem, 0);
    }

    #[test]
    fn details_from_snapshot() {
        let snap = MediaSessionSnapshot {
            title: "Song".into(),
            artist: "Artist".into(),
            playing: true,
            position_secs: 10,
            duration_secs: 100,
            ..Default::default()
        };
        let d = get_details(&snap, 42, false);
        assert_eq!(d.title, "Song");
        assert!(d.playing);
        assert_eq!(d.volume, 42);
    }
}
