//! O5: mirrors Windows notifications into the Dynamic Island with a strict RAM budget.
//!
//! Two modes:
//! - [`NotificationMirrorMode::MsixNative`]: packaged (MSIX) builds read Action Center toasts
//!   through `Windows.UI.Notifications.Management.UserNotificationListener` on a lazily spawned,
//!   `AtomicBool`-guarded background worker (only created when
//!   `is_msix_packaged() && settings.island_notifications_enabled()`). [`NotificationMirror::poll_msix`]
//!   drains a bounded `Mutex<VecDeque<IslandNotification>>` with `try_lock` and dispatches at most
//!   one background poll every 2 s, keeping the UI thread strictly O(1) non-blocking.
//! - [`NotificationMirrorMode::ShellAlertFallback`]: portable builds have no package identity,
//!   so `UserNotificationListener` would throw; the dock feeds taskbar flashes /
//!   `EVENT_SYSTEM_ALERT` windows through [`NotificationMirror::push_shell_alert`] instead.
//!
//! Memory bounds: at most [`MAX_NOTIFICATIONS`] mirrored entries, bodies capped at
//! [`MAX_BODY_CHARS`] characters, at most [`SEEN_ID_CAP`] remembered toast IDs, a small cooldown
//! table of hashes, and no retained bitmaps or WinRT objects besides the listener itself.
//!
//! Settings come from the typed `DockSettings` accessors:
//! `island_notifications_enabled`, `island_notifications_duration_ms`,
//! `island_notifications_respect_focus` and `island_notifications_muted_apps`.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentaskbar_core::settings::DockSettings;
use windows::UI::Notifications::Management::{
    UserNotificationListener, UserNotificationListenerAccessStatus,
};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

/// Ring-buffer size of mirrored notifications.
pub const MAX_NOTIFICATIONS: usize = 12;
/// Body text cap in characters (including the trailing ellipsis when truncated).
pub const MAX_BODY_CHARS: usize = 200;
/// Upper bound on remembered Action Center toast IDs.
pub const SEEN_ID_CAP: usize = 64;

const MAX_TITLE_CHARS: usize = 120;
const MAX_APP_CHARS: usize = 64;
/// Only the newest toasts of the Action Center list are inspected per poll.
const SCAN_WINDOW: u32 = 32;
/// Text elements read per toast (title + body lines).
const MAX_TEXT_ELEMENTS: u32 = 4;
/// `GetNotificationsAsync` is a cross-process call; never run it more often than this.
const MSIX_POLL_INTERVAL: Duration = Duration::from_millis(2000);
/// Identical `(app, title, body)` alerts inside this window are dropped.
const DUPLICATE_COOLDOWN: Duration = Duration::from_secs(10);
const MAX_COOLDOWN_ENTRIES: usize = 16;
/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_OFFSET_SECS: i64 = 11_644_473_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationMirrorMode {
    MsixNative,
    ShellAlertFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandNotification {
    /// Mirror-local, monotonically increasing ID.
    pub id: u64,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub timestamp_unix: u64,
    /// Window to activate on click (`0` for Action Center toasts).
    pub target_hwnd: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessState {
    Unknown,
    Requested,
    Allowed,
}

/// Raw notification text before filtering and truncation.
#[derive(Debug, Clone, Default)]
struct Candidate {
    app_name: String,
    title: String,
    body: String,
    timestamp_unix: u64,
    target_hwnd: isize,
}

/// Shared lock-free / bounded hand-off state between the UI thread and the lazy MSIX worker.
struct MsixWorkerShared {
    in_flight: AtomicBool,
    fallback_to_shell: AtomicBool,
    pending: Mutex<VecDeque<IslandNotification>>,
}

struct MsixWorker {
    tx: SyncSender<()>,
    shared: Arc<MsixWorkerShared>,
}

impl MsixWorker {
    fn spawn() -> Option<Self> {
        let (tx, rx) = sync_channel::<()>(1);
        let shared = Arc::new(MsixWorkerShared {
            in_flight: AtomicBool::new(false),
            fallback_to_shell: AtomicBool::new(false),
            pending: Mutex::new(VecDeque::new()),
        });
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("island-notif-msix".to_owned())
            .spawn(move || msix_worker_loop(rx, worker_shared))
            .ok()?;
        Some(Self { tx, shared })
    }

    /// Dispatches one non-blocking MSIX poll if none is already in flight.
    fn request_poll(&self) {
        if self
            .shared
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if self.tx.try_send(()).is_err() {
            self.shared.in_flight.store(false, Ordering::Release);
        }
    }
}

pub struct NotificationMirror {
    mode: NotificationMirrorMode,
    worker: Option<MsixWorker>,
    last_msix_poll: Option<Instant>,
    recent: VecDeque<IslandNotification>,
    cooldowns: VecDeque<(u64, Instant)>,
    next_id: u64,
}

impl Default for NotificationMirror {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationMirror {
    /// Cheap constructor: only checks package identity, never touches WinRT or spawns threads.
    pub fn new() -> Self {
        let mode = if Self::is_msix_packaged() {
            NotificationMirrorMode::MsixNative
        } else {
            NotificationMirrorMode::ShellAlertFallback
        };
        Self::with_mode(mode)
    }

    fn with_mode(mode: NotificationMirrorMode) -> Self {
        Self {
            mode,
            worker: None,
            last_msix_poll: None,
            recent: VecDeque::new(),
            cooldowns: VecDeque::new(),
            next_id: 1,
        }
    }

    pub fn mode(&self) -> NotificationMirrorMode {
        self.mode
    }

    /// How long a mirrored notification pill stays in the island (`IslandNotificationsDurationMs`).
    pub fn display_duration(settings: &DockSettings) -> Duration {
        Duration::from_millis(u64::from(settings.island_notifications_duration_ms()))
    }

    /// True when the process has MSIX package identity (`GetCurrentPackageFullName`).
    pub fn is_msix_packaged() -> bool {
        super::app_identity::is_packaged()
    }

    /// Non-blocking O(1) poll for Action Center toasts (packaged builds only).
    ///
    /// Drains any toasts already collected by the background worker and dispatches at most one
    /// background `GetNotificationsAsync` poll every `MSIX_POLL_INTERVAL`. Never blocks the UI
    /// thread on WinRT `.join()`, and only spawns the worker when
    /// `is_msix_packaged() && settings.island_notifications_enabled()`.
    pub fn poll_msix(
        &mut self,
        settings: &DockSettings,
        focus_active: bool,
    ) -> Option<IslandNotification> {
        if !notifications_enabled(settings) {
            self.release_listener();
            return None;
        }
        if focus_active && respect_focus(settings) {
            return None;
        }
        if self.mode != NotificationMirrorMode::MsixNative || !Self::is_msix_packaged() {
            return None;
        }

        let now = Instant::now();
        let newest = self.drain_worker_pending(settings, now);
        if self.mode != NotificationMirrorMode::MsixNative {
            return None;
        }

        let due = self
            .last_msix_poll
            .is_none_or(|last| now.saturating_duration_since(last) >= MSIX_POLL_INTERVAL);
        if due {
            self.last_msix_poll = Some(now);
            if self.worker.is_none() {
                self.worker = MsixWorker::spawn();
            }
            if let Some(worker) = &self.worker {
                worker.request_poll();
            }
        }
        newest
    }

    fn drain_worker_pending(
        &mut self,
        settings: &DockSettings,
        now: Instant,
    ) -> Option<IslandNotification> {
        let mut fallback = false;
        let mut drained: Vec<IslandNotification> = Vec::new();
        if let Some(worker) = &self.worker {
            if worker.shared.fallback_to_shell.load(Ordering::Acquire) {
                fallback = true;
            } else if let Ok(mut queue) = worker.shared.pending.try_lock() {
                while let Some(item) = queue.pop_front() {
                    drained.push(item);
                }
            }
        }
        if fallback {
            self.worker = None;
            self.mode = NotificationMirrorMode::ShellAlertFallback;
            return None;
        }
        let mut newest = None;
        for item in drained {
            let candidate = Candidate {
                app_name: item.app_name,
                title: item.title,
                body: item.body,
                timestamp_unix: item.timestamp_unix,
                target_hwnd: item.target_hwnd,
            };
            if let Some(accepted) = self.accept(candidate, settings, now) {
                newest = Some(accepted);
            }
        }
        newest
    }

    /// Mirrors a flashing / alerting background window (portable mode, or any non-toast alert).
    pub fn push_shell_alert(
        &mut self,
        app_name: String,
        title: String,
        target_hwnd: isize,
        settings: &DockSettings,
        focus_active: bool,
    ) -> Option<IslandNotification> {
        self.push_shell_alert_at(
            app_name,
            title,
            target_hwnd,
            settings,
            focus_active,
            Instant::now(),
        )
    }

    fn push_shell_alert_at(
        &mut self,
        app_name: String,
        title: String,
        target_hwnd: isize,
        settings: &DockSettings,
        focus_active: bool,
        now: Instant,
    ) -> Option<IslandNotification> {
        if !notifications_enabled(settings) || (focus_active && respect_focus(settings)) {
            return None;
        }
        self.accept(
            Candidate {
                app_name,
                title,
                body: String::new(),
                timestamp_unix: unix_now(),
                target_hwnd,
            },
            settings,
            now,
        )
    }

    pub fn recent(&self) -> &VecDeque<IslandNotification> {
        &self.recent
    }

    /// Clears mirrored entries and cooldowns. Seen toast IDs are kept so cleared toasts do not
    /// come back on the next poll.
    pub fn clear(&mut self) {
        self.recent = VecDeque::new();
        self.cooldowns = VecDeque::new();
    }

    fn release_listener(&mut self) {
        if self.worker.is_some() || self.last_msix_poll.is_some() || !self.recent.is_empty() {
            let mode = self.mode;
            *self = Self::with_mode(mode);
        }
    }

    /// Shared filter + ring-buffer insert for both modes.
    fn accept(
        &mut self,
        candidate: Candidate,
        settings: &DockSettings,
        now: Instant,
    ) -> Option<IslandNotification> {
        let app_name = truncate_chars(&candidate.app_name, MAX_APP_CHARS);
        let mut title = truncate_chars(&candidate.title, MAX_TITLE_CHARS);
        let body = truncate_chars(&candidate.body, MAX_BODY_CHARS);
        if title.is_empty() {
            if app_name.is_empty() {
                return None;
            }
            title = app_name.clone();
        }
        if is_muted(settings, &app_name) {
            return None;
        }

        self.cooldowns
            .retain(|(_, at)| now.saturating_duration_since(*at) < DUPLICATE_COOLDOWN);
        let key = cooldown_key(&app_name, &title, &body);
        if self.cooldowns.iter().any(|(hash, _)| *hash == key) {
            return None;
        }
        if self.cooldowns.len() >= MAX_COOLDOWN_ENTRIES {
            self.cooldowns.pop_front();
        }
        self.cooldowns.push_back((key, now));

        let notification = IslandNotification {
            id: self.next_id,
            app_name,
            title,
            body,
            timestamp_unix: candidate.timestamp_unix,
            target_hwnd: candidate.target_hwnd,
        };
        self.next_id = self.next_id.wrapping_add(1).max(1);
        while self.recent.len() >= MAX_NOTIFICATIONS {
            self.recent.pop_front();
        }
        self.recent.push_back(notification.clone());
        Some(notification)
    }
}

/// Background worker that owns `UserNotificationListener` and runs WinRT `GetNotificationsAsync`
/// strictly off the UI thread. Exits automatically when `NotificationMirror` drops its `SyncSender`.
fn msix_worker_loop(rx: Receiver<()>, shared: Arc<MsixWorkerShared>) {
    struct InFlightReset<'a>(&'a AtomicBool);
    impl Drop for InFlightReset<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    let mut listener: Option<UserNotificationListener> = None;
    let mut access = AccessState::Unknown;
    let mut seen_ids: HashSet<u32> = HashSet::new();
    let mut msix_seeded = false;

    while rx.recv().is_ok() {
        let _guard = InFlightReset(&shared.in_flight);

        if listener.is_none() {
            match UserNotificationListener::Current() {
                Ok(current) => listener = Some(current),
                Err(_) => {
                    shared.fallback_to_shell.store(true, Ordering::Release);
                    return;
                }
            }
        }
        let Some(active_listener) = listener.as_ref() else {
            return;
        };

        if access != AccessState::Allowed {
            match active_listener.GetAccessStatus() {
                Ok(status) if status == UserNotificationListenerAccessStatus::Allowed => {
                    access = AccessState::Allowed;
                }
                Ok(status) if status == UserNotificationListenerAccessStatus::Unspecified => {
                    if access == AccessState::Unknown {
                        access = AccessState::Requested;
                        let _ = active_listener.RequestAccessAsync();
                    }
                    continue;
                }
                _ => {
                    shared.fallback_to_shell.store(true, Ordering::Release);
                    return;
                }
            }
        }

        let Ok(op) = active_listener.GetNotificationsAsync(NotificationKinds::Toast) else {
            continue;
        };
        let Ok(list) = op.join() else {
            continue;
        };
        let Ok(size) = list.Size() else {
            continue;
        };

        let mut window: Vec<(u32, UserNotification)> = Vec::new();
        for index in size.saturating_sub(SCAN_WINDOW)..size {
            if let Ok(item) = list.GetAt(index)
                && let Ok(id) = item.Id()
            {
                window.push((id, item));
            }
        }
        drop(list);

        seen_ids.retain(|seen| window.iter().any(|(id, _)| id == seen));
        if !msix_seeded {
            msix_seeded = true;
            for (id, _) in &window {
                remember_seen_id(&mut seen_ids, *id);
            }
            continue;
        }

        let mut fresh: Vec<Candidate> = Vec::new();
        for (id, item) in &window {
            if seen_ids.contains(id) {
                continue;
            }
            remember_seen_id(&mut seen_ids, *id);
            if let Some(candidate) = read_toast(item) {
                fresh.push(candidate);
            }
        }
        if fresh.is_empty() {
            continue;
        }
        fresh.sort_by_key(|candidate| candidate.timestamp_unix);

        if let Ok(mut queue) = shared.pending.lock() {
            for candidate in fresh {
                while queue.len() >= MAX_NOTIFICATIONS {
                    queue.pop_front();
                }
                queue.push_back(IslandNotification {
                    id: 0,
                    app_name: candidate.app_name,
                    title: candidate.title,
                    body: candidate.body,
                    timestamp_unix: candidate.timestamp_unix,
                    target_hwnd: 0,
                });
            }
        }
    }
}

fn remember_seen_id(seen_ids: &mut HashSet<u32>, id: u32) {
    if seen_ids.len() >= SEEN_ID_CAP {
        seen_ids.clear();
    }
    seen_ids.insert(id);
}

fn notifications_enabled(settings: &DockSettings) -> bool {
    settings.island_notifications_enabled()
}

fn respect_focus(settings: &DockSettings) -> bool {
    settings.island_notifications_respect_focus()
}

/// Case-insensitive match against `IslandNotificationsMutedApps`.
fn is_muted(settings: &DockSettings, app_name: &str) -> bool {
    let app = app_name.trim();
    if app.is_empty() {
        return false;
    }
    let app = app.to_lowercase();
    settings
        .island_notifications_muted_apps()
        .iter()
        .any(|entry| entry.to_lowercase() == app)
}

fn cooldown_key(app: &str, title: &str, body: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    app.to_lowercase().hash(&mut hasher);
    title.to_lowercase().hash(&mut hasher);
    body.hash(&mut hasher);
    hasher.finish()
}

fn truncate_chars(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// WinRT `DateTime` (100 ns ticks since 1601) to Unix seconds.
fn filetime_ticks_to_unix(ticks: i64) -> u64 {
    let secs = ticks / 10_000_000 - FILETIME_UNIX_OFFSET_SECS;
    u64::try_from(secs).unwrap_or(0)
}

/// Reads app name and `ToastGeneric` text lines; never touches images or logos.
fn read_toast(item: &UserNotification) -> Option<Candidate> {
    let app_name = item
        .AppInfo()
        .and_then(|info| info.DisplayInfo())
        .and_then(|display| display.DisplayName())
        .map(|name| truncate_chars(&name.to_string_lossy(), MAX_APP_CHARS))
        .unwrap_or_default();
    let timestamp_unix = item
        .CreationTime()
        .map(|time| filetime_ticks_to_unix(time.UniversalTime))
        .unwrap_or_else(|_| unix_now());
    let binding = item
        .Notification()
        .ok()?
        .Visual()
        .ok()?
        .GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?)
        .ok()?;
    let texts = binding.GetTextElements().ok()?;
    let count = texts.Size().unwrap_or(0).min(MAX_TEXT_ELEMENTS);
    let mut title = String::new();
    let mut body = String::new();
    for index in 0..count {
        let Some(line) = texts
            .GetAt(index)
            .and_then(|element| element.Text())
            .ok()
            .map(|text| truncate_chars(&text.to_string_lossy(), MAX_BODY_CHARS))
        else {
            continue;
        };
        if line.is_empty() {
            continue;
        }
        if title.is_empty() {
            title = line;
        } else if body.chars().count() < MAX_BODY_CHARS {
            if !body.is_empty() {
                body.push(' ');
            }
            body.push_str(&line);
        }
    }
    Some(Candidate {
        app_name,
        title,
        body,
        timestamp_unix,
        target_hwnd: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_settings() -> DockSettings {
        let mut settings = DockSettings::default();
        settings.set_island_notifications_enabled(true);
        settings
    }

    fn mirror() -> NotificationMirror {
        NotificationMirror::with_mode(NotificationMirrorMode::ShellAlertFallback)
    }

    #[test]
    fn ring_buffer_never_exceeds_cap() {
        let settings = enabled_settings();
        let mut mirror = mirror();
        let now = Instant::now();
        for i in 0..40 {
            let pushed = mirror.push_shell_alert_at(
                "Chat".to_owned(),
                format!("Message {i}"),
                0x100,
                &settings,
                false,
                now,
            );
            assert!(pushed.is_some());
            assert!(mirror.recent().len() <= MAX_NOTIFICATIONS);
        }
        assert_eq!(mirror.recent().len(), MAX_NOTIFICATIONS);
        assert_eq!(mirror.recent().back().unwrap().title, "Message 39");
        assert!(mirror.cooldowns.len() <= MAX_COOLDOWN_ENTRIES);
        mirror.clear();
        assert!(mirror.recent().is_empty());
    }

    #[test]
    fn body_is_truncated_to_cap() {
        let settings = enabled_settings();
        let mut mirror = mirror();
        let accepted = mirror
            .accept(
                Candidate {
                    app_name: "Mail".to_owned(),
                    title: "Report".to_owned(),
                    body: "ğ".repeat(1000),
                    timestamp_unix: 1,
                    target_hwnd: 0,
                },
                &settings,
                Instant::now(),
            )
            .expect("accepted");
        assert!(accepted.body.chars().count() <= MAX_BODY_CHARS);
        assert!(accepted.body.ends_with('\u{2026}'));
        assert!(accepted.title.chars().count() <= MAX_TITLE_CHARS);
    }

    #[test]
    fn focus_mode_suppresses_when_respected() {
        let mut settings = enabled_settings();
        let mut mirror = mirror();
        assert_eq!(
            mirror.push_shell_alert("App".to_owned(), "Hi".to_owned(), 1, &settings, true),
            None
        );
        assert_eq!(mirror.poll_msix(&settings, true), None);
        settings.set_island_notifications_respect_focus(false);
        assert!(
            mirror
                .push_shell_alert("App".to_owned(), "Hi".to_owned(), 1, &settings, true)
                .is_some()
        );
    }

    #[test]
    fn disabled_feature_returns_none() {
        let mut settings = DockSettings::default();
        settings.set_island_notifications_enabled(false);
        let mut mirror = mirror();
        assert_eq!(
            mirror.push_shell_alert("App".to_owned(), "Hi".to_owned(), 1, &settings, false),
            None
        );
        assert_eq!(mirror.poll_msix(&settings, false), None);
        assert!(mirror.recent().is_empty());
    }

    #[test]
    fn unpackaged_poll_never_uses_winrt() {
        let settings = enabled_settings();
        let mut mirror = mirror();
        assert_eq!(mirror.poll_msix(&settings, false), None);
        assert!(mirror.worker.is_none());
        assert_eq!(mirror.mode(), NotificationMirrorMode::ShellAlertFallback);
    }

    #[test]
    fn msix_worker_is_never_spawned_when_unpackaged_or_disabled() {
        let mut disabled = DockSettings::default();
        disabled.set_island_notifications_enabled(false);
        let mut native = NotificationMirror::with_mode(NotificationMirrorMode::MsixNative);
        assert_eq!(native.poll_msix(&disabled, false), None);
        assert!(native.worker.is_none());

        // Even with notifications enabled, unpackaged test processes must not spawn a worker.
        let enabled = enabled_settings();
        assert!(!NotificationMirror::is_msix_packaged());
        assert_eq!(native.poll_msix(&enabled, false), None);
        assert!(native.worker.is_none());
    }

    #[test]
    fn drain_worker_pending_is_non_blocking_and_respects_filters() {
        let mut settings = enabled_settings();
        settings.set_island_notifications_muted_apps(vec!["MutedApp".to_owned()]);
        let (tx, _rx) = sync_channel::<()>(1);
        let shared = Arc::new(MsixWorkerShared {
            in_flight: AtomicBool::new(false),
            fallback_to_shell: AtomicBool::new(false),
            pending: Mutex::new(VecDeque::from([
                IslandNotification {
                    id: 0,
                    app_name: "MutedApp".to_owned(),
                    title: "Ignored".to_owned(),
                    body: String::new(),
                    timestamp_unix: 10,
                    target_hwnd: 0,
                },
                IslandNotification {
                    id: 0,
                    app_name: "Mail".to_owned(),
                    title: "Invoice".to_owned(),
                    body: "Ready".to_owned(),
                    timestamp_unix: 11,
                    target_hwnd: 0,
                },
            ])),
        });
        let mut mirror = NotificationMirror::with_mode(NotificationMirrorMode::MsixNative);
        mirror.worker = Some(MsixWorker { tx, shared });

        let drained = mirror
            .drain_worker_pending(&settings, Instant::now())
            .expect("unmuted item");
        assert_eq!(drained.app_name, "Mail");
        assert_eq!(drained.title, "Invoice");
        assert_eq!(drained.id, 1);
        assert_eq!(mirror.recent().len(), 1);
    }

    #[test]
    fn muted_apps_are_filtered() {
        let mut settings = enabled_settings();
        settings.set_island_notifications_muted_apps(vec!["Slack".to_owned()]);
        let mut mirror = mirror();
        assert_eq!(
            mirror.push_shell_alert("slack".to_owned(), "Ping".to_owned(), 1, &settings, false),
            None
        );
        assert!(
            mirror
                .push_shell_alert("Teams".to_owned(), "Ping".to_owned(), 1, &settings, false)
                .is_some()
        );

        settings
            .set_island_notifications_muted_apps(vec!["Teams".to_owned(), "Discord".to_owned()]);
        assert_eq!(
            mirror.push_shell_alert("discord".to_owned(), "Ping".to_owned(), 1, &settings, false),
            None
        );
    }

    #[test]
    fn duplicate_alerts_respect_cooldown() {
        let settings = enabled_settings();
        let mut mirror = mirror();
        let start = Instant::now();
        let push = |mirror: &mut NotificationMirror, title: &str, at: Instant| {
            mirror.push_shell_alert_at("App".to_owned(), title.to_owned(), 7, &settings, false, at)
        };
        assert!(push(&mut mirror, "Download done", start).is_some());
        assert_eq!(
            push(&mut mirror, "Download done", start + Duration::from_secs(3)),
            None
        );
        assert!(push(&mut mirror, "Other", start + Duration::from_secs(3)).is_some());
        assert!(
            push(
                &mut mirror,
                "Download done",
                start + Duration::from_secs(11)
            )
            .is_some()
        );
        assert_eq!(mirror.recent().len(), 3);
    }

    #[test]
    fn helpers_are_bounded() {
        assert_eq!(truncate_chars("  abc  ", 10), "abc");
        assert_eq!(truncate_chars("abcdef", 4).chars().count(), 4);
        assert_eq!(filetime_ticks_to_unix(0), 0);
        assert_eq!(
            filetime_ticks_to_unix((FILETIME_UNIX_OFFSET_SECS + 42) * 10_000_000),
            42
        );
        let mut seen_ids = HashSet::new();
        for id in 0..500 {
            remember_seen_id(&mut seen_ids, id);
        }
        assert!(seen_ids.len() <= SEEN_ID_CAP);
    }
}
