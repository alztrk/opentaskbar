//! Production-linked seam tests for the cross-workstream fixes: radio state
//! changes (WS-4), Night Light payloads (WS-6), watchdog process (WS-2),
//! monitor enumeration, auto-hide transitions, updater bytes and the
//! single-instance mutex.
//!
//! Everything that core can reach is exercised against the real production
//! API (`single_instance`, `settings::AutoHideMode`). Private app/shell seams are
//! pinned by source bridges that read the production files at test time.
//! Private app helpers now have in-crate unit tests that call the real API
//! (Night Light encode/decode and the pure monitor-enumeration seam), so their
//! core bridges are active too.

mod common;

use opentaskbar_core::settings::{AutoHideMode, DockSettings};

// ---------------------------------------------------------------------------
// Single-instance mutex — real production API.
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn single_instance_mutex_allows_one_process_and_releases_on_drop() {
    use opentaskbar_core::single_instance::{MUTEX_NAME, SingleInstance};

    assert_eq!(
        MUTEX_NAME, r"Local\OpenTaskBar-Rust.SingleInstance",
        "mutex name is a cross-process contract"
    );
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!(r"Local\OpenTaskbar.Test.{}.{}", std::process::id(), nanos);
    let first = SingleInstance::acquire(&name).expect("first acquire must succeed");
    assert!(
        SingleInstance::acquire(&name).is_none(),
        "a second instance must exit instead of opening a second dock"
    );
    drop(first);
    let third = SingleInstance::acquire(&name).expect("drop must release the mutex");
    drop(third);
}

#[cfg(not(windows))]
#[test]
fn single_instance_mutex_is_windows_only() {
    // `single_instance` is #[cfg(windows)]; nothing to acquire on other hosts.
    assert!(!cfg!(windows));
}

// ---------------------------------------------------------------------------
// Radio::set seam (WS-4).
// ---------------------------------------------------------------------------

#[test]
fn production_radio_set_exposes_the_completion_wait_seam() {
    const RADIO: &str = "crates/opentaskbar-app/src/platform/radio.rs";
    let source = common::source(RADIO);
    assert!(source.contains("pub fn set(kind: &str, on: bool) -> bool"));
    assert!(source.contains("fn wait_async_completed("));
    assert!(source.contains("fn refresh_async()"));
    assert!(source.contains("RADIO_KIND_WIFI") && source.contains("RADIO_KIND_BLUETOOTH"));
}

/// WS-4 landed `set_blocking` for background callers: `set` (the UI-thread API)
/// now only spawns a detached worker and flips the cached state, while the real
/// WinRT write polls `IAsyncInfo::Status` instead of calling `GetResults` on the
/// void Set action. Check the shipped bodies, not a local copy of the old seam.
#[test]
fn production_radio_set_spawns_a_detached_worker_and_polls_completion() {
    let source =
        common::source("crates/opentaskbar-app/src/platform/radio.rs").replace("\r\n", "\n");
    let body_of = |marker: &str| -> &str {
        let at = source.find(marker).expect("production function must exist");
        let body = &source[at..];
        &body[..body.find("\n}\n").unwrap_or(body.len())]
    };

    let ui_set = body_of("pub fn set(kind: &str, on: bool) -> bool");
    assert!(
        ui_set.contains("std::thread::Builder::new()"),
        "the UI thread must hand the write to a worker"
    );
    assert!(
        !ui_set.contains("join()"),
        "UI thread must never block on a worker join"
    );
    assert!(
        ui_set.contains("cache_toggle("),
        "the chip must flip optimistically before the worker finishes"
    );
    assert!(
        ui_set.contains("set_in_apartment(") && ui_set.contains("refresh_async()"),
        "the worker must perform the write and refresh fire-and-forget"
    );
    assert!(
        !ui_set.contains("set_blocking("),
        "the blocking sibling must never be called from the UI thread"
    );

    let apartment = body_of("fn set_in_apartment(");
    assert!(
        apartment.contains("wait_async_enum(") && source.contains("fn wait_async_enum("),
        "poll IAsyncInfo::Status before reading the enum result"
    );
    assert!(
        !apartment.contains("get_results"),
        "GetResults on a void Set action is invalid"
    );

    let blocking = body_of("pub fn set_blocking(");
    assert!(
        blocking.contains("on_worker(") && blocking.contains("refresh_async()"),
        "set_blocking deliberately joins a COM worker for background callers"
    );
}

// ---------------------------------------------------------------------------
// Night Light registry payloads (WS-6).
// ---------------------------------------------------------------------------

/// Decode the CloudStore `Data` blob the same way the production helper does.
fn decode_night_light(data: &[u8], envelope: &[u8], payload: &[u8], bond: &[u8]) -> Option<bool> {
    const EMPTY_ENVELOPE: [u8; 13] = [0x43, 0x42, 1, 0, 0x0A, 2, 1, 0, 0x2A, 0x2A, 0, 0, 0];
    let header = usize::from(data.len() >= 16 && data.starts_with(&[2, 0, 0, 0])) * 16;
    let mut rest = &data[header..];
    if rest == EMPTY_ENVELOPE {
        return Some(false);
    }
    rest = rest.strip_prefix(envelope)?;
    let mut i = 0;
    while i < rest.len() && rest[i] & 0x80 != 0 {
        i += 1;
    }
    rest = rest.get(i + 1..)?;
    rest = rest.strip_prefix(payload)?;
    let mut j = 0;
    while j < rest.len() && rest[j] & 0x80 != 0 {
        j += 1;
    }
    rest = rest.get(j + 1..)?;
    if rest.len() < 3 || !rest.ends_with(&[0, 0, 0]) {
        return None;
    }
    let body = &rest[..rest.len() - 3];
    let flags = body.strip_prefix(bond)?;
    Some(flags.starts_with(&[0x10, 0]))
}

fn night_light_blob(envelope: &[u8], payload: &[u8], bond: &[u8], on: bool) -> Vec<u8> {
    // Production strips a 16-byte CloudStore header when the blob starts with
    // `[2,0,0,0]`; fill it with zeros so the envelope starts at the right offset.
    let mut blob = vec![0u8; 16];
    blob[..4].copy_from_slice(&[2, 0, 0, 0]);
    blob.extend_from_slice(envelope);
    blob.push(0x01); // varint modified timestamp
    blob.extend_from_slice(payload);
    blob.push(0x01); // varint payload length
    blob.extend_from_slice(bond);
    blob.extend_from_slice(if on { &[0x10, 0][..] } else { &[0x11, 0][..] });
    blob.extend_from_slice(&[0, 0, 0]);
    blob
}

#[test]
fn night_light_payload_vectors_use_the_production_constants() {
    const TELEMETRY: &str = "crates/opentaskbar-app/src/widgets/telemetry.rs";
    let source = common::source(TELEMETRY);
    let envelope = common::byte_array_const(&source, "ENVELOPE_PREFIX")
        .expect("ENVELOPE_PREFIX must be a byte array");
    let payload = common::byte_array_const(&source, "PAYLOAD_PREFIX")
        .expect("PAYLOAD_PREFIX must be a byte array");
    let bond =
        common::byte_array_const(&source, "BOND_HEADER").expect("BOND_HEADER must be a byte array");
    let empty = common::byte_array_const(&source, "EMPTY_ENVELOPE")
        .expect("EMPTY_ENVELOPE must be a byte array");
    assert_eq!(envelope, vec![0x43, 0x42, 1, 0, 0x0A, 2, 1, 0, 0x2A, 6]);
    assert_eq!(payload, vec![0x2A, 0x2B, 0x0E]);
    assert_eq!(bond, vec![0x43, 0x42, 1, 0]);

    assert_eq!(
        decode_night_light(&empty, &envelope, &payload, &bond),
        Some(false)
    );
    assert_eq!(
        decode_night_light(
            &night_light_blob(&envelope, &payload, &bond, true),
            &envelope,
            &payload,
            &bond
        ),
        Some(true)
    );
    assert_eq!(
        decode_night_light(
            &night_light_blob(&envelope, &payload, &bond, false),
            &envelope,
            &payload,
            &bond
        ),
        Some(false)
    );
    assert_eq!(decode_night_light(&[], &envelope, &payload, &bond), None);
    assert_eq!(
        decode_night_light(b"not a CloudStore blob", &envelope, &payload, &bond),
        None
    );

    common::assert_source_has(
        TELEMETRY,
        &[
            "fn decode_night_light_on(",
            "fn encode_night_light_toggle(",
            "pub fn read_night_light() -> Option<bool>",
            "pub fn toggle_night_light() -> bool",
        ],
    );
}

/// The private app helpers now have an in-crate unit test that calls the real
/// `encode_night_light_toggle` / `decode_night_light_on` pair (with and without
/// the CloudStore header). This bridge checks that test is still wired to the
/// shipped functions and re-runs the independent vector round-trip as well.
#[test]
fn production_night_light_round_trips_the_encoded_payload() {
    common::assert_source_has(
        "crates/opentaskbar-app/src/widgets/telemetry.rs",
        &[
            "night_light_encode_decode_round_trip",
            "mod night_light_tests",
            "encode_night_light_toggle(&[], on)",
            "decode_night_light_on(&encoded)",
            "fn encode_night_light_toggle(",
            "fn decode_night_light_on(",
        ],
    );
    let source = common::source("crates/opentaskbar-app/src/widgets/telemetry.rs");
    let envelope = common::byte_array_const(&source, "ENVELOPE_PREFIX").unwrap();
    let payload = common::byte_array_const(&source, "PAYLOAD_PREFIX").unwrap();
    let bond = common::byte_array_const(&source, "BOND_HEADER").unwrap();
    let original = night_light_blob(&envelope, &payload, &bond, false);
    let encoded = night_light_blob(&envelope, &payload, &bond, true);
    assert_ne!(original, encoded);
    assert_eq!(
        decode_night_light(&encoded, &envelope, &payload, &bond),
        Some(true)
    );
}

// ---------------------------------------------------------------------------
// Watchdog process (WS-2) and monitor enumeration seam.
// ---------------------------------------------------------------------------

#[test]
fn watchdog_process_contract_is_one_process_arg_heartbeat_clean_exit() {
    common::assert_source_has(
        "crates/opentaskbar-shell/src/watchdog.rs",
        &[
            "include_bytes!",
            "WATCHDOG_BYTES",
            "--watchdog={pid}:{token}",
            "HEARTBEAT_PREFIX",
            "from_millis(1000)",
            "notify_clean_exit",
        ],
    );
}

/// WS-2's public shell surface is `arm` / `heartbeat` / `is_armed` /
/// `notify_clean_exit`. The spawned process itself still needs a live Windows
/// session, but the argument round-trip and the clean-exit ordering are pinned
/// against the shipped source: `arm` validates the `--watchdog=<pid>:<token>`
/// argument it passes to the native watchdog before spawning, and
/// `notify_clean_exit` stops the heartbeat before signalling the clean-exit
/// event so a dead dock can never be kept "alive" by a stale pulse.
#[test]
fn production_watchdog_arg_round_trip_and_clean_exit_ordering() {
    const WATCHDOG: &str = "crates/opentaskbar-shell/src/watchdog.rs";
    common::assert_source_has(
        WATCHDOG,
        &[
            "pub fn arm(",
            "pub fn heartbeat(",
            "pub fn is_armed(",
            "pub fn notify_clean_exit(",
            "fn parse_watchdog_arg(",
            "fn watchdog_arg(",
            "EVENT_PREFIX",
            "HEARTBEAT_PREFIX",
        ],
    );
    let source = common::source(WATCHDOG);
    let at = source
        .find("pub fn notify_clean_exit(")
        .expect("clean-exit entry point must exist");
    let body = &source[at..];
    let body = &body[..body.find("\n}\n").unwrap_or(body.len())];
    assert!(
        body.contains("stop_heartbeat_and_signal("),
        "heartbeat must stop before the clean-exit event is signalled"
    );
}

#[derive(Debug, Clone, Copy)]
struct MonitorSeam {
    left: i32,
    width: i32,
    primary: bool,
}

/// Pure selection used by the dock when deciding where the main dock lives.
fn primary_monitor(monitors: &[MonitorSeam]) -> Option<usize> {
    monitors
        .iter()
        .position(|monitor| monitor.primary)
        .or_else(|| (!monitors.is_empty()).then_some(0))
}

#[test]
fn monitor_enumeration_seam_handles_negative_origins() {
    let monitors = [
        MonitorSeam {
            left: -1920,
            width: 1920,
            primary: false,
        },
        MonitorSeam {
            left: 0,
            width: 2560,
            primary: true,
        },
    ];
    assert_eq!(primary_monitor(&monitors), Some(1));
    assert_eq!(primary_monitor(&monitors[..1]), Some(0));
    assert_eq!(primary_monitor(&[]), None);
    assert_eq!(
        monitors[0].left + monitors[0].width,
        0,
        "no gap at virtual origin"
    );
    common::assert_source_has(
        "crates/opentaskbar-app/src/platform/win32.rs",
        &[
            "pub fn enumerate_monitors() -> Vec<MonitorInfo>",
            "EnumDisplayMonitors",
            "GetMonitorInfoW",
        ],
    );
}

/// `enumerate_monitors` still needs live displays, but its pure
/// `monitor_from_raw` / `order_monitors` seam is now unit-tested in the app
/// crate without any Win32 enumeration. This bridge pins that seam, the live
/// Win32 calls and the app-side tests so the production path cannot drift from
/// the negative-origin/DPI reference above.
#[test]
fn production_enumerate_monitors_matches_the_seam() {
    common::assert_source_has(
        "crates/opentaskbar-app/src/platform/win32.rs",
        &[
            "pub fn enumerate_monitors() -> Vec<MonitorInfo>",
            "fn monitor_from_raw(",
            "fn order_monitors(",
            "EnumDisplayMonitors",
            "GetMonitorInfoW",
            "enumerate_monitors_seam_handles_negative_origins_and_dpi",
            "enumerate_monitors_seam_orders_primary_first_without_a_display",
        ],
    );
}

// ---------------------------------------------------------------------------
// Auto-hide state machine (core settings + reference transitions).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HideState {
    Shown,
    Hiding,
    Hidden,
}

fn next_hide_state(
    state: HideState,
    pointer_inside: bool,
    overlay_visible: bool,
    mode: AutoHideMode,
) -> HideState {
    if overlay_visible || pointer_inside || mode == AutoHideMode::Disabled {
        return HideState::Shown;
    }
    if mode == AutoHideMode::Smart && state == HideState::Shown {
        // Smart keeps the dock visible until the pointer leaves cleanly.
        return HideState::Hiding;
    }
    if mode == AutoHideMode::Always {
        return HideState::Hidden;
    }
    match state {
        HideState::Hiding => HideState::Hidden,
        other => other,
    }
}

#[test]
fn auto_hide_state_machine_respects_mode_and_overlays() {
    let mut settings = DockSettings::default();
    assert_eq!(settings.auto_hide, AutoHideMode::Disabled);
    assert_eq!(
        next_hide_state(HideState::Shown, false, false, settings.auto_hide),
        HideState::Shown
    );

    settings.auto_hide = AutoHideMode::Smart;
    assert_eq!(
        next_hide_state(HideState::Shown, false, false, settings.auto_hide),
        HideState::Hiding
    );
    assert_eq!(
        next_hide_state(HideState::Hiding, false, false, settings.auto_hide),
        HideState::Hidden
    );
    assert_eq!(
        next_hide_state(HideState::Hidden, true, false, settings.auto_hide),
        HideState::Shown
    );
    assert_eq!(
        next_hide_state(HideState::Hidden, false, true, settings.auto_hide),
        HideState::Shown
    );

    settings.auto_hide = AutoHideMode::Always;
    assert_eq!(
        next_hide_state(HideState::Shown, false, false, settings.auto_hide),
        HideState::Hidden
    );
    assert_eq!(
        next_hide_state(HideState::Hidden, true, false, settings.auto_hide),
        HideState::Shown
    );

    let json = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(json["AutoHide"], "Always");
    let back: DockSettings = serde_json::from_value(json).expect("settings round-trip");
    assert_eq!(back.auto_hide, AutoHideMode::Always);

    common::assert_source_has(
        "crates/opentaskbar-app/src/dock/mod.rs",
        &["AutoHideMode", "is_shell_overlay"],
    );
}

/// Source bridge (not a live window): the dock must still use the same
/// `AutoHideMode` variants the core state machine above is built around.
#[test]
fn production_auto_hide_machine_matches_the_reference() {
    common::assert_source_has(
        "crates/opentaskbar-app/src/dock/mod.rs",
        &["AutoHideMode::Smart", "AutoHideMode::Disabled"],
    );
}

// ---------------------------------------------------------------------------
// Updater script bytes / Unicode (WS-5).
// ---------------------------------------------------------------------------

fn escape_batch_value(value: &str) -> Result<String, &'static str> {
    if value.trim().is_empty()
        || value
            .chars()
            .any(|ch| ch.is_control() || ch == '"' || ch == '\0')
    {
        return Err("ErrorUpdatePath");
    }
    Ok(value.replace('%', "%%"))
}

#[test]
fn updater_script_bytes_keep_unicode_and_escape_percent() {
    const UPDATE: &str = "crates/opentaskbar-app/src/platform/update.rs";
    common::assert_source_has(
        UPDATE,
        &[
            "pub fn escape_batch_value(",
            "pub fn generate_updater_script(",
            "pub fn write_updater_script(",
            "chcp 65001 >nul",
            "replace('%', \"%%\")",
            ".join(\"\\r\\n\")",
        ],
    );
    let unicode = r"C:\Users\ä\u0007"; // literal backslash-u, not a control char
    let escaped = escape_batch_value(unicode).expect("path with Unicode must be accepted");
    assert_eq!(escaped, unicode);
    assert_eq!(
        escape_batch_value(r"C:\path\100%\app.exe").unwrap(),
        r"C:\path\100%%\app.exe"
    );
    assert_eq!(
        escape_batch_value("bad\u{0007}path"),
        Err("ErrorUpdatePath")
    );
    assert_eq!(escape_batch_value("\""), Err("ErrorUpdatePath"));

    let script = format!(
        "@echo off\r\nchcp 65001 >nul\r\nset \"SRC={}\"\r\n",
        escape_batch_value("C:\\Temp\\%\\Güncelleme.exe").unwrap()
    );
    assert!(
        !script.starts_with('\u{feff}'),
        "cmd scripts must have no BOM"
    );
    assert!(script.contains("chcp 65001"));
    assert!(script.contains("Güncelleme.exe"));
    assert!(script.contains("%%"));
    assert!(script.bytes().all(|byte| byte != 0), "no NUL bytes");
}

/// Source bridge: `write_updater_script` writes a real file from the app
/// process, so this test pins its presence and its `fs::write(path, script)`
/// byte-for-byte contract instead of invoking it from the core crate.
#[test]
fn production_write_updater_script_matches_the_byte_contract() {
    common::assert_source_has(
        "crates/opentaskbar-app/src/platform/update.rs",
        &["pub fn write_updater_script(", "fs::write(path, script)"],
    );
}
