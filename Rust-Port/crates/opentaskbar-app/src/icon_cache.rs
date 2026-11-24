//! Persistent raw-BGRA icon disk cache + short-lived out-of-process icon worker.
//!
//! Extracted icons are cached as `.oticon` files under `%LOCALAPPDATA%\OpenTaskBar-Rust\icon-cache`
//! and wrapped as `Arc<gpui::Image>` (32-bit BGRA BMP with alpha) for direct GPU rendering via `gpui::img`.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use gpui::{Image, ImageFormat};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP,
};
use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

pub const WORKER_ARG: &str = "--extract-icons";

const CACHE_MAGIC: &[u8; 8] = b"OTICON01";
const CACHE_HEADER_LEN: usize = 24;
const MAX_DIM: u32 = 256;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
struct RawIcon {
    width: u32,
    height: u32,
    /// Straight top-down BGRA32 pixels.
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

/// Encodes a top-down 32-bit BGRA icon as a minimal 54-byte BMP for GPUI `Image`.
fn to_gpui_image(icon: &RawIcon) -> Arc<Image> {
    let file_size = (54 + icon.pixels.len()) as u32;
    let mut bmp = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&54u32.to_le_bytes());
    // BITMAPINFOHEADER (40 bytes)
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(icon.width as i32).to_le_bytes());
    bmp.extend_from_slice(&(-(icon.height as i32)).to_le_bytes()); // top-down
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&32u16.to_le_bytes()); // bpp
    bmp.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    bmp.extend_from_slice(&(icon.pixels.len() as u32).to_le_bytes());
    bmp.extend_from_slice(&0i32.to_le_bytes());
    bmp.extend_from_slice(&0i32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&icon.pixels);
    Arc::new(Image::from_bytes(ImageFormat::Bmp, bmp))
}

fn hbitmap_to_bgra(bitmap: HBITMAP) -> Option<RawIcon> {
    // SAFETY: reading DIB bits from a valid GDI HBITMAP.
    unsafe {
        let mut bmp: BITMAP = std::mem::zeroed();
        if GetObjectW(
            bitmap,
            size_of::<BITMAP>() as i32,
            (&mut bmp as *mut BITMAP).cast(),
        ) == 0
        {
            return None;
        }
        let width = bmp.bmWidth.max(0) as u32;
        let height = bmp.bmHeight.unsigned_abs();
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
            return None;
        }
        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            bmiColors: [std::mem::zeroed()],
        };
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let rows = GetDIBits(
            dc,
            bitmap,
            0,
            height,
            pixels.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        DeleteDC(dc);
        (rows != 0).then_some(RawIcon {
            width,
            height,
            pixels,
        })
    }
}

fn extract_hicon(path: &str) -> Option<RawIcon> {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `SHGetFileInfoW` returns an owned HICON destroyed below.
    unsafe {
        let mut info: SHFILEINFOW = std::mem::zeroed();
        let res = SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if res == 0 || info.hIcon.is_null() {
            return None;
        }
        let hicon: HICON = info.hIcon;
        let mut icon_info: ICONINFO = std::mem::zeroed();
        let icon = if GetIconInfo(hicon, &mut icon_info) != 0 {
            let result = if !icon_info.hbmColor.is_null() {
                hbitmap_to_bgra(icon_info.hbmColor)
            } else {
                None
            };
            if !icon_info.hbmColor.is_null() {
                DeleteObject(icon_info.hbmColor);
            }
            if !icon_info.hbmMask.is_null() {
                DeleteObject(icon_info.hbmMask);
            }
            result
        } else {
            None
        };
        DestroyIcon(hicon);
        icon
    }
}

/// Worker entry point (`OpenTaskBar.exe --extract-icons`).
pub fn run_worker() {
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
            extract_hicon(path)
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
pub fn resolve_batch(paths: &[String]) -> HashMap<String, Option<Arc<Image>>> {
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
        result.insert(path.clone(), raw.map(|ic| to_gpui_image(&ic)));
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
