//! Persistent raw-BGRA icon disk cache + short-lived out-of-process icon worker.
//!
//! Mirrors C# `ApplicationIconProvider`:
//! - For `.exe`/`.ico`/`.dll` (with optional `,index`): extracts native 64x64 icons directly via
//!   `PrivateExtractIconsW` (zero Explorer backplate, pure straight alpha).
//! - For `shell:AppsFolder\<AUMID>` (Store/MSIX apps) and fallback items: uses
//!   `IShellItemImageFactory::GetImage(64x64, SIIGBF_BIGGERSIZEOK | SIIGBF_ICONONLY | SIIGBF_SCALEUP)`.

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
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP as SYS_BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
    DeleteDC, DeleteObject as SysDeleteObject, GetDIBits, GetObjectW as SysGetObjectW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, HICON, ICONINFO, PrivateExtractIconsW,
};

pub const WORKER_ARG: &str = "--extract-icons";

const ICON_PX: u32 = 64;
const CACHE_MAGIC: &[u8; 8] = b"OTICON05";
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

fn split_icon_location(spec: &str) -> (&str, i32) {
    if let Some((path, idx_str)) = spec.rsplit_once(',')
        && let Ok(idx) = idx_str.trim().parse::<i32>()
    {
        return (path.trim(), idx);
    }
    (spec.trim(), 0)
}

fn fingerprint(spec: &str) -> Option<u64> {
    if spec.starts_with("shell:") || spec.starts_with("::{") {
        return Some(0);
    }
    let (path, idx) = split_icon_location(spec);
    let meta = fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(meta.len().rotate_left(17) ^ modified ^ (idx as u64).rotate_left(7))
}

fn cache_path(spec: &str) -> PathBuf {
    cache_dir().join(format!("{:016x}.oticon", hash_key(spec)))
}

fn read_cached(spec: &str) -> Option<RawIcon> {
    let expected_fp = fingerprint(spec)?;
    let bytes = fs::read(cache_path(spec)).ok()?;
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

fn write_cached(spec: &str, icon: &RawIcon) {
    let Some(fp) = fingerprint(spec) else {
        return;
    };
    let dir = cache_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let dst = cache_path(spec);
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

fn to_render_image(icon: RawIcon) -> Option<Arc<RenderImage>> {
    let buffer = RgbaImage::from_raw(icon.width, icon.height, icon.pixels)?;
    Some(Arc::new(RenderImage::new(smallvec![Frame::new(buffer)])))
}

/// Extracts an HICON to top-down straight BGRA32, applying the AND mask when the icon has no alpha channel.
fn hicon_to_bgra(hicon: HICON) -> Option<RawIcon> {
    // SAFETY: reading icon color and mask bitmaps and freeing both before returning.
    unsafe {
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(hicon, &mut ii) == 0 {
            return None;
        }
        let result = if !ii.hbmColor.is_null() {
            let mut bmp: SYS_BITMAP = std::mem::zeroed();
            if SysGetObjectW(
                ii.hbmColor,
                size_of::<SYS_BITMAP>() as i32,
                (&mut bmp as *mut SYS_BITMAP).cast(),
            ) != 0
            {
                let width = bmp.bmWidth.max(0) as u32;
                let height = bmp.bmHeight.unsigned_abs();
                if width > 0 && height > 0 && width <= MAX_DIM && height <= MAX_DIM {
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
                        ii.hbmColor,
                        0,
                        height,
                        pixels.as_mut_ptr().cast(),
                        &mut info,
                        DIB_RGB_COLORS,
                    );
                    if rows != 0 {
                        let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
                        if !has_alpha && !ii.hbmMask.is_null() {
                            let mut mask = vec![0u8; (width as usize) * (height as usize) * 4];
                            let mrows = GetDIBits(
                                dc,
                                ii.hbmMask,
                                0,
                                height,
                                mask.as_mut_ptr().cast(),
                                &mut info,
                                DIB_RGB_COLORS,
                            );
                            if mrows != 0 {
                                for (p, m) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                                    // Mask white (0xFF) = transparent, black (0x00) = opaque.
                                    if m[0] != 0 {
                                        p[0] = 0;
                                        p[1] = 0;
                                        p[2] = 0;
                                        p[3] = 0;
                                    } else {
                                        p[3] = 255;
                                    }
                                }
                            }
                        } else {
                            // Zero RGB on fully transparent pixels to prevent dark filter halos.
                            for p in pixels.chunks_exact_mut(4) {
                                if p[3] == 0 {
                                    p[0] = 0;
                                    p[1] = 0;
                                    p[2] = 0;
                                }
                            }
                        }
                        DeleteDC(dc);
                        Some(RawIcon {
                            width,
                            height,
                            pixels,
                        })
                    } else {
                        DeleteDC(dc);
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if !ii.hbmColor.is_null() {
            SysDeleteObject(ii.hbmColor);
        }
        if !ii.hbmMask.is_null() {
            SysDeleteObject(ii.hbmMask);
        }
        result
    }
}

/// Extracts directly from `.exe` / `.ico` / `.dll` via `PrivateExtractIconsW` (mirrors C# `GetExecutableIcon`).
fn extract_private_icon(spec: &str) -> Option<RawIcon> {
    let (path, index) = split_icon_location(spec);
    let lower = path.to_lowercase();
    if !(lower.ends_with(".exe") || lower.ends_with(".ico") || lower.ends_with(".dll")) {
        return None;
    }
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut hicon: HICON = std::ptr::null_mut();
    let mut icon_id: u32 = 0;
    // SAFETY: PrivateExtractIconsW writes one owned HICON destroyed below.
    unsafe {
        let count = PrivateExtractIconsW(
            wide.as_ptr(),
            index,
            ICON_PX as i32,
            ICON_PX as i32,
            &mut hicon,
            &mut icon_id,
            1,
            0,
        );
        if count == 0 || count == u32::MAX || hicon.is_null() {
            return None;
        }
        let icon = hicon_to_bgra(hicon);
        DestroyIcon(hicon);
        icon
    }
}

/// Converts a Shell `IShellItemImageFactory` 32bpp DIB section to straight BGRA32.
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
                for p in pixels.chunks_exact_mut(4) {
                    let a = u16::from(p[3]);
                    if a == 0 {
                        p[0] = 0;
                        p[1] = 0;
                        p[2] = 0;
                    } else if a < 255 {
                        let b = u16::from(p[0]).min(a);
                        let g = u16::from(p[1]).min(a);
                        let r = u16::from(p[2]).min(a);
                        p[0] = ((b * 255 + a / 2) / a).min(255) as u8;
                        p[1] = ((g * 255 + a / 2) / a).min(255) as u8;
                        p[2] = ((r * 255 + a / 2) / a).min(255) as u8;
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

fn extract_best(spec: &str) -> Option<RawIcon> {
    if !spec.starts_with("shell:")
        && !spec.starts_with("::{")
        && let Some(icon) = extract_private_icon(spec)
    {
        return Some(icon);
    }
    let (path, _) = split_icon_location(spec);
    extract_shell_icon(path)
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
        let spec = line.trim_end_matches(['\r', '\n']);
        let icon = if spec.is_empty() {
            None
        } else {
            extract_best(spec)
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

    fn request(&mut self, spec: &str) -> Result<Option<RawIcon>, ()> {
        if spec.contains(['\r', '\n']) {
            return Ok(None);
        }
        writeln!(self.stdin, "{spec}").map_err(|_| ())?;
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

pub fn resolve_batch(specs: &[String]) -> HashMap<String, Option<Arc<RenderImage>>> {
    let mut result = HashMap::with_capacity(specs.len());
    let mut worker: Option<Worker> = None;

    for spec in specs {
        if result.contains_key(spec) {
            continue;
        }
        let raw = if let Some(cached) = read_cached(spec) {
            Some(cached)
        } else {
            let extracted = fetch_from_worker(&mut worker, spec);
            if let Some(ref ic) = extracted {
                write_cached(spec, ic);
            }
            extracted
        };
        result.insert(spec.clone(), raw.and_then(to_render_image));
    }
    result
}

fn fetch_from_worker(worker: &mut Option<Worker>, spec: &str) -> Option<RawIcon> {
    for _ in 0..2 {
        if worker.is_none() {
            *worker = Worker::spawn();
        }
        let active = worker.as_mut()?;
        match active.request(spec) {
            Ok(icon) => return icon,
            Err(()) => *worker = None,
        }
    }
    None
}
