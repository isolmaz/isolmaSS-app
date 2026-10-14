//! Anti-aliased GDI+ strokes for annotations. Every function returns `false`
//! without leaving a partial drawing behind when GDI+ cannot be used, so callers fall
//! back to plain GDI.

use super::ArrowGeometry;
use crate::capture::Rect;
use std::cell::RefCell;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::Graphics::GdiPlus::{
    FillModeAlternate, GdipCreateFromHDC, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush,
    GdipDeleteGraphics, GdipDeletePen, GdipDrawLines, GdipDrawRectangle, GdipFillPolygon,
    GdipSetPenEndCap, GdipSetPenLineJoin, GdipSetPenStartCap, GdipSetPixelOffsetMode,
    GdipSetSmoothingMode, GpGraphics, GpPen, GpSolidFill, LineCapRound, LineJoinRound,
    Ok as StatusOk, PixelOffsetModeHalf, PointF, SmoothingModeAntiAlias8x8, Status, UnitPixel,
};

/// `Some(())` when a GDI+ call succeeded, so a chain of calls stops at the first failure.
fn ok(status: Status) -> Option<()> {
    (status == StatusOk).then_some(())
}

/// Anti-aliased drawing surface over a device context. Deleting it flushes the drawing,
/// so every pixel is in the DIB again before any CPU pass reads it.
struct Graphics(*mut GpGraphics);

impl Graphics {
    fn new(hdc: HDC) -> Option<Self> {
        crate::drawing::gdiplus_ready().then_some(())?;
        let mut raw = std::ptr::null_mut();
        ok(unsafe { GdipCreateFromHDC(hdc, &mut raw) })?;
        let graphics = Self(raw);
        unsafe {
            GdipSetSmoothingMode(raw, SmoothingModeAntiAlias8x8);
            // Pixel `i` spans `[i, i + 1)`, as in `drawing::smooth_rounded`.
            GdipSetPixelOffsetMode(raw, PixelOffsetModeHalf);
        }
        Some(graphics)
    }
}

impl Drop for Graphics {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteGraphics(self.0);
        }
    }
}

struct Pen(*mut GpPen);

impl Pen {
    /// A `thickness` px pen; `round` gives round caps and joins, otherwise corners stay
    /// mitred like a GDI frame.
    fn new(color: [u8; 4], thickness: i32, round: bool) -> Option<Self> {
        let mut raw = std::ptr::null_mut();
        ok(unsafe { GdipCreatePen1(argb(color), thickness.max(1) as f32, UnitPixel, &mut raw) })?;
        let pen = Self(raw);
        if round {
            unsafe {
                GdipSetPenStartCap(raw, LineCapRound);
                GdipSetPenEndCap(raw, LineCapRound);
                GdipSetPenLineJoin(raw, LineJoinRound);
            }
        }
        Some(pen)
    }
}

impl Drop for Pen {
    fn drop(&mut self) {
        unsafe {
            GdipDeletePen(self.0);
        }
    }
}

struct Brush(*mut GpSolidFill);

impl Brush {
    fn new(color: [u8; 4]) -> Option<Self> {
        let mut raw = std::ptr::null_mut();
        ok(unsafe { GdipCreateSolidFill(argb(color), &mut raw) })?;
        Some(Self(raw))
    }
}

impl Drop for Brush {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteBrush(self.0.cast());
        }
    }
}

/// Opaque `0xAARRGGBB` for a BGRA colour; plain GDI ignores alpha, so does this.
fn argb(bgra: [u8; 4]) -> u32 {
    0xff00_0000 | ((bgra[2] as u32) << 16) | ((bgra[1] as u32) << 8) | bgra[0] as u32
}

/// Offset that puts a `thickness` px GDI+ line where plain GDI puts the pen: GDI centres
/// odd widths on the pixel and even widths on the pixel boundary towards the top-left.
/// Axis-aligned strokes therefore cover exactly the same whole pixels as before.
fn pen_center(thickness: i32) -> f32 {
    if thickness.max(1) % 2 == 1 { 0.5 } else { 0.0 }
}

fn point(x: f64, y: f64, center: f32) -> PointF {
    PointF {
        X: x as f32 + center,
        Y: y as f32 + center,
    }
}

/// `(x, y, width, height)` of the path GDI's `Rectangle` strokes for `rect`: from
/// `(left, top)` to `(right - 1, bottom - 1)`, the right and bottom edges being exclusive.
fn frame_outline(rect: Rect, thickness: i32) -> (f32, f32, f32, f32) {
    let center = pen_center(thickness);
    (
        rect.left as f32 + center,
        rect.top as f32 + center,
        (rect.right - 1 - rect.left) as f32,
        (rect.bottom - 1 - rect.top) as f32,
    )
}

/// Hollow frame with mitred corners.
pub(super) fn frame(hdc: HDC, rect: Rect, color: [u8; 4], thickness: i32) -> bool {
    // GDI draws nothing for an empty rectangle and a thin sliver is not worth smoothing.
    if rect.width() < 2 || rect.height() < 2 {
        return false;
    }
    draw_frame(hdc, rect, color, thickness).is_some()
}

fn draw_frame(hdc: HDC, rect: Rect, color: [u8; 4], thickness: i32) -> Option<()> {
    let graphics = Graphics::new(hdc)?;
    let pen = Pen::new(color, thickness, false)?;
    let (x, y, width, height) = frame_outline(rect, thickness);
    ok(unsafe { GdipDrawRectangle(graphics.0, pen.0, x, y, width, height) })
}

/// Freehand stroke through every input point, with round caps and joins.
pub(super) fn polyline(hdc: HDC, points: &[(i32, i32)], color: [u8; 4], thickness: i32) -> bool {
    draw_polyline(hdc, points, color, thickness).is_some()
}

fn draw_polyline(hdc: HDC, points: &[(i32, i32)], color: [u8; 4], thickness: i32) -> Option<()> {
    thread_local! { static SCRATCH: RefCell<Vec<PointF>> = const { RefCell::new(Vec::new()) }; }
    let graphics = Graphics::new(hdc)?;
    let pen = Pen::new(color, thickness, true)?;
    let center = pen_center(thickness);
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.clear();
        scratch.extend(
            points
                .iter()
                .map(|&(x, y)| point(x as f64, y as f64, center)),
        );
        ok(unsafe { GdipDrawLines(graphics.0, pen.0, scratch.as_ptr(), scratch.len() as i32) })
    })
}

/// Round-capped shaft from `start` to the head's base plus the filled head triangle.
pub(super) fn arrow(
    hdc: HDC,
    start: (i32, i32),
    geometry: &ArrowGeometry,
    color: [u8; 4],
    thickness: i32,
) -> bool {
    draw_arrow(hdc, start, geometry, color, thickness).is_some()
}

fn draw_arrow(
    hdc: HDC,
    start: (i32, i32),
    geometry: &ArrowGeometry,
    color: [u8; 4],
    thickness: i32,
) -> Option<()> {
    let graphics = Graphics::new(hdc)?;
    let pen = Pen::new(color, thickness, true)?;
    let brush = Brush::new(color)?;
    let center = pen_center(thickness);
    let shaft = [
        point(start.0 as f64, start.1 as f64, center),
        point(geometry.base.0, geometry.base.1, center),
    ];
    let head = geometry.head.map(|(x, y)| point(x, y, center));
    unsafe {
        ok(GdipDrawLines(
            graphics.0,
            pen.0,
            shaft.as_ptr(),
            shaft.len() as i32,
        ))?;
        ok(GdipFillPolygon(
            graphics.0,
            brush.0.cast(),
            head.as_ptr(),
            head.len() as i32,
            FillModeAlternate,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_ok_status_counts_as_success() {
        use windows::Win32::Graphics::GdiPlus::{GenericError, InvalidParameter};
        assert_eq!(ok(StatusOk), Some(()));
        assert_eq!(ok(InvalidParameter), None);
        assert_eq!(ok(GenericError), None);
    }

    #[test]
    fn argb_is_opaque_and_orders_channels() {
        // BGRA input; alpha is dropped because GDI never used it.
        assert_eq!(argb([0x11, 0x22, 0x33, 0x44]), 0xff33_2211);
        assert_eq!(argb([49, 49, 224, 255]), 0xffe0_3131);
    }

    #[test]
    fn pen_center_follows_gdi_pen_parity() {
        for (thickness, center) in [(0, 0.5), (1, 0.5), (2, 0.0), (3, 0.5), (4, 0.0), (7, 0.5)] {
            assert_eq!(pen_center(thickness), center, "thickness {thickness}");
        }
    }

    #[test]
    fn frame_outline_matches_the_pixels_gdi_rectangle_strokes() {
        let rect = Rect::new(20, 30, 60, 50);
        // Odd widths centre on pixel centres, even widths on pixel boundaries, and the
        // path always ends at the last included pixel (`right - 1`, `bottom - 1`).
        assert_eq!(frame_outline(rect, 1), (20.5, 30.5, 39.0, 19.0));
        assert_eq!(frame_outline(rect, 2), (20.0, 30.0, 39.0, 19.0));
        assert_eq!(frame_outline(rect, 3), (20.5, 30.5, 39.0, 19.0));
        assert_eq!(frame_outline(rect, 4), (20.0, 30.0, 39.0, 19.0));
    }
}
