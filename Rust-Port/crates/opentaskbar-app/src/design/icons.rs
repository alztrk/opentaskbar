//! Embedded Lucide SVG icon set rasterized on demand with `resvg` + `tiny-skia`.
//!
//! Every non-app icon across the Dock shell, Dock widget chips, Context Menus, and Flyouts
//! renders from clean 24x24 Lucide SVG geometry with full subpixel anti-aliasing and an
//! in-memory `(icon, size_px, rgba)` cache so icons are rasterized once and reused at 0 CPU cost.

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HDC, StretchDIBits,
};

use super::Rgb;

/// Unified Lucide SVG icon catalog shared across Dock, Menus, and Flyouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppIcon {
    // Dock shell
    Search,
    TaskView,
    TrayChevron,
    RecycleBinEmpty,
    RecycleBinFull,
    // Dock widget chips
    Sliders,
    Music,
    Volume2,
    VolumeX,
    SunCloud,
    Battery,
    Bluetooth,
    HardDrive,
    Timer,
    Folder,
    LayoutGrid,
    // Context menus & flyouts
    Settings,
    Activity,
    Terminal,
    Shield,
    Play,
    Monitor,
    RefreshCw,
    Power,
    Plus,
    Pin,
    Close,
    Trash2,
    ChevronLeft,
    ChevronRight,
    Layers,
}

/// Official Lucide 24x24 SVG inner path/shape definitions.
fn lucide_inner(icon: AppIcon) -> &'static str {
    match icon {
        AppIcon::Search => r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"#,
        AppIcon::TaskView => {
            r#"<rect width="14" height="10" x="3" y="5" rx="2"/><path d="M9 19h10a2 2 0 0 0 2-2v-6"/>"#
        }
        AppIcon::TrayChevron => r#"<path d="m18 15-6-6-6 6"/>"#,
        AppIcon::RecycleBinEmpty => {
            r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/><line x1="10" x2="10" y1="11" y2="17"/><line x1="14" x2="14" y1="11" y2="17"/>"#
        }
        AppIcon::RecycleBinFull => {
            r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" fill="currentColor" fill-opacity="0.22"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/><line x1="10" x2="10" y1="11" y2="17"/><line x1="14" x2="14" y1="11" y2="17"/>"#
        }
        AppIcon::Sliders => {
            r#"<line x1="21" x2="14" y1="4" y2="4"/><line x1="10" x2="3" y1="4" y2="4"/><line x1="21" x2="12" y1="12" y2="12"/><line x1="8" x2="3" y1="12" y2="12"/><line x1="21" x2="16" y1="20" y2="20"/><line x1="12" x2="3" y1="20" y2="20"/><line x1="14" x2="14" y1="2" y2="6"/><line x1="8" x2="8" y1="10" y2="14"/><line x1="16" x2="16" y1="18" y2="22"/>"#
        }
        AppIcon::Music => {
            r#"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#
        }
        AppIcon::Volume2 => {
            r#"<path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z"/><path d="M16 9a5 5 0 0 1 0 6"/><path d="M19.364 18.364a9 9 0 0 0 0-12.728"/>"#
        }
        AppIcon::VolumeX => {
            r#"<path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z"/><line x1="22" x2="16" y1="9" y2="15"/><line x1="16" x2="22" y1="9" y2="15"/>"#
        }
        AppIcon::SunCloud => {
            r#"<path d="M12 2v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="M20 12h2"/><path d="m19.07 4.93-1.41 1.41"/><path d="M15.947 12.65a4 4 0 0 0-5.925-4.128"/><path d="M13 22H7a5 5 0 1 1 4.9-6H13a3 3 0 0 1 0 6Z"/>"#
        }
        AppIcon::Battery => {
            r#"<rect width="16" height="10" x="2" y="7" rx="2" ry="2"/><line x1="22" x2="22" y1="11" y2="13"/><line x1="6" x2="6" y1="11" y2="13"/><line x1="10" x2="10" y1="11" y2="13"/>"#
        }
        AppIcon::Bluetooth => r#"<path d="m7 7 10 10-5 5V2l5 5L7 17"/>"#,
        AppIcon::HardDrive => {
            r#"<line x1="22" x2="2" y1="12" y2="12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/><line x1="6" x2="6.01" y1="16" y2="16"/><line x1="10" x2="10.01" y1="16" y2="16"/>"#
        }
        AppIcon::Timer => {
            r#"<line x1="10" x2="14" y1="2" y2="2"/><line x1="12" x2="15" y1="14" y2="11"/><circle cx="12" cy="14" r="8"/>"#
        }
        AppIcon::Folder => {
            r#"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"#
        }
        AppIcon::LayoutGrid => {
            r#"<rect width="7" height="7" x="3" y="3" rx="1.5"/><rect width="7" height="7" x="14" y="3" rx="1.5"/><rect width="7" height="7" x="14" y="14" rx="1.5"/><rect width="7" height="7" x="3" y="14" rx="1.5"/>"#
        }
        AppIcon::Settings => {
            r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"#
        }
        AppIcon::Activity => {
            r#"<path d="M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2"/>"#
        }
        AppIcon::Terminal => {
            r#"<polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/>"#
        }
        AppIcon::Shield => {
            r#"<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/>"#
        }
        AppIcon::Play => r#"<polygon points="6 3 20 12 6 21 6 3"/>"#,
        AppIcon::Monitor => {
            r#"<rect width="20" height="14" x="2" y="3" rx="2"/><line x1="8" x2="16" y1="21" y2="21"/><line x1="12" x2="12" y1="17" y2="21"/>"#
        }
        AppIcon::RefreshCw => {
            r#"<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/>"#
        }
        AppIcon::Power => r#"<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/>"#,
        AppIcon::Plus => r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#,
        AppIcon::Pin => {
            r#"<path d="M12 17v5"/><path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"/>"#
        }
        AppIcon::Close => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
        AppIcon::Trash2 => {
            r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/><line x1="10" x2="10" y1="11" y2="17"/><line x1="14" x2="14" y1="11" y2="17"/>"#
        }
        AppIcon::ChevronLeft => r#"<path d="m15 18-6-6 6-6"/>"#,
        AppIcon::ChevronRight => r#"<path d="m9 18 6-6-6-6"/>"#,
        AppIcon::Layers => {
            r#"<path d="m12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83Z"/><path d="m22 17.65-9.17 4.16a2 2 0 0 1-1.66 0L2 17.65"/><path d="m22 12.65-9.17 4.16a2 2 0 0 1-1.66 0L2 12.65"/>"#
        }
    }
}

type CacheKey = (AppIcon, u32, u8, u8, u8, u8);
static SVG_CACHE: OnceLock<Mutex<HashMap<CacheKey, Arc<Vec<u8>>>>> = OnceLock::new();

/// Rasterizes a Lucide SVG icon at `size_px x size_px` into premultiplied **BGRA32** bytes.
/// Results are cached in memory for zero-allocation redraws.
pub fn render_svg_premul_bgra(
    icon: AppIcon,
    size_px: u32,
    color: Rgb,
    alpha: f32,
) -> Option<Arc<Vec<u8>>> {
    let size = size_px.clamp(8, 256);
    let alpha_u8 = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    let key = (icon, size, color.0, color.1, color.2, alpha_u8);
    let cache = SVG_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(hit) = guard.get(&key)
    {
        return Some(Arc::clone(hit));
    }

    let hex = format!("#{:02x}{:02x}{:02x}", color.0, color.1, color.2);
    let inner = lucide_inner(icon).replace("currentColor", &hex);
    let stroke_w = if size <= 22 { "2.15" } else { "2.0" };
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 24 24" fill="none" stroke="{hex}" stroke-width="{stroke_w}" stroke-linecap="round" stroke-linejoin="round" shape-rendering="geometricPrecision" opacity="{a:.3}">{inner}</svg>"#,
        a = f32::from(alpha_u8) / 255.0
    );

    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(&svg, &opts).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / 24.0;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // `tiny-skia` outputs linear premultiplied RGBA; convert to premultiplied BGRA with a subtle
    // perceptual alpha lift (`powf(0.76)`) so thin strokes stay razor-sharp on dark surfaces.
    let mut bgra = pixmap.take();
    for px in bgra.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        let a_f = f32::from(a) / 255.0;
        let new_a_f = a_f.powf(0.76).clamp(0.0, 1.0);
        let gain = new_a_f / a_f;
        let r = (f32::from(px[0]) * gain).round().min(255.0) as u8;
        let g = (f32::from(px[1]) * gain).round().min(255.0) as u8;
        let b = (f32::from(px[2]) * gain).round().min(255.0) as u8;
        px[0] = b;
        px[1] = g;
        px[2] = r;
        px[3] = (new_a_f * 255.0).round().min(255.0) as u8;
    }
    let arc = Arc::new(bgra);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, Arc::clone(&arc));
    }
    Some(arc)
}

/// Blits an anti-aliased Lucide SVG icon onto a Win32 GDI `HDC`, alpha-composited over
/// `bg_colorref` (`0x00BBGGRR`) centered at `(cx, cy)`.
pub unsafe fn draw_svg_icon_gdi(
    hdc: HDC,
    icon: AppIcon,
    cx: i32,
    cy: i32,
    size_px: i32,
    fg_colorref: u32,
    bg_colorref: u32,
) {
    let sz = size_px.max(8) as u32;
    let fg_rgb = (
        (fg_colorref & 0xff) as u8,
        ((fg_colorref >> 8) & 0xff) as u8,
        ((fg_colorref >> 16) & 0xff) as u8,
    );
    let Some(premul_bgra) = render_svg_premul_bgra(icon, sz, fg_rgb, 1.0) else {
        return;
    };

    let bg_r = bg_colorref & 0xff;
    let bg_g = (bg_colorref >> 8) & 0xff;
    let bg_b = (bg_colorref >> 16) & 0xff;

    let mut flat = vec![0u8; premul_bgra.len()];
    for (src, dst) in premul_bgra.chunks_exact(4).zip(flat.chunks_exact_mut(4)) {
        let a = u32::from(src[3]);
        let inv = 255 - a;
        dst[0] = (u32::from(src[0]) + (bg_b * inv + 127) / 255).min(255) as u8;
        dst[1] = (u32::from(src[1]) + (bg_g * inv + 127) / 255).min(255) as u8;
        dst[2] = (u32::from(src[2]) + (bg_r * inv + 127) / 255).min(255) as u8;
        dst[3] = 255;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: sz as i32,
            biHeight: -(sz as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        },
        ..unsafe { std::mem::zeroed() }
    };
    let dst_x = cx - (sz as i32) / 2;
    let dst_y = cy - (sz as i32) / 2;
    unsafe {
        StretchDIBits(
            hdc,
            dst_x,
            dst_y,
            sz as i32,
            sz as i32,
            0,
            0,
            sz as i32,
            sz as i32,
            flat.as_ptr().cast(),
            &bmi,
            DIB_RGB_COLORS,
            0x00CC_0020, // SRCCOPY
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{AppIcon, render_svg_premul_bgra};

    #[test]
    fn all_lucide_icons_parse_and_rasterize_non_empty() {
        let all = [
            AppIcon::Search,
            AppIcon::TaskView,
            AppIcon::TrayChevron,
            AppIcon::RecycleBinEmpty,
            AppIcon::RecycleBinFull,
            AppIcon::Sliders,
            AppIcon::Music,
            AppIcon::Volume2,
            AppIcon::VolumeX,
            AppIcon::SunCloud,
            AppIcon::Battery,
            AppIcon::Bluetooth,
            AppIcon::HardDrive,
            AppIcon::Timer,
            AppIcon::Folder,
            AppIcon::LayoutGrid,
            AppIcon::Settings,
            AppIcon::Activity,
            AppIcon::Terminal,
            AppIcon::Shield,
            AppIcon::Play,
            AppIcon::Monitor,
            AppIcon::RefreshCw,
            AppIcon::Power,
            AppIcon::Plus,
            AppIcon::Pin,
            AppIcon::Close,
            AppIcon::Trash2,
            AppIcon::ChevronLeft,
            AppIcon::ChevronRight,
            AppIcon::Layers,
        ];
        for icon in all {
            let bytes = render_svg_premul_bgra(icon, 24, (0xf4, 0xf4, 0xf5), 1.0)
                .unwrap_or_else(|| panic!("SVG failed to rasterize: {icon:?}"));
            assert_eq!(bytes.len(), 24 * 24 * 4);
            assert!(
                bytes.chunks_exact(4).any(|px| px[3] > 0),
                "SVG rendered fully transparent: {icon:?}"
            );
        }
    }
}
