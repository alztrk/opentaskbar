//! Icon loading on a background thread without pulling Windows Shell COM/WIC DLLs into the dock process.
//!
//! Icons are served from the persistent raw-BGRA disk cache first (< 0.2 ms, zero COM). Cache misses (and dynamic
//! icons such as the Recycle Bin) are extracted by a short-lived `OpenTaskBar.exe --extract-icons` child process
//! that exits automatically after 1.5 s of idle time, keeping the dock's private memory free of Shell DLL bloat.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::icons::{self, Icon};

pub type IconKey = (String, u32);
type Finished = Arc<Mutex<Vec<(IconKey, Option<Arc<Icon>>)>>>;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_PAYLOAD_BYTES: usize = 512 * 512 * 4;

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Worker {
    fn spawn() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let mut child = Command::new(exe)
            .arg(icons::WORKER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = BufReader::new(child.stdout.take()?);
        Some(Self {
            child,
            stdin,
            stdout,
        })
    }

    fn extract(&mut self, parsing_name: &str, size: u32) -> Result<Option<Icon>, ()> {
        // Control characters would corrupt the line-delimited protocol.
        if parsing_name.contains(['\r', '\n', '\t']) {
            return Ok(None);
        }
        writeln!(self.stdin, "{size}\t{parsing_name}").map_err(|_| ())?;
        self.stdin.flush().map_err(|_| ())?;

        let mut status = [0u8; 1];
        self.stdout.read_exact(&mut status).map_err(|_| ())?;
        if status[0] == 0 {
            return Ok(None);
        }
        let mut header = [0u8; 12];
        self.stdout.read_exact(&mut header).map_err(|_| ())?;
        let width = u32::from_le_bytes(header[0..4].try_into().map_err(|_| ())?);
        let height = u32::from_le_bytes(header[4..8].try_into().map_err(|_| ())?);
        let len = u32::from_le_bytes(header[8..12].try_into().map_err(|_| ())?) as usize;
        if len > MAX_PAYLOAD_BYTES
            || len
                != (width as usize)
                    .saturating_mul(height as usize)
                    .saturating_mul(4)
        {
            return Err(());
        }
        let mut pixels = vec![0u8; len];
        self.stdout.read_exact(&mut pixels).map_err(|_| ())?;
        Ok(Some(Icon {
            width,
            height,
            pixels,
        }))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct IconLoader {
    requests: Sender<IconKey>,
    results: Finished,
    cache: HashMap<IconKey, Option<Arc<Icon>>>,
    pending: Vec<IconKey>,
}

impl IconLoader {
    /// `notify` runs on the loader thread after each result (e.g. posts a message to the dock window).
    pub fn new(notify: impl Fn() + Send + 'static) -> Self {
        let (requests, receiver) = mpsc::channel::<IconKey>();
        let results: Finished = Arc::default();
        let sink = results.clone();
        std::thread::Builder::new()
            .name("ot-icons".into())
            .spawn(move || {
                let mut worker: Option<Worker> = None;
                loop {
                    let key = if worker.is_some() {
                        match receiver.recv_timeout(WORKER_IDLE_TIMEOUT) {
                            Ok(key) => key,
                            Err(RecvTimeoutError::Timeout) => {
                                // Drop the worker cleanly so Shell COM/WIC memory is released back to the OS.
                                worker = None;
                                continue;
                            }
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    } else {
                        match receiver.recv() {
                            Ok(key) => key,
                            Err(_) => break,
                        }
                    };

                    let icon = if let Some(cached) = icons::read_cached(&key.0, key.1) {
                        Some(Arc::new(cached))
                    } else {
                        let extracted = extract_via_worker(&mut worker, &key.0, key.1);
                        if let Some(ref icon) = extracted {
                            icons::write_cached(&key.0, key.1, icon);
                        }
                        extracted.map(Arc::new)
                    };

                    if let Ok(mut sink) = sink.lock() {
                        sink.push((key, icon));
                    }
                    notify();
                }
            })
            .expect("icon thread");
        Self {
            requests,
            results,
            cache: HashMap::new(),
            pending: Vec::new(),
        }
    }

    /// The icon if loaded; otherwise requests it once and returns `None` for now.
    pub fn get(&mut self, parsing_name: &str, size: u32) -> Option<Arc<Icon>> {
        let key = (parsing_name.to_owned(), size);
        if let Some(icon) = self.cache.get(&key) {
            return icon.clone();
        }
        if !self.pending.contains(&key) {
            self.pending.push(key.clone());
            let _ = self.requests.send(key);
        }
        None
    }

    /// Moves finished results into the cache; returns true when something new arrived.
    pub fn drain(&mut self) -> bool {
        let finished: Vec<_> = self
            .results
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default();
        let any = !finished.is_empty();
        for (key, icon) in finished {
            self.pending.retain(|k| k != &key);
            self.cache.insert(key, icon);
        }
        any
    }
}

fn extract_via_worker(worker: &mut Option<Worker>, parsing_name: &str, size: u32) -> Option<Icon> {
    for _ in 0..2 {
        if worker.is_none() {
            *worker = Worker::spawn();
        }
        let active = worker.as_mut()?;
        match active.extract(parsing_name, size) {
            Ok(icon) => return icon,
            Err(()) => {
                // Worker exited or broke pipe; respawn once.
                *worker = None;
            }
        }
    }
    None
}
