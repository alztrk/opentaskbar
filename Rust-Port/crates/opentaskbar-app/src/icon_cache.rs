//! Persistent raw-BGRA icon disk cache + short-lived out-of-process icon worker.
//!
//! Uses `IShellItemImageFactory::GetImage(64x64, SIIGBF_ICONONLY)` inside the
//! short-lived `--extract-icons` worker so icons are high-DPI sharp, have full
//! 32-bit alpha (zero black boxes), never show `.lnk` shortcut arrows, and
//! support Store/UWP AUMIDs (`shell:AppsFolder\<AUMID>`).

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::smallvec;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{BITMAP, DeleteObject, GetObjectW, HBITMAP, HGDIOBJ};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    SIIGBF_SCALEUP,
};
use windows::core::HSTRING;

pub const WORKER_ARG: &str = "--extract-icons";

const ICON_PX: u32 = 64;
const CACHE_MAGIC: &[u8; 8] = b"OTICON02";
const CACHE_HEADER_LEN: usize = 24;
const MAX_DIM: u32 = 256;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
struct RawIcon {
    width: u32,
    height: u32,
    /// Top-down straight BGRA32 pixels (GPUI DirectX `RenderImage` native format).
    pixels: Vec<u8>,
}

fn cache_dir() -> PathBuf {
    opentaskbar_core::settings::app_data_dir().join("icon-cache")
}

fn hash_key(path: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_bytes() {
        h ^= u64::from(b.to_ascii_lowercase());
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn fingerprint(path: &str) -> Option<u64> {
    if path.starts_with("shell:") || path.starts_with("::{") {
        return Some(0);
    }
    let meta = fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(meta.len().rotate_left(17) ^ modified)
}

fn cache_path(path: &str) -> PathBuf {
    cache_dir().join(format!("{:016x}.oticon", hash_key(path)))
}

fn read_cached(path: &str) -> Option<RawIcon> {
    let expected_fp = fingerprint(path)?;
    let bytes = fs::read(cache_path(path)).ok()?;
    if bytes.len() < CACHE_HEADER_LEN || &bytes[..8] != CACHE_MAGIC {
        return None;
    }
    let width = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let fp = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    if fp != expected_fp || width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return None;
    }
    let payload = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if bytes.len() != CACHE_HEADER_LEN + payload {
        return None;
    }
    Some(RawIcon {
        width,
        height,
        pixels: bytes[CACHE_HEADER_LEN..].to_vec(),
    })
}

fn write_cached(path: &str, icon: &RawIcon) {
    let Some(fp) = fingerprint(path) else {
        return;
    };
    let dir = cache_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let dst = cache_path(path);
    let tmp = dst.with_extension("tmp");
    let mut out = Vec::with_capacity(CACHE_HEADER_LEN + icon.pixels.len());
    out.extend_from_slice(CACHE_MAGIC);
    out.extend_from_slice(&icon.width.to_le_bytes());
    out.extend_from_slice(&icon.height.to_le_bytes());
    out.extend_from_slice(&fp.to_le_bytes());
    out.extend_from_slice(&icon.pixels);
    if fs::write(&tmp, &out).is_ok() {
        let _ = fs::rename(&tmp, &dst);
    }
}

/// Wraps BGRA32 pixels directly as a `gpui::RenderImage` with full 32-bit alpha.
fn to_render_image(icon: RawIcon) -> Option<Arc<RenderImage>> {
    // GPUI's Windows DirectX renderer expects BGRA bytes inside `RgbaImage`.
    let buffer = RgbaImage::from_raw(icon.width, icon.height, icon.pixels)?;
    Some(Arc::new(RenderImage::new(smallvec![Frame::new(buffer)])))
}

/// Converts a Shell DIB section (premultiplied or straight BGRA) to top-down straight BGRA
/// suitable for GPUI's shader pipeline, and deletes the HBITMAP.
fn from_shell_hbitmap(bitmap: HBITMAP) -> Option<RawIcon> {
    // SAFETY: reading DIB section pixels owned by `bitmap`, deleted before returning.
    unsafe {
        let mut info = BITMAP::default();
        let read = GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some((&mut info as *mut BITMAP).cast()),
        );
        let result = if read != 0 && !info.bmBits.is_null() && info.bmBitsPixel == 32 {
            let width = info.bmWidth.max(0) as usize;
            let height = info.bmHeight.unsigned_abs() as usize;
            let stride = info.bmWidthBytes as usize;
            if width == 0 || height == 0 || width > MAX_DIM as usize || height > MAX_DIM as usize {
                None
            } else {
                let src = std::slice::from_raw_parts(info.bmBits as *const u8, stride * height);
                let mut pixels = vec![0u8; width * height * 4];
                for row in 0..height {
                    let src_row = if info.bmHeight > 0 {
                        height - 1 - row
                    } else {
                        row
                    };
                    pixels[row * width * 4..(row + 1) * width * 4]
                        .copy_from_slice(&src[src_row * stride..src_row * stride + width * 4]);
                }
                // Un-premultiply if the bitmap was premultiplied so GPUI does not double-darken edges.
                let is_premultiplied = pixels
                    .chunks_exact(4)
                    .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]);
                if is_premultiplied {
                    for p in pixels.chunks_exact_mut(4) {
                        let a = u16::from(p[3]);
                        if a > 0 && a < 255 {
                            p[0] = ((u16::from(p[0]) * 255 + a / 2) / a).min(255) as u8;
                            p[1] = ((u16::from(p[1]) * 255 + a / 2) / a).min(255) as u8;
                            p[2] = ((u16::from(p[2]) * 255 + a / 2) / a).min(255) as u8;
                        }
                    }
                }
                Some(RawIcon {
                    width: width as u32,
                    height: height as u32,
                    pixels,
                })
            }
        } else {
            None
        };
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        result
    }
}

fn extract_shell_icon(parsing_name: &str) -> Option<RawIcon> {
    // SAFETY: COM STA initialized in `run_worker`; HBITMAP freed in `from_shell_hbitmap`.
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok()?;
        let bitmap = factory
            .GetImage(
                SIZE {
                    cx: ICON_PX as i32,
                    cy: ICON_PX as i32,
                },
                SIIGBF_BIGGERSIZEOK | SIIGBF_ICONONLY | SIIGBF_SCALEUP,
            )
            .ok()?;
        from_shell_hbitmap(bitmap)
    }
}

/// Worker entry point (`OpenTaskBar.exe --extract-icons`).
pub fn run_worker() {
    // SAFETY: COM STA for IShellItemImageFactory, isolated to this short-lived worker process.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let path = line.trim_end_matches(['\r', '\n']);
        let icon = if path.is_empty() {
            None
        } else {
            extract_shell_icon(path)
        };
        let ok = match icon {
            Some(ic) => writer
                .write_all(&[1u8])
                .and_then(|()| writer.write_all(&ic.width.to_le_bytes()))
                .and_then(|()| writer.write_all(&ic.height.to_le_bytes()))
                .and_then(|()| writer.write_all(&(ic.pixels.len() as u32).to_le_bytes()))
                .and_then(|()| writer.write_all(&ic.pixels)),
            None => writer.write_all(&[0u8]),
        };
        if ok.is_err() || writer.flush().is_err() {
            break;
        }
        line.clear();
    }
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Worker {
    fn spawn() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let mut child = Command::new(exe)
            .arg(WORKER_ARG)
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

    fn request(&mut self, path: &str) -> Result<Option<RawIcon>, ()> {
        if path.contains(['\r', '\n']) {
            return Ok(None);
        }
        writeln!(self.stdin, "{path}").map_err(|_| ())?;
        self.stdin.flush().map_err(|_| ())?;
        let mut status = [0u8; 1];
        self.stdout.read_exact(&mut status).map_err(|_| ())?;
        if status[0] == 0 {
            return Ok(None);
        }
        let mut hdr = [0u8; 12];
        self.stdout.read_exact(&mut hdr).map_err(|_| ())?;
        let width = u32::from_le_bytes(hdr[0..4].try_into().map_err(|_| ())?);
        let height = u32::from_le_bytes(hdr[4..8].try_into().map_err(|_| ())?);
        let len = u32::from_le_bytes(hdr[8..12].try_into().map_err(|_| ())?) as usize;
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM || len != expected {
            return Err(());
        }
        let mut pixels = vec![0u8; len];
        self.stdout.read_exact(&mut pixels).map_err(|_| ())?;
        Ok(Some(RawIcon {
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

/// Resolves a batch of icon paths using disk cache first and a single short-lived
/// worker process for misses; drops the worker immediately when done.
pub fn resolve_batch(paths: &[String]) -> HashMap<String, Option<Arc<RenderImage>>> {
    let mut result = HashMap::with_capacity(paths.len());
    let mut worker: Option<Worker> = None;

    for path in paths {
        if result.contains_key(path) {
            continue;
        }
        let raw = if let Some(cached) = read_cached(path) {
            Some(cached)
        } else {
            let extracted = fetch_from_worker(&mut worker, path);
            if let Some(ref ic) = extracted {
                write_cached(path, ic);
            }
            extracted
        };
        result.insert(path.clone(), raw.and_then(to_render_image));
    }
    result
}

fn fetch_from_worker(worker: &mut Option<Worker>, path: &str) -> Option<RawIcon> {
    for _ in 0..2 {
        if worker.is_none() {
            *worker = Worker::spawn();
        }
        let active = worker.as_mut()?;
        match active.request(path) {
            Ok(icon) => return icon,
            Err(()) => *worker = None,
        }
    }
    None
}
