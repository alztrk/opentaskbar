//! NetSpeed multi-stream throughput test — `NetSpeedWidget` speed-test parity.
//!
//! Six parallel HTTP streams + a TCP connect-latency probe, with cancel / retry / restart and a
//! per-stream result grid. All network work runs on detached worker threads behind an
//! `AtomicBool` cancellation flag so the dock UI thread never blocks.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use opentaskbar_core::i18n as loc;

use crate::ui::flyout::{BarTone, FlyoutBlock, FlyoutSpec};

/// `NetSpeedWidget.StreamCount`.
pub const STREAM_COUNT: usize = 6;
/// Measurement window per stream.
pub const TEST_WINDOW_MS: u64 = 5_000;
const TEST_HOST: &str = "speed.cloudflare.com";
const TEST_PORT: u16 = 80;
const CONNECT_TIMEOUT_MS: u64 = 2_000;

/// Lifecycle of a speed test, mirroring `NetSpeedWidget.SpeedTestState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpeedTestState {
    #[default]
    Idle,
    MeasuringPing,
    Downloading,
    Finished,
    Cancelled,
    Failed,
}

impl SpeedTestState {
    pub fn label(self) -> String {
        match self {
            Self::Idle => loc::t("Idle"),
            Self::MeasuringPing => loc::t("SpeedTestTestingPing"),
            Self::Downloading => loc::t("SpeedTestTestingDownload"),
            Self::Finished => loc::t("SpeedTestComplete"),
            Self::Cancelled => loc::t("Cancelled"),
            Self::Failed => loc::t("QuickUnavailable"),
        }
    }
}

/// Result of one completed test run.
#[derive(Clone, Debug, Default)]
pub struct SpeedTestResult {
    pub ping_ms: Option<u32>,
    pub down_bps: u64,
    pub per_stream_bps: Vec<u64>,
    pub elapsed_ms: u64,
    pub server: String,
}

impl SpeedTestResult {
    pub fn aggregate_bps(&self) -> u64 {
        self.per_stream_bps.iter().sum()
    }

    pub fn is_success(&self) -> bool {
        self.aggregate_bps() > 0
    }
}

struct RunningTest {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<SpeedTestResult>,
}

/// Owns a single in-flight test. `poll()` joins a finished worker and publishes its result.
#[derive(Default)]
pub struct SpeedTestController {
    state: SpeedTestState,
    running: Option<RunningTest>,
    /// Cancelled workers are kept here until `poll()` can reap them; dropping a
    /// `JoinHandle` detaches the thread instead of joining it.
    reaper: Vec<JoinHandle<SpeedTestResult>>,
    result: Option<SpeedTestResult>,
    runs_completed: u32,
}

impl SpeedTestController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> SpeedTestState {
        self.state
    }

    pub fn result(&self) -> Option<&SpeedTestResult> {
        self.result.as_ref()
    }

    pub fn runs_completed(&self) -> u32 {
        self.runs_completed
    }

    /// Starts a fresh 6-stream test (`StartSpeedTest`).
    pub fn start(&mut self) {
        self.cancel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let handle = thread::Builder::new()
            .name("otb-speedtest".to_owned())
            .spawn(move || run_speed_test(worker_cancel));
        match handle {
            Ok(handle) => {
                self.running = Some(RunningTest { cancel, handle });
                self.state = SpeedTestState::MeasuringPing;
            }
            Err(_) => {
                // No worker exists, so `poll()` would return early forever.
                self.running = None;
                self.state = SpeedTestState::Failed;
            }
        }
    }

    /// `CancelSpeedTest`: flips the flag; workers check it every 64 KB.
    pub fn cancel(&mut self) {
        if let Some(run) = self.running.take() {
            run.cancel.store(true, Ordering::Release);
            // Do not join here — the worker may be mid-connect. Keep the handle
            // so `poll()` can reap it without blocking the UI thread.
            self.reaper.push(run.handle);
        }
        if self.state == SpeedTestState::MeasuringPing || self.state == SpeedTestState::Downloading
        {
            self.state = SpeedTestState::Cancelled;
        }
    }

    /// `RetrySpeedTest`: restart with the same settings.
    pub fn retry(&mut self) {
        self.start();
    }

    /// `RestartSpeedTest`: drop prior result counters and start clean.
    pub fn restart(&mut self) {
        self.result = None;
        self.runs_completed = 0;
        self.start();
    }

    /// Advances the state machine. Must be called from the flyout's live tick / timer.
    pub fn poll(&mut self, now: Instant, started_at: Option<Instant>) -> bool {
        // Reap cancelled workers. `is_finished()` guarantees the join cannot block.
        if !self.reaper.is_empty() {
            let mut pending = Vec::with_capacity(self.reaper.len());
            for handle in std::mem::take(&mut self.reaper) {
                if handle.is_finished() {
                    let _ = handle.join();
                } else {
                    pending.push(handle);
                }
            }
            self.reaper = pending;
        }
        let Some(run) = self.running.as_ref() else {
            return false;
        };
        if run.handle.is_finished() {
            let done = self.running.take().expect("checked above");
            match done.handle.join() {
                Ok(result) => {
                    if done.cancel.load(Ordering::Acquire) || !result.is_success() {
                        self.state = if done.cancel.load(Ordering::Acquire) {
                            SpeedTestState::Cancelled
                        } else {
                            SpeedTestState::Failed
                        };
                    } else {
                        self.result = Some(result);
                        self.runs_completed = self.runs_completed.saturating_add(1);
                        self.state = SpeedTestState::Finished;
                    }
                }
                Err(_) => self.state = SpeedTestState::Failed,
            }
            return true;
        }
        if let Some(started) = started_at
            && now.duration_since(started) >= Duration::from_millis(600)
            && self.state == SpeedTestState::MeasuringPing
        {
            self.state = SpeedTestState::Downloading;
        }
        false
    }
}

fn run_speed_test(cancel: Arc<AtomicBool>) -> SpeedTestResult {
    let ping_ms = measure_ping(&cancel);
    if cancel.load(Ordering::Acquire) {
        return SpeedTestResult {
            ping_ms,
            ..Default::default()
        };
    }
    let deadline = Instant::now() + Duration::from_millis(TEST_WINDOW_MS);
    let mut handles = Vec::with_capacity(STREAM_COUNT);
    for idx in 0..STREAM_COUNT {
        let flag = Arc::clone(&cancel);
        if let Ok(handle) = thread::Builder::new()
            .name(format!("otb-speed-stream-{idx}"))
            .spawn(move || download_stream(idx, flag, deadline))
        {
            handles.push(handle);
        }
    }
    let mut per_stream = Vec::with_capacity(handles.len());
    for handle in handles {
        per_stream.push(handle.join().unwrap_or(0));
    }
    let elapsed = TEST_WINDOW_MS.saturating_sub(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64,
    );
    SpeedTestResult {
        ping_ms,
        down_bps: per_stream.iter().sum(),
        per_stream_bps: per_stream,
        elapsed_ms: elapsed.max(1),
        server: TEST_HOST.to_owned(),
    }
}

fn resolve_host(host: &str, port: u16) -> Option<std::net::SocketAddr> {
    (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut iter| iter.next())
}

/// TCP connect latency in milliseconds (`NetSpeedWidget.MeasurePing`).
pub fn measure_ping(cancel: &AtomicBool) -> Option<u32> {
    let addr = resolve_host(TEST_HOST, TEST_PORT)?;
    let started = Instant::now();
    let stream =
        TcpStream::connect_timeout(&addr, Duration::from_millis(CONNECT_TIMEOUT_MS)).ok()?;
    let ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    drop(stream);
    if cancel.load(Ordering::Acquire) {
        None
    } else {
        Some(ms)
    }
}

fn download_stream(index: usize, cancel: Arc<AtomicBool>, deadline: Instant) -> u64 {
    let Some(addr) = resolve_host(TEST_HOST, TEST_PORT) else {
        return 0;
    };
    let Ok(mut stream) =
        TcpStream::connect_timeout(&addr, Duration::from_millis(CONNECT_TIMEOUT_MS))
    else {
        return 0;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let request = format!(
        "GET /__down?bytes=104857600&stream={index} HTTP/1.1\r\nHost: {TEST_HOST}\r\nUser-Agent: OpenTaskbar/1.0\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return 0;
    }
    let started = Instant::now();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    while Instant::now() < deadline && !cancel.load(Ordering::Acquire) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total = total.saturating_add(n as u64),
            Err(_) => break,
        }
    }
    let secs = started.elapsed().as_secs_f64().max(0.1);
    (total as f64 / secs) as u64
}

/// Formats throughput in reference style (`Mbit/s` above 1 Mbit, else `kbit/s`).
pub fn format_throughput(bps: u64) -> String {
    let mbit = bps as f64 * 8.0 / 1_000_000.0;
    if mbit >= 1.0 {
        format!("{mbit:.1} Mbit/s")
    } else {
        format!("{:.0} kbit/s", mbit * 1000.0)
    }
}

pub fn format_ping(ping_ms: Option<u32>) -> String {
    ping_ms.map_or("—".to_owned(), |ms| format!("{ms} ms"))
}

/// Builds the reference result-grid flyout (ping, aggregate, per-stream bars, retry/cancel/restart).
pub fn build_speedtest_flyout(
    state: SpeedTestState,
    result: Option<&SpeedTestResult>,
) -> FlyoutSpec {
    let mut blocks = vec![
        FlyoutBlock::StatRow {
            label: loc::t("SpeedTestStatus"),
            value: state.label(),
        },
        FlyoutBlock::Hero {
            value: result.map_or_else(|| "—".to_owned(), |r| format_throughput(r.aggregate_bps())),
            caption: Some(loc::t("SpeedTestAggregateDownload")),
            centered: false,
            large: true,
            right_meta: None,
        },
        FlyoutBlock::MetricTiles(vec![
            (
                loc::t("SpeedTestPing"),
                result.map_or_else(|| "—".to_owned(), |r| format_ping(r.ping_ms)),
            ),
            (
                loc::t("SpeedTestStreams"),
                loc::t_fmt("SpeedTestStreamsActive", &[&STREAM_COUNT.to_string()]),
            ),
            (
                loc::t("SeekDuration"),
                result.map_or_else(
                    || "—".to_owned(),
                    |r| format!("{:.1}s", r.elapsed_ms as f64 / 1000.0),
                ),
            ),
            (
                loc::t("SpeedTestServer"),
                result.map_or_else(|| "Cloudflare".to_owned(), |r| r.server.clone()),
            ),
        ]),
    ];
    if let Some(result) = result {
        blocks.push(FlyoutBlock::Separator);
        for (idx, bps) in result.per_stream_bps.iter().enumerate() {
            let ratio = (*bps as f32 / result.aggregate_bps().max(1) as f32) * STREAM_COUNT as f32;
            blocks.push(FlyoutBlock::ProgressBar {
                label: loc::t_fmt("SpeedTestStream", &[&(idx + 1).to_string()]),
                value_text: format_throughput(*bps),
                ratio: ratio.clamp(0.0, 1.0),
                tone: BarTone::Accent,
            });
        }
    }
    blocks.push(FlyoutBlock::Separator);
    blocks.push(FlyoutBlock::ButtonBar(vec![
        (1, loc::t("Start"), state == SpeedTestState::Idle),
        (2, loc::t("SpeedTestCancel"), false),
        (3, loc::t("SpeedTestRetry"), false),
        (4, loc::t("SpeedTestRestartButton"), false),
    ]));
    FlyoutSpec {
        title: loc::t("SpeedTestTitle"),
        subtitle: Some(loc::t_fmt(
            "SpeedTestParallelStreams",
            &[&STREAM_COUNT.to_string()],
        )),
        width_dips: 300.0,
        blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throughput_formatting() {
        assert_eq!(format_throughput(0), "0 kbit/s");
        assert_eq!(format_throughput(125_000), "1.0 Mbit/s");
        assert_eq!(format_throughput(12_500_000), "100.0 Mbit/s");
    }

    #[test]
    fn ping_formatting() {
        assert_eq!(format_ping(None), "—");
        assert_eq!(format_ping(Some(23)), "23 ms");
    }

    #[test]
    fn result_aggregates_streams() {
        let result = SpeedTestResult {
            per_stream_bps: vec![100, 200, 300, 400, 500, 600],
            ..Default::default()
        };
        assert_eq!(result.aggregate_bps(), 2100);
        assert!(result.is_success());
    }

    #[test]
    fn state_labels_follow_the_active_locale() {
        assert_eq!(SpeedTestState::Idle.label(), loc::t("Idle"));
        assert_eq!(SpeedTestState::Cancelled.label(), loc::t("Cancelled"));
        assert_eq!(
            SpeedTestState::Finished.label(),
            loc::t("SpeedTestComplete")
        );
    }

    #[test]
    fn cancelled_handle_is_reaped_by_poll() {
        let mut controller = SpeedTestController::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            SpeedTestResult::default()
        });
        controller.running = Some(RunningTest { cancel, handle });
        controller.state = SpeedTestState::MeasuringPing;
        controller.cancel();
        assert!(controller.running.is_none());
        assert_eq!(controller.reaper.len(), 1);
        assert_eq!(controller.state, SpeedTestState::Cancelled);
        for _ in 0..100 {
            controller.poll(Instant::now(), None);
            if controller.reaper.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            controller.reaper.is_empty(),
            "poll must join cancelled workers"
        );
        assert_eq!(controller.state, SpeedTestState::Cancelled);
    }

    #[test]
    fn panicked_worker_marks_the_state_failed() {
        let mut controller = SpeedTestController::new();
        controller.running = Some(RunningTest {
            cancel: Arc::new(AtomicBool::new(false)),
            handle: thread::spawn(|| panic!("worker failed")),
        });
        controller.state = SpeedTestState::Downloading;
        for _ in 0..100 {
            if controller.poll(Instant::now(), None) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(controller.state, SpeedTestState::Failed);
    }
}
