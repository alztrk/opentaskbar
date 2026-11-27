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
