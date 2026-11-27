//! User-created widgets.
//!
//! A custom widget is a folder under `%LOCALAPPDATA%\OpenTaskbar\Widgets\<slug>\`
//! containing `widget.json` and a small program. OpenTaskbar spawns that program
//! as a sidecar and speaks newline-delimited JSON over stdin/stdout:
//!
//! ```text
//! host  -> {"cmd":"poll"}
//! widget -> {"chip":{"label":"USD","value":"42,15","ratio":0.6},
//!            "flyout":{"title":"USD/TRY","blocks":[{"kind":"stat","label":"Kur","value":"42,15"}]}}
//! host  -> {"cmd":"click","id":7}
//! widget -> {"action":"openUrl","target":"https://..."}
//! ```
//!
//! The host never learns HTTP, JSONPath or credentials: the widget owns its data
//! source. A sidecar is data, not in-process code, and a crash cannot take the
//! dock down.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use opentaskbar_core::paths;
use serde_json::Value;

use crate::design::widget_pack::parse_json_lenient;
use crate::dock::dcomp::WidgetIcon;
use crate::platform::pins;
use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

use super::registry::{HardwareNeed, ShrinkPriority, WIDGET_CATEGORIES, WidgetDescriptor};
use super::widget_icons::{resolve_widget_icon, safe_id};

pub const ID_PREFIX: &str = "custom:";
const MANIFEST_FILE: &str = "widget.json";
const MAX_WIDGETS: usize = 64;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MIN_POLL_SECS: u32 = 5;
const MAX_POLL_SECS: u32 = 86_400;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, PartialEq)]
pub struct CustomWidgetSpec {
    /// Full persisted id, e.g. `custom:usd-try`.
    pub id: String,
    /// Folder name and path-safe slug.
    pub slug: String,
    pub name: String,
    pub category: &'static str,
    pub width: f32,
    pub poll_seconds: u32,
    pub exec: Vec<String>,
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
    error: Option<String>,
    at: Option<Instant>,
}

struct SidecarState {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    poll_started: Option<Instant>,
}

struct Sidecar {
    state: Mutex<SidecarState>,
    snapshot: RwLock<CustomSnapshot>,
    in_flight: AtomicBool,
}

impl Sidecar {
    fn new() -> Self {
        Self {
            state: Mutex::new(SidecarState {
                child: None,
                stdin: None,
                stdout: None,
                poll_started: None,
            }),
            snapshot: RwLock::new(CustomSnapshot::default()),
            in_flight: AtomicBool::new(false),
        }
    }
}

#[derive(Default)]
struct CustomRegistry {
    descriptors: Vec<&'static WidgetDescriptor>,
    specs: HashMap<String, CustomWidgetSpec>,
}

static REGISTRY: LazyLock<RwLock<CustomRegistry>> =
    LazyLock::new(|| RwLock::new(CustomRegistry::default()));
static SIDECARS: LazyLock<Mutex<HashMap<String, Arc<Sidecar>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// Root folder for user-created widgets.
pub fn root_dir() -> PathBuf {
    paths::shared_app_data_dir().join("Widgets")
}

fn widget_dir(slug: &str) -> PathBuf {
    root_dir().join(slug)
}

fn parse_manifest(value: &Value, slug: &str) -> Option<CustomWidgetSpec> {
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?.trim();
    if name.is_empty() || name.len() > 128 {
        return None;
    }
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
        name: name.to_owned(),
        category,
        width,
        poll_seconds,
        exec,
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

/// Scans the widget folder and replaces the in-memory registry. Safe to call
/// repeatedly from Settings after the user drops in a new widget.
pub fn reload() -> usize {
    let mut next = CustomRegistry::default();
    if let Ok(entries) = std::fs::read_dir(root_dir()) {
        for entry in entries.flatten() {
            if next.specs.len() >= MAX_WIDGETS {
                break;
            }
            let Some(slug) = safe_id(&entry.file_name().to_string_lossy()).map(str::to_owned)
            else {
                continue;
            };
            let path = entry.path().join(MANIFEST_FILE);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.len() > MAX_MANIFEST_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = parse_json_lenient(&text) else {
                continue;
            };
            let Some(spec) = parse_manifest(&value, &slug) else {
                continue;
            };
            next.descriptors
                .push(Box::leak(Box::new(descriptor_for(&spec))));
            next.specs.insert(spec.id.clone(), spec);
        }
    }
    let count = next.specs.len();
    *REGISTRY.write().expect("custom widget registry poisoned") = next;
    // Drop sidecars whose widget no longer exists.
    SIDECARS
        .lock()
        .expect("sidecar registry poisoned")
        .retain(|id, _| count > 0 && REGISTRY.read().is_ok_and(|r| r.specs.contains_key(id)));
    count
}

fn ensure_loaded() {
    // `REGISTRY` starts empty; the first catalog read populates it once.
    let empty = REGISTRY.read().is_ok_and(|r| r.specs.is_empty());
    if empty
        && let Ok(guard) = REGISTRY.write()
        && guard.specs.is_empty()
    {
        drop(guard);
        reload();
    }
}

/// Snapshot of descriptors for the settings catalog and dock layout.
pub fn descriptors() -> Vec<&'static WidgetDescriptor> {
    ensure_loaded();
    REGISTRY
        .read()
        .map(|r| r.descriptors.clone())
        .unwrap_or_default()
}

pub fn spec(id: &str) -> Option<CustomWidgetSpec> {
    ensure_loaded();
    REGISTRY.read().ok().and_then(|r| r.specs.get(id).cloned())
}

pub fn is_custom(id: &str) -> bool {
    id.len() > ID_PREFIX.len() && id[..ID_PREFIX.len()].eq_ignore_ascii_case(ID_PREFIX)
}

fn sidecar(id: &str) -> Arc<Sidecar> {
    SIDECARS
        .lock()
        .expect("sidecar registry poisoned")
        .entry(id.to_owned())
        .or_insert_with(|| Arc::new(Sidecar::new()))
        .clone()
}

fn ensure_child(spec: &CustomWidgetSpec, state: &mut SidecarState) -> Result<(), String> {
    if let Some(child) = state.child.as_mut()
        && child.try_wait().ok().flatten().is_none()
    {
        return Ok(());
    }
    let dir = widget_dir(&spec.slug);
    let mut command = Command::new(&spec.exec[0]);
    command
        .args(&spec.exec[1..])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("sidecar stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("sidecar stdout unavailable")?;
    state.child = Some(child);
    state.stdin = Some(stdin);
    state.stdout = Some(BufReader::new(stdout));
    Ok(())
}

fn request(spec: &CustomWidgetSpec, sidecar: &Sidecar, line: &str) -> Result<Value, String> {
    let mut state = sidecar
        .state
        .lock()
        .map_err(|_| "sidecar lock poisoned".to_owned())?;
    ensure_child(spec, &mut state)?;
    state.poll_started = Some(Instant::now());
    let stdin = state.stdin.as_mut().ok_or("sidecar stdin unavailable")?;
    writeln!(stdin, "{line}").map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;
    let stdout = state.stdout.as_mut().ok_or("sidecar stdout unavailable")?;
    let mut response = String::new();
    let read = stdout.read_line(&mut response).map_err(|e| e.to_string())?;
    if read == 0 {
        return Err("sidecar closed stdout".to_owned());
    }
    state.poll_started = None;
    serde_json::from_str(&response).map_err(|e| e.to_string())
}

fn kill(state: &mut SidecarState) {
    if let Some(child) = state.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    state.child = None;
    state.stdin = None;
    state.stdout = None;
    state.poll_started = None;
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

fn apply_reply(id: &str, reply: Result<Value, String>) -> Option<Value> {
    let sidecar = sidecar(id);
    match reply {
        Ok(value) => {
            let mut snapshot = sidecar.snapshot.write().expect("sidecar snapshot poisoned");
            snapshot.chip = chip_from_value(&value);
            if let Some(flyout) = flyout_from_value(&value) {
                snapshot.flyout = Some(flyout);
            }
            snapshot.error = None;
            snapshot.at = Some(Instant::now());
            Some(value)
        }
        Err(error) => {
            if let Ok(mut state) = sidecar.state.lock() {
                kill(&mut state);
            }
            let mut snapshot = sidecar.snapshot.write().expect("sidecar snapshot poisoned");
            snapshot.error = Some(error);
            None
        }
    }
}

/// Requests a poll on a worker thread. The UI thread never blocks on the sidecar.
pub fn poll_widget(id: &str) {
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
    if sidecar.in_flight.swap(true, Ordering::AcqRel) {
        // A hung sidecar is killed so the next poll can restart it.
        if let Ok(mut state) = sidecar.state.lock()
            && state
                .poll_started
                .is_some_and(|at| at.elapsed() > IO_TIMEOUT)
        {
            kill(&mut state);
            sidecar.in_flight.store(false, Ordering::Release);
        }
        return;
    }
    let worker_id = id.to_owned();
    let worker_sidecar = Arc::clone(&sidecar);
    let spawned = std::thread::Builder::new()
        .name(format!("otb-widget-{id}"))
        .spawn(move || {
            let reply = request(&spec, &worker_sidecar, r#"{"cmd":"poll"}"#);
            apply_reply(&worker_id, reply);
            worker_sidecar.in_flight.store(false, Ordering::Release);
        })
        .is_ok();
    if !spawned {
        sidecar.in_flight.store(false, Ordering::Release);
    }
}

/// Sends a flyout click to the sidecar and applies an `openUrl` action if returned.
pub fn click_widget(id: &str, action_id: u32) {
    let Some(spec) = spec(id) else {
        return;
    };
    let sidecar = sidecar(id);
    let worker_id = id.to_owned();
    let _ = std::thread::Builder::new()
        .name(format!("otb-widget-click-{id}"))
        .spawn(move || {
            let line = format!(r#"{{"cmd":"click","id":{action_id}}}"#);
            if let Some(value) = apply_reply(&worker_id, request(&spec, &sidecar, &line))
                && value.get("action").and_then(Value::as_str) == Some("openUrl")
                && let Some(target) = value.get("target").and_then(Value::as_str)
            {
                pins::launch(target.to_owned());
            }
        });
}

pub fn chip(id: &str, settings: &opentaskbar_core::settings::DockSettings) -> Option<CustomChip> {
    let mut chip = sidecar(id).snapshot.read().ok()?.chip.clone()?;
    if let Some(hint) = chip.icon.take() {
        chip.icon = Some(hint);
    }
    let _ = settings;
    Some(chip)
}

pub fn flyout(id: &str) -> Option<FlyoutSpec> {
    sidecar(id).snapshot.read().ok()?.flyout.clone()
}

pub fn icon_for(
    chip: &CustomChip,
    id: &str,
    settings: &opentaskbar_core::settings::DockSettings,
) -> WidgetIcon {
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

    #[test]
    fn manifest_parses_and_clamps() {
        let value: Value = serde_json::from_str(
            r#"{"name":"USD/TRY","category":"Time & Weather","width":9999,"pollSeconds":1,"exec":["python","widget.py"]}"#,
        )
        .unwrap();
        let spec = parse_manifest(&value, "usd-try").unwrap();
        assert_eq!(spec.id, "custom:usd-try");
        assert_eq!(spec.category, "Time & Weather");
        assert_eq!(spec.width, 512.0);
        assert_eq!(spec.poll_seconds, MIN_POLL_SECS);
        assert_eq!(spec.exec, ["python", "widget.py"]);
    }

    #[test]
    fn manifest_rejects_missing_exec_and_bad_names() {
        let value: Value = serde_json::from_str(r#"{"name":"No exec"}"#).unwrap();
        assert!(parse_manifest(&value, "x").is_none());
        let value: Value = serde_json::from_str(r#"{"name":"","exec":["python"]}"#).unwrap();
        assert!(parse_manifest(&value, "x").is_none());
    }

    #[test]
    fn protocol_maps_chips_and_blocks() {
        let reply: Value = serde_json::from_str(
            r#"{"chip":{"label":"USD","value":"42","ratio":0.5,"icon":"M0 0 L1 1 Z"},
                 "flyout":{"title":"Kur","blocks":[{"kind":"stat","label":"USD","value":"42"},
                 {"kind":"progress","label":"Dolu","ratio":0.5},{"kind":"note","text":"ok"}]}}"#,
        )
        .unwrap();
        let chip = chip_from_value(&reply).unwrap();
        assert_eq!(chip.value, "42");
        assert_eq!(chip.ratio, Some(0.5));
        let flyout = flyout_from_value(&reply).unwrap();
        assert_eq!(flyout.title, "Kur");
        assert_eq!(flyout.blocks.len(), 3);
        assert!(matches!(flyout.blocks[0], FlyoutBlock::StatRow { .. }));
        assert!(matches!(flyout.blocks[1], FlyoutBlock::ProgressBar { .. }));
        assert!(matches!(flyout.blocks[2], FlyoutBlock::Note(_)));
    }

    #[test]
    fn custom_ids_are_recognized() {
        assert!(is_custom("custom:clock"));
        assert!(is_custom("CUSTOM:x"));
        assert!(!is_custom("clock"));
        assert!(!is_custom("custom:"));
    }

    #[test]
    fn sidecar_process_round_trip() {
        let dir = std::env::temp_dir().join(format!("otb-sidecar-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("sidecar.ps1");
        std::fs::write(
            &script,
            r#"while (($line = [Console]::In.ReadLine()) -ne $null) {
    $msg = $line | ConvertFrom-Json
    if ($msg.cmd -eq 'poll') {
        '{"chip":{"label":"TEMP","value":"42","ratio":0.5},"flyout":{"title":"TEMP","blocks":[{"kind":"stat","label":"Kur","value":"42"}]}}'
    } elseif ($msg.cmd -eq 'click') {
        '{"action":"none"}'
    }
}
"#,
        )
        .unwrap();

        let spec = CustomWidgetSpec {
            id: "custom:smoke-test".to_owned(),
            slug: dir.to_string_lossy().into_owned(),
            name: "Smoke".to_owned(),
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
        };
        let id = spec.id.clone();
        let state = sidecar(&id);
        let reply = request(&spec, &state, r#"{"cmd":"poll"}"#).unwrap();
        let chip = chip_from_value(&reply).unwrap();
        assert_eq!(chip.value, "42");
        assert_eq!(chip.ratio, Some(0.5));
        let flyout = flyout_from_value(&reply).unwrap();
        assert_eq!(flyout.title, "TEMP");
        if let Ok(mut guard) = state.state.lock() {
            kill(&mut guard);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
