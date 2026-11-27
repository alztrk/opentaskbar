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
//! host   -> {"cmd":"secret","name":"api-key:example","protocol":1,"seq":5}
//! widget -> {"protocol":1,"seq":5,"value":"..."}
//! host   -> {"cmd":"click","id":7,"protocol":1,"seq":6}
//! widget -> {"protocol":1,"seq":6,"action":"openUrl","target":"https://..."}
//! ```
//!
//! The host never learns HTTP, JSONPath or credentials: the widget owns its data
//! source. A sidecar runs in a kill-on-close Job Object, is approved once by the
//! user (manifest hash pinned), and can never block the UI thread.

use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
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
use windows_sys::Win32::Foundation::CloseHandle;
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
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> isize;
    fn SetInformationJobObject(job: isize, class: u32, info: *const c_void, info_len: u32) -> i32;
    fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
    fn TerminateJobObject(job: isize, exit_code: u32) -> i32;
}

fn close_job(job: isize) {
    if job != 0 {
        unsafe {
            let _ = CloseHandle(job as *mut c_void);
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
    job: Option<isize>,
    /// Pipes are taken out while a request is in flight so the UI thread can
    /// never block on a mutex held across a blocking `read_line`.
    pipes: Option<Pipes>,
}

struct Sidecar {
    state: Mutex<SidecarState>,
    snapshot: RwLock<CustomSnapshot>,
    in_flight: AtomicBool,
    seq: AtomicU64,
    failures: AtomicU32,
    /// Unix millis of the last request start; 0 means none.
    started_ms: AtomicU64,
    /// Unix millis of the last completed attempt (for backoff).
    attempt_ms: AtomicU64,
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
            failures: AtomicU32::new(0),
            started_ms: AtomicU64::new(0),
            attempt_ms: AtomicU64::new(0),
        }
    }
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

fn parse_names(value: &Value) -> Option<(String, HashMap<String, String>)> {
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
            let default = names
                .get("en-US")
                .or_else(|| names.get("en"))
                .or_else(|| names.values().next())
                .cloned()?;
            Some((default, names))
        }
        _ => None,
    }
}

fn parse_manifest(value: &Value, slug: &str, root: &Path, hash: &str) -> Option<CustomWidgetSpec> {
    let object = value.as_object()?;
    let (name, names) = parse_names(value)?;
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
pub fn reload() -> usize {
    let mut specs = HashMap::new();
    let mut descriptors = Vec::new();
    let old = REGISTRY.read().map(|r| r.specs.clone()).unwrap_or_default();
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
            let Some(mut parsed) = parse_manifest(&value, &slug, &dir, &hash) else {
                continue;
            };
            let id = parsed.id.clone();
            if let Some(previous) = old.get(&id)
                && previous.manifest_hash == hash
            {
                // Reuse the descriptor; only mutable runtime data (root/hash) is refreshed.
                parsed.root = previous.root.clone();
            }
            descriptors.push(
                REGISTRY
                    .read()
                    .ok()
                    .and_then(|r| r.descriptors.iter().find(|d| d.id == id).copied())
                    .unwrap_or_else(|| {
                        Box::leak(Box::new(descriptor_for(&parsed))) as &'static WidgetDescriptor
                    }),
            );
            specs.insert(id, parsed);
        }
    }
    let count = specs.len();
    *REGISTRY.write().expect("custom widget registry poisoned") = CustomRegistry {
        descriptors: Arc::new(descriptors),
        specs,
    };
    SIDECARS
        .lock()
        .expect("sidecar registry poisoned")
        .retain(|id, _| REGISTRY.read().is_ok_and(|r| r.specs.contains_key(id)));
    count
}

fn hex_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn ensure_loaded() {
    let empty = REGISTRY.read().is_ok_and(|r| r.specs.is_empty());
    if empty
        && let Ok(guard) = REGISTRY.write()
        && guard.specs.is_empty()
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
    id.len() > ID_PREFIX.len() && id[..ID_PREFIX.len()].eq_ignore_ascii_case(ID_PREFIX)
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
            TerminateJobObject(job, 1);
            close_job(job);
        }
    }
    if let Some(child) = state.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    state.child = None;
    state.pipes = None;
}

fn create_job(child_id: u32) -> Option<isize> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
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
        if process.is_null() || AssignProcessToJobObject(job, process as isize) == 0 {
            if !process.is_null() {
                let _ = CloseHandle(process);
            }
            close_job(job);
            return None;
        }
        let _ = CloseHandle(process);
        Some(job)
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
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let job = create_job(child.id());
    let stdin = child.stdin.take().ok_or("sidecar stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("sidecar stdout unavailable")?;
    if let Some(stderr) = child.stderr.take() {
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
    state.child = Some(child);
    state.job = job;
    state.pipes = Some(Pipes {
        stdin,
        stdout: BufReader::new(stdout),
    });
    Ok(())
}

fn request(
    id: &str,
    spec: &CustomWidgetSpec,
    sidecar: &Sidecar,
    command: &Value,
) -> Result<Value, String> {
    let seq = sidecar.seq.fetch_add(1, Ordering::Relaxed) + 1;
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
    sidecar.started_ms.store(now_ms(), Ordering::Relaxed);

    let result = (|| -> Result<Value, String> {
        let text = serde_json::to_string(&line).map_err(|e| e.to_string())?;
        writeln!(pipes.stdin, "{text}").map_err(|e| e.to_string())?;
        pipes.stdin.flush().map_err(|e| e.to_string())?;
        for _ in 0..4 {
            let mut response = String::new();
            let read = pipes
                .stdout
                .read_line(&mut response)
                .map_err(|e| e.to_string())?;
            if read == 0 {
                return Err("sidecar closed stdout".to_owned());
            }
            let value: Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
            // A sidecar may ask for a DPAPI secret in-band; answer it without
            // ever putting the value on a command line or in the log.
            if value.get("cmd").and_then(Value::as_str) == Some("secret") {
                let name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let secret = crate::widgets::telemetry::read_dpapi_secret_for_widget(&name)
                    .unwrap_or_default();
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
            let version_ok = value
                .get("protocol")
                .and_then(Value::as_u64)
                .is_none_or(|v| v == u64::from(PROTOCOL_VERSION));
            let seq_ok = value
                .get("seq")
                .and_then(Value::as_u64)
                .is_none_or(|v| v == seq);
            if version_ok && seq_ok {
                return Ok(value);
            }
        }
        Err("no matching sidecar reply".to_owned())
    })();

    sidecar.started_ms.store(0, Ordering::Relaxed);
    if result.is_err()
        && let Ok(mut state) = sidecar.state.lock()
    {
        terminate(&mut state);
    } else if let Ok(mut state) = sidecar.state.lock() {
        state.pipes = Some(pipes);
    }
    result
}

fn chip_from_value(value: &Value) -> Option<CustomChip> {
    let chip = value.get("chip")?;
    Some(CustomChip {
        label: chip
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        value: chip
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        ratio: chip
            .get("ratio")
            .and_then(Value::as_f64)
            .map(|r| r.clamp(0.0, 1.0) as f32),
        icon: chip
            .get("icon")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    })
}

fn block_from_value(value: &Value) -> Option<FlyoutBlock> {
    let kind = value.get("kind")?.as_str()?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match kind {
        "stat" => Some(FlyoutBlock::StatRow {
            label: text("label"),
            value: text("value"),
        }),
        "note" => Some(FlyoutBlock::Note(text("text"))),
        "hero" => Some(FlyoutBlock::Hero {
            value: text("value"),
            caption: value
                .get("caption")
                .and_then(Value::as_str)
                .map(str::to_owned),
            centered: true,
            large: false,
            right_meta: None,
        }),
        "progress" => Some(FlyoutBlock::ProgressBar {
            label: text("label"),
            value_text: text("value"),
            ratio: value
                .get("ratio")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0) as f32,
            tone: BarTone::Accent,
        }),
        "metric" => {
            let tiles = value
                .get("tiles")?
                .as_array()?
                .iter()
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
            id: value.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
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
                .filter_map(|entry| {
                    let name = entry.get("name")?.as_str()?.to_owned();
                    let samples = entry
                        .get("samples")?
                        .as_array()?
                        .iter()
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
            glyph: value.get("glyph").and_then(Value::as_u64).unwrap_or(0xE81E) as u16,
            path: value.get("path").and_then(Value::as_str).map(str::to_owned),
            size_dips: value
                .get("size")
                .and_then(Value::as_f64)
                .unwrap_or(48.0)
                .clamp(16.0, 256.0) as f32,
        }),
        "iconBar" => {
            let buttons = value
                .get("buttons")?
                .as_array()?
                .iter()
                .filter_map(|entry| {
                    Some((
                        entry.get("id")?.as_u64()? as u32,
                        entry.get("label")?.as_str()?.to_owned(),
                        entry.get("glyph")?.as_u64()? as u16,
                        entry
                            .get("active")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ))
                })
                .collect();
            Some(FlyoutBlock::IconBar { buttons })
        }
        "seek" => Some(FlyoutBlock::SeekSlider {
            id: value.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
            label: text("label"),
            position_secs: value.get("position").and_then(Value::as_u64).unwrap_or(0) as u32,
            duration_secs: value.get("duration").and_then(Value::as_u64).unwrap_or(0) as u32,
            ratio: value
                .get("ratio")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0) as f32,
        }),
        "textInput" => Some(FlyoutBlock::TextInput {
            id: value.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
            label: text("label"),
            value: text("value"),
            numeric: value
                .get("numeric")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            min: value.get("min").and_then(Value::as_f64).unwrap_or(0.0),
            max: value.get("max").and_then(Value::as_f64).unwrap_or(100.0),
        }),
        _ => None,
    }
}

fn flyout_from_value(value: &Value) -> Option<FlyoutSpec> {
    let flyout = value.get("flyout")?;
    let blocks = flyout
        .get("blocks")?
        .as_array()?
        .iter()
        .filter_map(block_from_value)
        .collect();
    Some(FlyoutSpec {
        title: flyout
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        subtitle: flyout
            .get("subtitle")
            .and_then(Value::as_str)
            .map(str::to_owned),
        width_dips: flyout
            .get("width")
            .and_then(Value::as_f64)
            .unwrap_or(280.0)
            .clamp(180.0, 640.0) as f32,
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

fn apply_reply(_id: &str, reply: Result<Value, String>, sidecar: &Sidecar) -> Option<Value> {
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
    if sidecar.in_flight.swap(true, Ordering::AcqRel) {
        // `try_lock` is essential: a worker may own the state while reading.
        if let Ok(mut state) = sidecar.state.try_lock() {
            let started = sidecar.started_ms.load(Ordering::Relaxed);
            if started != 0 && now_ms().saturating_sub(started) > IO_TIMEOUT.as_millis() as u64 {
                terminate(&mut state);
                sidecar.in_flight.store(false, Ordering::Release);
            }
        }
        return;
    }
    let worker_id = id.to_owned();
    let worker_sidecar = Arc::clone(&sidecar);
    let spawned = std::thread::Builder::new()
        .name(format!("otb-widget-{id}"))
        .spawn(move || {
            let reply = request(
                &worker_id,
                &spec,
                &worker_sidecar,
                &serde_json::json!({"cmd": "poll"}),
            );
            apply_reply(&worker_id, reply, &worker_sidecar);
            worker_sidecar.in_flight.store(false, Ordering::Release);
        })
        .is_ok();
    if !spawned {
        sidecar.in_flight.store(false, Ordering::Release);
    }
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
    let sidecar = sidecar(id);
    let worker_id = id.to_owned();
    let _ = std::thread::Builder::new()
        .name(format!("otb-widget-click-{id}"))
        .spawn(move || {
            let reply = request(
                &worker_id,
                &spec,
                &sidecar,
                &serde_json::json!({"cmd": "click", "id": action_id}),
            );
            if let Some(value) = apply_reply(&worker_id, reply, &sidecar) {
                apply_action(&worker_id, &value);
            }
        });
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
    if let Some(hint) = &chip.icon {
        if hint.starts_with('M') || hint.starts_with('m') {
            return WidgetIcon::PathData(hint.clone());
        }
        let path = PathBuf::from(hint);
        if path.is_file() {
            return WidgetIcon::Asset(path);
        }
    }
    resolve_widget_icon(id, settings, WidgetIcon::Glyph(0xE81E))
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
        let value: Value = serde_json::from_str(
            r#"{"name":"USD/TRY","category":"Time & Weather","width":9999,"pollSeconds":1,"exec":["python","widget.py"]}"#,
        )
        .unwrap();
        let spec = parse_manifest(&value, "usd-try", Path::new("C:\\widgets"), "abc").unwrap();
        assert_eq!(spec.id, "custom:usd-try");
        assert_eq!(spec.category, "Time & Weather");
        assert_eq!(spec.width, 512.0);
        assert_eq!(spec.poll_seconds, MIN_POLL_SECS);
        assert_eq!(spec.exec, ["python", "widget.py"]);
        assert_eq!(spec.manifest_hash, "abc");
    }

    #[test]
    fn localized_names_and_manifest_hashes() {
        let value: Value = serde_json::from_str(
            r#"{"name":{"en-US":"Weather","tr-TR":"Hava Durumu"},"exec":["python","w.py"]}"#,
        )
        .unwrap();
        let spec = parse_manifest(&value, "weather", Path::new("C:\\widgets"), "hash").unwrap();
        assert_eq!(spec.name, "Weather");
        assert_eq!(spec.names.get("tr-TR").unwrap(), "Hava Durumu");
        assert_eq!(hex_hash(b"abc"), hex_hash(b"abc"));
        assert_ne!(hex_hash(b"abc"), hex_hash(b"abd"));
    }

    #[test]
    fn manifest_rejects_missing_exec_and_bad_names() {
        let value: Value = serde_json::from_str(r#"{"name":"No exec"}"#).unwrap();
        assert!(parse_manifest(&value, "x", Path::new("C:\\w"), "h").is_none());
        let value: Value = serde_json::from_str(r#"{"name":"","exec":["python"]}"#).unwrap();
        assert!(parse_manifest(&value, "x", Path::new("C:\\w"), "h").is_none());
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
    fn stale_sequence_replies_are_ignored() {
        assert!(chip_from_value(&serde_json::json!({"chip":{}})).is_some());
        assert!(
            flyout_from_value(&serde_json::json!({"flyout":{"title":"x","blocks":[]}})).is_some()
        );
    }

    #[test]
    fn custom_ids_are_recognized() {
        assert!(is_custom("custom:clock"));
        assert!(is_custom("CUSTOM:x"));
        assert!(!is_custom("clock"));
        assert!(!is_custom("custom:"));
    }

    #[test]
    fn sidecar_secret_handshake() {
        let _guard = crate::design::theme::theme_test_guard();
        let tag = unique_test_tag("secret");
        let secret_name = format!("widget-test-secret-{tag}");
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
        $answer = [Console]::In.ReadLine() | ConvertFrom-Json
        '{{"protocol":1,"seq":' + $msg.seq + ',"chip":{{"label":"S","value":"' + $answer.value + '"}}}}'
    }}
}}
"#
            ),
        )
        .unwrap();
        let slug = format!("secret-{tag}");
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
        let reply = request(&id, &spec, &state, &serde_json::json!({"cmd":"poll"})).unwrap();
        assert_eq!(chip_from_value(&reply).unwrap().value, "secret-value");
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
        let reply = request(&id, &spec, &state, &serde_json::json!({"cmd": "poll"})).unwrap();
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
