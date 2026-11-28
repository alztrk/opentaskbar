//! Local AI coding agent monitor & inline approval cards (N7).
//!
//! Event-driven, zero-CPU / zero-RAM when disabled:
//! - Spawns a single blocking Win32 `ConnectNamedPipe` acceptor thread ONLY when
//!   [`AgentMonitorState::start`] is called.
//! - Speaks newline-delimited JSON over `\\.\pipe\opentaskbar-agents-<sanitized_username>`.
//! - Supports `claude`, `codex`, `cursor`, `antigravity`, `opencode`, `copilot`, or custom agents.
//! - Enforces strict memory bounds: max 8 active sessions, max 12 recent steps per session,
//!   and `diff_preview` capped to `<= 8192` bytes.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
    HWND, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OVERLAPPED, FlushFileBuffers, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, CreateEventW, GetCurrentProcess, INFINITE, ResetEvent, SetEvent,
    WaitForMultipleObjects,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
    IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

use crate::design::{self, Rgb};
use crate::platform::win32_helpers::wide;
use crate::ui::flyout::{FlyoutBlock, FlyoutSpec};
use crate::widgets::{IslandDetailAction, IslandDetailRow, IslandDetailSpec};

/// Maximum number of active/recent agent sessions retained in memory.
pub const MAX_ACTIVE_SESSIONS: usize = 8;
/// Maximum number of recent tool/action steps retained per session.
pub const MAX_STEPS_PER_SESSION: usize = 12;
/// Maximum byte length for an approval `diff_preview` payload.
pub const MAX_DIFF_PREVIEW_BYTES: usize = 8192;
/// Timeout in seconds for unanswered approval requests before falling back to `"terminal"`.
pub const APPROVAL_TIMEOUT_SECS: u64 = 120;

const MAX_INFLIGHT_PIPE_CLIENTS: usize = 16;
const MAX_PIPE_LINE_BYTES: usize = 64 * 1024;
const PIPE_BUFFER_BYTES: u32 = 16 * 1024;

// Win32 Named Pipe / Security constants (declared locally so extra `windows-sys` features are not needed).
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER_CLASS: u32 = 1;
const SDDL_REVISION_1: u32 = 1;

// Deterministic FlyoutBlock button/action IDs.
const ACTION_ALLOW_ID: u32 = 10;
const ACTION_DENY_ID: u32 = 11;
const ACTION_ALWAYS_ID: u32 = 12;
const ACTION_JUMP_PENDING_ID: u32 = 13;
const ACTION_INSTALL_HOOK_ID: u32 = 20;
const ACTION_CLEAR_FINISHED_ID: u32 = 21;
const ACTION_SESSION_JUMP_BASE: u32 = 100;

#[repr(C)]
struct SecurityAttributes {
    n_length: u32,
    lp_security_descriptor: *mut c_void,
    b_inherit_handle: i32,
}

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

#[repr(C)]
struct TokenUser {
    user: SidAndAttributes,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        lp_name: *const u16,
        dw_open_mode: u32,
        dw_pipe_mode: u32,
        n_max_instances: u32,
        n_out_buffer_size: u32,
        n_in_buffer_size: u32,
        n_default_time_out: u32,
        lp_security_attributes: *const c_void,
    ) -> HANDLE;
    fn ConnectNamedPipe(h_named_pipe: HANDLE, lp_overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(h_named_pipe: HANDLE) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(
        process_handle: HANDLE,
        desired_access: u32,
        token_handle: *mut HANDLE,
    ) -> i32;
    fn GetTokenInformation(
        token_handle: HANDLE,
        token_information_class: u32,
        token_information: *mut c_void,
        token_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *const c_void, string_sid: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        string_sd_revision: u32,
        security_descriptor: *mut *mut c_void,
        security_descriptor_size: *mut u32,
    ) -> i32;
}

/// Execution state of a monitored local coding agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionState {
    Working,
    WaitingApproval,
    WaitingQuestion,
    Done,
    Error,
    Idle,
}

impl AgentSessionState {
    /// Canonical lowercase wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::WaitingApproval => "waiting_approval",
            Self::WaitingQuestion => "waiting_question",
            Self::Done => "done",
            Self::Error => "error",
            Self::Idle => "idle",
        }
    }

    /// Short human-readable label for chips, island rows, and flyouts.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::WaitingApproval => "Approval",
            Self::WaitingQuestion => "Question",
            Self::Done => "Done",
            Self::Error => "Error",
            Self::Idle => "Idle",
        }
    }

    /// Parses state strings case-insensitively with a safe `Working` fallback.
    #[must_use]
    pub fn from_str_lossy(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "working" | "running" | "in_progress" | "active" => Self::Working,
            "waiting_approval" | "approval" | "permission" | "pending_approval" => {
                Self::WaitingApproval
            }
            "waiting_question" | "question" | "input" | "waiting_input" => Self::WaitingQuestion,
            "done" | "completed" | "finished" | "stop" | "stopped" => Self::Done,
            "error" | "failed" | "failure" => Self::Error,
            "idle" | "waiting" => Self::Idle,
            _ => Self::Working,
        }
    }

    #[must_use]
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Idle | Self::Error)
    }

    fn semantic_color(self, pal: &design::Palette) -> Rgb {
        match self {
            Self::WaitingApproval => pal.warning,
            Self::WaitingQuestion => pal.accent,
            Self::Working => pal.widget_accent,
            Self::Done => pal.success,
            Self::Error => pal.danger,
            Self::Idle => pal.text_secondary,
        }
    }
}

/// One tool invocation or step emitted by a hooked coding agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStep {
    pub tool: String,
    pub summary: String,
    pub diff_stats: Option<String>,
    pub timestamp_unix: u64,
}

impl AgentStep {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "tool": self.tool,
            "summary": self.summary,
            "diff_stats": self.diff_stats,
            "timestamp_unix": self.timestamp_unix,
        })
    }

    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let tool = str_field(obj, &["tool", "tool_name"]).unwrap_or_else(|| "step".to_owned());
        let summary = str_field(obj, &["summary", "headline", "detail"]).unwrap_or_default();
        let diff_stats = str_field(obj, &["diff_stats"]);
        let timestamp_unix = obj
            .get("timestamp_unix")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_unix);
        Some(Self {
            tool,
            summary,
            diff_stats,
            timestamp_unix,
        })
    }
}

/// Pending tool/permission approval request waiting for a user decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentApprovalRequest {
    pub request_id: String,
    pub agent: String,
    pub session_id: String,
    pub project: String,
    pub action_title: String,
    pub detail: String,
    pub diff_preview: String,
    pub terminal_hwnd: isize,
}

impl AgentApprovalRequest {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "request_id": self.request_id,
            "agent": self.agent,
            "session_id": self.session_id,
            "project": self.project,
            "action_title": self.action_title,
            "detail": self.detail,
            "diff_preview": self.diff_preview,
            "terminal_hwnd": self.terminal_hwnd,
        })
    }

    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let request_id = str_field(obj, &["request_id"])?;
        let agent = str_field(obj, &["agent"]).unwrap_or_else(|| "claude".to_owned());
        let session_id = str_field(obj, &["session_id"]).unwrap_or_else(|| "default".to_owned());
        let project = str_field(obj, &["project"]).unwrap_or_default();
        let action_title =
            str_field(obj, &["action_title", "title"]).unwrap_or_else(|| "Approval".to_owned());
        let detail = str_field(obj, &["detail", "summary", "message"]).unwrap_or_default();
        let diff_preview = truncate_utf8_bytes(
            &str_field(obj, &["diff_preview", "diff"]).unwrap_or_default(),
            MAX_DIFF_PREVIEW_BYTES,
        );
        let terminal_hwnd = obj
            .get("terminal_hwnd")
            .and_then(Value::as_i64)
            .unwrap_or(0) as isize;
        Some(Self {
            request_id,
            agent,
            session_id,
            project,
            action_title,
            detail,
            diff_preview,
            terminal_hwnd,
        })
    }
}

/// Tracked state of a local AI coding agent session.
#[derive(Debug, Clone)]
pub struct AgentSession {
    pub session_id: String,
    pub agent: String,
    pub project: String,
    pub state: AgentSessionState,
    pub headline: String,
    pub steps: VecDeque<AgentStep>,
    pub terminal_hwnd: isize,
    pub updated_unix: u64,
}

/// Wire packet parsed from a newline-delimited JSON event sent over the named pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHookEvent {
    pub event: String,
    pub agent: String,
    pub session_id: String,
    pub project: String,
    pub state: Option<AgentSessionState>,
    pub headline: Option<String>,
    pub tool: Option<String>,
    pub summary: Option<String>,
    pub diff_stats: Option<String>,
    pub request_id: Option<String>,
    pub action_title: Option<String>,
    pub detail: Option<String>,
    pub diff_preview: Option<String>,
    pub terminal_hwnd: isize,
    pub timestamp_unix: u64,
}

impl AgentHookEvent {
    /// Parses one newline-delimited JSON line into an [`AgentHookEvent`].
    pub fn from_json_str(line: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
        Self::from_value(&value)
    }

    /// Parses a [`serde_json::Value`] object into an [`AgentHookEvent`].
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "agent hook event must be a JSON object".to_owned())?;
        let event = str_field(obj, &["event", "type", "hook_event_name"])
            .unwrap_or_else(|| "step".to_owned());
        let agent = normalize_agent_name(
            str_field(obj, &["agent", "provider"])
                .as_deref()
                .unwrap_or("claude"),
        );
        let session_id = str_field(obj, &["session_id"]).unwrap_or_else(|| "default".to_owned());
        let project = str_field(obj, &["project"])
            .or_else(|| str_field(obj, &["cwd"]).map(|p| project_name_from_path(&p)))
            .unwrap_or_default();
        let state = str_field(obj, &["state"]).map(|s| AgentSessionState::from_str_lossy(&s));
        let headline = str_field(obj, &["headline", "title"]);
        let tool = str_field(obj, &["tool", "tool_name"]);
        let summary = str_field(obj, &["summary", "message"]);
        let diff_stats = str_field(obj, &["diff_stats"]);
        let request_id = str_field(obj, &["request_id"]);
        let action_title = str_field(obj, &["action_title", "title"]);
        let detail = str_field(obj, &["detail", "message", "summary"]);
        let diff_preview = str_field(obj, &["diff_preview", "diff"])
            .map(|d| truncate_utf8_bytes(&d, MAX_DIFF_PREVIEW_BYTES));
        let terminal_hwnd = obj
            .get("terminal_hwnd")
            .and_then(Value::as_i64)
            .unwrap_or(0) as isize;
        let timestamp_unix = obj
            .get("timestamp_unix")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_unix);

        Ok(Self {
            event,
            agent,
            session_id,
            project,
            state,
            headline,
            tool,
            summary,
            diff_stats,
            request_id,
            action_title,
            detail,
            diff_preview,
            terminal_hwnd,
            timestamp_unix,
        })
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "event": self.event,
            "agent": self.agent,
            "session_id": self.session_id,
            "project": self.project,
            "state": self.state.map(AgentSessionState::as_str),
            "headline": self.headline,
            "tool": self.tool,
            "summary": self.summary,
            "diff_stats": self.diff_stats,
            "request_id": self.request_id,
            "action_title": self.action_title,
            "detail": self.detail,
            "diff_preview": self.diff_preview,
            "terminal_hwnd": self.terminal_hwnd,
            "timestamp_unix": self.timestamp_unix,
        })
    }

    fn is_approval_request(&self) -> bool {
        let ev = self.event.trim().to_ascii_lowercase();
        ev == "approval_request" || ev == "permissionrequest" || ev == "permission_request"
    }
}

struct PendingApprovalEntry {
    request: AgentApprovalRequest,
    reply_tx: SyncSender<String>,
    created_at: Instant,
}

enum InboxItem {
    Event(AgentHookEvent),
    Approval {
        event: AgentHookEvent,
        request: AgentApprovalRequest,
        reply_tx: SyncSender<String>,
    },
    ApprovalTimedOut(String),
}

#[derive(Default)]
struct SharedInbox {
    items: VecDeque<InboxItem>,
}

/// Event-driven Win32 Named Pipe monitor and in-memory session/approval state.
///
/// Construct via [`AgentMonitorState::disabled`] (or [`Default`]) when the widget is off
/// so zero threads or kernel pipe handles are created. Call [`AgentMonitorState::start`]
/// only when `agent_monitor_enabled == true`.
pub struct AgentMonitorState {
    enabled: bool,
    pipe_path: String,
    sessions: Vec<AgentSession>,
    pending_approvals: VecDeque<PendingApprovalEntry>,
    last_status_note: Option<String>,
    shared: Arc<Mutex<SharedInbox>>,
    running: Arc<AtomicBool>,
    stop_event: Option<Arc<SharedEventHandle>>,
    acceptor_thread: Option<JoinHandle<()>>,
}

impl Default for AgentMonitorState {
    fn default() -> Self {
        Self::disabled()
    }
}

impl AgentMonitorState {
    /// Disabled / inert monitor consuming zero threads and zero pipe handles.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            pipe_path: String::new(),
            sessions: Vec::new(),
            pending_approvals: VecDeque::new(),
            last_status_note: None,
            shared: Arc::new(Mutex::new(SharedInbox::default())),
            running: Arc::new(AtomicBool::new(false)),
            stop_event: None,
            acceptor_thread: None,
        }
    }

    /// Starts the named-pipe monitor on [`pipe_name()`] with one overlapped acceptor thread.
    #[must_use]
    pub fn start() -> Self {
        Self::start_with_pipe(pipe_name())
    }

    /// Starts the named-pipe monitor on an explicit pipe path (useful in tests).
    #[must_use]
    pub fn start_with_pipe(pipe_path: String) -> Self {
        let shared = Arc::new(Mutex::new(SharedInbox::default()));
        let running = Arc::new(AtomicBool::new(true));
        let active_clients = Arc::new(AtomicUsize::new(0));
        let stop_event = SharedEventHandle::new_manual_reset().map(Arc::new);

        let acceptor = stop_event.as_ref().and_then(|ev| {
            let thread_pipe = pipe_path.clone();
            let thread_shared = Arc::clone(&shared);
            let thread_running = Arc::clone(&running);
            let thread_clients = Arc::clone(&active_clients);
            let thread_stop = Arc::clone(ev);

            thread::Builder::new()
                .name("opentaskbar-agents-pipe".to_owned())
                .spawn(move || {
                    pipe_acceptor_loop(
                        thread_pipe,
                        thread_shared,
                        thread_running,
                        thread_clients,
                        thread_stop,
                    );
                })
                .ok()
        });

        Self {
            enabled: true,
            pipe_path,
            sessions: Vec::new(),
            pending_approvals: VecDeque::new(),
            last_status_note: None,
            shared,
            running,
            stop_event,
            acceptor_thread: acceptor,
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Drains any pending pipe events received by background worker threads into local state.
    /// Returns `true` if state changed.
    pub fn poll_events(&mut self) -> bool {
        let mut changed = false;
        let drained: Vec<InboxItem> = if let Ok(mut guard) = self.shared.lock() {
            guard.items.drain(..).collect()
        } else {
            Vec::new()
        };

        for item in drained {
            changed = true;
            match item {
                InboxItem::Event(ev) => {
                    self.ingest_event(ev);
                }
                InboxItem::Approval {
                    event,
                    request,
                    reply_tx,
                } => {
                    self.ingest_event(event);
                    self.push_pending_approval(request, reply_tx);
                }
                InboxItem::ApprovalTimedOut(req_id) => {
                    self.remove_pending_approval(&req_id);
                }
            }
        }

        // Prune approvals that exceeded the 120s timeout even if their worker notification raced.
        let timeout = Duration::from_secs(APPROVAL_TIMEOUT_SECS);
        let before = self.pending_approvals.len();
        self.pending_approvals
            .retain(|entry| entry.created_at.elapsed() < timeout);
        if self.pending_approvals.len() != before {
            changed = true;
        }
        changed
    }

    /// Ingests one parsed hook event into the bounded session ring buffers.
    pub fn ingest_event(&mut self, event: AgentHookEvent) {
        let step = event
            .tool
            .as_ref()
            .or(event.summary.as_ref())
            .map(|_| AgentStep {
                tool: event.tool.clone().unwrap_or_else(|| event.event.clone()),
                summary: event
                    .summary
                    .clone()
                    .or_else(|| event.headline.clone())
                    .or_else(|| event.detail.clone())
                    .unwrap_or_default(),
                diff_stats: event.diff_stats.clone(),
                timestamp_unix: event.timestamp_unix,
            });

        let derived_state = event.state.unwrap_or_else(|| {
            let ev = event.event.trim().to_ascii_lowercase();
            if event.is_approval_request() {
                AgentSessionState::WaitingApproval
            } else if ev == "question" || ev == "waiting_question" {
                AgentSessionState::WaitingQuestion
            } else if ev == "session_end" || ev == "stop" || ev == "done" {
                AgentSessionState::Done
            } else if ev == "error" || ev == "failed" {
                AgentSessionState::Error
            } else if ev == "idle" {
                AgentSessionState::Idle
            } else {
                AgentSessionState::Working
            }
        });

        let headline = event
            .headline
            .clone()
            .or_else(|| event.action_title.clone())
            .or_else(|| event.summary.clone())
            .unwrap_or_else(|| derived_state.label().to_owned());

        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|s| s.session_id == event.session_id)
        {
            if !event.agent.is_empty() {
                existing.agent = event.agent;
            }
            if !event.project.is_empty() {
                existing.project = event.project;
            }
            existing.state = derived_state;
            if !headline.is_empty() {
                existing.headline = headline;
            }
            if event.terminal_hwnd != 0 {
                existing.terminal_hwnd = event.terminal_hwnd;
            }
            existing.updated_unix = event.timestamp_unix;
            if let Some(step) = step {
                push_bounded_step(&mut existing.steps, step);
            }
        } else {
            self.evict_session_if_full();
            let mut steps = VecDeque::with_capacity(MAX_STEPS_PER_SESSION);
            if let Some(step) = step {
                push_bounded_step(&mut steps, step);
            }
            self.sessions.push(AgentSession {
                session_id: event.session_id,
                agent: event.agent,
                project: event.project,
                state: derived_state,
                headline,
                steps,
                terminal_hwnd: event.terminal_hwnd,
                updated_unix: event.timestamp_unix,
            });
        }
    }

    /// Registers a pending approval request and its reply channel directly (also used in tests).
    pub fn push_pending_approval(
        &mut self,
        mut request: AgentApprovalRequest,
        reply_tx: SyncSender<String>,
    ) {
        request.diff_preview = truncate_utf8_bytes(&request.diff_preview, MAX_DIFF_PREVIEW_BYTES);
        self.pending_approvals
            .retain(|p| p.request.request_id != request.request_id);
        if self.pending_approvals.len() >= MAX_ACTIVE_SESSIONS {
            self.pending_approvals.pop_front();
        }
        self.pending_approvals.push_back(PendingApprovalEntry {
            request,
            reply_tx,
            created_at: Instant::now(),
        });
    }

    fn remove_pending_approval(&mut self, request_id: &str) -> Option<PendingApprovalEntry> {
        let pos = self
            .pending_approvals
            .iter()
            .position(|p| p.request.request_id == request_id)?;
        self.pending_approvals.remove(pos)
    }

    fn evict_session_if_full(&mut self) {
        if self.sessions.len() < MAX_ACTIVE_SESSIONS {
            return;
        }
        // Prefer evicting the oldest finished session that does not hold a pending approval.
        let victim = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                !self
                    .pending_approvals
                    .iter()
                    .any(|p| p.request.session_id == s.session_id)
            })
            .min_by_key(|(_, s)| (u8::from(!s.state.is_finished()), s.updated_unix))
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.sessions.remove(victim);
    }

    /// Sends `"allow"`, `"deny"`, or `"always"` back over the named pipe and removes the request.
    pub fn respond_approval(&mut self, request_id: &str, decision: &str) -> bool {
        let normalized = match decision.trim().to_ascii_lowercase().as_str() {
            "allow" => "allow",
            "deny" => "deny",
            "always" => "always",
            "terminal" => "terminal",
            _ => return false,
        };
        let Some(entry) = self.remove_pending_approval(request_id) else {
            return false;
        };
        let sent = entry.reply_tx.try_send(normalized.to_owned()).is_ok();
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|s| s.session_id == entry.request.session_id)
        {
            session.state = if normalized == "deny" {
                AgentSessionState::Idle
            } else {
                AgentSessionState::Working
            };
            session.updated_unix = now_unix();
        }
        sent
    }

    /// Highest-priority pending approval request for the Dynamic Island priority banner.
    #[must_use]
    pub fn pending_approval(&self) -> Option<&AgentApprovalRequest> {
        self.pending_approvals.front().map(|entry| &entry.request)
    }

    /// Active and recently tracked agent sessions (at most [`MAX_ACTIVE_SESSIONS`]).
    #[must_use]
    pub fn active_sessions(&self) -> Vec<&AgentSession> {
        self.sessions.iter().collect()
    }

    /// Concise badge text for the Dock widget chip (e.g., `"1!"`, `"2"`, or `"Idle"`).
    #[must_use]
    pub fn summary_badge(&self) -> String {
        if !self.pending_approvals.is_empty() {
            return format!("{}!", self.pending_approvals.len());
        }
        let working = self
            .sessions
            .iter()
            .filter(|s| {
                matches!(
                    s.state,
                    AgentSessionState::Working
                        | AgentSessionState::WaitingApproval
                        | AgentSessionState::WaitingQuestion
                )
            })
            .count();
        if working > 0 {
            return format!("{working}");
        }
        if self.sessions.is_empty() {
            "0".to_owned()
        } else {
            "Done".to_owned()
        }
    }

    /// Safely restores and brings an agent's terminal window to the foreground.
    pub fn jump_to_terminal(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let target = hwnd as HWND;
        // SAFETY: Win32 window validity check before interacting with the HWND.
        unsafe {
            if IsWindow(target) == 0 {
                return false;
            }
            if IsIconic(target) != 0 {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            let mut granted = SetForegroundWindow(target) != 0;
            if !granted {
                let foreground = GetForegroundWindow();
                if foreground != target {
                    let target_thread = GetWindowThreadProcessId(target, std::ptr::null_mut());
                    let fg_thread = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
                    if target_thread != 0
                        && fg_thread != 0
                        && target_thread != fg_thread
                        && AttachThreadInput(fg_thread, target_thread, 1) != 0
                    {
                        granted = SetForegroundWindow(target) != 0;
                        let _ = AttachThreadInput(fg_thread, target_thread, 0);
                    }
                }
            }
            granted || GetForegroundWindow() == target
        }
    }

    /// Dynamic Island expanded detail payload, prioritizing pending approvals.
    #[must_use]
    pub fn build_island_detail(&self) -> IslandDetailSpec {
        if let Some(req) = self.pending_approval() {
            let mut rows = vec![
                IslandDetailRow {
                    label: "Project".to_owned(),
                    value: if req.project.is_empty() {
                        req.session_id.clone()
                    } else {
                        req.project.clone()
                    },
                    progress: None,
                },
                IslandDetailRow {
                    label: "Detail".to_owned(),
                    value: truncate_chars(&req.detail, 90),
                    progress: None,
                },
            ];
            if !req.diff_preview.is_empty() {
                rows.push(IslandDetailRow {
                    label: "Preview".to_owned(),
                    value: truncate_chars(&req.diff_preview, 90),
                    progress: None,
                });
            }
            return IslandDetailSpec {
                widget_id: "agents".to_owned(),
                title: format!("{} · Approval Required", display_agent_name(&req.agent)),
                headline: req.action_title.clone(),
                subtitle: if req.project.is_empty() {
                    req.agent.clone()
                } else {
                    req.project.clone()
                },
                rows,
                actions: vec![
                    IslandDetailAction {
                        id: format!("approve_allow:{}", req.request_id),
                        label: "Allow".to_owned(),
                        primary: true,
                    },
                    IslandDetailAction {
                        id: format!("approve_deny:{}", req.request_id),
                        label: "Deny".to_owned(),
                        primary: false,
                    },
                    IslandDetailAction {
                        id: format!("approve_always:{}", req.request_id),
                        label: "Always".to_owned(),
                        primary: false,
                    },
                ],
                has_full_flyout: true,
            };
        }

        let working_count = self
            .sessions
            .iter()
            .filter(|s| s.state == AgentSessionState::Working)
            .count();
        let rows: Vec<IslandDetailRow> = if self.sessions.is_empty() {
            vec![IslandDetailRow {
                label: "Pipe".to_owned(),
                value: "Listening".to_owned(),
                progress: None,
            }]
        } else {
            self.sessions
                .iter()
                .rev()
                .take(4)
                .map(|s| {
                    let place = if s.project.is_empty() {
                        display_agent_name(&s.agent).to_owned()
                    } else {
                        format!("{} ({})", display_agent_name(&s.agent), s.project)
                    };
                    IslandDetailRow {
                        label: place,
                        value: format!("{} · {}", s.state.label(), truncate_chars(&s.headline, 36)),
                        progress: None,
                    }
                })
                .collect()
        };

        let mut actions = Vec::new();
        if let Some(latest) = self.sessions.iter().rev().find(|s| s.terminal_hwnd != 0) {
            actions.push(IslandDetailAction {
                id: format!("jump:{}", latest.session_id),
                label: "Jump to Terminal".to_owned(),
                primary: true,
            });
        }
        actions.push(IslandDetailAction {
            id: "install_hook".to_owned(),
            label: "Install Claude Hook".to_owned(),
            primary: actions.is_empty(),
        });

        IslandDetailSpec {
            widget_id: "agents".to_owned(),
            title: "AI Coding Agents".to_owned(),
            headline: if self.sessions.is_empty() {
                "No Active Agents".to_owned()
            } else if working_count > 0 {
                format!("{working_count} Active")
            } else {
                format!("{} Tracked", self.sessions.len())
            },
            subtitle: format!("{} session(s)", self.sessions.len()),
            rows,
            actions,
            has_full_flyout: true,
        }
    }

    /// Reference-parity DockFlyout spec showing pending approval cards, active sessions, and hook setup.
    #[must_use]
    pub fn build_flyout(&self, is_dark: bool) -> FlyoutSpec {
        let pal = design::palette(is_dark);
        let mut blocks = Vec::new();

        if let Some(req) = self.pending_approval() {
            blocks.push(FlyoutBlock::StatusHeader {
                title: format!("{} · {}", display_agent_name(&req.agent), req.project),
                status_label: "Approval Needed".to_owned(),
                status_color: pal.warning,
                action_id: (req.terminal_hwnd != 0).then_some(ACTION_JUMP_PENDING_ID),
                action_label: (req.terminal_hwnd != 0).then(|| "Terminal".to_owned()),
            });
            blocks.push(FlyoutBlock::Hero {
                value: req.action_title.clone(),
                caption: (!req.detail.is_empty()).then(|| truncate_chars(&req.detail, 180)),
                centered: false,
                large: false,
                right_meta: None,
            });
            if !req.diff_preview.is_empty() {
                blocks.push(FlyoutBlock::Note(truncate_chars(&req.diff_preview, 400)));
            }
            blocks.push(FlyoutBlock::ButtonBar(vec![
                (ACTION_ALLOW_ID, "Allow".to_owned(), true),
                (ACTION_DENY_ID, "Deny".to_owned(), false),
                (ACTION_ALWAYS_ID, "Always".to_owned(), false),
            ]));
            blocks.push(FlyoutBlock::Separator);
        }

        if self.sessions.is_empty() {
            blocks.push(FlyoutBlock::HeroHeader {
                glyph: 0xE99A, // Robot / Agent glyph
                glyph_color: pal.widget_accent,
                title: "No agent sessions yet".to_owned(),
                subtitle: Some("Connect Claude Code, Codex, Cursor, or OpenCode hooks".to_owned()),
                right_value: Some(self.summary_badge()),
            });
        } else {
            blocks.push(FlyoutBlock::StatusHeader {
                title: "Sessions".to_owned(),
                status_label: format!("{} active", self.sessions.len()),
                status_color: pal.widget_accent,
                action_id: None,
                action_label: None,
            });
            for (idx, session) in self.sessions.iter().enumerate() {
                let subtitle = if session.project.is_empty() {
                    truncate_chars(&session.headline, 48)
                } else {
                    format!(
                        "{} · {}",
                        session.project,
                        truncate_chars(&session.headline, 34)
                    )
                };
                blocks.push(FlyoutBlock::GlyphActionRow {
                    id: ACTION_SESSION_JUMP_BASE + idx as u32,
                    glyph: 0xE756, // CommandPrompt glyph
                    glyph_color: session.state.semantic_color(&pal),
                    title: display_agent_name(&session.agent).to_owned(),
                    subtitle: Some(subtitle),
                    badge: Some(session.state.label().to_owned()),
                });
            }

            if let Some(recent) = self.sessions.last()
                && !recent.steps.is_empty()
            {
                blocks.push(FlyoutBlock::Separator);
                for step in recent.steps.iter().rev().take(4) {
                    let val = match &step.diff_stats {
                        Some(stats) if !stats.is_empty() => {
                            format!("{} ({stats})", truncate_chars(&step.summary, 28))
                        }
                        _ => truncate_chars(&step.summary, 36),
                    };
                    blocks.push(FlyoutBlock::StatRow {
                        label: step.tool.clone(),
                        value: val,
                    });
                }
            }
        }

        if let Some(note) = &self.last_status_note {
            blocks.push(FlyoutBlock::Note(note.clone()));
        }

        blocks.push(FlyoutBlock::Separator);
        blocks.push(FlyoutBlock::ButtonBar(vec![
            (
                ACTION_INSTALL_HOOK_ID,
                "Install Claude Hook".to_owned(),
                false,
            ),
            (ACTION_CLEAR_FINISHED_ID, "Clear Finished".to_owned(), false),
        ]));

        let subtitle = if self.pipe_path.is_empty() {
            pipe_name()
        } else {
            self.pipe_path.clone()
        };

        FlyoutSpec {
            title: "AI Coding Agents".to_owned(),
            subtitle: Some(subtitle),
            width_dips: 340.0,
            blocks,
        }
    }

    /// Handles flyout button/row actions (accepts both semantic string IDs and numeric `u32` IDs).
    pub fn handle_flyout_action(&mut self, action_id: &str) -> bool {
        let trimmed = action_id.trim();
        if let Some(req_id) = trimmed
            .strip_prefix("approve_allow:")
            .or_else(|| trimmed.strip_prefix("allow:"))
        {
            return self.respond_approval(req_id, "allow");
        }
        if let Some(req_id) = trimmed
            .strip_prefix("approve_deny:")
            .or_else(|| trimmed.strip_prefix("deny:"))
        {
            return self.respond_approval(req_id, "deny");
        }
        if let Some(req_id) = trimmed
            .strip_prefix("approve_always:")
            .or_else(|| trimmed.strip_prefix("always:"))
        {
            return self.respond_approval(req_id, "always");
        }
        if let Some(target) = trimmed.strip_prefix("jump:") {
            if let Ok(hwnd) = target.parse::<isize>() {
                return Self::jump_to_terminal(hwnd);
            }
            if let Some(session) = self.sessions.iter().find(|s| s.session_id == target) {
                return Self::jump_to_terminal(session.terminal_hwnd);
            }
            return false;
        }

        match trimmed {
            "install_hook" => {
                match install_claude_code_hook() {
                    Ok(msg) => {
                        self.last_status_note = Some(msg);
                    }
                    Err(err) => {
                        self.last_status_note = Some(err);
                    }
                }
                true
            }
            "clear_finished" => {
                let before = self.sessions.len();
                self.sessions.retain(|s| !s.state.is_finished());
                self.sessions.len() != before
            }
            "jump_pending" => {
                let hwnd = self.pending_approval().map_or(0, |r| r.terminal_hwnd);
                Self::jump_to_terminal(hwnd)
            }
            _ => {
                if let Ok(id) = trimmed.parse::<u32>() {
                    return self.handle_flyout_click(id);
                }
                false
            }
        }
    }

    /// Convenience adapter for `LiveEvent::Click(u32)` handlers in `ui/flyout.rs`.
    pub fn handle_flyout_click(&mut self, id: u32) -> bool {
        match id {
            ACTION_ALLOW_ID => {
                if let Some(req_id) = self.pending_approval().map(|r| r.request_id.clone()) {
                    return self.respond_approval(&req_id, "allow");
                }
                false
            }
            ACTION_DENY_ID => {
                if let Some(req_id) = self.pending_approval().map(|r| r.request_id.clone()) {
                    return self.respond_approval(&req_id, "deny");
                }
                false
            }
            ACTION_ALWAYS_ID => {
                if let Some(req_id) = self.pending_approval().map(|r| r.request_id.clone()) {
                    return self.respond_approval(&req_id, "always");
                }
                false
            }
            ACTION_JUMP_PENDING_ID => {
                let hwnd = self.pending_approval().map_or(0, |r| r.terminal_hwnd);
                Self::jump_to_terminal(hwnd)
            }
            ACTION_INSTALL_HOOK_ID => self.handle_flyout_action("install_hook"),
            ACTION_CLEAR_FINISHED_ID => self.handle_flyout_action("clear_finished"),
            n if n >= ACTION_SESSION_JUMP_BASE => {
                let idx = (n - ACTION_SESSION_JUMP_BASE) as usize;
                self.sessions
                    .get(idx)
                    .is_some_and(|s| Self::jump_to_terminal(s.terminal_hwnd))
            }
            _ => false,
        }
    }
}

impl Drop for AgentMonitorState {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        self.running.store(false, Ordering::Release);
        // Signal the manual-reset stop event BEFORE joining so overlapped `ConnectNamedPipe`
        // (and any pending wait) wakes immediately without TOCTOU races.
        if let Some(ev) = &self.stop_event {
            ev.signal();
        }
        // Drop all pending SyncSenders immediately so any blocked pipe approval workers wake cleanly.
        self.pending_approvals.clear();
        if let Ok(mut guard) = self.shared.lock() {
            guard.items.clear();
        }
        if let Some(handle) = self.acceptor_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Deterministic per-user Win32 named-pipe path: `\\.\pipe\opentaskbar-agents-<sanitized_username>`.
#[must_use]
pub fn pipe_name() -> String {
    let raw_user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".to_owned());
    format!(
        r"\\.\pipe\opentaskbar-agents-{}",
        sanitize_username(&raw_user)
    )
}

/// Sanitizes a username for safe inclusion in a Windows named-pipe path.
#[must_use]
pub fn sanitize_username(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "user".to_owned()
    } else {
        cleaned
    }
}

/// Path to the PowerShell relay script under `%LOCALAPPDATA%\OpenTaskbar\agent-hook.ps1`.
#[must_use]
pub fn hook_relay_script_path() -> PathBuf {
    opentaskbar_core::paths::shared_app_data_dir().join("agent-hook.ps1")
}

/// Installs the OpenTaskbar PowerShell relay script and merges hook entries into `~/.claude/settings.json`
/// after creating a `.bak` backup if the settings file already exists.
pub fn install_claude_code_hook() -> Result<String, String> {
    let script_path = hook_relay_script_path();
    if let Some(parent) = script_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let short_pipe = pipe_name()
        .strip_prefix(r"\\.\pipe\")
        .unwrap_or("opentaskbar-agents-user")
        .to_owned();
    std::fs::write(&script_path, build_relay_ps1(&short_pipe))
        .map_err(|e| format!("failed to write {}: {e}", script_path.display()))?;

    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| "could not resolve USERPROFILE or HOME".to_owned())?;
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .map_err(|e| format!("failed to create {}: {e}", claude_dir.display()))?;
    let settings_path = claude_dir.join("settings.json");
    if settings_path.is_file() {
        let backup_path = claude_dir.join("settings.json.bak");
        std::fs::copy(&settings_path, &backup_path)
            .map_err(|e| format!("failed to backup {}: {e}", settings_path.display()))?;
    }
    let existing = if settings_path.is_file() {
        std::fs::read_to_string(&settings_path)
            .map_err(|e| format!("failed to read {}: {e}", settings_path.display()))?
    } else {
        "{}".to_owned()
    };
    let merged = merge_claude_settings_json(&existing, &script_path)?;
    std::fs::write(&settings_path, merged)
        .map_err(|e| format!("failed to write {}: {e}", settings_path.display()))?;

    Ok(format!("Hook installed in {}", settings_path.display()))
}

/// Idempotently merges OpenTaskbar Claude Code hook entries into an existing `settings.json` document.
pub fn merge_claude_settings_json(existing: &str, script_path: &Path) -> Result<String, String> {
    let trimmed = existing.trim();
    let mut root: Value = if trimmed.is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(trimmed).map_err(|e| format!("invalid settings.json: {e}"))?
    };
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "settings.json root must be a JSON object".to_owned())?;

    let hooks_val = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = hooks_val
        .as_object_mut()
        .ok_or_else(|| "settings.json \"hooks\" must be a JSON object".to_owned())?;

    let script_str = script_path.to_string_lossy();
    for event_name in ["PreToolUse", "PostToolUse", "Notification", "Stop"] {
        let cmd = format!(
            "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{script_str}\" -Event {event_name}"
        );
        let desired_entry = json!({
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": cmd,
            }]
        });

        let arr_val = hooks_obj
            .entry(event_name)
            .or_insert_with(|| Value::Array(Vec::new()));
        let arr = arr_val
            .as_array_mut()
            .ok_or_else(|| format!("settings.json \"hooks.{event_name}\" must be an array"))?;

        if let Some(existing_idx) = arr.iter().position(is_opentaskbar_hook_entry) {
            arr[existing_idx] = desired_entry;
        } else {
            arr.push(desired_entry);
        }
    }

    serde_json::to_string_pretty(&root).map_err(|e| e.to_string())
}

fn is_opentaskbar_hook_entry(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|list| {
            list.iter().any(|item| {
                item.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|cmd| {
                        cmd.contains("agent-hook.ps1") || cmd.contains("OpenTaskbar")
                    })
            })
        })
}

fn build_relay_ps1(short_pipe_name: &str) -> String {
    let escaped_pipe = short_pipe_name.replace('\'', "''");
    format!(
        r#"param([string]$Event = '')
$ErrorActionPreference = 'SilentlyContinue'
try {{
    $raw = [Console]::In.ReadToEnd()
    if ([string]::IsNullOrWhiteSpace($raw)) {{ exit 0 }}
    $in = $raw | ConvertFrom-Json
    $evName = if ($Event) {{ $Event }} elseif ($in.hook_event_name) {{ [string]$in.hook_event_name }} else {{ 'step' }}
    $isApproval = ($evName -eq 'PreToolUse' -or $evName -eq 'PermissionRequest')
    $wireEvent = if ($isApproval) {{ 'approval_request' }} elseif ($evName -eq 'Stop') {{ 'done' }} elseif ($evName -eq 'Notification') {{ 'waiting_question' }} else {{ 'step' }}
    $reqId = if ($isApproval) {{ [guid]::NewGuid().ToString('N') }} else {{ $null }}
    $proj = if ($in.cwd) {{ Split-Path -Leaf ([string]$in.cwd) }} else {{ '' }}
    $hwnd = 0
    try {{
        $p = Get-Process -Id $PID -ErrorAction SilentlyContinue
        if ($p -and $p.MainWindowHandle -ne [IntPtr]::Zero) {{ $hwnd = [int64]$p.MainWindowHandle }}
    }} catch {{}}
    $payload = @{{
        event = $wireEvent
        agent = 'claude'
        session_id = if ($in.session_id) {{ [string]$in.session_id }} else {{ 'claude-default' }}
        project = $proj
        tool = if ($in.tool_name) {{ [string]$in.tool_name }} else {{ $evName }}
        summary = if ($in.message) {{ [string]$in.message }} elseif ($in.tool_name) {{ [string]$in.tool_name }} else {{ $evName }}
        request_id = $reqId
        action_title = if ($in.tool_name) {{ [string]$in.tool_name }} else {{ $evName }}
        detail = if ($in.tool_input) {{ ($in.tool_input | ConvertTo-Json -Compress -Depth 3) }} else {{ '' }}
        terminal_hwnd = $hwnd
    }} | ConvertTo-Json -Compress

    $pipe = [System.IO.Pipes.NamedPipeClientStream]::new('.', '{escaped_pipe}', [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect(150)
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    $writer = [System.IO.StreamWriter]::new($pipe, $utf8, 4096, $true)
    $writer.AutoFlush = $true
    $writer.WriteLine($payload)

    if ($isApproval) {{
        $reader = [System.IO.StreamReader]::new($pipe, $utf8, $false, 4096, $true)
        $respLine = $reader.ReadLine()
        if ($respLine) {{
            $resp = $respLine | ConvertFrom-Json
            $dec = [string]$resp.decision
            if ($dec -eq 'allow' -or $dec -eq 'always' -or $dec -eq 'deny') {{
                $mapped = if ($dec -eq 'always') {{ 'allow' }} else {{ $dec }}
                if ($evName -eq 'PermissionRequest') {{
                    @{{ hookSpecificOutput = @{{ hookEventName = 'PermissionRequest'; decision = @{{ behavior = $mapped }} }} }} | ConvertTo-Json -Compress -Depth 4
                }} else {{
                    @{{ hookSpecificOutput = @{{ hookEventName = 'PreToolUse'; permissionDecision = $mapped }} }} | ConvertTo-Json -Compress -Depth 4
                }}
            }}
        }}
    }}
    $pipe.Dispose()
}} catch {{
    exit 0
}}
exit 0
"#
    )
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new_manual_reset_event() -> Option<Self> {
        // SAFETY: creates an unnamed, non-signaled manual-reset Win32 event handle.
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(Self(handle))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: owned exclusively by this guard and closed once on drop.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct SharedEventHandle(HANDLE);
// SAFETY: Win32 event handles are kernel-synchronized for `SetEvent`/`WaitForMultipleObjects`
// and closed once when the final `Arc<SharedEventHandle>` is dropped.
unsafe impl Send for SharedEventHandle {}
unsafe impl Sync for SharedEventHandle {}

impl SharedEventHandle {
    fn new_manual_reset() -> Option<Self> {
        // SAFETY: creates an unnamed, non-signaled manual-reset Win32 event handle.
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(Self(handle))
        }
    }

    fn signal(&self) {
        // SAFETY: `self.0` stays valid for the lifetime of `self`.
        unsafe {
            let _ = SetEvent(self.0);
        }
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for SharedEventHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: closed once when the final `Arc<SharedEventHandle>` is dropped.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct LocalMemGuard(*mut c_void);

impl Drop for LocalMemGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: pointer was allocated via Win32 `LocalAlloc` (`ConvertSidToStringSidW` /
            // `ConvertStringSecurityDescriptorToSecurityDescriptorW`) and freed once via `LocalFree`.
            unsafe {
                let _ = LocalFree(self.0);
            }
        }
    }
}

struct PipeSecurityDescriptor {
    sd: LocalMemGuard,
}

impl PipeSecurityDescriptor {
    /// Builds a protected DACL restricting pipe access strictly to the current user SID + `SYSTEM`
    /// (falling back to Owner Rights `OW` + `SYSTEM` if the token SID cannot be queried).
    fn current_user_and_system() -> Option<Self> {
        let sddl = match current_process_user_sid_string() {
            Some(user_sid) => format!("D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)"),
            None => "D:P(A;;GA;;;OW)(A;;GA;;;SY)".to_owned(),
        };
        let wide_sddl = wide(&sddl);
        let mut sd_ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `wide_sddl` is NUL-terminated UTF-16; on success `sd_ptr` is wrapped in `LocalMemGuard`.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd_ptr,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || sd_ptr.is_null() {
            return None;
        }
        Some(Self {
            sd: LocalMemGuard(sd_ptr),
        })
    }

    fn security_attributes(&self) -> SecurityAttributes {
        SecurityAttributes {
            n_length: std::mem::size_of::<SecurityAttributes>() as u32,
            lp_security_descriptor: self.sd.0,
            b_inherit_handle: 0,
        }
    }
}

fn current_process_user_sid_string() -> Option<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess()` returns a pseudo-handle; `token` is closed via `OwnedHandle`.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 || token.is_null() {
        return None;
    }
    let _token_guard = OwnedHandle(token);

    let mut needed = 0u32;
    // SAFETY: first call queries the required byte length for `TokenUser`.
    unsafe {
        let _ = GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if (needed as usize) < std::mem::size_of::<TokenUser>() {
        return None;
    }

    // Align backing storage to pointer alignment before reading `TokenUser`.
    let word_count = (needed as usize).div_ceil(std::mem::size_of::<usize>());
    let mut buf = vec![0usize; word_count];
    let mut actual = 0u32;
    // SAFETY: `buf` is pointer-aligned and at least `needed` bytes wide.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            buf.as_mut_ptr().cast::<c_void>(),
            (word_count * std::mem::size_of::<usize>()) as u32,
            &mut actual,
        )
    };
    if ok == 0 {
        return None;
    }

    // SAFETY: `GetTokenInformation(TokenUser)` initializes `TokenUser` at offset 0 of `buf`.
    let sid = unsafe { (*buf.as_ptr().cast::<TokenUser>()).user.sid };
    if sid.is_null() {
        return None;
    }

    let mut sid_wide_ptr: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` points into `buf`; on success `sid_wide_ptr` is freed via `LocalMemGuard`.
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut sid_wide_ptr) };
    if ok == 0 || sid_wide_ptr.is_null() {
        return None;
    }
    let _sid_guard = LocalMemGuard(sid_wide_ptr.cast::<c_void>());

    // SAFETY: `ConvertSidToStringSidW` returns a NUL-terminated UTF-16 string.
    let mut len = 0usize;
    unsafe {
        while *sid_wide_ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(sid_wide_ptr, len);
        String::from_utf16(slice).ok()
    }
}

fn pipe_acceptor_loop(
    pipe_path: String,
    shared: Arc<Mutex<SharedInbox>>,
    running: Arc<AtomicBool>,
    active_clients: Arc<AtomicUsize>,
    stop_event: Arc<SharedEventHandle>,
) {
    let wide_name = wide(&pipe_path);
    let security = PipeSecurityDescriptor::current_user_and_system();

    while running.load(Ordering::Acquire) {
        let sa = security
            .as_ref()
            .map(PipeSecurityDescriptor::security_attributes);
        let sa_ptr = sa
            .as_ref()
            .map_or(std::ptr::null(), |attrs| std::ptr::from_ref(attrs).cast());

        // SAFETY: `wide_name` is NUL-terminated UTF-16; `sa_ptr` references a live `SECURITY_ATTRIBUTES`
        // whose descriptor is owned by `security` for the entire loop.
        let pipe = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                sa_ptr,
            )
        };
        if pipe == INVALID_HANDLE_VALUE || pipe.is_null() {
            let stop_handles = [stop_event.raw()];
            // SAFETY: interruptible backoff wakes immediately when `stop_event` is signaled.
            let wait = unsafe { WaitForMultipleObjects(1, stop_handles.as_ptr(), 0, 200) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
            continue;
        }

        let Some(connect_event) = OwnedHandle::new_manual_reset_event() else {
            unsafe {
                let _ = CloseHandle(pipe);
            }
            break;
        };

        // SAFETY: zero-initialized `OVERLAPPED` with valid manual-reset `connect_event`.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = connect_event.raw();

        // SAFETY: `pipe` and `overlapped` stay live until connect finishes or cancellation settles.
        let connect_ok =
            unsafe { ConnectNamedPipe(pipe, std::ptr::from_mut(&mut overlapped).cast()) != 0 };
        let connected = if connect_ok {
            true
        } else {
            let err = unsafe { GetLastError() };
            if err == ERROR_PIPE_CONNECTED {
                true
            } else if err == ERROR_IO_PENDING {
                let wait_handles = [stop_event.raw(), connect_event.raw()];
                // SAFETY: both event handles are valid for the duration of the wait.
                let wait = unsafe { WaitForMultipleObjects(2, wait_handles.as_ptr(), 0, INFINITE) };
                if wait == WAIT_OBJECT_0 {
                    // Stop event fired: cancel pending overlapped connect and wait for cancellation
                    // to settle before dropping stack-allocated `overlapped`.
                    let mut transferred = 0u32;
                    unsafe {
                        let _ = CancelIoEx(pipe, &overlapped);
                        let _ = GetOverlappedResult(pipe, &overlapped, &mut transferred, 1);
                        let _ = DisconnectNamedPipe(pipe);
                        let _ = CloseHandle(pipe);
                    }
                    drop(connect_event);
                    break;
                } else if wait == WAIT_OBJECT_0 + 1 {
                    let mut transferred = 0u32;
                    unsafe { GetOverlappedResult(pipe, &overlapped, &mut transferred, 0) != 0 }
                } else {
                    let mut transferred = 0u32;
                    unsafe {
                        let _ = CancelIoEx(pipe, &overlapped);
                        let _ = GetOverlappedResult(pipe, &overlapped, &mut transferred, 1);
                    }
                    false
                }
            } else {
                false
            }
        };

        drop(connect_event);

        if !running.load(Ordering::Acquire) || !connected {
            unsafe {
                let _ = DisconnectNamedPipe(pipe);
                let _ = CloseHandle(pipe);
            }
            if !running.load(Ordering::Acquire) {
                break;
            }
            continue;
        }

        if active_clients.load(Ordering::Acquire) >= MAX_INFLIGHT_PIPE_CLIENTS {
            unsafe {
                let _ = DisconnectNamedPipe(pipe);
                let _ = CloseHandle(pipe);
            }
            continue;
        }

        active_clients.fetch_add(1, Ordering::AcqRel);
        let pipe_handle = SendPipeHandle(pipe);
        let worker_shared = Arc::clone(&shared);
        let worker_running = Arc::clone(&running);
        let worker_clients = Arc::clone(&active_clients);
        let worker_stop = Arc::clone(&stop_event);
        if thread::Builder::new()
            .name("opentaskbar-agents-client".to_owned())
            .spawn(move || {
                handle_pipe_client(pipe_handle, &worker_shared, &worker_running, &worker_stop);
                worker_clients.fetch_sub(1, Ordering::AcqRel);
            })
            .is_err()
        {
            active_clients.fetch_sub(1, Ordering::AcqRel);
            unsafe {
                let _ = DisconnectNamedPipe(pipe);
                let _ = CloseHandle(pipe);
            }
        }
    }
}

struct SendPipeHandle(HANDLE);
// SAFETY: each connected pipe instance is uniquely owned by a single client worker thread until closed.
unsafe impl Send for SendPipeHandle {}

fn handle_pipe_client(
    pipe: SendPipeHandle,
    shared: &Arc<Mutex<SharedInbox>>,
    running: &Arc<AtomicBool>,
    stop_event: &SharedEventHandle,
) {
    let handle = pipe.0;
    if let Some(line) = read_pipe_line(handle, stop_event)
        && let Ok(event) = AgentHookEvent::from_json_str(&line)
    {
        if event.is_approval_request() {
            let req_id = event
                .request_id
                .clone()
                .unwrap_or_else(|| format!("req-{}", now_unix()));
            let request = AgentApprovalRequest {
                request_id: req_id.clone(),
                agent: event.agent.clone(),
                session_id: event.session_id.clone(),
                project: event.project.clone(),
                action_title: event
                    .action_title
                    .clone()
                    .or_else(|| event.tool.clone())
                    .unwrap_or_else(|| "Agent Action".to_owned()),
                detail: event
                    .detail
                    .clone()
                    .or_else(|| event.summary.clone())
                    .unwrap_or_default(),
                diff_preview: event.diff_preview.clone().unwrap_or_default(),
                terminal_hwnd: event.terminal_hwnd,
            };
            let (tx, rx) = mpsc::sync_channel::<String>(1);
            if let Ok(mut guard) = shared.lock() {
                guard.items.push_back(InboxItem::Approval {
                    event,
                    request,
                    reply_tx: tx,
                });
            }

            let decision = if running.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_secs(APPROVAL_TIMEOUT_SECS)) {
                    Ok(dec) => dec,
                    Err(RecvTimeoutError::Timeout) => {
                        if let Ok(mut guard) = shared.lock() {
                            guard.items.push_back(InboxItem::ApprovalTimedOut(req_id));
                        }
                        "terminal".to_owned()
                    }
                    Err(RecvTimeoutError::Disconnected) => "terminal".to_owned(),
                }
            } else {
                "terminal".to_owned()
            };

            let reply = format!("{}\n", json!({ "decision": decision }));
            write_all_pipe(handle, reply.as_bytes());
        } else if let Ok(mut guard) = shared.lock() {
            guard.items.push_back(InboxItem::Event(event));
        }
    }

    // SAFETY: `handle` is exclusively owned here; disconnect and close once.
    unsafe {
        let _ = FlushFileBuffers(handle);
        let _ = DisconnectNamedPipe(handle);
        let _ = CloseHandle(handle);
    }
}

fn read_pipe_line(handle: HANDLE, stop_event: &SharedEventHandle) -> Option<String> {
    let io_event = OwnedHandle::new_manual_reset_event()?;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    while buf.len() < MAX_PIPE_LINE_BYTES {
        // SAFETY: reset our manual-reset event before issuing the next overlapped `ReadFile`.
        unsafe {
            let _ = ResetEvent(io_event.raw());
        }
        // SAFETY: zero-initialized `OVERLAPPED` with valid manual-reset event.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = io_event.raw();
        let mut read = 0u32;

        // SAFETY: `chunk` and `overlapped` remain live until completion or cancellation settles.
        let ok = unsafe {
            ReadFile(
                handle,
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                std::ptr::null_mut(),
                &mut overlapped,
            )
        };
        if ok != 0 {
            let res = unsafe { GetOverlappedResult(handle, &overlapped, &mut read, 0) };
            if res == 0 && (unsafe { GetLastError() } != ERROR_MORE_DATA || read == 0) {
                break;
            }
        } else {
            let err = unsafe { GetLastError() };
            if err == ERROR_IO_PENDING {
                let wait_handles = [stop_event.raw(), io_event.raw()];
                let wait = unsafe { WaitForMultipleObjects(2, wait_handles.as_ptr(), 0, INFINITE) };
                if wait == WAIT_OBJECT_0 + 1 {
                    let res = unsafe { GetOverlappedResult(handle, &overlapped, &mut read, 0) };
                    if res == 0 && (unsafe { GetLastError() } != ERROR_MORE_DATA || read == 0) {
                        break;
                    }
                } else {
                    let mut cancelled = 0u32;
                    unsafe {
                        let _ = CancelIoEx(handle, &overlapped);
                        let _ = GetOverlappedResult(handle, &overlapped, &mut cancelled, 1);
                    }
                    break;
                }
            } else if err == ERROR_MORE_DATA {
                unsafe {
                    let _ = GetOverlappedResult(handle, &overlapped, &mut read, 0);
                }
                if read == 0 {
                    break;
                }
            } else {
                break;
            }
        }
        if read == 0 {
            break;
        }
        let slice = &chunk[..read as usize];
        if let Some(nl) = slice.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&slice[..nl]);
            break;
        }
        buf.extend_from_slice(slice);
    }
    if buf.is_empty() {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn write_all_pipe(handle: HANDLE, mut bytes: &[u8]) {
    let Some(io_event) = OwnedHandle::new_manual_reset_event() else {
        return;
    };
    while !bytes.is_empty() {
        unsafe {
            let _ = ResetEvent(io_event.raw());
        }
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = io_event.raw();
        let mut written = 0u32;
        // SAFETY: `bytes` and `overlapped` remain live until the overlapped `WriteFile` completes.
        let ok = unsafe {
            WriteFile(
                handle,
                bytes.as_ptr(),
                bytes.len() as u32,
                std::ptr::null_mut(),
                &mut overlapped,
            )
        };
        if ok != 0 {
            let res = unsafe { GetOverlappedResult(handle, &overlapped, &mut written, 0) };
            if res == 0 || written == 0 {
                break;
            }
        } else {
            let err = unsafe { GetLastError() };
            if err == ERROR_IO_PENDING {
                let res = unsafe { GetOverlappedResult(handle, &overlapped, &mut written, 1) };
                if res == 0 || written == 0 {
                    break;
                }
            } else {
                break;
            }
        }
        bytes = &bytes[written as usize..];
    }
}

fn push_bounded_step(steps: &mut VecDeque<AgentStep>, step: AgentStep) {
    if steps.len() >= MAX_STEPS_PER_SESSION {
        steps.pop_front();
    }
    steps.push_back(step);
}

fn str_field(obj: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        obj.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    })
}

fn project_name_from_path(path: &str) -> String {
    Path::new(path.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(path.trim())
        .to_owned()
}

fn normalize_agent_name(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        "custom".to_owned()
    } else {
        lower
    }
}

fn display_agent_name(agent: &str) -> &str {
    match agent.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => "Claude Code",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "antigravity" => "Antigravity",
        "opencode" => "OpenCode",
        "copilot" => "Copilot",
        _ => agent,
    }
}

/// Truncates `text` cleanly on a UTF-8 char boundary to at most `max_bytes` bytes.
fn truncate_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_step_event(session_id: &str, tool: &str, ts: u64) -> AgentHookEvent {
        AgentHookEvent::from_value(&json!({
            "event": "step",
            "agent": "claude",
            "session_id": session_id,
            "project": "OpenTaskbar",
            "tool": tool,
            "summary": format!("Running {tool}"),
            "diff_stats": "+10 -2",
            "timestamp_unix": ts
        }))
        .expect("valid step event")
    }

    #[test]
    fn json_event_ingestion_parses_and_updates_session() {
        let mut monitor = AgentMonitorState::disabled();
        let line = r#"{"event":"step","agent":"codex","session_id":"s1","cwd":"C:\\Work\\MyRepo","tool":"Edit","summary":"Updated flyout","diff_stats":"+12 -4","terminal_hwnd":12345,"timestamp_unix":1700000001}"#;
        let ev = AgentHookEvent::from_json_str(line).expect("must parse line");
        assert_eq!(ev.project, "MyRepo");
        assert_eq!(ev.agent, "codex");
        monitor.ingest_event(ev);

        let sessions = monitor.active_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, AgentSessionState::Working);
        assert_eq!(sessions[0].steps.len(), 1);
        assert_eq!(sessions[0].steps[0].tool, "Edit");
        assert_eq!(sessions[0].terminal_hwnd, 12345);
        assert_eq!(monitor.summary_badge(), "1");
    }

    #[test]
    fn ring_buffer_bounds_sessions_steps_and_diff_cap() {
        let mut monitor = AgentMonitorState::disabled();

        // Cap 1: max 8 active sessions.
        for i in 0..12 {
            let mut ev = make_step_event(&format!("sess-{i}"), "Bash", 100 + i as u64);
            if i < 4 {
                ev.state = Some(AgentSessionState::Done);
            }
            monitor.ingest_event(ev);
        }
        assert_eq!(monitor.active_sessions().len(), MAX_ACTIVE_SESSIONS);
        assert!(
            monitor
                .active_sessions()
                .iter()
                .all(|s| !s.session_id.starts_with("sess-0"))
        );

        // Cap 2: max 12 recent steps per session.
        for j in 0..18 {
            monitor.ingest_event(make_step_event("sess-11", &format!("Tool{j}"), 500 + j));
        }
        let target = monitor
            .active_sessions()
            .into_iter()
            .find(|s| s.session_id == "sess-11")
            .expect("session must exist");
        assert_eq!(target.steps.len(), MAX_STEPS_PER_SESSION);
        assert_eq!(target.steps.front().unwrap().tool, "Tool6");
        assert_eq!(target.steps.back().unwrap().tool, "Tool17");

        // Cap 3: diff_preview truncated to <= 8192 bytes cleanly on UTF-8 boundary.
        let big_diff = "é".repeat(5000); // 10,000 UTF-8 bytes
        let approval_ev = AgentHookEvent::from_value(&json!({
            "event": "approval_request",
            "agent": "claude",
            "session_id": "sess-11",
            "request_id": "req-big",
            "action_title": "Write large patch",
            "diff_preview": big_diff,
        }))
        .expect("approval event parses");
        let diff = approval_ev.diff_preview.as_ref().unwrap();
        assert!(diff.len() <= MAX_DIFF_PREVIEW_BYTES);
        assert!(std::str::from_utf8(diff.as_bytes()).is_ok());
    }

    #[test]
    fn approval_response_channel_round_trip() {
        let mut monitor = AgentMonitorState::disabled();
        monitor.ingest_event(make_step_event("s-approve", "Bash", 10));

        let (tx, rx) = mpsc::sync_channel::<String>(1);
        monitor.push_pending_approval(
            AgentApprovalRequest {
                request_id: "req-42".to_owned(),
                agent: "claude".to_owned(),
                session_id: "s-approve".to_owned(),
                project: "OpenTaskbar".to_owned(),
                action_title: "Run cargo test".to_owned(),
                detail: "Execute test suite".to_owned(),
                diff_preview: "diff --git a/x b/x".to_owned(),
                terminal_hwnd: 0,
            },
            tx,
        );

        assert_eq!(monitor.summary_badge(), "1!");
        assert_eq!(
            monitor.pending_approval().map(|r| r.request_id.as_str()),
            Some("req-42")
        );
        let detail = monitor.build_island_detail();
        assert_eq!(detail.actions[0].id, "approve_allow:req-42");

        assert!(monitor.handle_flyout_action("approve_allow:req-42"));
        assert_eq!(rx.try_recv().ok().as_deref(), Some("allow"));
        assert!(monitor.pending_approval().is_none());
    }

    #[test]
    fn pipe_name_sanitizes_username() {
        assert_eq!(sanitize_username("Ali.Ozturk+Dev"), "ali_ozturk_dev");
        assert_eq!(sanitize_username("   "), "user");
        assert!(pipe_name().starts_with(r"\\.\pipe\opentaskbar-agents-"));
    }

    #[test]
    fn claude_settings_hook_merge_is_idempotent_and_preserves_existing() {
        let initial = r#"{
  "model": "claude-opus",
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Read",
        "hooks": [{ "type": "command", "command": "echo existing" }]
      }
    ]
  }
}"#;
        let script = Path::new(r"C:\Users\Test\AppData\Local\OpenTaskbar\agent-hook.ps1");
        let first = merge_claude_settings_json(initial, script).expect("first merge succeeds");
        let second = merge_claude_settings_json(&first, script).expect("second merge succeeds");
        assert_eq!(first, second);

        let parsed: Value = serde_json::from_str(&second).expect("valid merged json");
        assert_eq!(parsed["model"], "claude-opus");
        let pre = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["matcher"], "Read");
    }

    #[test]
    fn start_and_drop_rapidly_never_deadlocks() {
        let pid = std::process::id();
        let started = Instant::now();
        for iter in 0..10u32 {
            let path = format!(r"\\.\pipe\opentaskbar-agents-stress-{pid}-{iter}");
            let monitor = AgentMonitorState::start_with_pipe(path);
            assert!(monitor.is_enabled());
            if iter % 3 == 1 {
                thread::sleep(Duration::from_millis(2));
            }
            drop(monitor);
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "rapid start/drop took too long ({:?}), indicating possible pipe shutdown stall",
            started.elapsed()
        );
    }

    #[test]
    fn overlapped_pipe_with_user_dacl_exchanges_event_and_approval() {
        use std::fs::OpenOptions;
        use std::io::{BufRead, BufReader, Write};

        let pid = std::process::id();
        let path = format!(r"\\.\pipe\opentaskbar-agents-e2e-{pid}");
        let mut monitor = AgentMonitorState::start_with_pipe(path.clone());

        // Spawn a client that connects as the current user to the DACL-hardened overlapped pipe,
        // sends an approval request, and verifies it receives the JSON decision back.
        let client_path = path.clone();
        let client = thread::spawn(move || -> Result<String, String> {
            let mut pipe = None;
            for _ in 0..50 {
                match OpenOptions::new().read(true).write(true).open(&client_path) {
                    Ok(f) => {
                        pipe = Some(f);
                        break;
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
            let mut file = pipe.ok_or_else(|| "client failed to open named pipe".to_owned())?;
            let req = r#"{"event":"approval_request","agent":"claude","session_id":"e2e-1","project":"OpenTaskbar","request_id":"req-e2e","action_title":"Run tests","detail":"cargo test"}"#;
            writeln!(file, "{req}").map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;

            let mut line = String::new();
            let mut reader = BufReader::new(file);
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            Ok(line)
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut responded = false;
        while Instant::now() < deadline {
            if monitor.poll_events() && monitor.pending_approval().is_some() {
                assert!(monitor.respond_approval("req-e2e", "allow"));
                responded = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            responded,
            "monitor did not receive approval request over overlapped pipe"
        );

        let reply = client
            .join()
            .expect("client thread must not panic")
            .expect("client must exchange JSON over DACL-hardened pipe");
        let parsed: Value = serde_json::from_str(reply.trim()).expect("valid JSON reply");
        assert_eq!(parsed["decision"], "allow");
        drop(monitor);
    }
}
