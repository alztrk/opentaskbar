//! Application icons from the Shell (`ApplicationIconProvider.GetForShellItem` in OpenTaskbar): any parsing name
//! (an .exe, `shell:AppsFolder\<AUMID>`, a CLSID like the Recycle Bin) at a requested pixel size, as premultiplied
//! BGRA rows ready for Direct2D. Extraction runs off the UI thread; results are cached per (name, size).

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    SIIGBF_SCALEUP,
};
use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, HICON, ICONINFO};
use windows::core::HSTRING;

/// A premultiplied BGRA image.
#[derive(Clone, Debug)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Loads an icon for a Shell parsing name. COM must be initialized on the calling thread.
pub fn load(parsing_name: &str, size: u32) -> Option<Icon> {
    // SAFETY: COM calls; the HBITMAP is deleted after its pixels are copied.
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok()?;
        let bitmap = factory
            .GetImage(
                SIZE {
                    cx: size as i32,
                    cy: size as i32,
                },
                SIIGBF_BIGGERSIZEOK | SIIGBF_ICONONLY | SIIGBF_SCALEUP,
            )
            .ok()?;
        from_hbitmap(bitmap)
    }
}

/// Copies a 32-bit DIB section's pixels (premultiplied) and deletes the bitmap.
/// Decodes an image file (PNG and the other WIC formats) to premultiplied BGRA, optionally scaled to `size` with
/// the Fant filter (WPF's high-quality scaling). COM must be initialized on the calling thread.
pub fn decode_image(bytes: &[u8], size: Option<(u32, u32)>) -> Option<Icon> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource,
        IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
        WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnLoad,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::core::Interface;
    if bytes.is_empty() {
        return None;
    }
    // SAFETY: WIC calls with valid arguments; `bytes` outlives the stream.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnLoad)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).ok()?;
        let mut source: IWICBitmapSource = frame.cast().ok()?;
        if let Some((w, h)) = size
            && (w, h) != (width, height)
            && w > 0
            && h > 0
        {
            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&source, w, h, WICBitmapInterpolationModeFant)
                .ok()?;
            source = scaler.cast().ok()?;
            width = w;
            height = h;
        }
        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        converter
            .CopyPixels(std::ptr::null(), width * 4, &mut pixels)
            .ok()?;
        Some(Icon {
            width,
            height,
            pixels,
        })
    }
}

pub fn from_hbitmap(bitmap: HBITMAP) -> Option<Icon> {
    // SAFETY: the bitmap is a DIB section from the Shell; it is deleted after its pixels are copied.
    unsafe {
        let mut info = BITMAP::default();
        let read = GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some((&mut info as *mut BITMAP).cast()),
        );
        let icon = if read != 0 && !info.bmBits.is_null() && info.bmBitsPixel == 32 {
            let width = info.bmWidth as usize;
            let height = info.bmHeight.unsigned_abs() as usize;
            let stride = info.bmWidthBytes as usize;
            let source = std::slice::from_raw_parts(info.bmBits as *const u8, stride * height);
            let mut pixels = vec![0u8; width * height * 4];
            for row in 0..height {
                // DIB sections with a positive height are stored bottom-up.
                let source_row = if info.bmHeight > 0 {
                    height - 1 - row
                } else {
                    row
                };
                pixels[row * width * 4..(row + 1) * width * 4]
                    .copy_from_slice(&source[source_row * stride..source_row * stride + width * 4]);
            }
            ensure_premultiplied(&mut pixels);
            Some(Icon {
                width: width as u32,
                height: height as u32,
                pixels,
            })
        } else {
            None
        };
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        icon
    }
}

/// The Shell parsing name for an app: its packaged AUMID when known (Store apps cannot be read from WindowsApps),
/// otherwise the executable.
pub fn parsing_name(executable_path: &str, app_user_model_id: Option<&str>) -> String {
    match app_user_model_id {
        Some(aumid) if aumid.contains('!') => format!(r"shell:AppsFolder\{aumid}"),
        _ => executable_path.to_owned(),
    }
}

/// The Recycle Bin's CLSID parsing name; its icon shows empty or full.
pub const RECYCLE_BIN: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

/// Shell bitmaps are usually premultiplied, but some icons (e.g. browser web-app .ico files) come back with straight
/// alpha, which shows as a bright fringe. A color channel above its alpha is impossible in premultiplied data, so
/// that proves straight alpha; the image is then premultiplied.
pub fn ensure_premultiplied(bgra: &mut [u8]) {
    let straight = bgra
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
    if !straight {
        return;
    }
    for pixel in bgra.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// The BGRA pixels of an icon: its color bitmap, with the mask as alpha for icons that have none.
pub fn from_hicon(icon: HICON) -> Option<Icon> {
    // SAFETY: GDI reads into buffers sized from the bitmaps; the bitmaps from GetIconInfo are deleted.
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        let result = (|| {
            if info.hbmColor.is_invalid() {
                return None;
            }
            let mut bitmap = BITMAP::default();
            if GetObjectW(
                HGDIOBJ(info.hbmColor.0),
                size_of::<BITMAP>() as i32,
                Some((&mut bitmap as *mut BITMAP).cast()),
            ) == 0
            {
                return None;
            }
            let (width, height) = (bitmap.bmWidth, bitmap.bmHeight.abs());
            let color = dib_rows(info.hbmColor, width, height)?;
            let mut pixels = color;
            if pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
                let mask = dib_rows(info.hbmMask, width, height)?;
                for (pixel, m) in pixels
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(mask.as_chunks::<4>().0)
                {
                    pixel[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
            ensure_premultiplied(&mut pixels);
            Some(Icon {
                width: width as u32,
                height: height as u32,
                pixels,
            })
        })();
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        result
    }
}

/// A bitmap's pixels as top-down 32-bit BGRA rows.
fn dib_rows(bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: a memory DC for GetDIBits, deleted below; the buffer holds `height` rows of `width` pixels.
    unsafe {
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        (lines == height).then_some(pixels)
    }
}

/// Command-line flag for the short-lived out-of-process icon extraction worker.
pub const WORKER_ARG: &str = "--extract-icons";

const CACHE_MAGIC: &[u8; 8] = b"OTICON01";
const CACHE_HEADER_LEN: usize = 8 + 4 + 4 + 8;
const MAX_ICON_DIM: u32 = 512;

fn cache_dir() -> std::path::PathBuf {
    ot_core::store::app_data_dir().join("icon-cache")
}

fn cache_key_hash(parsing_name: &str, size: u32) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in parsing_name.as_bytes() {
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash ^= u64::from(size);
    hash.wrapping_mul(0x0100_0000_01b3)
}

/// Returns a file-change fingerprint for cache validation, or `None` when the icon is dynamic and must not be
/// cached on disk (such as the Recycle Bin, which changes between empty and full).
fn cache_fingerprint(parsing_name: &str) -> Option<u64> {
    if parsing_name.eq_ignore_ascii_case(RECYCLE_BIN) {
        return None;
    }
    if parsing_name.starts_with("shell:") || parsing_name.starts_with("::{") {
        return Some(0);
    }
    let meta = std::fs::metadata(parsing_name).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(meta.len().rotate_left(17) ^ modified)
}

fn cache_path_in(dir: &std::path::Path, parsing_name: &str, size: u32) -> std::path::PathBuf {
    dir.join(format!(
        "{:016x}.oticon",
        cache_key_hash(parsing_name, size)
    ))
}

/// Reads a cached premultiplied BGRA icon from disk if its fingerprint still matches.
pub fn read_cached(parsing_name: &str, size: u32) -> Option<Icon> {
    read_cached_in(&cache_dir(), parsing_name, size)
}

fn read_cached_in(dir: &std::path::Path, parsing_name: &str, size: u32) -> Option<Icon> {
    let expected_fp = cache_fingerprint(parsing_name)?;
    let bytes = std::fs::read(cache_path_in(dir, parsing_name, size)).ok()?;
    if bytes.len() < CACHE_HEADER_LEN || &bytes[..8] != CACHE_MAGIC {
        return None;
    }
    let width = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let fp = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    if fp != expected_fp
        || width == 0
        || height == 0
        || width > MAX_ICON_DIM
        || height > MAX_ICON_DIM
    {
        return None;
    }
    let payload_len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if bytes.len() != CACHE_HEADER_LEN + payload_len {
        return None;
    }
    Some(Icon {
        width,
        height,
        pixels: bytes[CACHE_HEADER_LEN..].to_vec(),
    })
}

/// Persists a premultiplied BGRA icon into the disk cache (atomic write).
pub fn write_cached(parsing_name: &str, size: u32, icon: &Icon) {
    write_cached_in(&cache_dir(), parsing_name, size, icon);
}

fn write_cached_in(dir: &std::path::Path, parsing_name: &str, size: u32, icon: &Icon) {
    let Some(fp) = cache_fingerprint(parsing_name) else {
        return;
    };
    if icon.width == 0
        || icon.height == 0
        || icon.width > MAX_ICON_DIM
        || icon.height > MAX_ICON_DIM
        || icon.pixels.len() != (icon.width as usize) * (icon.height as usize) * 4
    {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = cache_path_in(dir, parsing_name, size);
    let tmp = path.with_extension("tmp");
    let mut out = Vec::with_capacity(CACHE_HEADER_LEN + icon.pixels.len());
    out.extend_from_slice(CACHE_MAGIC);
    out.extend_from_slice(&icon.width.to_le_bytes());
    out.extend_from_slice(&icon.height.to_le_bytes());
    out.extend_from_slice(&fp.to_le_bytes());
    out.extend_from_slice(&icon.pixels);
    if std::fs::write(&tmp, &out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Runs the short-lived out-of-process icon extraction worker (`OpenTaskBar.exe --extract-icons`).
/// Reads `<size>\t<parsing_name>\n` lines from stdin, extracts via Shell COM, and writes binary frames to stdout.
pub fn run_worker() {
    use std::io::{BufRead, BufReader, Write};

    // SAFETY: COM STA for Shell icon extraction, isolated to this short-lived worker process.
    let _ = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let icon = trimmed
            .split_once('\t')
            .and_then(|(size, name)| Some((size.parse::<u32>().ok()?, name)))
            .and_then(|(size, name)| load(name, size));
        let write_result = match icon {
            Some(icon) => writer
                .write_all(&[1u8])
                .and_then(|()| writer.write_all(&icon.width.to_le_bytes()))
                .and_then(|()| writer.write_all(&icon.height.to_le_bytes()))
                .and_then(|()| writer.write_all(&(icon.pixels.len() as u32).to_le_bytes()))
                .and_then(|()| writer.write_all(&icon.pixels)),
            None => writer.write_all(&[0u8]),
        };
        if write_result.is_err() || writer.flush().is_err() {
            break;
        }
        line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_alpha_is_premultiplied_once() {
        let mut straight = vec![255, 0, 0, 128, 10, 10, 10, 255];
        ensure_premultiplied(&mut straight);
        assert_eq!(straight, vec![128, 0, 0, 128, 10, 10, 10, 255]);
        let mut premultiplied = vec![100, 0, 0, 128];
        ensure_premultiplied(&mut premultiplied);
        assert_eq!(
            premultiplied,
            vec![100, 0, 0, 128],
            "already valid data is untouched"
        );
    }

    #[test]
    fn disk_cache_round_trips_and_skips_recycle_bin() {
        let dir = std::env::temp_dir().join(format!("ot-icon-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sample = Icon {
            width: 2,
            height: 2,
            pixels: vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
        };
        let aumid = r"shell:AppsFolder\Example.App!App";
        write_cached_in(&dir, aumid, 32, &sample);
        let loaded = read_cached_in(&dir, aumid, 32).expect("cached icon should load");
        assert_eq!(loaded.width, 2);
        assert_eq!(loaded.height, 2);
        assert_eq!(loaded.pixels, sample.pixels);

        write_cached_in(&dir, RECYCLE_BIN, 32, &sample);
        assert!(
            read_cached_in(&dir, RECYCLE_BIN, 32).is_none(),
            "Recycle Bin icon must stay dynamic"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
