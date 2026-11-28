//! Synced LRC lyrics parser and opt-in LRCLIB LRU cache (N6).
//!
//! Zero-overhead when disabled:
//! - [`LyricsCache::poll_for_track`] returns `None` immediately with zero HTTP, disk, or heap
//!   allocations whenever `enabled == false` or `title` is empty.
//! - Keeps at most [`MAX_CACHED_TRACKS`] (8) tracks in memory (`< 32 KB` RAM) via LRU eviction.
//! - Persists fetched `.lrc` files under `%LOCALAPPDATA%\OpenTaskbar\Lyrics\<hash>.lrc`.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::platform::http::{HttpCfg, get_body};

/// Maximum number of parsed tracks kept in memory simultaneously.
pub const MAX_CACHED_TRACKS: usize = 8;

const MAX_LRCLIB_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_DISK_LRC_BYTES: u64 = 128 * 1024;
const MAX_NEGATIVE_CACHE_KEYS: usize = 128;

/// One timestamped lyric line from an LRC document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    pub time_ms: u64,
    pub text: String,
}

/// Parsed, time-sorted synced lyrics for a single track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedLyrics {
    pub track_key: String,
    pub lines: Vec<LyricLine>,
}

impl SyncedLyrics {
    /// Parses an LRC string into a sorted [`SyncedLyrics`] value.
    #[must_use]
    pub fn from_lrc(track_key: impl Into<String>, lrc: &str) -> Self {
        Self {
            track_key: track_key.into(),
            lines: Self::parse_lrc(lrc),
        }
    }

    /// Parses standard and extended LRC timestamps (`[mm:ss.xx]`, `[mm:ss.xxx]`, multi-timestamp
    /// lines `[00:12.00][01:12.00] Chorus`), trims empty/metadata lines, and sorts by `time_ms`.
    #[must_use]
    pub fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
        parse_lrc(lrc)
    }

    /// Binary-search lookup (`O(log N)`) returning `(index, line)` active at `pos_ms`.
    #[must_use]
    pub fn line_at(&self, pos_ms: u64) -> Option<(usize, &LyricLine)> {
        let idx = self.lines.partition_point(|line| line.time_ms <= pos_ms);
        if idx == 0 {
            None
        } else {
            let active = idx - 1;
            Some((active, &self.lines[active]))
        }
    }

    /// Returns a slice window `(index, line, is_current)` around the lyric active at `pos_ms`,
    /// suitable for displaying clickable seekable lines in an expanded Media panel or flyout.
    #[must_use]
    pub fn window_around(
        &self,
        pos_ms: u64,
        before: usize,
        after: usize,
    ) -> Vec<(usize, &LyricLine, bool)> {
        if self.lines.is_empty() {
            return Vec::new();
        }
        if let Some((cur, _)) = self.line_at(pos_ms) {
            let start = cur.saturating_sub(before);
            let end = cur
                .saturating_add(after)
                .saturating_add(1)
                .min(self.lines.len());
            (start..end)
                .map(|i| (i, &self.lines[i], i == cur))
                .collect()
        } else {
            let end = after.saturating_add(1).min(self.lines.len());
            (0..end).map(|i| (i, &self.lines[i], false)).collect()
        }
    }
}

/// Parses an LRC document into sorted [`LyricLine`] entries.
#[must_use]
pub fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw_line in lrc.lines() {
        let mut rest = raw_line.trim();
        if rest.is_empty() {
            continue;
        }
        let mut timestamps = Vec::new();
        while rest.starts_with('[') {
            let Some(close) = rest.find(']') else {
                break;
            };
            let tag = &rest[1..close];
            if let Some(ms) = parse_lrc_timestamp(tag) {
                timestamps.push(ms);
                rest = rest[close + 1..].trim_start();
            } else {
                // Metadata tag such as [ar:Artist], [ti:Title], [offset:0] -> skip whole tag.
                rest = rest[close + 1..].trim_start();
            }
        }
        let text = rest.trim();
        if timestamps.is_empty() || text.is_empty() {
            continue;
        }
        for time_ms in timestamps {
            lines.push(LyricLine {
                time_ms,
                text: text.to_owned(),
            });
        }
    }
    lines.sort_by_key(|line| line.time_ms);
    lines
}

fn parse_lrc_timestamp(tag: &str) -> Option<u64> {
    let (mins_str, sec_part) = tag.trim().split_once(':')?;
    if mins_str.is_empty() || !mins_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mins = mins_str.parse::<u64>().ok()?;
    let (secs_str, frac_ms) = if let Some((s, frac)) = sec_part.split_once('.') {
        (s, parse_fraction_ms(frac)?)
    } else if let Some((s, frac)) = sec_part.split_once(':') {
        (s, parse_fraction_ms(frac)?)
    } else {
        (sec_part, 0)
    };
    if secs_str.is_empty() || !secs_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let secs = secs_str.parse::<u64>().ok()?;
    if secs >= 60 {
        return None;
    }
    mins.checked_mul(60_000)?
        .checked_add(secs.checked_mul(1000)?)?
        .checked_add(frac_ms)
}

fn parse_fraction_ms(frac: &str) -> Option<u64> {
    if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let raw = frac.parse::<u64>().ok()?;
    let ms = match frac.len() {
        1 => raw * 100,
        2 => raw * 10,
        _ => raw,
    };
    Some(ms)
}

enum FetchResult {
    Found(SyncedLyrics),
    Miss(String),
}

/// Bounded 8-track LRU cache with local `.lrc` disk cache and opt-in non-blocking LRCLIB lookup.
pub struct LyricsCache {
    entries: VecDeque<SyncedLyrics>,
    negative_misses: HashSet<String>,
    in_flight: Option<(String, Receiver<FetchResult>)>,
}

impl Default for LyricsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(MAX_CACHED_TRACKS),
            negative_misses: HashSet::new(),
            in_flight: None,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn contains(&self, track_key: &str) -> bool {
        self.entries.iter().any(|e| e.track_key == track_key)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.negative_misses.clear();
        self.in_flight = None;
    }

    /// Inserts `lyrics` at the MRU front, evicting the oldest entry if [`MAX_CACHED_TRACKS`] is reached.
    pub fn insert(&mut self, lyrics: SyncedLyrics) {
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.track_key == lyrics.track_key)
        {
            self.entries.remove(pos);
        } else if self.entries.len() >= MAX_CACHED_TRACKS {
            self.entries.pop_back();
        }
        self.entries.push_front(lyrics);
    }

    /// Non-blocking lyrics lookup:
    /// 1. Returns `None` immediately (0 HTTP, 0 disk, 0 allocations) when `!enabled` or `title` is empty.
    /// 2. Checks in-memory 8-track LRU cache.
    /// 3. Checks `%LOCALAPPDATA%\OpenTaskbar\Lyrics\<hash>.lrc`.
    /// 4. Spawns at most 1 background WinHTTP GET to `lrclib.net` (`<= 128 KB` response cap).
    pub fn poll_for_track(
        &mut self,
        enabled: bool,
        title: &str,
        artist: &str,
        duration_secs: Option<u64>,
    ) -> Option<&SyncedLyrics> {
        if !enabled || title.trim().is_empty() {
            return None;
        }
        self.drain_in_flight();

        let key = track_key(title, artist);
        if self.promote_if_present(&key) {
            return self.entries.front();
        }
        if self.negative_misses.contains(&key) {
            return None;
        }

        if let Some(disk_lyrics) = load_from_disk_cache(&key) {
            self.insert(disk_lyrics);
            return self.entries.front();
        }

        if self.in_flight.is_none() {
            self.spawn_lrclib_fetch(
                key,
                title.trim().to_owned(),
                artist.trim().to_owned(),
                duration_secs,
            );
        }
        None
    }

    fn promote_if_present(&mut self, key: &str) -> bool {
        let Some(pos) = self.entries.iter().position(|e| e.track_key == key) else {
            return false;
        };
        if pos > 0
            && let Some(item) = self.entries.remove(pos)
        {
            self.entries.push_front(item);
        }
        true
    }

    fn record_miss(&mut self, key: String) {
        if self.negative_misses.len() >= MAX_NEGATIVE_CACHE_KEYS {
            self.negative_misses.clear();
        }
        self.negative_misses.insert(key);
    }

    fn drain_in_flight(&mut self) {
        let Some((_key, rx)) = &self.in_flight else {
            return;
        };
        match rx.try_recv() {
            Ok(FetchResult::Found(lyrics)) => {
                self.in_flight = None;
                self.insert(lyrics);
            }
            Ok(FetchResult::Miss(miss_key)) => {
                self.in_flight = None;
                self.record_miss(miss_key);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                if let Some((miss_key, _)) = self.in_flight.take() {
                    self.record_miss(miss_key);
                }
            }
        }
    }

    fn spawn_lrclib_fetch(
        &mut self,
        key: String,
        title: String,
        artist: String,
        duration_secs: Option<u64>,
    ) {
        let (tx, rx) = mpsc::sync_channel::<FetchResult>(1);
        let key_for_thread = key.clone();
        if thread::Builder::new()
            .name("lyrics-lrclib".to_owned())
            .spawn(move || {
                let outcome =
                    match fetch_lrclib_synced(&key_for_thread, &title, &artist, duration_secs) {
                        Some(lyrics) => {
                            save_to_disk_cache(&lyrics);
                            FetchResult::Found(lyrics)
                        }
                        None => FetchResult::Miss(key_for_thread),
                    };
                let _ = tx.send(outcome);
            })
            .is_ok()
        {
            self.in_flight = Some((key, rx));
        } else {
            self.record_miss(key);
        }
    }
}

/// Canonical normalized cache key for `(title, artist)`.
#[must_use]
pub fn track_key(title: &str, artist: &str) -> String {
    format!(
        "{}\u{1f}{}",
        artist.trim().to_ascii_lowercase(),
        title.trim().to_ascii_lowercase()
    )
}

fn lyrics_cache_dir() -> PathBuf {
    opentaskbar_core::paths::shared_app_data_dir().join("Lyrics")
}

fn disk_cache_path(key: &str) -> PathBuf {
    let digest = Sha256::digest(key.as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    lyrics_cache_dir().join(format!("{hex}.lrc"))
}

fn load_from_disk_cache(key: &str) -> Option<SyncedLyrics> {
    let path = disk_cache_path(key);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_DISK_LRC_BYTES {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let lines = parse_lrc(&raw);
    (!lines.is_empty()).then(|| SyncedLyrics {
        track_key: key.to_owned(),
        lines,
    })
}

fn save_to_disk_cache(lyrics: &SyncedLyrics) {
    let dir = lyrics_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = disk_cache_path(&lyrics.track_key);
    let mut content = String::new();
    for line in &lyrics.lines {
        let total_secs = line.time_ms / 1000;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        let ms = line.time_ms % 1000;
        use std::fmt::Write;
        let _ = writeln!(content, "[{mins:02}:{secs:02}.{ms:03}] {}", line.text);
    }
    let _ = std::fs::write(path, content);
}

fn fetch_lrclib_synced(
    key: &str,
    title: &str,
    artist: &str,
    duration_secs: Option<u64>,
) -> Option<SyncedLyrics> {
    let mut url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}",
        percent_encode(title),
        percent_encode(artist),
    );
    if let Some(dur) = duration_secs.filter(|d| *d > 0) {
        url.push_str(&format!("&duration={dur}"));
    }

    let mut cfg = HttpCfg::new("OpenTaskbar");
    cfg.timeouts = [4_000, 4_000, 5_000, 6_000];
    cfg.max_response_bytes = Some(MAX_LRCLIB_RESPONSE_BYTES);
    cfg.headers
        .push(("Accept".to_owned(), "application/json".to_owned()));

    let (status, body) = get_body(&cfg, &url)?;
    if status != 200 || body.is_empty() || body.len() > MAX_LRCLIB_RESPONSE_BYTES {
        return None;
    }
    let json: Value = serde_json::from_slice(&body).ok()?;
    let synced_lrc = json.get("syncedLyrics").and_then(Value::as_str)?;
    let lines = parse_lrc(synced_lrc);
    (!lines.is_empty()).then(|| SyncedLyrics {
        track_key: key.to_owned(),
        lines,
    })
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push_str("%20"),
            b => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_parser_handles_meta_multi_timestamps_and_ms_precision() {
        let lrc = "\
[ar:Daft Punk]
[ti:Veridis Quo]
[00:12.00][01:12.500] Chorus line
[00:05.25] First line
[00:08.5] Half second
[00:10.00]
[invalid:tag]
";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            LyricLine {
                time_ms: 5250,
                text: "First line".into()
            }
        );
        assert_eq!(
            lines[1],
            LyricLine {
                time_ms: 8500,
                text: "Half second".into()
            }
        );
        assert_eq!(
            lines[2],
            LyricLine {
                time_ms: 12000,
                text: "Chorus line".into()
            }
        );
        assert_eq!(
            lines[3],
            LyricLine {
                time_ms: 72500,
                text: "Chorus line".into()
            }
        );
    }

    #[test]
    fn binary_search_line_at_and_window_around() {
        let synced = SyncedLyrics::from_lrc(
            "artist\u{1f}track",
            "[00:05.00] Line 1\n[00:10.00] Line 2\n[00:15.00] Line 3\n[00:20.00] Line 4\n",
        );
        assert_eq!(synced.line_at(4999), None);
        assert_eq!(
            synced.line_at(5000).map(|(i, l)| (i, l.text.as_str())),
            Some((0, "Line 1"))
        );
        assert_eq!(
            synced.line_at(14999).map(|(i, l)| (i, l.text.as_str())),
            Some((1, "Line 2"))
        );
        assert_eq!(
            synced.line_at(25000).map(|(i, l)| (i, l.text.as_str())),
            Some((3, "Line 4"))
        );

        let win = synced.window_around(16000, 1, 1);
        assert_eq!(win.len(), 3);
        assert_eq!((win[0].0, win[0].2), (1, false));
        assert_eq!((win[1].0, win[1].2), (2, true));
        assert_eq!((win[2].0, win[2].2), (3, false));
    }

    #[test]
    fn lru_evicts_oldest_beyond_eight_tracks_and_disabled_is_noop() {
        let mut cache = LyricsCache::new();
        assert!(
            cache
                .poll_for_track(false, "Song", "Artist", Some(180))
                .is_none()
        );
        assert!(cache.poll_for_track(true, "   ", "Artist", None).is_none());
        assert_eq!(cache.len(), 0);

        for i in 0..10 {
            cache.insert(SyncedLyrics::from_lrc(
                track_key(&format!("Song {i}"), "Artist"),
                "[00:01.00] Hello",
            ));
        }
        assert_eq!(cache.len(), MAX_CACHED_TRACKS);
        assert!(!cache.contains(&track_key("Song 0", "Artist")));
        assert!(!cache.contains(&track_key("Song 1", "Artist")));
        assert!(cache.contains(&track_key("Song 2", "Artist")));
        assert!(cache.contains(&track_key("Song 9", "Artist")));

        // Touch Song 2 via poll_for_track so it moves to MRU and survives next eviction.
        assert!(
            cache
                .poll_for_track(true, "Song 2", "Artist", None)
                .is_some()
        );
        cache.insert(SyncedLyrics::from_lrc(
            track_key("Song A", "Artist A"),
            "[00:01.00] A",
        ));
        assert_eq!(cache.len(), MAX_CACHED_TRACKS);
        assert!(cache.contains(&track_key("Song 2", "Artist")));
        assert!(!cache.contains(&track_key("Song 3", "Artist")));
        assert!(
            cache
                .poll_for_track(true, "Song A", "Artist A", None)
                .is_some()
        );
    }
}
