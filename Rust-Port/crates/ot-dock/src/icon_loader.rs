//! Icon extraction on a background thread (COM STA). The UI thread only receives finished pixels, so a slow
//! shell extension can never stall the dock. Results are cached per (parsing name, size) for the process.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use crate::icons::{self, Icon};

pub type IconKey = (String, u32);
type Finished = Arc<Mutex<Vec<(IconKey, Option<Arc<Icon>>)>>>;

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
                // SAFETY: COM for the Shell image factory on this thread.
                let _ = unsafe {
                    windows::Win32::System::Com::CoInitializeEx(
                        None,
                        windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                    )
                };
                for key in receiver {
                    let icon = icons::load(&key.0, key.1).map(Arc::new);
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
