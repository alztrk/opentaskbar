//! User-created widgets.
//!
//! A custom widget is a folder under `%LOCALAPPDATA%\OpenTaskbar\Widgets\<slug>\`
//! containing `widget.json` and a small program. OpenTaskbar launches that program
//! as a sidecar and speaks newline-delimited JSON on stdin/stdout. Every request
//! carries a protocol version and sequence number:
//!
//! ```text
//! host   -> {"cmd":"poll","protocol":1,"seq":4}
//! widget -> {"protocol":1,"seq":4,"chip":{...},"flyout":{...}}
//! host   -> {"cmd":"secret","name":"widget:<your-slug>:token","protocol":1,"seq":5}
//! widget -> {"protocol":1,"seq":5,"value":"..."}
//!
//! A sidecar may only request secrets under its own `widget:<slug>:` namespace; every other
//! name (including AI provider keys such as `api-key:deepseek`) receives an empty value.
//! host   -> {"cmd":"click","id":7,"protocol":1,"seq":6}
//! widget -> {"protocol":1,"seq":6,"action":"openUrl","target":"https://..."}
//! ```
//!
//! The host never learns HTTP, JSONPath or credentials: the widget owns its data
//! source. A sidecar runs in a kill-on-close Job Object, is approved once by the
//! user (manifest hash pinned), and can never block the UI thread.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::fmt::Write as FmtWrite;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentaskbar_core::i18n as loc;
use opentaskbar_core::paths;
use opentaskbar_core::settings::{DockSettings, load_from, save_to, settings_path};
use serde_json::Value;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::OpenProcess;

use crate::design::widget_pack::parse_json_lenient;
use crate::dock::dcomp::WidgetIcon;
use crate::platform::{pins, toast};
use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

use super::catalog_ops::setting_value;
use super::registry::{HardwareNeed, ShrinkPriority, WIDGET_CATEGORIES, WidgetDescriptor};
use super::widget_icons::{resolve_widget_icon, safe_id};

pub const ID_PREFIX: &str = "custom:";
pub const PROTOCOL_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "widget.json";
const MAX_WIDGETS: usize = 64;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MIN_POLL_SECS: u32 = 5;
const MAX_POLL_SECS: u32 = 86_400;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_BACKOFF_SECS: u64 = 60;
/// A single NDJSON reply must fit in this budget; oversized lines are rejected.
const MAX_REPLY_LINE_BYTES: usize = 512 * 1024;
/// In-band secret requests have their own budget, independent of reply attempts.
const MAX_SECRET_REQUESTS: u32 = 8;
const MAX_REPLY_ATTEMPTS: u32 = 4;
const MAX_PENDING_REQUESTS: usize = 16;
const MAX_BLOCKS: usize = 64;
const MAX_SAMPLES: usize = 512;
const MAX_DETAILS: usize = 64;

// Win32 Job Object. `windows-sys` 0.61 does not expose these APIs, so declare
// the small kernel32 surface we need directly.
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const JOB_OBJECT_BASIC_LIMIT_INFORMATION_CLASS: u32 = 2;
#[repr(C)]
#[derive(Default)]
struct JobBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> HANDLE;
    fn SetInformationJobObject(job: HANDLE, class: u32, info: *const c_void, info_len: u32) -> i32;
    fn AssignProcessToJobObject(job: HANDLE, process: HANDLE) -> i32;
    fn TerminateJobObject(job: HANDLE, exit_code: u32) -> i32;
}

/// Job handles are only touched while holding `SidecarState`'s mutex; the newtype exists so
/// the raw Win32 pointer can cross thread boundaries with the rest of that state.
#[derive(Clone, Copy)]
struct JobHandle(HANDLE);

// SAFETY: every use of the raw handle is serialized by `Sidecar::state` and the handle is
// closed exactly once (`terminate` uses `job.take()`).
unsafe impl Send for JobHandle {}

fn close_job(job: HANDLE) {
    if !job.is_null() {
        unsafe {
            let _ = CloseHandle(job);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CustomWidgetSpec {
    /// Full persisted id, e.g. `custom:usd-try`.
    pub id: String,
    /// Folder name and path-safe slug.
    pub slug: String,
    /// Default display name (also used as the i18n fallback key).
    pub name: String,
    /// Locale → localized name, taken from the manifest.
    pub names: HashMap<String, String>,
    pub category: &'static str,
    pub width: f32,
    pub poll_seconds: u32,
    pub exec: Vec<String>,
    /// Absolute folder of the manifest; also used when loading bundled assets.
    pub root: PathBuf,
    /// SHA-256 of the manifest text (hex). Trust is pinned to this hash.
    pub manifest_hash: String,
}

#[derive(Clone, Debug, Default)]
pub struct CustomChip {
    pub label: String,
    pub value: String,
    pub ratio: Option<f32>,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct CustomSnapshot {
    chip: Option<CustomChip>,
    flyout: Option<FlyoutSpec>,
    details: Vec<(String, String)>,
    error: Option<String>,
    at: Option<Instant>,
}

struct Pipes {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct SidecarState {
    child: Option<Child>,
    job: Option<JobHandle>,
    /// Pipes are taken out while a request is in flight so the UI thread can
    /// never block on a mutex held across a blocking `read_line`.
    pipes: Option<Pipes>,
}

struct Sidecar {
    state: Mutex<SidecarState>,
    snapshot: RwLock<CustomSnapshot>,
    in_flight: AtomicBool,
    seq: AtomicU64,
    /// Incremented whenever a request is invalidated (timeout, manifest change, removal).
    /// Replies from an older generation are discarded instead of overwriting a newer snapshot.
    generation: AtomicU64,
    failures: AtomicU32,
    /// The `seq` of the active request, or 0 when idle. The watchdog compares this token so a
    /// newly started request cannot be killed by a watchdog belonging to the previous one.
    active_token: AtomicU64,
    /// Unix millis of the last completed attempt (for backoff).
    attempt_ms: AtomicU64,
    /// Poll/click requests waiting for the shared single worker slot.
    pending: Mutex<VecDeque<(String, CustomWidgetSpec, RequestKind)>>,
}

impl Sidecar {
    fn new() -> Self {
        Self {
            state: Mutex::new(SidecarState {
                child: None,
                job: None,
                pipes: None,
            }),
            snapshot: RwLock::new(CustomSnapshot::default()),
            in_flight: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            failures: AtomicU32::new(0),
            active_token: AtomicU64::new(0),
            attempt_ms: AtomicU64::new(0),
            pending: Mutex::new(VecDeque::new()),
        }
    }

    /// Kills the current child and discards every reply that belongs to it. `in_flight` is
    /// deliberately left alone: only the request's RAII guard may clear it.
    fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.active_token.store(0, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            terminate(&mut state);
        }
        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = CustomSnapshot::default();
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
        self.failures.store(0, Ordering::Relaxed);
        self.attempt_ms.store(0, Ordering::Relaxed);
    }
}

/// Clears `in_flight` when a request worker exits — the only writer of that flag.
struct InFlightGuard(Arc<Sidecar>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.in_flight.store(false, Ordering::Release);
        // Hand the worker slot to the next queued request (if any) without blocking.
        let next = self
            .0
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.pop_front());
        if let Some((id, spec, kind)) = next {
            dispatch(&self.0, id, spec, kind);
        }
    }
}

/// Kind of a queued sidecar request. Poll replies update the shared snapshot; click replies
/// only trigger their action and never touch poll state.
#[derive(Clone, Copy)]
enum RequestKind {
    Poll,
    Click(u32),
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            terminate(&mut state);
        }
    }
}

#[derive(Default)]
struct CustomRegistry {
    descriptors: Arc<Vec<&'static WidgetDescriptor>>,
    specs: HashMap<String, CustomWidgetSpec>,
    /// True once the widget folder has been scanned; an empty folder is not scanned again
    /// until an explicit `reload()` (the 1 Hz runtime tick used to re-scan forever).
    loaded: bool,
}

static REGISTRY: LazyLock<RwLock<CustomRegistry>> =
    LazyLock::new(|| RwLock::new(CustomRegistry::default()));
static SIDECARS: LazyLock<Mutex<HashMap<String, Arc<Sidecar>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Root folder for user-created widgets.
pub fn root_dir() -> PathBuf {
    paths::shared_app_data_dir().join("Widgets")
}

/// Absolute folder of a widget, exposed for the Settings "open/edit/remove" actions.
pub fn widget_dir(id: &str) -> Option<PathBuf> {
    spec(id).map(|s| s.root)
}

/// Keys of the manifest's `name` object in JSON source order. `serde_json::Map` is ordered,
/// so `values().next()` would silently pick an arbitrary locale instead of the documented
/// "first entry" fallback.
fn name_key_order(text: &str) -> Vec<String> {
    let Some(start) = text.find("\"name\"") else {
        return Vec::new();
    };
    let rest = &text[start + "\"name\"".len()..];
    let Some(open) = rest.find('{') else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut expect_key = false;
    let mut current = String::new();
    for ch in rest[open..].chars() {
        if in_string {
            if ch == '"' {
                in_string = false;
                if expect_key && depth == 1 {
                    keys.push(std::mem::take(&mut current));
                    expect_key = false;
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                current.clear();
            }
            '{' => {
                depth += 1;
                if depth == 1 {
                    expect_key = true;
                }
            }
            '}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 1 => expect_key = true,
            ':' => expect_key = false,
            _ => {}
        }
    }
    keys
}

fn parse_names(value: &Value, text: &str) -> Option<(String, HashMap<String, String>)> {
    match value.get("name")? {
        Value::String(text) => {
            let name = text.trim().to_owned();
            (!name.is_empty() && name.len() <= 128).then(|| (name, HashMap::new()))
        }
        Value::Object(map) => {
            let mut names = HashMap::new();
            for (culture, translated) in map {
                if let Some(text) = translated.as_str() {
                    let text = text.trim();
                    if !text.is_empty() && text.len() <= 128 {
                        names.insert(culture.clone(), text.to_owned());
                    }
                }
            }
            let order = name_key_order(text);
            let default = names
                .get("en-US")
                .or_else(|| names.get("en"))
                .or_else(|| order.iter().find_map(|key| names.get(key)))
                .cloned()?;
            Some((default, names))
        }
        _ => None,
    }
}

fn parse_manifest(
    value: &Value,
    text: &str,
    slug: &str,
    root: &Path,
    hash: &str,
) -> Option<CustomWidgetSpec> {
    let object = value.as_object()?;
    let (name, names) = parse_names(value, text)?;
    let category = object
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("Productivity");
    let category = WIDGET_CATEGORIES
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(category))
        .unwrap_or("Productivity");
    let width = object
        .get("width")
        .and_then(Value::as_f64)
        .unwrap_or(120.0)
        .clamp(48.0, 512.0) as f32;
    let poll_seconds = object
        .get("pollSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(MIN_POLL_SECS as u64, MAX_POLL_SECS as u64) as u32;
    let exec: Vec<String> = match object.get("exec")? {
        Value::String(text) => text.split_whitespace().map(str::to_owned).collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => return None,
    };
    if exec.is_empty() || exec.iter().any(|part| part.is_empty() || part.len() > 512) {
        return None;
    }
    Some(CustomWidgetSpec {
        id: format!("{ID_PREFIX}{slug}"),
        slug: slug.to_owned(),
        name,
        names,
        category,
        width,
        poll_seconds,
        exec,
        root: root.to_path_buf(),
        manifest_hash: hash.to_owned(),
    })
}

fn descriptor_for(spec: &CustomWidgetSpec) -> WidgetDescriptor {
    WidgetDescriptor {
        id: leak(spec.id.clone()),
        name: leak(spec.name.clone()),
        name_key: leak(spec.name.clone()),
        category: spec.category,
        width_dips: spec.width,
        catalog: true,
        dock_descriptor: true,
        virtual_widget: false,
        has_flyout: true,
        has_settings: false,
        live_preview: true,
        shrink_priority: ShrinkPriority::Normal,
        hardware: HardwareNeed::None,
    }
}

/// Manifest hash the user approved for this widget, if any.
pub fn approved_hash(settings: &DockSettings, id: &str) -> Option<String> {
    setting_value(settings, "CustomWidgetConsent")?
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(id))
        .and_then(|(_, v)| v.as_str())
        .map(str::to_owned)
}

/// Whether the currently loaded manifest is still the one the user approved.
pub fn is_approved(id: &str, settings: &DockSettings) -> bool {
    spec(id).is_some_and(|s| {
        approved_hash(settings, id).is_some_and(|approved| approved == s.manifest_hash)
    })
}

/// Records approval for the widget's current manifest hash.
pub fn approve(settings: &mut DockSettings, id: &str) -> bool {
    let Some(spec) = spec(id) else {
        return false;
    };
    let mut consent = setting_value(settings, "CustomWidgetConsent")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    consent.insert(id.to_owned(), Value::String(spec.manifest_hash.clone()));
    super::catalog_ops::set_setting(settings, "CustomWidgetConsent", Value::Object(consent));
    true
}

/// Scans the widget folder and replaces the in-memory registry. Unchanged
/// manifests reuse their existing leaked descriptors, so repeated Reloads do
/// not leak.
/// Descriptor fields that affect rendering; the manifest hash is deliberately not among them.
fn descriptor_matches(spec: &CustomWidgetSpec, descriptor: &WidgetDescriptor) -> bool {
    descriptor.id == spec.id
        && descriptor.name == spec.name
        && descriptor.category == spec.category
        && (descriptor.width_dips - spec.width).abs() < f32::EPSILON
}

pub fn reload() -> usize {
    let mut specs = HashMap::new();
    let mut descriptors = Vec::new();
    let old = REGISTRY.read().map(|r| r.specs.clone()).unwrap_or_default();
    let old_descriptors = REGISTRY
        .read()
        .map(|r| Arc::clone(&r.descriptors))
        .unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(root_dir()) {
        for entry in entries.flatten() {
            if specs.len() >= MAX_WIDGETS {
                break;
            }
            let Some(slug) = safe_id(&entry.file_name().to_string_lossy()).map(str::to_owned)
            else {
                continue;
            };
            let dir = entry.path();
            let path = dir.join(MANIFEST_FILE);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text.len() as u64 > MAX_MANIFEST_BYTES {
                continue;
            }
            let hash = hex_hash(text.as_bytes());
            let Ok(value) = parse_json_lenient(&text) else {
                continue;
            };
            let Some(parsed) = parse_manifest(&value, &text, &slug, &dir, &hash) else {
                continue;
            };
            let id = parsed.id.clone();
            // Reuse the leaked descriptor for unchanged metadata; keep the *newly resolved*
            // root so a moved widget folder no longer points at the old directory.
            descriptors.push(
                old_descriptors
                    .iter()
                    .find(|d| descriptor_matches(&parsed, d))
                    .copied()
                    .unwrap_or_else(|| Box::leak(Box::new(descriptor_for(&parsed)))),
            );
            specs.insert(id, parsed);
        }
    }
    let count = specs.len();
    // A changed hash means the user must approve new code. Kill the old child now so approval
    // cannot silently keep running the previous exec (and clear its snapshot/backoff/in_flight).
    let restarted: Vec<String> = specs
        .iter()
        .filter(|(_, spec)| hash_changed(&old, spec))
        .map(|(id, _)| id.clone())
        .collect();
    *REGISTRY.write().expect("custom widget registry poisoned") = CustomRegistry {
        descriptors: Arc::new(descriptors),
        specs,
        loaded: true,
    };
    super::widget_icons::invalidate();
    if let Ok(mut sidecars) = SIDECARS.lock() {
        sidecars.retain(|id, _| REGISTRY.read().is_ok_and(|r| r.specs.contains_key(id)));
        for id in restarted {
            if let Some(sidecar) = sidecars.get(&id) {
                sidecar.invalidate();
            }
        }
    }
    count
}

fn hex_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// True when a manifest change requires killing and restarting this widget's sidecar.
fn hash_changed(old: &HashMap<String, CustomWidgetSpec>, new: &CustomWidgetSpec) -> bool {
    old.get(&new.id)
        .is_some_and(|prev| prev.manifest_hash != new.manifest_hash)
}

fn ensure_loaded() {
    let needs_scan = REGISTRY.read().is_ok_and(|r| !r.loaded);
    if needs_scan
        && let Ok(guard) = REGISTRY.write()
        && !guard.loaded
    {
        drop(guard);
        reload();
    }
}

/// Arc snapshot of descriptors, for callers that keep the list alive.
pub fn descriptors_arc() -> Arc<Vec<&'static WidgetDescriptor>> {
    ensure_loaded();
    REGISTRY
        .read()
        .map(|r| Arc::clone(&r.descriptors))
        .unwrap_or_default()
}

/// Descriptor list for the catalog and dock layout (cheap pointer-vector clone).
pub fn descriptors() -> Vec<&'static WidgetDescriptor> {
    descriptors_arc().as_ref().clone()
}

pub fn spec(id: &str) -> Option<CustomWidgetSpec> {
    ensure_loaded();
    REGISTRY.read().ok().and_then(|r| r.specs.get(id).cloned())
}

pub fn is_custom(id: &str) -> bool {
    // `get(..n)` returns None instead of panicking when byte n splits a UTF-8 character.
    id.len() > ID_PREFIX.len()
        && id
            .get(..ID_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(ID_PREFIX))
}

/// Localized display name: per-locale manifest names win, then English, then id.
pub fn display_name(id: &str) -> String {
    spec(id).map_or_else(
        || id.to_owned(),
        |s| {
            let language = loc::language();
            s.names
                .get(language)
                .cloned()
                .or_else(|| {
                    language
                        .split('-')
                        .next()
                        .and_then(|short| s.names.get(short))
                        .cloned()
                })
                .unwrap_or_else(|| {
                    if s.name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-')
                    {
                        loc::t(&s.name)
                    } else {
                        s.name.clone()
                    }
                })
        },
    )
}

fn sidecar(id: &str) -> Arc<Sidecar> {
    SIDECARS
        .lock()
        .expect("sidecar registry poisoned")
        .entry(id.to_owned())
        .or_insert_with(|| Arc::new(Sidecar::new()))
        .clone()
}

fn terminate(state: &mut SidecarState) {
    if let Some(job) = state.job.take() {
        unsafe {
            TerminateJobObject(job.0, 1);
            close_job(job.0);
        }
    }
    if let Some(child) = state.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    state.child = None;
    state.pipes = None;
}

fn create_job(child_id: u32) -> Option<JobHandle> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let info = JobBasicLimitInformation {
            limit_flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            ..Default::default()
        };
        let size = size_of::<JobBasicLimitInformation>() as u32;
        if SetInformationJobObject(
            job,
            JOB_OBJECT_BASIC_LIMIT_INFORMATION_CLASS,
            &raw const info as *const c_void,
            size,
        ) == 0
        {
            close_job(job);
            return None;
        }
        const PROCESS_TERMINATE: u32 = 0x0001;
        const PROCESS_SET_QUOTA: u32 = 0x0100;
        let process = OpenProcess(PROCESS_TERMINATE | PROCESS_SET_QUOTA, 0, child_id);
        if process.is_null() || AssignProcessToJobObject(job, process) == 0 {
            if !process.is_null() {
                let _ = CloseHandle(process);
            }
            close_job(job);
            return None;
        }
        let _ = CloseHandle(process);
        Some(JobHandle(job))
    }
}

fn ensure_child(id: &str, spec: &CustomWidgetSpec, state: &mut SidecarState) -> Result<(), String> {
    if let Some(child) = state.child.as_mut()
        && child.try_wait().ok().flatten().is_none()
    {
        return Ok(());
    }
    terminate(state);
    // Relative program paths must be resolved against the widget folder: on
    // Windows CreateProcess resolves a relative application name against the
    // *parent's* cwd, not `lpCurrentDirectory`.
    let program = Path::new(&spec.exec[0]);
    let program = if program.is_absolute() {
        program.to_path_buf()
    } else if program.components().count() > 1 || spec.root.join(program).is_file() {
        // Explicit relative path: resolve against the widget folder, because
        // CreateProcess resolves it against the *parent's* cwd on Windows.
        spec.root.join(program)
    } else {
        // A bare program name is resolved through PATH (powershell.exe, python, …).
        program.to_path_buf()
    };
    let mut command = Command::new(program);
    command
        .args(&spec.exec[1..])
        .current_dir(&spec.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let child = command.spawn().map_err(|e| e.to_string())?;
    let job = create_job(child.id());
    // Register the child + job *before* any fallible pipe take so an error path can still
    // terminate the process and close the job handle.
    state.child = Some(child);
    state.job = job;
    let stdin = state
        .child
        .as_mut()
        .and_then(|child| child.stdin.take())
        .ok_or_else(|| {
            terminate(state);
            "sidecar stdin unavailable".to_owned()
        })?;
    let stdout = state
        .child
        .as_mut()
        .and_then(|child| child.stdout.take())
        .ok_or_else(|| {
            terminate(state);
            "sidecar stdout unavailable".to_owned()
        })?;
    if let Some(stderr) = state.child.as_mut().and_then(|child| child.stderr.take()) {
        let widget_id = id.to_owned();
        let _ = std::thread::Builder::new()
            .name(format!("otb-widget-stderr-{id}"))
            .spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    opentaskbar_core::log::error(format!("[widget:{widget_id}] {}", line.trim()));
                    line.clear();
                }
            });
    }
    state.pipes = Some(Pipes {
        stdin,
        stdout: BufReader::new(stdout),
    });
    Ok(())
}

/// Reads one bounded NDJSON line. An oversized / unterminated line is an error, never an
/// unbounded allocation driven by a misbehaving sidecar.
fn read_reply_line(stdout: &mut BufReader<ChildStdout>) -> Result<String, String> {
    let mut limited = stdout.take(MAX_REPLY_LINE_BYTES as u64);
    let mut bytes = Vec::new();
    let read = limited
        .read_until(b'\n', &mut bytes)
        .map_err(|e| e.to_string())?;
    if read == 0 {
        return Err("sidecar closed stdout".to_owned());
    }
    if !bytes.ends_with(b"\n") {
        return Err("sidecar reply line too long".to_owned());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A reply must carry the current protocol version *and* the exact request seq. Missing
/// fields are rejected, as documented in the sidecar protocol.
fn reply_matches(value: &Value, seq: u64) -> bool {
    value.get("protocol").and_then(Value::as_u64) == Some(u64::from(PROTOCOL_VERSION))
        && value.get("seq").and_then(Value::as_u64) == Some(seq)
}

fn request(
    id: &str,
    spec: &CustomWidgetSpec,
    sidecar: &Sidecar,
    command: &Value,
    seq: u64,
) -> Result<Value, String> {
    let mut line = command.clone();
    let object = line.as_object_mut().ok_or("command must be an object")?;
    object.insert("protocol".to_owned(), Value::from(PROTOCOL_VERSION));
    object.insert("seq".to_owned(), Value::from(seq));

    // Take the pipes out of the shared state; the lock is released before I/O.
    let mut pipes = {
        let mut state = sidecar
            .state
            .lock()
            .map_err(|_| "sidecar lock poisoned".to_owned())?;
        ensure_child(id, spec, &mut state)?;
        state.pipes.take().ok_or("sidecar busy")?
    };

    let result = (|| -> Result<Value, String> {
        let text = serde_json::to_string(&line).map_err(|e| e.to_string())?;
        writeln!(pipes.stdin, "{text}").map_err(|e| e.to_string())?;
        pipes.stdin.flush().map_err(|e| e.to_string())?;
        let mut secrets = 0u32;
        let mut replies = 0u32;
        let mut lines = 0u32;
        // Secret requests have their own budget: a widget asking for eight secrets still
        // gets its matching reply, and a spammer cannot keep the worker alive forever.
        while lines < MAX_SECRET_REQUESTS + MAX_REPLY_ATTEMPTS {
            lines += 1;
            let response = read_reply_line(&mut pipes.stdout)?;
            let value: Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
            // A sidecar may ask for a DPAPI secret in-band; answer it without
            // ever putting the value on a command line or in the log. Out-of-scope names
            // still get a `secretResult` — with an empty value.
            if value.get("cmd").and_then(Value::as_str) == Some("secret") {
                let name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let secret = if secrets < MAX_SECRET_REQUESTS {
                    secrets += 1;
                    crate::widgets::telemetry::read_dpapi_secret_for_widget(&name, &spec.slug)
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let answer = serde_json::json!({
                    "cmd": "secretResult",
                    "name": name,
                    "value": secret,
                    "seq": value.get("seq").and_then(Value::as_u64).unwrap_or(seq),
                    "protocol": PROTOCOL_VERSION,
                });
                let answer_text = serde_json::to_string(&answer).map_err(|e| e.to_string())?;
                writeln!(pipes.stdin, "{answer_text}").map_err(|e| e.to_string())?;
                pipes.stdin.flush().map_err(|e| e.to_string())?;
                continue;
            }
            replies += 1;
            if reply_matches(&value, seq) {
                return Ok(value);
            }
            if replies >= MAX_REPLY_ATTEMPTS {
                break;
            }
        }
        Err("no matching sidecar reply".to_owned())
    })();

    if result.is_err()
        && let Ok(mut state) = sidecar.state.lock()
    {
        terminate(&mut state);
    } else if let Ok(mut state) = sidecar.state.lock() {
        state.pipes = Some(pipes);
    }
    result
}

/// Kills a request that is still active after `IO_TIMEOUT`, without relying on another poll.
/// `in_flight` stays set: the worker's RAII guard clears it when the killed child unblocks it.
fn spawn_watchdog(sidecar: &Arc<Sidecar>, token: u64) {
    let handle = Arc::clone(sidecar);
    let _ = std::thread::Builder::new()
        .name(format!("otb-widget-watchdog-{token}"))
        .spawn(move || {
            std::thread::sleep(IO_TIMEOUT);
            if handle
                .active_token
                .compare_exchange(token, 0, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                if let Ok(mut state) = handle.state.lock() {
                    terminate(&mut state);
                }
                handle.generation.fetch_add(1, Ordering::AcqRel);
                handle.failures.fetch_add(1, Ordering::Relaxed);
                handle.attempt_ms.store(now_ms(), Ordering::Relaxed);
            }
        });
}

/// Shared dispatcher for poll and click requests. Both kinds pass through the single
/// `in_flight` gate, so a click can never steal the poll's pipes or write a "sidecar busy"
/// error into the poll snapshot. Click replies never touch snapshot/backoff state.
fn dispatch(sidecar: &Arc<Sidecar>, id: String, spec: CustomWidgetSpec, kind: RequestKind) {
    if sidecar.in_flight.swap(true, Ordering::AcqRel) {
        // Busy: queue the request instead of racing for the pipes or logging a bogus
        // "sidecar busy" error into the poll snapshot.
        if let Ok(mut pending) = sidecar.pending.lock()
            && pending.len() < MAX_PENDING_REQUESTS
        {
            pending.push_back((id, spec, kind));
        }
        return;
    }
    let guard = InFlightGuard(Arc::clone(sidecar));
    let generation = sidecar.generation.load(Ordering::Acquire);
    let seq = sidecar.seq.fetch_add(1, Ordering::Relaxed) + 1;
    sidecar.active_token.store(seq, Ordering::Release);
    spawn_watchdog(sidecar, seq);
    let command = match &kind {
        RequestKind::Poll => serde_json::json!({"cmd": "poll"}),
        RequestKind::Click(action_id) => serde_json::json!({"cmd": "click", "id": action_id}),
    };
    let worker_sidecar = Arc::clone(sidecar);
    let spawned = std::thread::Builder::new()
        .name(format!("otb-widget-{id}-{seq}"))
        .spawn(move || {
            let _guard = guard;
            let reply = request(&id, &spec, &worker_sidecar, &command, seq);
            worker_sidecar.active_token.store(0, Ordering::Release);
            match kind {
                RequestKind::Poll => {
                    apply_reply(&id, reply, &worker_sidecar, generation);
                }
                RequestKind::Click(_) => {
                    // A late reply from a killed worker must not trigger an action either.
                    if worker_sidecar.generation.load(Ordering::Acquire) == generation
                        && let Some(value) = reply.ok()
                    {
                        apply_action(&id, &value);
                    }
                }
            }
        })
        .is_ok();
    if !spawned {
        sidecar.active_token.store(0, Ordering::Release);
    }
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt_str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn num_field(value: &Value, key: &str, default: f64) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn u32_field(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

fn u16_field(value: &Value, key: &str, default: u16) -> u16 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(default)
}

fn ratio_field(value: &Value, key: &str) -> f32 {
    num_field(value, key, 0.0).clamp(0.0, 1.0) as f32
}

fn chip_from_value(value: &Value) -> Option<CustomChip> {
    let chip = value.get("chip")?;
    Some(CustomChip {
        label: str_field(chip, "label"),
        value: str_field(chip, "value"),
        ratio: chip
            .get("ratio")
            .and_then(Value::as_f64)
            .map(|r| r.clamp(0.0, 1.0) as f32),
        icon: opt_str_field(chip, "icon"),
    })
}

fn block_from_value(value: &Value) -> Option<FlyoutBlock> {
    let kind = value.get("kind")?.as_str()?;
    let text = |key: &str| str_field(value, key);
    match kind {
        "stat" => Some(FlyoutBlock::StatRow {
            label: text("label"),
            value: text("value"),
        }),
        "note" => Some(FlyoutBlock::Note(text("text"))),
        "hero" => Some(FlyoutBlock::Hero {
            value: text("value"),
            caption: opt_str_field(value, "caption"),
            centered: true,
            large: false,
            right_meta: None,
        }),
        "progress" => Some(FlyoutBlock::ProgressBar {
            label: text("label"),
            value_text: text("value"),
            ratio: ratio_field(value, "ratio"),
            tone: BarTone::Accent,
        }),
        "metric" => {
            let tiles = value
                .get("tiles")?
                .as_array()?
                .iter()
                .take(MAX_DETAILS)
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    Some((
                        pair.first()?.as_str()?.to_owned(),
                        pair.get(1)?.as_str()?.to_owned(),
                    ))
                })
                .collect();
            Some(FlyoutBlock::MetricTiles(tiles))
        }
        "button" => Some(FlyoutBlock::ActionRow {
            id: u32_field(value, "id"),
            title: text("label"),
            subtitle: None,
            badge: None,
            icon: None,
            destructive: false,
        }),
        "chart" => {
            let series = value
                .get("series")?
                .as_array()?
                .iter()
                .take(MAX_BLOCKS)
                .filter_map(|entry| {
                    let name = entry.get("name")?.as_str()?.to_owned();
                    let samples = entry
                        .get("samples")?
                        .as_array()?
                        .iter()
                        .take(MAX_SAMPLES)
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect();
                    Some((name, samples))
                })
                .collect();
            Some(FlyoutBlock::Chart {
                label: text("label"),
                series,
                accent: (0x58, 0xd8, 0xb2),
            })
        }
        "image" => Some(FlyoutBlock::Image {
            glyph: u16_field(value, "glyph", 0xE81E),
            path: opt_str_field(value, "path"),
            size_dips: num_field(value, "size", 48.0).clamp(16.0, 256.0) as f32,
        }),
        "iconBar" => {
            let buttons = value
                .get("buttons")?
                .as_array()?
                .iter()
                .take(MAX_BLOCKS)
                .filter_map(|entry| {
                    Some((
                        u32_field(entry, "id"),
                        entry.get("label")?.as_str()?.to_owned(),
                        u16_field(entry, "glyph", 0xE81E),
                        bool_field(entry, "active"),
                    ))
                })
                .collect();
            Some(FlyoutBlock::IconBar { buttons })
        }
        "seek" => Some(FlyoutBlock::SeekSlider {
            id: u32_field(value, "id"),
            label: text("label"),
            position_secs: u32_field(value, "position"),
            duration_secs: u32_field(value, "duration"),
            ratio: ratio_field(value, "ratio"),
        }),
        "textInput" => {
            // A reversed range would panic in `f64::clamp` when the user presses Enter.
            let raw_min = num_field(value, "min", 0.0);
            let raw_max = num_field(value, "max", 100.0);
            let (min, max) = if raw_min <= raw_max {
                (raw_min, raw_max)
            } else {
                (raw_max, raw_min)
            };
            Some(FlyoutBlock::TextInput {
                id: u32_field(value, "id"),
                label: text("label"),
                value: text("value"),
                numeric: bool_field(value, "numeric"),
                min,
                max,
            })
        }
        _ => None,
    }
}

fn flyout_from_value(value: &Value) -> Option<FlyoutSpec> {
    let flyout = value.get("flyout")?;
    let blocks = flyout
        .get("blocks")?
        .as_array()?
        .iter()
        .take(MAX_BLOCKS)
        .filter_map(block_from_value)
        .collect();
    Some(FlyoutSpec {
        title: str_field(flyout, "title"),
        subtitle: opt_str_field(flyout, "subtitle"),
        width_dips: num_field(flyout, "width", 280.0).clamp(180.0, 640.0) as f32,
        blocks,
    })
}

fn details_from_value(value: &Value) -> Vec<(String, String)> {
    value
        .get("flyout")
        .and_then(|f| f.get("details"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .take(MAX_DETAILS)
                .filter_map(|row| {
                    let row = row.as_array()?;
                    Some((
                        row.first()?.as_str()?.to_owned(),
                        row.get(1)?.as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn apply_reply(
    _id: &str,
    reply: Result<Value, String>,
    sidecar: &Sidecar,
    generation: u64,
) -> Option<Value> {
    // A reply from a killed worker (timeout / manifest change / removal) must never
    // overwrite a newer snapshot or reset its failure counter.
    if sidecar.generation.load(Ordering::Acquire) != generation {
        return None;
    }
    match reply {
        Ok(value) => {
            sidecar.failures.store(0, Ordering::Relaxed);
            let mut snapshot = sidecar.snapshot.write().expect("sidecar snapshot poisoned");
            snapshot.chip = chip_from_value(&value);
            if let Some(flyout) = flyout_from_value(&value) {
                snapshot.flyout = Some(flyout);
            }
            snapshot.details = details_from_value(&value);
            snapshot.error = None;
            snapshot.at = Some(Instant::now());
            Some(value)
        }
        Err(error) => {
            sidecar.failures.fetch_add(1, Ordering::Relaxed);
            sidecar.attempt_ms.store(now_ms(), Ordering::Relaxed);
            let mut snapshot = sidecar.snapshot.write().expect("sidecar snapshot poisoned");
            snapshot.error = Some(error);
            None
        }
    }
}

fn backoff(failures: u32) -> Duration {
    let secs = 1u64 << failures.min(6);
    Duration::from_secs(secs.min(MAX_BACKOFF_SECS))
}

/// Requests a poll on a worker thread. The UI thread never blocks on a sidecar.
pub fn poll_widget(id: &str, settings: &DockSettings) {
    if !is_approved(id, settings) {
        return;
    }
    let Some(spec) = spec(id) else {
        return;
    };
    let sidecar = sidecar(id);
    if sidecar
        .snapshot
        .read()
        .ok()
        .and_then(|snapshot| snapshot.at)
        .is_some_and(|at| at.elapsed().as_secs() < u64::from(spec.poll_seconds))
    {
        return;
    }
    let failures = sidecar.failures.load(Ordering::Relaxed);
    if failures > 0
        && now_ms().saturating_sub(sidecar.attempt_ms.load(Ordering::Relaxed))
            < backoff(failures).as_millis() as u64
    {
        return;
    }
    // Timeouts are handled by the request watchdog; the worker alone clears `in_flight`.
    dispatch(&sidecar, id.to_owned(), spec, RequestKind::Poll);
}

/// Applies a sidecar action. Unknown actions are ignored.
fn apply_action(id: &str, value: &Value) {
    let Some(action) = value.get("action").and_then(Value::as_str) else {
        return;
    };
    match action {
        "openUrl" | "launch" => {
            if let Some(target) = value.get("target").and_then(Value::as_str) {
                pins::launch(target.to_owned());
            }
        }
        "copy" => {
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                opentaskbar_core::log::event("WidgetCopy", &[("widget", id.to_owned())]);
                let escaped = text.replace('\'', "''");
                let _ = Command::new("powershell.exe")
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        &format!("Set-Clipboard -Value '{escaped}'"),
                    ])
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
            }
        }
        "notify" => {
            let title = value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("OpenTaskbar");
            let body = value
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let _ = toast::show_info_toast(&format!("{title}: {body}"));
        }
        "toggleSetting" => {
            let Some(key) = value.get("key").and_then(Value::as_str) else {
                return;
            };
            let mut settings = load_from(&settings_path());
            if let Ok(mut root) = serde_json::to_value(&settings) {
                root[key] = Value::Bool(
                    root.get(key)
                        .and_then(Value::as_bool)
                        .is_none_or(|current| !current),
                );
                if let Ok(next) = serde_json::from_value::<DockSettings>(root) {
                    settings = next;
                    let _ = save_to(&settings_path(), &settings);
                }
            }
        }
        _ => {}
    }
}

/// Sends a flyout click to the sidecar and applies the returned action.
pub fn click_widget(id: &str, action_id: u32, settings: &DockSettings) {
    if !is_approved(id, settings) {
        return;
    }
    let Some(spec) = spec(id) else {
        return;
    };
    // A click shares the poll gate: if a poll is active the click is dropped rather than
    // racing for the pipes, and its reply is applied as an action only — never as a snapshot.
    dispatch(
        &sidecar(id),
        id.to_owned(),
        spec,
        RequestKind::Click(action_id),
    );
}

pub fn chip(id: &str) -> Option<CustomChip> {
    sidecar(id).snapshot.read().ok()?.chip.clone()
}

pub fn flyout(id: &str) -> Option<FlyoutSpec> {
    sidecar(id).snapshot.read().ok()?.flyout.clone()
}

/// Dynamic Island detail rows supplied by the widget, or an error row.
pub fn details(id: &str) -> Vec<(String, String)> {
    let handle = sidecar(id);
    let snapshot = handle.snapshot.read();
    snapshot.as_ref().map_or_else(
        |_| Vec::new(),
        |s| {
            if !s.details.is_empty() {
                s.details.clone()
            } else if let Some(error) = &s.error {
                vec![("Error".to_owned(), error.clone())]
            } else {
                Vec::new()
            }
        },
    )
}

pub fn error(id: &str) -> Option<String> {
    sidecar(id).snapshot.read().ok()?.error.clone()
}

pub fn icon_for(chip: &CustomChip, id: &str, settings: &DockSettings) -> WidgetIcon {
    chip.icon
        .as_deref()
        .and_then(super::widget_icons::icon_from_text)
        .unwrap_or_else(|| resolve_widget_icon(id, settings, WidgetIcon::Glyph(0xE81E)))
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_UNIQUE: AtomicU64 = AtomicU64::new(0);

    /// Unique per invocation, even when several test binaries run at once.
    fn unique_test_tag(tag: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!(
            "{tag}-{}-{}-{nanos}",
            std::process::id(),
            TEST_UNIQUE.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Private `%TEMP%` directory for a test; removed on drop (also after a panic).
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("otb-{tag}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn manifest_parses_and_clamps() {
        let text = r#"{"name":"USD/TRY","category":"Time & Weather","width":9999,"pollSeconds":1,"exec":["python","widget.py"]}"#;
        let value: Value = serde_json::from_str(text).unwrap();
        let spec =
            parse_manifest(&value, text, "usd-try", Path::new("C:\\widgets"), "abc").unwrap();
        assert_eq!(spec.id, "custom:usd-try");
        assert_eq!(spec.category, "Time & Weather");
        assert_eq!(spec.width, 512.0);
        assert_eq!(spec.poll_seconds, MIN_POLL_SECS);
        assert_eq!(spec.exec, ["python", "widget.py"]);
        assert_eq!(spec.manifest_hash, "abc");
    }

    #[test]
    fn localized_names_and_manifest_hashes() {
        let text = r#"{"name":{"en-US":"Weather","tr-TR":"Hava Durumu"},"exec":["python","w.py"]}"#;
        let value: Value = serde_json::from_str(text).unwrap();
        let spec =
            parse_manifest(&value, text, "weather", Path::new("C:\\widgets"), "hash").unwrap();
        assert_eq!(spec.name, "Weather");
        assert_eq!(spec.names.get("tr-TR").unwrap(), "Hava Durumu");
        assert_eq!(hex_hash(b"abc"), hex_hash(b"abc"));
        assert_ne!(hex_hash(b"abc"), hex_hash(b"abd"));

        // Without en-US/en the *first JSON entry* wins, deterministically.
        let text = r#"{"name":{"de-DE":"Wetter","fr-FR":"Meteo"},"exec":["python","w.py"]}"#;
        let value: Value = serde_json::from_str(text).unwrap();
        let spec =
            parse_manifest(&value, text, "weather", Path::new("C:\\widgets"), "hash").unwrap();
        assert_eq!(spec.name, "Wetter");
    }

    #[test]
    fn manifest_hash_change_forces_sidecar_restart() {
        let mut old = HashMap::new();
        old.insert(
            "custom:demo".to_owned(),
            CustomWidgetSpec {
                id: "custom:demo".to_owned(),
                slug: "demo".to_owned(),
                name: "Demo".to_owned(),
                names: HashMap::new(),
                category: "Productivity",
                width: 120.0,
                poll_seconds: 5,
                exec: vec!["python".to_owned(), "old.py".to_owned()],
                root: PathBuf::from("C:\\old"),
                manifest_hash: "old-hash".to_owned(),
            },
        );
        let mut next = old["custom:demo"].clone();
        assert!(!hash_changed(&old, &next));
        next.manifest_hash = "new-hash".to_owned();
        next.root = PathBuf::from("C:\\new");
        assert!(hash_changed(&old, &next));
        // The descriptor is reusable (it does not depend on the hash), but the sidecar is not.
        let descriptor = descriptor_for(&next);
        assert!(descriptor_matches(&next, &descriptor));
        assert_eq!(next.root, PathBuf::from("C:\\new"));
    }

    #[test]
    fn manifest_rejects_missing_exec_and_bad_names() {
        let text = r#"{"name":"No exec"}"#;
        let value: Value = serde_json::from_str(text).unwrap();
        assert!(parse_manifest(&value, text, "x", Path::new("C:\\w"), "h").is_none());
        let text = r#"{"name":"","exec":["python"]}"#;
        let value: Value = serde_json::from_str(text).unwrap();
        assert!(parse_manifest(&value, text, "x", Path::new("C:\\w"), "h").is_none());
    }

    #[test]
    fn protocol_maps_chips_and_blocks() {
        let reply: Value = serde_json::from_str(
            r#"{"protocol":1,"seq":4,
                 "chip":{"label":"USD","value":"42","ratio":0.5,"icon":"M0 0 L1 1 Z"},
                 "flyout":{"title":"Kur","details":[["Kur","42"]],
                   "blocks":[{"kind":"stat","label":"USD","value":"42"},
                   {"kind":"progress","label":"Dolu","ratio":0.5},{"kind":"note","text":"ok"},
                   {"kind":"chart","label":"C","series":[{"name":"s","samples":[1,2]}]},
                   {"kind":"iconBar","buttons":[{"id":1,"label":"A","glyph":58321}]}]}}"#,
        )
        .unwrap();
        let chip = chip_from_value(&reply).unwrap();
        assert_eq!(chip.value, "42");
        assert_eq!(chip.ratio, Some(0.5));
        let flyout = flyout_from_value(&reply).unwrap();
        assert_eq!(flyout.title, "Kur");
        assert_eq!(flyout.blocks.len(), 5);
        assert!(matches!(flyout.blocks[0], FlyoutBlock::StatRow { .. }));
        assert!(matches!(flyout.blocks[1], FlyoutBlock::ProgressBar { .. }));
        assert!(matches!(flyout.blocks[2], FlyoutBlock::Note(_)));
        assert!(matches!(flyout.blocks[3], FlyoutBlock::Chart { .. }));
        assert!(matches!(flyout.blocks[4], FlyoutBlock::IconBar { .. }));
        assert_eq!(
            details_from_value(&reply),
            [("Kur".to_owned(), "42".to_owned())]
        );
    }

    #[test]
    fn replies_require_protocol_and_exact_sequence() {
        assert!(reply_matches(&serde_json::json!({"protocol":1,"seq":4}), 4));
        assert!(!reply_matches(&serde_json::json!({"seq":4}), 4));
        assert!(!reply_matches(&serde_json::json!({"protocol":1}), 4));
        assert!(!reply_matches(
            &serde_json::json!({"protocol":1,"seq":3}),
            4
        ));
        assert!(!reply_matches(
            &serde_json::json!({"protocol":2,"seq":4}),
            4
        ));
    }

    #[test]
    fn oversized_reply_collections_are_capped_and_ranges_normalized() {
        let blocks: Vec<serde_json::Value> = (0..MAX_BLOCKS + 20)
            .map(|_| serde_json::json!({"kind":"note","text":"x"}))
            .collect();
        let samples: Vec<f64> = (0..MAX_SAMPLES + 50).map(|i| i as f64).collect();
        let reply = serde_json::json!({
            "flyout": {
                "title": "many",
                "blocks": blocks,
                "details": (0..MAX_DETAILS + 10).map(|i| serde_json::json!([i.to_string(), "v"])).collect::<Vec<_>>()
            },
            "chart": {"series":[{"name":"s","samples":samples}]}
        });
        let flyout = flyout_from_value(&reply).unwrap();
        assert_eq!(flyout.blocks.len(), MAX_BLOCKS);
        assert_eq!(details_from_value(&reply).len(), MAX_DETAILS);

        let bad_range = serde_json::json!({"kind":"textInput","min":100.0,"max":1.0});
        let FlyoutBlock::TextInput { min, max, .. } = block_from_value(&bad_range).unwrap() else {
            panic!("expected text input");
        };
        assert_eq!((min, max), (1.0, 100.0));
        // Huge u64 values are clamped to the field width instead of truncating silently.
        let huge = serde_json::json!({"kind":"button","id":u64::MAX,"label":"x"});
        assert!(matches!(
            block_from_value(&huge),
            Some(FlyoutBlock::ActionRow { .. })
        ));
    }

    #[test]
    fn custom_ids_are_recognized_without_unicode_panics() {
        assert!(is_custom("custom:clock"));
        assert!(is_custom("CUSTOM:x"));
        assert!(!is_custom("clock"));
        assert!(!is_custom("custom:"));
        // The 7th byte falls inside the 2-byte 'ğ' — a byte slice used to panic here.
        assert!(!is_custom("customğx"));
        assert!(!is_custom("😀custom"));
        assert!(is_custom("custom:ğ"));
    }

    #[test]
    fn sidecar_secret_handshake() {
        let _guard = crate::design::theme::theme_test_guard();
        let tag = unique_test_tag("secret");
        let slug = format!("secret-{tag}");
        let secret_name = format!("widget:{slug}:token");
        assert!(crate::widgets::telemetry::save_dpapi_secret(
            &secret_name,
            "secret-value"
        ));
        let dir = TestDir::new(&tag);
        let script = dir.path().join("secret.ps1");
        std::fs::write(
            &script,
            format!(
                r#"while (($line = [Console]::In.ReadLine()) -ne $null) {{
    $msg = $line | ConvertFrom-Json
    if ($msg.cmd -eq 'poll') {{
        '{{"cmd":"secret","name":"{secret_name}","seq":99,"protocol":1}}'
        $allowed = [Console]::In.ReadLine() | ConvertFrom-Json
        '{{"cmd":"secret","name":"api-key:deepseek","seq":100,"protocol":1}}'
        $denied = [Console]::In.ReadLine() | ConvertFrom-Json
        '{{"protocol":1,"seq":' + $msg.seq + ',"chip":{{"label":"S","value":"' + $allowed.value + '|' + $denied.value + '"}}}}'
    }}
}}
"#
            ),
        )
        .unwrap();
        let spec = CustomWidgetSpec {
            id: format!("custom:{slug}"),
            slug,
            name: "Secret".to_owned(),
            names: HashMap::new(),
            category: "Productivity",
            width: 120.0,
            poll_seconds: 5,
            exec: vec![
                "powershell.exe".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-File".to_owned(),
                script.to_string_lossy().into_owned(),
            ],
            root: dir.path().to_path_buf(),
            manifest_hash: "test".to_owned(),
        };
        let id = spec.id.clone();
        let state = sidecar(&id);
        let reply = request(&id, &spec, &state, &serde_json::json!({"cmd":"poll"}), 1).unwrap();
        // The scoped name resolves; the AI-provider namespace gets an empty secretResult.
        assert_eq!(chip_from_value(&reply).unwrap().value, "secret-value|");
        if let Ok(mut guard) = state.state.lock() {
            terminate(&mut guard);
        }
        let _ = crate::widgets::telemetry::clear_dpapi_secret(&secret_name);
    }

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(99), Duration::from_secs(MAX_BACKOFF_SECS));
    }

    #[test]
    fn requests_are_queued_while_the_worker_slot_is_busy() {
        let handle = Arc::new(Sidecar::new());
        handle.in_flight.store(true, Ordering::Release);
        let spec = CustomWidgetSpec {
            id: "custom:demo".to_owned(),
            slug: "demo".to_owned(),
            name: "Demo".to_owned(),
            names: HashMap::new(),
            category: "Productivity",
            width: 120.0,
            poll_seconds: 5,
            exec: vec!["python".to_owned()],
            root: PathBuf::from("C:\\demo"),
            manifest_hash: "h".to_owned(),
        };
        dispatch(
            &handle,
            spec.id.clone(),
            spec.clone(),
            RequestKind::Click(7),
        );
        assert_eq!(handle.pending.lock().unwrap().len(), 1);
        assert!(matches!(
            handle.pending.lock().unwrap().front(),
            Some((_, _, RequestKind::Click(7)))
        ));
        handle.invalidate();
        assert!(handle.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn invalidated_sidecar_discards_late_replies() {
        let handle = Arc::new(Sidecar::new());
        handle.snapshot.write().unwrap().chip = Some(CustomChip {
            label: "old".to_owned(),
            ..Default::default()
        });
        handle.invalidate();
        assert!(handle.snapshot.read().unwrap().chip.is_none());
        let late = serde_json::json!({"protocol":1,"seq":7,"chip":{"label":"late","value":"99"}});
        assert!(apply_reply("custom:demo", Ok(late), &handle, 0).is_none());
        assert!(handle.snapshot.read().unwrap().chip.is_none());
        assert_eq!(handle.failures.load(Ordering::Relaxed), 0);
        assert!(
            apply_reply(
                "custom:demo",
                Ok(serde_json::json!({"chip":{"label":"fresh"}})),
                &handle,
                handle.generation.load(Ordering::Relaxed)
            )
            .is_some()
        );
        assert_eq!(
            handle.snapshot.read().unwrap().chip.as_ref().unwrap().label,
            "fresh"
        );
    }

    #[test]
    fn sidecar_process_round_trip() {
        let _guard = crate::design::theme::theme_test_guard();
        let dir = TestDir::new(&unique_test_tag("sidecar"));
        let script = dir.path().join("sidecar.ps1");
        std::fs::write(
            &script,
            r#"while (($line = [Console]::In.ReadLine()) -ne $null) {
    $msg = $line | ConvertFrom-Json
    if ($msg.cmd -eq 'poll') {
        '{"protocol":1,"seq":' + $msg.seq + ',"chip":{"label":"TEMP","value":"42","ratio":0.5},"flyout":{"title":"TEMP","blocks":[{"kind":"stat","label":"Kur","value":"42"}]}}'
    } elseif ($msg.cmd -eq 'click') {
        '{"protocol":1,"seq":' + $msg.seq + ',"action":"none"}'
    }
}
"#,
        )
        .unwrap();

        let slug = format!("smoke-{}", unique_test_tag("widget"));
        let spec = CustomWidgetSpec {
            id: format!("custom:{slug}"),
            slug,
            name: "Smoke".to_owned(),
            names: HashMap::new(),
            category: "Productivity",
            width: 120.0,
            poll_seconds: 5,
            exec: vec![
                std::env::var_os("WINDIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
                    .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
                    .to_string_lossy()
                    .into_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-File".to_owned(),
                script.to_string_lossy().into_owned(),
            ],
            root: dir.path().to_path_buf(),
            manifest_hash: "test".to_owned(),
        };
        let id = spec.id.clone();
        let state = sidecar(&id);
        let reply = request(&id, &spec, &state, &serde_json::json!({"cmd": "poll"}), 1).unwrap();
        let chip = chip_from_value(&reply).unwrap();
        assert_eq!(chip.value, "42");
        assert_eq!(chip.ratio, Some(0.5));
        let flyout = flyout_from_value(&reply).unwrap();
        assert_eq!(flyout.title, "TEMP");
        if let Ok(mut guard) = state.state.lock() {
            terminate(&mut guard);
        }
    }
}
