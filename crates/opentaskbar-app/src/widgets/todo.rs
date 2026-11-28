//! N8: Local Timed To-Do + Zero-OAuth iCal (`.ics`) Calendar Countdown.
//!
//! Strict low-RAM invariants:
//! - Tasks persist atomically to `%LOCALAPPDATA%\OpenTaskbar\todo.json` (capped at 200 tasks).
//! - Background WinHTTP `.ics` fetching ONLY runs when `settings.ical_feed_url()` is non-empty
//!   and the poll interval has elapsed, with response size strictly capped at 512 KB and parsed
//!   upcoming events capped at 12.
//! - Zero OAuth tokens, zero webviews, zero steady-state allocations on idle ticks.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentaskbar_core::paths;
use opentaskbar_core::settings::DockSettings;
use serde::{Deserialize, Serialize};

use crate::design;
use crate::platform::http::{HttpCfg, get_body};
use crate::ui::flyout::{FlyoutBlock, FlyoutSpec};
use crate::widgets::{IslandDetailAction, IslandDetailRow, IslandDetailSpec};

/// File name under `%LOCALAPPDATA%\OpenTaskbar`.
pub const TODO_FILE_NAME: &str = "todo.json";
/// Maximum number of local tasks persisted in RAM and on disk.
pub const MAX_TODO_TASKS: usize = 200;
/// Maximum UTF-8 character count for a single task title.
pub const MAX_TASK_TITLE_CHARS: usize = 200;
/// Maximum upcoming `.ics` calendar events retained in memory.
pub const MAX_CALENDAR_EVENTS: usize = 12;
/// Maximum HTTP response payload accepted for an `.ics` calendar feed (512 KB).
pub const MAX_ICS_RESPONSE_BYTES: usize = 512 * 1024;
/// Default interval between background `.ics` refreshes.
pub const ICAL_POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Flyout `TextInput` block ID for adding a new task.
pub const FLYOUT_ADD_INPUT_ID: u32 = 10;

const CLICK_TOGGLE_BASE: u32 = 100;
const CLICK_SNOOZE_BASE: u32 = 200;
const CLICK_REMOVE_BASE: u32 = 300;
const CLICK_CLEAR_COMPLETED: u32 = 1;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One local timed to-do item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoTask {
    pub id: u64,
    pub title: String,
    pub due_hhmm: Option<String>,
    pub done: bool,
    pub snoozed_until_unix: Option<u64>,
}

/// One parsed upcoming iCal (`.ics`) calendar event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub summary: String,
    pub start_unix: u64,
    pub end_unix: u64,
    pub location: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedTodo {
    #[serde(default)]
    tasks: Vec<TodoTask>,
}

/// Combined state for Local Timed To-Do tasks and zero-OAuth iCal `.ics` countdowns.
#[derive(Debug)]
pub struct TodoState {
    tasks: Vec<TodoTask>,
    next_id: u64,
    /// Tracks `(task_id -> last reminded minute key)` so each due/snooze reminder fires once.
    reminded_minute: HashMap<u64, u64>,
    /// Cached upcoming events sorted ascending by `start_unix` (capped at 12).
    events: Vec<CalendarEvent>,
    /// Mailbox populated by the background WinHTTP poller.
    pending_events: Arc<Mutex<Option<Vec<CalendarEvent>>>>,
    ical_in_flight: Arc<AtomicBool>,
    last_ical_poll: Option<Instant>,
    last_ical_url: String,
}

impl Default for TodoState {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
            reminded_minute: HashMap::new(),
            events: Vec::new(),
            pending_events: Arc::new(Mutex::new(None)),
            ical_in_flight: Arc::new(AtomicBool::new(false)),
            last_ical_poll: None,
            last_ical_url: String::new(),
        }
    }
}

struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl TodoState {
    /// Default persistence path (`%LOCALAPPDATA%\OpenTaskbar\todo.json`).
    pub fn default_path() -> PathBuf {
        paths::shared_app_data_dir().join(TODO_FILE_NAME)
    }

    /// Loads tasks from `%LOCALAPPDATA%\OpenTaskbar\todo.json`.
    pub fn load_default() -> Self {
        Self::load_from(&Self::default_path())
    }

    /// Atomically persists tasks to `%LOCALAPPDATA%\OpenTaskbar\todo.json`.
    pub fn save_default(&self) -> io::Result<()> {
        self.save_to(&Self::default_path())
    }

    /// Loads tasks from an explicit JSON path while enforcing the 200-task cap.
    pub fn load_from(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(parsed) = serde_json::from_str::<PersistedTodo>(&raw) else {
            return Self::default();
        };
        let mut state = Self::default();
        let mut max_id = 0u64;
        for mut task in parsed.tasks.into_iter().take(MAX_TODO_TASKS) {
            task.title = clamp_chars(task.title.trim(), MAX_TASK_TITLE_CHARS);
            if task.title.is_empty() {
                continue;
            }
            task.due_hhmm = task.due_hhmm.as_deref().and_then(normalize_hhmm);
            if task.id == 0 {
                max_id = max_id.saturating_add(1);
                task.id = max_id;
            } else {
                max_id = max_id.max(task.id);
            }
            state.tasks.push(task);
        }
        state.next_id = max_id.saturating_add(1);
        state
    }

    /// Atomically persists tasks to `path` via temp file + rename.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        let persisted = PersistedTodo {
            tasks: self.tasks.clone(),
        };
        let json = serde_json::to_vec_pretty(&persisted)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        atomic_write_bytes(path, &json)
    }

    /// Adds a new task. When `due_hhmm` is `None`, supports smart inline time parsing such as
    /// `"15:30 Team standup"` or `"Team standup @15:30"`.
    pub fn add_task(&mut self, title: &str, due_hhmm: Option<String>) -> Option<u64> {
        if self.tasks.len() >= MAX_TODO_TASKS {
            return None;
        }
        let (clean_title, resolved_due) = match due_hhmm {
            Some(explicit) => {
                let t = clamp_chars(title.trim(), MAX_TASK_TITLE_CHARS);
                if t.is_empty() {
                    return None;
                }
                (t, normalize_hhmm(&explicit))
            }
            None => {
                let (t, parsed_due) = parse_inline_task(title)?;
                (clamp_chars(&t, MAX_TASK_TITLE_CHARS), parsed_due)
            }
        };
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.tasks.push(TodoTask {
            id,
            title: clean_title,
            due_hhmm: resolved_due,
            done: false,
            snoozed_until_unix: None,
        });
        Some(id)
    }

    /// Toggles a task's completed state.
    pub fn toggle_done(&mut self, id: u64) -> bool {
        let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        task.done = !task.done;
        if task.done {
            task.snoozed_until_unix = None;
        }
        true
    }

    /// Removes task `id`.
    pub fn remove_task(&mut self, id: u64) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.id != id);
        self.reminded_minute.remove(&id);
        self.tasks.len() != before
    }

    /// Snoozes task `id` for `mins` minutes from the current wall-clock time.
    pub fn snooze_task(&mut self, id: u64, mins: u64) -> bool {
        self.snooze_task_from(id, now_unix_secs(), mins)
    }

    /// Snoozes task `id` for `mins` minutes starting from `now_unix`.
    pub fn snooze_task_from(&mut self, id: u64, now_unix: u64, mins: u64) -> bool {
        let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        let delta_secs = mins.max(1).saturating_mul(60);
        task.snoozed_until_unix = Some(now_unix.saturating_add(delta_secs));
        self.reminded_minute.remove(&id);
        true
    }

    /// Removes all completed tasks and returns how many were removed.
    pub fn clear_completed(&mut self) -> usize {
        let before = self.tasks.len();
        self.tasks.retain(|t| !t.done);
        let kept_ids: Vec<u64> = self.tasks.iter().map(|t| t.id).collect();
        self.reminded_minute.retain(|id, _| kept_ids.contains(id));
        before.saturating_sub(self.tasks.len())
    }

    /// Read-only view of local tasks.
    pub fn tasks(&self) -> &[TodoTask] {
        &self.tasks
    }

    /// Fires once when an incomplete task's `due_hhmm` matches `now_hhmm` (and it is not
    /// snoozed), or when a previously snoozed task reaches its `snoozed_until_unix` deadline.
    pub fn poll_due_reminder(
        &mut self,
        now_unix: u64,
        now_hhmm: &str,
        enabled: bool,
    ) -> Option<TodoTask> {
        if !enabled {
            return None;
        }
        let minute_bucket = now_unix / 60;
        let normalized_now = normalize_hhmm(now_hhmm);

        for task in &mut self.tasks {
            if task.done {
                continue;
            }
            if let Some(until) = task.snoozed_until_unix {
                if now_unix < until {
                    continue;
                }
                // Snooze expired: fire once and clear snooze.
                task.snoozed_until_unix = None;
                self.reminded_minute.insert(task.id, minute_bucket);
                return Some(task.clone());
            }
            let Some(due) = task.due_hhmm.as_deref() else {
                continue;
            };
            if normalized_now.as_deref() != Some(due) {
                continue;
            }
            if self.reminded_minute.get(&task.id) == Some(&minute_bucket) {
                continue;
            }
            self.reminded_minute.insert(task.id, minute_bucket);
            return Some(task.clone());
        }
        None
    }

    /// Absorbs any freshly fetched iCal events from the background worker without blocking.
    pub fn sync_ical_events(&mut self) {
        if let Ok(mut slot) = self.pending_events.try_lock()
            && let Some(next) = slot.take()
        {
            self.events = next;
        }
    }

    /// Polls the configured zero-OAuth `.ics` feed in the background when `settings.ical_feed_url()`
    /// is non-empty and the refresh interval has elapsed.
    pub fn poll_ical(&mut self, settings: &DockSettings) {
        self.sync_ical_events();
        let raw_url = settings.ical_feed_url();
        self.poll_ical_url(raw_url.trim(), ICAL_POLL_INTERVAL);
    }

    /// Polls an explicit `.ics` URL on a detached worker when non-empty and due.
    pub fn poll_ical_url(&mut self, url: &str, interval: Duration) {
        self.sync_ical_events();
        let trimmed = url.trim();
        if trimmed.is_empty() {
            if !self.last_ical_url.is_empty() {
                self.last_ical_url.clear();
                self.events.clear();
                self.last_ical_poll = None;
            }
            return;
        }
        let normalized_url = normalize_ical_url(trimmed);
        let url_changed = normalized_url != self.last_ical_url;
        if !url_changed
            && self
                .last_ical_poll
                .is_some_and(|at| at.elapsed() < interval)
        {
            return;
        }
        if self.ical_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        self.last_ical_url = normalized_url.clone();
        self.last_ical_poll = Some(Instant::now());

        let pending = Arc::clone(&self.pending_events);
        let guard = InFlightGuard(Arc::clone(&self.ical_in_flight));
        let spawned = std::thread::Builder::new()
            .name("opentaskbar-ical-poll".into())
            .spawn(move || {
                let _guard = guard;
                let mut cfg = HttpCfg::new(concat!("OpenTaskbar/", env!("CARGO_PKG_VERSION")));
                cfg.timeouts = [5_000, 5_000, 10_000, 10_000];
                cfg.max_response_bytes = Some(MAX_ICS_RESPONSE_BYTES);
                if let Some((status, body)) = get_body(&cfg, &normalized_url)
                    && (200..300).contains(&status)
                {
                    let text = String::from_utf8_lossy(&body);
                    let parsed = parse_ics(&text, now_unix_secs());
                    drop(text);
                    drop(body);
                    if let Ok(mut slot) = pending.lock() {
                        *slot = Some(parsed);
                    }
                }
            });
        if spawned.is_err() {
            self.ical_in_flight.store(false, Ordering::Release);
        }
    }

    /// Replaces the cached upcoming events list (capped at [`MAX_CALENDAR_EVENTS`]).
    pub fn set_events(&mut self, mut events: Vec<CalendarEvent>) {
        events.sort_by(|a, b| {
            a.start_unix
                .cmp(&b.start_unix)
                .then_with(|| a.end_unix.cmp(&b.end_unix))
                .then_with(|| a.summary.cmp(&b.summary))
        });
        events.truncate(MAX_CALENDAR_EVENTS);
        self.events = events;
    }

    /// Returns all cached upcoming calendar events.
    pub fn events(&self) -> &[CalendarEvent] {
        &self.events
    }

    /// Returns the nearest active or upcoming calendar event (`end_unix > now_unix`).
    pub fn next_event(&self, now_unix: u64) -> Option<&CalendarEvent> {
        self.events.iter().find(|event| event.end_unix > now_unix)
    }

    /// Builds the Dynamic Island expanded detail card for To-Do & Agenda.
    pub fn build_island_detail(&self, now_unix: u64) -> IslandDetailSpec {
        let open_count = self.tasks.iter().filter(|t| !t.done).count();
        let done_count = self.tasks.len().saturating_sub(open_count);
        let next = self.next_event(now_unix);

        let (headline, subtitle) = if let Some(ev) = next {
            let countdown = format_countdown(ev.start_unix, ev.end_unix, now_unix);
            let sub = if !ev.location.is_empty() {
                format!("{countdown} · {}", ev.location)
            } else if open_count > 0 {
                format!("In {countdown} · {open_count} open tasks")
            } else {
                format!("Starts in {countdown}")
            };
            (ev.summary.clone(), sub)
        } else if open_count > 0 {
            (
                format!(
                    "{open_count} task{}",
                    if open_count == 1 { "" } else { "s" }
                ),
                if done_count > 0 {
                    format!("{done_count} completed")
                } else {
                    "Local reminders active".to_owned()
                },
            )
        } else {
            (
                "All Clear".to_owned(),
                "No pending tasks or events".to_owned(),
            )
        };

        let mut rows = Vec::with_capacity(4);
        if let Some(ev) = next {
            rows.push(IslandDetailRow {
                label: format_countdown(ev.start_unix, ev.end_unix, now_unix),
                value: ev.summary.clone(),
                progress: None,
            });
        }
        for task in self.tasks.iter().filter(|t| !t.done) {
            if rows.len() >= 4 {
                break;
            }
            rows.push(IslandDetailRow {
                label: task.due_hhmm.clone().unwrap_or_else(|| "Task".to_owned()),
                value: task.title.clone(),
                progress: None,
            });
        }
        if rows.is_empty() {
            rows.push(IslandDetailRow {
                label: "Tip".to_owned(),
                value: "Type \"15:30 Standup\" in the flyout to schedule".to_owned(),
                progress: None,
            });
        }

        let mut actions = Vec::new();
        if open_count > 0 {
            actions.push(IslandDetailAction {
                id: "complete_next".to_owned(),
                label: "Complete Next".to_owned(),
                primary: true,
            });
            actions.push(IslandDetailAction {
                id: "snooze_next".to_owned(),
                label: "Snooze 10m".to_owned(),
                primary: false,
            });
        }
        if done_count > 0 {
            actions.push(IslandDetailAction {
                id: "clear_completed".to_owned(),
                label: "Clear Done".to_owned(),
                primary: false,
            });
        }

        IslandDetailSpec {
            widget_id: "todo".to_owned(),
            title: "To-Do & Agenda".to_owned(),
            headline,
            subtitle,
            rows,
            actions,
            has_full_flyout: true,
        }
    }

    /// Builds the interactive `DockFlyout` spec with inline `TextInput` task entry,
    /// upcoming `.ics` countdowns, and toggleable task rows.
    pub fn build_flyout(&self, now_unix: u64, is_dark: bool) -> FlyoutSpec {
        let pal = design::palette(is_dark);
        let open_count = self.tasks.iter().filter(|t| !t.done).count();
        let done_count = self.tasks.len().saturating_sub(open_count);

        let mut blocks = vec![FlyoutBlock::HeroHeader {
            glyph: 0xE73A, // CheckboxComposite
            glyph_color: pal.widget_accent,
            title: "To-Do & Agenda".to_owned(),
            subtitle: Some(format!("{open_count} open · {done_count} done")),
            right_value: Some(open_count.to_string()),
        }];

        let upcoming: Vec<&CalendarEvent> = self
            .events
            .iter()
            .filter(|e| e.end_unix > now_unix)
            .take(4)
            .collect();
        if !upcoming.is_empty() {
            blocks.push(FlyoutBlock::Separator);
            for ev in upcoming {
                blocks.push(FlyoutBlock::GlyphStatRow {
                    glyph: 0xE787, // Calendar
                    glyph_color: pal.widget_blue,
                    label: ev.summary.clone(),
                    detail: (!ev.location.is_empty()).then(|| ev.location.clone()),
                    value: format_countdown(ev.start_unix, ev.end_unix, now_unix),
                });
            }
        }

        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::TextInput {
            id: FLYOUT_ADD_INPUT_ID,
            label: "Add task (e.g. 15:30 Standup)".to_owned(),
            value: String::new(),
            numeric: false,
            min: 0.0,
            max: 0.0,
        });

        if self.tasks.is_empty() {
            blocks.push(FlyoutBlock::Note(
                "Press Enter above to add a task. Include HH:MM for a timed reminder.".to_owned(),
            ));
        } else {
            for (idx, task) in self.tasks.iter().take(12).enumerate() {
                let badge = if task.done {
                    Some("Done".to_owned())
                } else {
                    task.due_hhmm.clone()
                };
                blocks.push(FlyoutBlock::GlyphActionRow {
                    id: CLICK_TOGGLE_BASE + idx as u32,
                    glyph: if task.done { 0xE73E } else { 0xE739 }, // CheckMark / Checkbox
                    glyph_color: if task.done {
                        pal.success
                    } else {
                        pal.widget_accent
                    },
                    title: task.title.clone(),
                    subtitle: task
                        .snoozed_until_unix
                        .filter(|&until| until > now_unix)
                        .map(|until| {
                            format!("Snoozed {}", format_countdown(until, until + 1, now_unix))
                        }),
                    badge,
                });
                blocks.push(FlyoutBlock::ButtonBar(vec![
                    (
                        CLICK_SNOOZE_BASE + idx as u32,
                        "Snooze 10m".to_owned(),
                        false,
                    ),
                    (CLICK_REMOVE_BASE + idx as u32, "Delete".to_owned(), false),
                ]));
            }
        }

        if done_count > 0 {
            blocks.push(FlyoutBlock::Separator);
            blocks.push(FlyoutBlock::ButtonBar(vec![(
                CLICK_CLEAR_COMPLETED,
                "Clear Completed".to_owned(),
                false,
            )]));
        }

        FlyoutSpec {
            title: String::new(),
            subtitle: None,
            width_dips: 328.0,
            blocks,
        }
    }

    /// Resolves a numeric `LiveEvent::Click(id)` emitted by [`Self::build_flyout`] into the
    /// symbolic `action_id` accepted by [`Self::handle_flyout_action`].
    pub fn flyout_action_id_for_click(&self, click_id: u32) -> Option<String> {
        match click_id {
            CLICK_CLEAR_COMPLETED => Some("clear_completed".to_owned()),
            FLYOUT_ADD_INPUT_ID => Some("add".to_owned()),
            id if (CLICK_TOGGLE_BASE..CLICK_SNOOZE_BASE).contains(&id) => {
                let idx = (id - CLICK_TOGGLE_BASE) as usize;
                self.tasks.get(idx).map(|t| format!("toggle:{}", t.id))
            }
            id if (CLICK_SNOOZE_BASE..CLICK_REMOVE_BASE).contains(&id) => {
                let idx = (id - CLICK_SNOOZE_BASE) as usize;
                self.tasks.get(idx).map(|t| format!("snooze:{}:10", t.id))
            }
            id if id >= CLICK_REMOVE_BASE => {
                let idx = (id - CLICK_REMOVE_BASE) as usize;
                self.tasks.get(idx).map(|t| format!("remove:{}", t.id))
            }
            _ => None,
        }
    }

    /// Handles a flyout or Dynamic Island detail action.
    ///
    /// Supported actions: `"add"` (with `input_text`), `"toggle:<id>"`, `"remove:<id>"`,
    /// `"snooze:<id>"`, `"snooze:<id>:<mins>"`, `"complete_next"`, `"snooze_next"`,
    /// `"clear_completed"`.
    pub fn handle_flyout_action(&mut self, action_id: &str, input_text: Option<&str>) -> bool {
        if action_id == "add" {
            return input_text.is_some_and(|raw| self.add_task(raw, None).is_some());
        }
        if let Some(id_str) = action_id.strip_prefix("toggle:") {
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.toggle_done(id));
        }
        if let Some(id_str) = action_id.strip_prefix("remove:") {
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.remove_task(id));
        }
        if let Some(rest) = action_id.strip_prefix("snooze:") {
            let (id_str, mins) = match rest.split_once(':') {
                Some((id_part, m_part)) => (id_part, m_part.parse::<u64>().unwrap_or(10)),
                None => (rest, 10),
            };
            return id_str
                .parse::<u64>()
                .ok()
                .is_some_and(|id| self.snooze_task(id, mins));
        }
        match action_id {
            "complete_next" => self
                .tasks
                .iter()
                .find(|t| !t.done)
                .map(|t| t.id)
                .is_some_and(|id| self.toggle_done(id)),
            "snooze_next" => self
                .tasks
                .iter()
                .find(|t| !t.done)
                .map(|t| t.id)
                .is_some_and(|id| self.snooze_task(id, 10)),
            "clear_completed" => self.clear_completed() > 0,
            _ => false,
        }
    }
}

/// Parses an inline task string, extracting a leading or trailing `HH:MM` (or `@HH:MM`) time.
///
/// Example: `"15:30 Team standup"` -> `Some(("Team standup", Some("15:30")))`.
pub fn parse_inline_task(raw: &str) -> Option<(String, Option<String>)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((first, rest)) = trimmed.split_once(char::is_whitespace) {
        let token = first.strip_prefix('@').unwrap_or(first);
        if let Some(hhmm) = normalize_hhmm(token) {
            let title = rest.trim();
            if !title.is_empty() {
                return Some((title.to_owned(), Some(hhmm)));
            }
        }
    }
    if let Some((head, last)) = trimmed.rsplit_once(char::is_whitespace) {
        let token = last.strip_prefix('@').unwrap_or(last);
        if let Some(hhmm) = normalize_hhmm(token) {
            let title = head.trim();
            if !title.is_empty() {
                return Some((title.to_owned(), Some(hhmm)));
            }
        }
    }
    Some((trimmed.to_owned(), None))
}

/// Formats a human-friendly countdown string such as `"Now"`, `"12m"`, `"2h 15m"`, or `"1d 3h"`.
pub fn format_countdown(start_unix: u64, end_unix: u64, now_unix: u64) -> String {
    if now_unix >= start_unix && now_unix < end_unix {
        return "Now".to_owned();
    }
    if now_unix >= end_unix {
        return "Ended".to_owned();
    }
    let delta = start_unix.saturating_sub(now_unix);
    if delta < 60 {
        return "<1m".to_owned();
    }
    let total_mins = delta / 60;
    if total_mins < 60 {
        return format!("{total_mins}m");
    }
    let total_hours = total_mins / 60;
    let rem_mins = total_mins % 60;
    if total_hours < 24 {
        if rem_mins == 0 {
            format!("{total_hours}h")
        } else {
            format!("{total_hours}h {rem_mins}m")
        }
    } else {
        let days = total_hours / 24;
        let rem_hours = total_hours % 24;
        if rem_hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {rem_hours}h")
        }
    }
}

/// Pure Rust RFC 5545 `.ics` parser returning up to 12 upcoming events sorted ascending.
///
/// Supports line unfolding (`CRLF`/`LF` + space/tab), `BEGIN:VEVENT..END:VEVENT`, `SUMMARY`,
/// `LOCATION`, `DTSTART`/`DTEND` in UTC (`...Z`), `TZID=...`, floating local, and `VALUE=DATE`.
pub fn parse_ics(ics_text: &str, now_unix: u64) -> Vec<CalendarEvent> {
    parse_ics_with_local_offset(ics_text, now_unix, local_utc_offset_secs())
}

/// Deterministic variant of [`parse_ics`] accepting an explicit `local_offset_secs`
/// (seconds east of UTC, e.g. `+10800` for UTC+3) for timezone-independent tests.
pub fn parse_ics_with_local_offset(
    ics_text: &str,
    now_unix: u64,
    local_offset_secs: i64,
) -> Vec<CalendarEvent> {
    let unfolded = unfold_ics_lines(ics_text);
    let mut events = Vec::new();
    let mut in_vevent = false;
    let mut in_valarm = false;

    let mut summary = String::new();
    let mut location = String::new();
    let mut start: Option<(u64, bool)> = None;
    let mut end: Option<(u64, bool)> = None;

    for line in unfolded {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_vevent = true;
            in_valarm = false;
            summary.clear();
            location.clear();
            start = None;
            end = None;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VEVENT") {
            if in_vevent && let Some((start_unix, is_all_day)) = start {
                let default_dur = if is_all_day { 86_400 } else { 3_600 };
                let end_unix = end
                    .map(|(e, _)| e)
                    .filter(|&e| e > start_unix)
                    .unwrap_or_else(|| start_unix.saturating_add(default_dur));
                if end_unix > now_unix {
                    let clean_summary = if summary.trim().is_empty() {
                        "Untitled Event".to_owned()
                    } else {
                        clamp_chars(summary.trim(), MAX_TASK_TITLE_CHARS)
                    };
                    events.push(CalendarEvent {
                        summary: clean_summary,
                        start_unix,
                        end_unix,
                        location: clamp_chars(location.trim(), MAX_TASK_TITLE_CHARS),
                    });
                }
            }
            in_vevent = false;
            in_valarm = false;
            continue;
        }
        if !in_vevent {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("BEGIN:VALARM") {
            in_valarm = true;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VALARM") {
            in_valarm = false;
            continue;
        }
        if in_valarm {
            continue;
        }

        let Some((key_and_params, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = key_and_params.split(';');
        let prop_name = parts.next().unwrap_or("").trim();
        let params: Vec<&str> = parts.collect();

        if prop_name.eq_ignore_ascii_case("SUMMARY") {
            summary = unescape_ics_text(value);
        } else if prop_name.eq_ignore_ascii_case("LOCATION") {
            location = unescape_ics_text(value);
        } else if prop_name.eq_ignore_ascii_case("DTSTART") {
            start = parse_ics_datetime(value.trim(), &params, local_offset_secs);
        } else if prop_name.eq_ignore_ascii_case("DTEND") {
            end = parse_ics_datetime(value.trim(), &params, local_offset_secs);
        }
    }

    events.sort_by(|a, b| {
        a.start_unix
            .cmp(&b.start_unix)
            .then_with(|| a.end_unix.cmp(&b.end_unix))
            .then_with(|| a.summary.cmp(&b.summary))
    });
    events.truncate(MAX_CALENDAR_EVENTS);
    events
}

fn unfold_ics_lines(ics_text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in ics_text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(cont) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')) {
            if let Some(prev) = lines.last_mut() {
                prev.push_str(cont);
            } else {
                lines.push(cont.to_owned());
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}

fn unescape_ics_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push(' '),
                Some('\\') => out.push('\\'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parses an iCal date or date-time value into `(unix_secs, is_all_day)`.
fn parse_ics_datetime(value: &str, params: &[&str], local_offset_secs: i64) -> Option<(u64, bool)> {
    let is_date_param = params
        .iter()
        .any(|p| p.trim().eq_ignore_ascii_case("VALUE=DATE"));
    let tzid = params.iter().find_map(|p| {
        let p = p.trim();
        (p.len() > 5 && p[..5].eq_ignore_ascii_case("TZID=")).then(|| p[5..].trim_matches('"'))
    });

    if is_date_param || value.len() == 8 {
        if value.len() < 8 {
            return None;
        }
        let year: i32 = value.get(0..4)?.parse().ok()?;
        let month: u32 = value.get(4..6)?.parse().ok()?;
        let day: u32 = value.get(6..8)?.parse().ok()?;
        let utc_midnight = civil_to_unix_secs(year, month, day, 0, 0, 0)?;
        let adjusted = utc_midnight - local_offset_secs;
        return u64::try_from(adjusted).ok().map(|ts| (ts, true));
    }

    let is_utc = value.ends_with('Z') || value.ends_with('z');
    let core = value.trim_end_matches(['Z', 'z']);
    let (date_part, time_part) = core.split_once(['T', 't'])?;
    if date_part.len() != 8 || time_part.len() < 6 {
        return None;
    }
    let year: i32 = date_part.get(0..4)?.parse().ok()?;
    let month: u32 = date_part.get(4..6)?.parse().ok()?;
    let day: u32 = date_part.get(6..8)?.parse().ok()?;
    let hour: u32 = time_part.get(0..2)?.parse().ok()?;
    let min: u32 = time_part.get(2..4)?.parse().ok()?;
    let sec: u32 = time_part.get(4..6)?.parse().ok()?;

    let raw_secs = civil_to_unix_secs(year, month, day, hour, min, sec)?;
    let offset = if is_utc {
        0
    } else if let Some(tz) = tzid {
        parse_tzid_offset_secs(tz).unwrap_or(local_offset_secs)
    } else {
        local_offset_secs
    };
    let adjusted = raw_secs - offset;
    u64::try_from(adjusted).ok().map(|ts| (ts, false))
}

fn parse_tzid_offset_secs(tzid: &str) -> Option<i64> {
    let t = tzid.trim();
    if t.eq_ignore_ascii_case("UTC")
        || t.eq_ignore_ascii_case("Etc/UTC")
        || t.eq_ignore_ascii_case("GMT")
        || t.eq_ignore_ascii_case("Etc/GMT")
    {
        return Some(0);
    }
    // Support explicit `UTC+03:00` / `GMT-05:00` / `UTC+3` offsets.
    let rest = if t.len() >= 4
        && (t[..3].eq_ignore_ascii_case("UTC") || t[..3].eq_ignore_ascii_case("GMT"))
    {
        &t[3..]
    } else {
        return None;
    };
    let (sign, digits) = if let Some(r) = rest.strip_prefix('+') {
        (1i64, r)
    } else {
        let r = rest.strip_prefix('-')?;
        (-1i64, r)
    };
    let (hh, mm) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None => (digits.parse::<i64>().ok()?, 0i64),
    };
    if !(0..=14).contains(&hh) || !(0..=59).contains(&mm) {
        return None;
    }
    Some(sign * (hh * 3600 + mm * 60))
}

/// Converts a proleptic Gregorian UTC civil timestamp to Unix epoch seconds
/// (Howard Hinnant's `days_from_civil` algorithm).
fn civil_to_unix_secs(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + i64::from(hour) * 3_600 + i64::from(min) * 60 + i64::from(sec))
}

fn local_utc_offset_secs() -> i64 {
    super::telemetry::local_utc_offset_secs()
}

fn normalize_hhmm(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let (h_str, m_str) = trimmed.split_once(':')?;
    if h_str.is_empty() || h_str.len() > 2 || m_str.len() != 2 {
        return None;
    }
    if !h_str.bytes().all(|b| b.is_ascii_digit()) || !m_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: u32 = h_str.parse().ok()?;
    let m: u32 = m_str.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(format!("{h:02}:{m:02}"))
}

fn normalize_ical_url(url: &str) -> String {
    if let Some(rest) = url
        .strip_prefix("webcal://")
        .or_else(|| url.strip_prefix("WEBCAL://"))
    {
        format!("https://{rest}")
    } else {
        url.to_owned()
    }
}

fn clamp_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        text.chars().take(max_chars).collect()
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(TODO_FILE_NAME);
    let tmp_path = path.with_file_name(format!("{file_name}.tmp.{}.{seq}", std::process::id()));
    let write_res = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_inline_time_parsing_extracts_due_time() {
        assert_eq!(
            parse_inline_task("15:30 Team standup"),
            Some(("Team standup".to_owned(), Some("15:30".to_owned())))
        );
        assert_eq!(
            parse_inline_task("9:05 Coffee"),
            Some(("Coffee".to_owned(), Some("09:05".to_owned())))
        );
        assert_eq!(
            parse_inline_task("Ship release @18:45"),
            Some(("Ship release".to_owned(), Some("18:45".to_owned())))
        );
        assert_eq!(
            parse_inline_task("25:99 Invalid time stays title"),
            Some(("25:99 Invalid time stays title".to_owned(), None))
        );
        assert_eq!(parse_inline_task("   "), None);
    }

    #[test]
    fn reminder_triggers_once_and_respects_snooze() {
        let mut todo = TodoState::default();
        let id = todo.add_task("15:30 Standup", None).expect("add task");
        assert_eq!(todo.tasks()[0].due_hhmm.as_deref(), Some("15:30"));

        // Disabled flag suppresses reminders.
        assert!(
            todo.poll_due_reminder(1_700_000_000, "15:30", false)
                .is_none()
        );

        // Fires once at matching HH:MM.
        let fired = todo
            .poll_due_reminder(1_700_000_000, "15:30", true)
            .expect("should trigger");
        assert_eq!(fired.id, id);
        // Same minute does not double-fire.
        assert!(
            todo.poll_due_reminder(1_700_000_010, "15:30", true)
                .is_none()
        );

        // Snooze for 10 minutes: silent before deadline, fires once at deadline.
        assert!(todo.snooze_task_from(id, 1_700_000_000, 10));
        assert!(
            todo.poll_due_reminder(1_700_000_300, "15:35", true)
                .is_none()
        );
        let snoozed = todo
            .poll_due_reminder(1_700_000_600, "15:40", true)
            .expect("snooze should fire");
        assert_eq!(snoozed.id, id);
        assert!(
            todo.poll_due_reminder(1_700_000_605, "15:40", true)
                .is_none()
        );

        // Completed tasks never fire.
        assert!(todo.toggle_done(id));
        assert!(todo.tasks()[0].done);
        assert_eq!(todo.clear_completed(), 1);
        assert!(todo.tasks().is_empty());
    }

    #[test]
    fn rfc5545_ics_parser_handles_folding_utc_tzid_and_date() {
        // Reference time: 2026-11-28 12:00:00 UTC.
        let now_unix = civil_to_unix_secs(2026, 11, 28, 12, 0, 0).unwrap() as u64;
        let ics = concat!(
            "BEGIN:VCALENDAR\r\n",
            "VERSION:2.0\r\n",
            "BEGIN:VEVENT\r\n",
            "SUMMARY:Past Meeting\r\n",
            "DTSTART:20261128T090000Z\r\n",
            "DTEND:20261128T100000Z\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "SUMMARY:Quarterly Design\r\n",
            "  Review\\, Phase 2\r\n",
            "LOCATION:Room 4A\r\n",
            "DTSTART:20261128T123000Z\r\n",
            "DTEND:20261128T133000Z\r\n",
            "BEGIN:VALARM\r\n",
            "SUMMARY:Alarm Noise Should Be Ignored\r\n",
            "END:VALARM\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "SUMMARY:Local TZID Sync\r\n",
            "DTSTART;TZID=Europe/Istanbul:20261128T170000\r\n",
            "DTEND;TZID=Europe/Istanbul:20261128T180000\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "SUMMARY:All-Day Launch\r\n",
            "DTSTART;VALUE=DATE:20261129\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n"
        );

        // Pass explicit +03:00 (10800 s) local offset.
        let events = parse_ics_with_local_offset(ics, now_unix, 3 * 3600);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].summary, "Quarterly Design Review, Phase 2");
        assert_eq!(events[0].location, "Room 4A");
        assert_eq!(
            events[0].start_unix,
            civil_to_unix_secs(2026, 11, 28, 12, 30, 0).unwrap() as u64
        );

        // 17:00 UTC+3 == 14:00 UTC.
        assert_eq!(events[1].summary, "Local TZID Sync");
        assert_eq!(
            events[1].start_unix,
            civil_to_unix_secs(2026, 11, 28, 14, 0, 0).unwrap() as u64
        );

        // 2026-11-29 00:00 UTC+3 == 2026-11-28 21:00 UTC.
        assert_eq!(events[2].summary, "All-Day Launch");
        assert_eq!(
            events[2].start_unix,
            civil_to_unix_secs(2026, 11, 28, 21, 0, 0).unwrap() as u64
        );
        assert_eq!(events[2].end_unix - events[2].start_unix, 86_400);
    }

    #[test]
    fn countdown_formatting_and_flyout_integration() {
        assert_eq!(format_countdown(1000, 2000, 1500), "Now");
        assert_eq!(format_countdown(1030, 2000, 1000), "<1m");
        assert_eq!(format_countdown(1000 + 12 * 60, 5000, 1000), "12m");
        assert_eq!(format_countdown(1000 + 135 * 60, 20_000, 1000), "2h 15m");
        assert_eq!(format_countdown(1000 + 2 * 3600, 20_000, 1000), "2h");
        assert_eq!(format_countdown(1000 + 27 * 3600, 200_000, 1000), "1d 3h");

        let mut state = TodoState::default();
        assert!(state.handle_flyout_action("add", Some("16:00 Demo")));
        assert_eq!(state.tasks().len(), 1);
        state.set_events(vec![CalendarEvent {
            summary: "Sprint Review".to_owned(),
            start_unix: 2_000,
            end_unix: 5_600,
            location: "Zoom".to_owned(),
        }]);
        assert_eq!(
            state.next_event(1_000).map(|e| e.summary.as_str()),
            Some("Sprint Review")
        );

        let detail = state.build_island_detail(1_000);
        assert_eq!(detail.widget_id, "todo");
        assert!(detail.rows.len() <= 4);
        assert!(detail.actions.len() <= 3);

        let flyout = state.build_flyout(1_000, true);
        assert!(
            flyout.blocks.iter().any(
                |b| matches!(b, FlyoutBlock::TextInput { id, .. } if *id == FLYOUT_ADD_INPUT_ID)
            )
        );
    }
}
