//! Small error-path helpers shared by the Direct2D/DirectComposition drawing code.
//!
//! These keep a drawing surface from being left locked in its drawing state, and
//! keep a device-context transform from leaking after an early `?` return.

/// Runs `draw` and **always** calls `end` (the surface's `EndDraw`) before
/// propagating either error. This mirrors `dock/dcomp.rs::draw_surface`: a
/// failed brush creation inside `BeginDraw`/`EndDraw` must not leave a
/// `CompositionDrawingSurface` locked, which would make every later repaint
/// fail and freeze the glass layer.
pub fn with_surface_draw<R, E>(
    draw: impl FnOnce() -> Result<R, E>,
    end: impl FnOnce() -> Result<(), E>,
) -> Result<R, E> {
    let draw_res = draw();
    // EndDraw is unconditional and happens before any error is propagated.
    let end_res = end();
    match draw_res {
        Ok(value) => end_res.map(|()| value),
        // The draw error is the primary cause; a concurrent EndDraw error is
        // still reported when the draw itself succeeded.
        Err(err) => Err(err),
    }
}

/// Restores a saved device-context transform when it goes out of scope,
/// including on early `?` returns. The setter is a closure so this works with
/// both `ID2D1DeviceContext` and lightweight test doubles.
pub struct TransformGuard<'a, T, S: Fn(&T)> {
    previous: &'a T,
    set: S,
}

impl<'a, T, S: Fn(&T)> TransformGuard<'a, T, S> {
    pub fn new(previous: &'a T, set: S) -> Self {
        Self { previous, set }
    }
}

impl<T, S: Fn(&T)> Drop for TransformGuard<'_, T, S> {
    fn drop(&mut self) {
        (self.set)(self.previous);
    }
}

/// Converts a design `Rgb` + alpha into the normalized Direct2D colour used by
/// every dock popup/renderer. Keeps the conversion math in one place instead of
/// five near-identical local helpers.
#[cfg(windows)]
pub fn d2d_color(
    rgb: super::Rgb,
    alpha: f32,
) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    d2d_color_channels(rgb.0, rgb.1, rgb.2, alpha)
}

#[cfg(windows)]
pub fn d2d_color_channels(
    r: u8,
    g: u8,
    b: u8,
    alpha: f32,
) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: alpha.clamp(0.0, 1.0),
    }
}

#[cfg(windows)]
pub use dc_renderer::{DcD2d, TextAlign, d2d_rect, paint_dib_d2d};

#[cfg(windows)]
mod dc_renderer {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use windows::Win32::Foundation::RECT as WinRect;
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    };
    use windows::Win32::Graphics::Direct2D::{
        D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
        D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE,
        D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
        D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
        D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
        D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory1, ID2D1SolidColorBrush,
    };
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
        DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
        DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
        DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
        DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_RANGE,
        DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
        DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTypography,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Gdi::HDC as WinHdc;
    use windows::core::HSTRING;
    use windows_numerics::Vector2;
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CreateCompatibleDC,
        CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, PAINTSTRUCT, SRCCOPY,
        SelectObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

    use crate::design::spatial_glass::DCompForeground;
    use crate::design::{fonts, icons};

    #[inline]
    pub fn d2d_rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
        D2D_RECT_F {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[derive(Clone, Copy)]
    pub enum TextAlign {
        Left,
        Center,
        Right,
    }

    impl TextAlign {
        const fn code(self) -> u8 {
            match self {
                Self::Left => 0,
                Self::Center => 1,
                Self::Right => 2,
            }
        }
    }

    /// Thread-local Direct2D + DirectWrite renderer bound to a 32-bit DIB section.
    ///
    /// Shared across dock flyouts, live thumbnail previews, and the window picker so
    /// every popup gets cached DirectWrite formats, tabular numerals, and antialiased
    /// primitives without duplicating GDI/DIB/D2D setup.
    pub struct DcD2d {
        _factory: ID2D1Factory1,
        dwrite: IDWriteFactory,
        target: ID2D1DCRenderTarget,
        tabular: Option<IDWriteTypography>,
        formats: HashMap<(bool, u16, u16, u8, String), IDWriteTextFormat>,
        covers: HashMap<String, Option<ID2D1Bitmap>>,
    }

    thread_local! {
        static DC_D2D: RefCell<Option<DcD2d>> = const { RefCell::new(None) };
    }

    impl DcD2d {
        fn create() -> Option<Self> {
            unsafe {
                let factory: ID2D1Factory1 =
                    D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
                let dwrite: IDWriteFactory =
                    DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let target = factory.CreateDCRenderTarget(&props).ok()?;
                target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let tabular = dwrite.CreateTypography().ok().inspect(|typo| {
                    let _ = typo.AddFontFeature(DWRITE_FONT_FEATURE {
                        nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                        parameter: 1,
                    });
                });
                Some(Self {
                    _factory: factory,
                    dwrite,
                    target,
                    tabular,
                    formats: HashMap::new(),
                    covers: HashMap::new(),
                })
            }
        }

        pub fn target(&self) -> &ID2D1DCRenderTarget {
            &self.target
        }

        pub fn clear(&self, color: D2D1_COLOR_F) {
            unsafe {
                self.target.Clear(Some(&color));
            }
        }

        fn brush(&self, color: D2D1_COLOR_F) -> Option<ID2D1SolidColorBrush> {
            unsafe { self.target.CreateSolidColorBrush(&color, None).ok() }
        }

        fn text_format(
            &mut self,
            fluent: bool,
            size_px: f32,
            weight: DWRITE_FONT_WEIGHT,
            align: TextAlign,
        ) -> Option<IDWriteTextFormat> {
            let eff_size = if fluent {
                size_px.max(9.0)
            } else {
                fonts::scaled_size(size_px).max(9.0)
            };
            let family = if fluent {
                icons::icon_font_family().to_owned()
            } else {
                fonts::ui_family_list()
            };
            let key = (
                fluent,
                (eff_size * 2.0).round() as u16,
                weight.0 as u16,
                align.code(),
                family.clone(),
            );
            if let Some(fmt) = self.formats.get(&key) {
                return Some(fmt.clone());
            }
            let locale = if fluent {
                "en-US".to_owned()
            } else {
                fonts::ui_locale()
            };
            unsafe {
                let fmt = self
                    .dwrite
                    .CreateTextFormat(
                        &HSTRING::from(family.as_str()),
                        None,
                        weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        eff_size,
                        &HSTRING::from(locale.as_str()),
                    )
                    .ok()?;
                let _ = fmt.SetTextAlignment(match align {
                    TextAlign::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                    TextAlign::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                    TextAlign::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
                });
                let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                let sign = self.dwrite.CreateEllipsisTrimmingSign(&fmt).ok();
                let _ = fmt.SetTrimming(&trimming, sign.as_ref());
                if self.formats.len() > 96 {
                    self.formats.clear();
                }
                self.formats.insert(key, fmt.clone());
                Some(fmt)
            }
        }

        pub fn fill_rect(&self, l: f32, t: f32, r: f32, b: f32, color: D2D1_COLOR_F) {
            if r <= l || b <= t {
                return;
            }
            if let Some(brush) = self.brush(color) {
                let rect = d2d_rect(l, t, r, b);
                unsafe {
                    self.target.FillRectangle(&rect, &brush);
                }
            }
        }

        pub fn fill_rrect(&self, l: f32, t: f32, r: f32, b: f32, rad: f32, color: D2D1_COLOR_F) {
            if r <= l || b <= t {
                return;
            }
            if let Some(brush) = self.brush(color) {
                let rr = D2D1_ROUNDED_RECT {
                    rect: d2d_rect(l, t, r, b),
                    radiusX: rad,
                    radiusY: rad,
                };
                unsafe {
                    self.target.FillRoundedRectangle(&rr, &brush);
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn stroke_rrect(
            &self,
            l: f32,
            t: f32,
            r: f32,
            b: f32,
            rad: f32,
            color: D2D1_COLOR_F,
            stroke_w: f32,
        ) {
            if stroke_w <= 0.0 || r <= l || b <= t {
                return;
            }
            if let Some(brush) = self.brush(color) {
                let inset = stroke_w * 0.5;
                let rr = D2D1_ROUNDED_RECT {
                    rect: d2d_rect(l + inset, t + inset, r - inset, b - inset),
                    radiusX: (rad - inset).max(0.0),
                    radiusY: (rad - inset).max(0.0),
                };
                unsafe {
                    self.target
                        .DrawRoundedRectangle(&rr, &brush, stroke_w, None);
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn fill_rrect_bordered(
            &self,
            l: f32,
            t: f32,
            r: f32,
            b: f32,
            rad: f32,
            fill: D2D1_COLOR_F,
            border: D2D1_COLOR_F,
            stroke_w: f32,
        ) {
            self.fill_rrect(l, t, r, b, rad, fill);
            self.stroke_rrect(l, t, r, b, rad, border, stroke_w);
        }

        pub fn fill_circle(&self, cx: f32, cy: f32, rad: f32, color: D2D1_COLOR_F) {
            if rad <= 0.0 {
                return;
            }
            if let Some(brush) = self.brush(color) {
                let ell = D2D1_ELLIPSE {
                    point: Vector2 { X: cx, Y: cy },
                    radiusX: rad,
                    radiusY: rad,
                };
                unsafe {
                    self.target.FillEllipse(&ell, &brush);
                }
            }
        }

        pub fn stroke_circle(
            &self,
            cx: f32,
            cy: f32,
            rad: f32,
            color: D2D1_COLOR_F,
            stroke_w: f32,
        ) {
            if rad <= 0.0 || stroke_w <= 0.0 {
                return;
            }
            if let Some(brush) = self.brush(color) {
                let ell = D2D1_ELLIPSE {
                    point: Vector2 { X: cx, Y: cy },
                    radiusX: rad,
                    radiusY: rad,
                };
                unsafe {
                    self.target.DrawEllipse(&ell, &brush, stroke_w, None);
                }
            }
        }

        pub fn draw_line(
            &self,
            x0: f32,
            y0: f32,
            x1: f32,
            y1: f32,
            color: D2D1_COLOR_F,
            stroke_w: f32,
        ) {
            if let Some(brush) = self.brush(color) {
                unsafe {
                    self.target.DrawLine(
                        Vector2 { X: x0, Y: y0 },
                        Vector2 { X: x1, Y: y1 },
                        &brush,
                        stroke_w,
                        None,
                    );
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn draw_text(
            &mut self,
            text: &str,
            rect: D2D_RECT_F,
            size_px: f32,
            weight: DWRITE_FONT_WEIGHT,
            align: TextAlign,
            color: D2D1_COLOR_F,
            tabular: bool,
        ) {
            if text.is_empty() || rect.right <= rect.left || rect.bottom <= rect.top {
                return;
            }
            let Some(fmt) = self.text_format(false, size_px, weight, align) else {
                return;
            };
            let Some(brush) = self.brush(color) else {
                return;
            };
            let wtxt: Vec<u16> = text.encode_utf16().collect();
            unsafe {
                if let Ok(layout) = self.dwrite.CreateTextLayout(
                    &wtxt,
                    &fmt,
                    (rect.right - rect.left).max(1.0),
                    (rect.bottom - rect.top).max(1.0),
                ) {
                    if tabular && let Some(typo) = &self.tabular {
                        let _ = layout.SetTypography(
                            typo,
                            DWRITE_TEXT_RANGE {
                                startPosition: 0,
                                length: wtxt.len() as u32,
                            },
                        );
                    }
                    self.target.DrawTextLayout(
                        Vector2 {
                            X: rect.left,
                            Y: rect.top,
                        },
                        &layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    );
                }
            }
        }

        pub fn draw_glyph(
            &mut self,
            glyph: char,
            rect: D2D_RECT_F,
            size_px: f32,
            color: D2D1_COLOR_F,
        ) {
            let Some(fmt) =
                self.text_format(true, size_px, DWRITE_FONT_WEIGHT_NORMAL, TextAlign::Center)
            else {
                return;
            };
            let Some(brush) = self.brush(color) else {
                return;
            };
            let mut buf = [0u16; 2];
            let encoded = glyph.encode_utf16(&mut buf);
            unsafe {
                self.target.DrawText(
                    encoded,
                    &fmt,
                    &rect,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }

        fn create_premul_bitmap(&self, w: u32, h: u32, premul: &[u8]) -> Option<ID2D1Bitmap> {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            unsafe {
                self.target
                    .CreateBitmap(
                        D2D_SIZE_U {
                            width: w,
                            height: h,
                        },
                        Some(premul.as_ptr().cast()),
                        w * 4,
                        &props,
                    )
                    .ok()
            }
        }

        /// Draws a straight-alpha BGRA32 buffer into `dst` (premultiplies for D2D).
        pub fn draw_bgra_bitmap(
            &self,
            width: u32,
            height: u32,
            straight_bgra: &[u8],
            dst: D2D_RECT_F,
        ) {
            if width == 0
                || height == 0
                || straight_bgra.len() < (width as usize) * (height as usize) * 4
            {
                return;
            }
            let mut premul = Vec::with_capacity(straight_bgra.len());
            for px in straight_bgra.as_chunks::<4>().0 {
                let a = u16::from(px[3]);
                premul.push(((u16::from(px[0]) * a + 127) / 255) as u8);
                premul.push(((u16::from(px[1]) * a + 127) / 255) as u8);
                premul.push(((u16::from(px[2]) * a + 127) / 255) as u8);
                premul.push(px[3]);
            }
            if let Some(bitmap) = self.create_premul_bitmap(width, height, &premul) {
                unsafe {
                    self.target.DrawBitmap(
                        &bitmap,
                        Some(&dst),
                        1.0,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
            }
        }

        fn load_cover_bitmap(&self, path: &str) -> Option<ID2D1Bitmap> {
            let img = image::open(path).ok()?.to_rgba8();
            let (w, h) = (img.width(), img.height());
            if w == 0 || h == 0 {
                return None;
            }
            let mut premul = Vec::with_capacity(w as usize * h as usize * 4);
            for px in img.as_raw().as_chunks::<4>().0 {
                let a = u16::from(px[3]);
                premul.push(((u16::from(px[2]) * a + 127) / 255) as u8);
                premul.push(((u16::from(px[1]) * a + 127) / 255) as u8);
                premul.push(((u16::from(px[0]) * a + 127) / 255) as u8);
                premul.push(px[3]);
            }
            self.create_premul_bitmap(w, h, &premul)
        }

        pub fn draw_cover_image(&mut self, path: &str, dst: D2D_RECT_F) {
            if !self.covers.contains_key(path) {
                let bitmap = self.load_cover_bitmap(path);
                self.covers.insert(path.to_owned(), bitmap);
            }
            let Some(Some(bitmap)) = self.covers.get(path) else {
                return;
            };
            unsafe {
                self.target.DrawBitmap(
                    bitmap,
                    Some(&dst),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        }
    }

    /// Runs `BeginPaint`, allocates a 32-bit top-down DIB section, binds the thread-local
    /// [`DcD2d`] render target, invokes `draw(d2d, width, height)`, and presents via
    /// `DCompForeground::upload_bgra` (when `dcomp_fg` is `Some`) or `BitBlt`.
    pub fn paint_dib_d2d(
        hwnd: HWND,
        dcomp_fg: Option<&mut DCompForeground>,
        draw: impl FnOnce(&mut DcD2d, i32, i32),
    ) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let screen_hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        if screen_hdc.is_null() {
            return;
        }
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client) };
        let cw = (client.right - client.left).max(1);
        let ch = (client.bottom - client.top).max(1);

        let mem_dc = unsafe { CreateCompatibleDC(screen_hdc) };
        if mem_dc.is_null() {
            unsafe { EndPaint(hwnd, &ps) };
            return;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: cw,
                biHeight: -ch,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..unsafe { std::mem::zeroed() }
            },
            ..unsafe { std::mem::zeroed() }
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mem_bmp = unsafe {
            CreateDIBSection(
                mem_dc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if mem_bmp.is_null() {
            unsafe {
                DeleteDC(mem_dc);
                EndPaint(hwnd, &ps);
            }
            return;
        }
        let old_bmp = unsafe { SelectObject(mem_dc, mem_bmp as _) };

        DC_D2D.with(|slot| {
            let mut guard = slot.borrow_mut();
            if guard.is_none() {
                *guard = DcD2d::create();
            }
            let Some(d2d) = guard.as_mut() else {
                return;
            };
            let win_rc = WinRect {
                left: 0,
                top: 0,
                right: cw,
                bottom: ch,
            };
            if unsafe { d2d.target.BindDC(WinHdc(mem_dc as _), &win_rc) }.is_err() {
                *guard = None;
                return;
            }
            unsafe { d2d.target.BeginDraw() };
            draw(d2d, cw, ch);
            if unsafe { d2d.target.EndDraw(None, None) }.is_err() {
                *guard = None;
            }
        });

        if let Some(fg) = dcomp_fg
            && !bits.is_null()
        {
            let byte_len = (cw as usize).saturating_mul(ch as usize).saturating_mul(4);
            let pixels = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), byte_len) };
            let _ = fg.upload_bgra(cw as u32, ch as u32, pixels);
        } else {
            unsafe {
                let _ = BitBlt(screen_hdc, 0, 0, cw, ch, mem_dc, 0, 0, SRCCOPY);
            }
        }

        unsafe {
            SelectObject(mem_dc, old_bmp);
            DeleteObject(mem_bmp as _);
            DeleteDC(mem_dc);
            EndPaint(hwnd, &ps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn end_draw_runs_even_when_draw_fails() {
        let ended = RefCell::new(0);
        let result: Result<(), &str> = with_surface_draw(
            || Err("brush creation failed"),
            || {
                *ended.borrow_mut() += 1;
                Ok(())
            },
        );
        assert_eq!(result, Err("brush creation failed"));
        assert_eq!(*ended.borrow(), 1, "EndDraw must run on the error path");
    }

    #[test]
    fn end_draw_error_is_reported_when_draw_succeeds() {
        let result: Result<u8, &str> = with_surface_draw(|| Ok(7), || Err("EndDraw failed"));
        assert_eq!(result, Err("EndDraw failed"));
    }

    #[test]
    fn successful_draw_returns_its_value() {
        let result: Result<u8, &str> = with_surface_draw(|| Ok(42), || Ok(()));
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn transform_is_restored_when_a_brush_fails() {
        // Models `SetTransform(&combined)` followed by a failing
        // `CreateSolidColorBrush(...)?`: the guard must put `previous` back.
        let current = RefCell::new(99.0_f32);
        let previous = 1.0_f32;
        *current.borrow_mut() = 2.0;
        let restore_fails = || -> Result<(), &str> {
            let _guard = TransformGuard::new(&previous, |value| *current.borrow_mut() = *value);
            // Simulated brush-creation failure.
            Err("brush failed")?;
            Ok(())
        };
        assert_eq!(restore_fails(), Err("brush failed"));
        assert_eq!(
            *current.borrow(),
            previous,
            "transform must be restored on the error path"
        );
    }
}
