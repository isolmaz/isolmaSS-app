use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, RGBQUAD,
    ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows::core::{Error, Result};

/// An integer rectangle with inclusive-exclusive bounds.
/// In our coordinate system:
/// - `left` and `top` are inclusive.
/// - `right` and `bottom` are exclusive (e.g. width = right - left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Hit-testing zones on a committed selection rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionHitZone {
    TopLeftCorner,
    TopRightCorner,
    BottomLeftCorner,
    BottomRightCorner,
    TopEdge,
    RightEdge,
    BottomEdge,
    LeftEdge,
    BorderEdge,
    Interior,
    None,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    #[inline]
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    #[inline]
    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    /// Normalizes two arbitrary points (e.g. drag start and drag current) into a top-left/bottom-right rect.
    pub fn normalized(p1: (i32, i32), p2: (i32, i32)) -> Self {
        let left = p1.0.min(p2.0);
        let right = p1.0.max(p2.0);
        let top = p1.1.min(p2.1);
        let bottom = p1.1.max(p2.1);
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Clamps the rectangle to fit within `[0, max_width)` and `[0, max_height)`.
    pub fn clamp(&self, max_width: i32, max_height: i32) -> Self {
        let max_width = max_width.max(0);
        let max_height = max_height.max(0);
        let left = self.left.clamp(0, max_width);
        let right = self.right.clamp(0, max_width);
        let top = self.top.clamp(0, max_height);
        let bottom = self.bottom.clamp(0, max_height);
        Self {
            left,
            top,
            right: right.max(left),
            bottom: bottom.max(top),
        }
    }

    /// Computes the bounding box of `self` and `other`.
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    /// Inflates the rectangle by `dx` horizontally and `dy` vertically.
    pub fn inflate(&self, dx: i32, dy: i32) -> Self {
        Self {
            left: self.left - dx,
            top: self.top - dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }

    /// Checks if a point `(x, y)` is within this rectangle.
    #[inline]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// Thickness of the accent frame around a selection at `dpi`.
    pub fn selection_frame_thickness(dpi: u32) -> i32 {
        (2 * dpi / 96).max(2) as i32
    }

    /// Outer rectangle of the accent frame around this selection. The frame ring hugs the
    /// selection from outside so every exported pixel stays visible; grips are centred on
    /// this rectangle's corners and edge midpoints, and hit-testing must use it too.
    pub fn selection_frame(&self, dpi: u32) -> Self {
        let thickness = Self::selection_frame_thickness(dpi);
        self.inflate(thickness, thickness)
    }

    /// Hit-tests a point `pt` against this selection rectangle:
    /// - 4 Corner handles (8x8 px centered on vertices) -> diagonal resize
    /// - 4 Border edge bands (6-8 px centered on edges, excluding corners) -> drag-to-move
    /// - Interior -> move (or draw)
    /// - Outside -> None
    pub fn hit_test_selection(
        &self,
        pt: (i32, i32),
        band: i32,
        handle_size: i32,
    ) -> SelectionHitZone {
        if self.is_empty() {
            return SelectionHitZone::None;
        }

        let half_h = handle_size / 2;
        let (x, y) = pt;

        // 1. 4 Corner handles (8x8 px centered on vertices)
        let tl = Rect::new(
            self.left - half_h,
            self.top - half_h,
            self.left + half_h,
            self.top + half_h,
        );
        if tl.contains(x, y) {
            return SelectionHitZone::TopLeftCorner;
        }
        let tr = Rect::new(
            self.right - half_h,
            self.top - half_h,
            self.right + half_h,
            self.top + half_h,
        );
        if tr.contains(x, y) {
            return SelectionHitZone::TopRightCorner;
        }
        let bl = Rect::new(
            self.left - half_h,
            self.bottom - half_h,
            self.left + half_h,
            self.bottom + half_h,
        );
        if bl.contains(x, y) {
            return SelectionHitZone::BottomLeftCorner;
        }
        let br = Rect::new(
            self.right - half_h,
            self.bottom - half_h,
            self.right + half_h,
            self.bottom + half_h,
        );
        if br.contains(x, y) {
            return SelectionHitZone::BottomRightCorner;
        }

        // Midpoint grips change one dimension; the remaining border still moves.
        let middle_x = self.left + self.width() / 2;
        let middle_y = self.top + self.height() / 2;
        for (zone, cx, cy) in [
            (SelectionHitZone::TopEdge, middle_x, self.top),
            (SelectionHitZone::RightEdge, self.right, middle_y),
            (SelectionHitZone::BottomEdge, middle_x, self.bottom),
            (SelectionHitZone::LeftEdge, self.left, middle_y),
        ] {
            if (x - cx).abs() <= half_h && (y - cy).abs() <= half_h {
                return zone;
            }
        }

        // Remaining border bands move the selection.
        let outer = self.inflate(band, band);
        let inner = self.inflate(-band, -band);

        if outer.contains(x, y)
            && (!inner.contains(x, y) || inner.width() <= 0 || inner.height() <= 0)
        {
            return SelectionHitZone::BorderEdge;
        }

        // 3. Interior of selection
        if self.contains(x, y) {
            return SelectionHitZone::Interior;
        }

        SelectionHitZone::None
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureTimings {
    pub setup: Duration,
    pub bit_blt: Duration,
    pub dim: Duration,
    pub total: Duration,
}

enum PixelStorage {
    Owned(Vec<u8>),
    Dib(DibSection),
}

pub struct PixelBuffer(PixelStorage);

impl std::ops::Deref for PixelBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            PixelStorage::Owned(bytes) => bytes,
            PixelStorage::Dib(section) => section.pixels(),
        }
    }
}

/// GDI steps that can fail without leaving a Win32 error code to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GdiFailure {
    NoBitmap,
    SelectBitmap,
    Flush,
}

impl From<GdiFailure> for Error {
    fn from(failure: GdiFailure) -> Self {
        let message = match failure {
            GdiFailure::NoBitmap => {
                crate::i18n::t("Windows did not allocate a usable screenshot bitmap.")
            }
            GdiFailure::SelectBitmap => {
                crate::i18n::t("Windows could not select the screenshot bitmap.")
            }
            GdiFailure::Flush => {
                crate::i18n::t("Windows could not finish drawing the captured screen.")
            }
        };
        Error::new(windows::core::HRESULT::from_win32(31), message)
    }
}

/// Win32 reports a failed device-context call with a null handle and the thread's last error.
fn valid_dc(dc: HDC) -> Result<HDC> {
    if dc.is_invalid() {
        Err(Error::from_win32())
    } else {
        Ok(dc)
    }
}

/// Size in bytes of a `width` x `height` 32-bit pixel buffer.
fn pixel_bytes(width: usize, height: usize) -> Result<usize> {
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            Error::new(
                windows::core::HRESULT::from_win32(534),
                crate::i18n::t("The virtual screen exceeds the supported pixel-buffer size."),
            )
        })
}

// RAII cleanup helper for HDC released via ReleaseDC
struct DcReleaseGuard {
    hwnd: HWND,
    hdc: HDC,
}

impl Drop for DcReleaseGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(self.hwnd, self.hdc);
        }
    }
}

/// A memory device context, deleted on drop.
struct MemoryDc(HDC);

impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// A GDI bitmap, deleted on drop.
struct OwnedBitmap(HBITMAP);

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}

/// A top-down 32-bit DIB section selected into its own memory device context. GDI draws
/// into it and the CPU reads the pixels in place, without a copy.
struct DibSection {
    // Declaration order is drop order: the device context goes before the bitmap it holds.
    memory_dc: MemoryDc,
    _bitmap: OwnedBitmap,
    old_bitmap: HGDIOBJ,
    bits: *mut u8,
    len: usize,
    width: i32,
    height: i32,
}

impl DibSection {
    /// Creates a `width` x `height` section whose device context is compatible with
    /// `compatible_with`.
    fn new(compatible_with: HDC, width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(Error::new(
                windows::core::HRESULT::from_win32(87),
                crate::i18n::t("The virtual screen has no capturable area."),
            ));
        }
        let len = pixel_bytes(width as usize, height as usize)?;

        let memory_dc = MemoryDc(valid_dc(unsafe { CreateCompatibleDC(compatible_with) })?);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // negative = top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD::default()],
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = OwnedBitmap(unsafe {
            CreateDIBSection(compatible_with, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?
        });
        let bits = std::ptr::NonNull::new(bits.cast::<u8>()).ok_or(GdiFailure::NoBitmap)?;

        let old_bitmap = unsafe { SelectObject(memory_dc.0, HGDIOBJ(bitmap.0.0)) };
        (!old_bitmap.is_invalid())
            .then_some(())
            .ok_or(GdiFailure::SelectBitmap)?;

        Ok(Self {
            memory_dc,
            _bitmap: bitmap,
            old_bitmap,
            bits: bits.as_ptr(),
            len,
            width,
            height,
        })
    }

    /// Copies the `width` x `height` area of `source` at `(x, y)` into the section.
    fn copy_from(&self, source: HDC, x: i32, y: i32) -> Result<()> {
        unsafe {
            BitBlt(
                self.memory_dc.0,
                0,
                0,
                self.width,
                self.height,
                source,
                x,
                y,
                SRCCOPY | CAPTUREBLT,
            )?;
        }
        // GDI batches writes to DIB sections. Complete them before reading pixels on the CPU.
        unsafe { GdiFlush() }
            .as_bool()
            .then_some(())
            .ok_or(GdiFailure::Flush)?;
        Ok(())
    }

    fn pixels(&self) -> &[u8] {
        // SAFETY: `bits` points at `len` bytes that live as long as `bitmap`.
        unsafe { std::slice::from_raw_parts(self.bits, self.len) }
    }
}

impl Drop for DibSection {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memory_dc.0, self.old_bitmap);
        }
    }
}

/// Holds the full virtual screen capture along with a pre-rendered dimmed buffer.
pub struct CaptureBuffer {
    /// Virtual screen X offset (can be negative on multi-monitor setups).
    pub x: i32,
    /// Virtual screen Y offset (can be negative on multi-monitor setups).
    pub y: i32,
    /// Virtual screen width in physical pixels.
    pub width: i32,
    /// Virtual screen height in physical pixels.
    pub height: i32,
    /// Original 32-bit BGRA pixel data. Length is `width * height * 4`.
    pub original: PixelBuffer,
    /// Pre-rendered dimmed 32-bit BGRA pixel data.
    pub dimmed: Vec<u8>,
    pub timings: CaptureTimings,
}

static BUFFER_POOL: LazyLock<Mutex<Option<Vec<u8>>>> = LazyLock::new(|| Mutex::new(None));
// Do not retain a multi-monitor screenshot's allocation while the daemon is idle.
const MAX_POOLED_BYTES: usize = 4 * 1024 * 1024;

fn take_dimmed_buffer(size: usize) -> Vec<u8> {
    let mut dimmed = BUFFER_POOL
        .lock()
        .ok()
        .and_then(|mut pool| pool.take())
        .unwrap_or_default();
    dimmed.resize(size, 0);
    dimmed
}

impl Drop for CaptureBuffer {
    fn drop(&mut self) {
        let dimmed = std::mem::take(&mut self.dimmed);
        if dimmed.capacity() <= MAX_POOLED_BYTES
            && let Ok(mut pool) = BUFFER_POOL.lock()
            && pool
                .as_ref()
                .is_none_or(|current| current.capacity() < dimmed.capacity())
        {
            *pool = Some(dimmed);
        }
    }
}

impl CaptureBuffer {
    /// Creates a dummy in-memory capture buffer for testing without capturing the display.
    pub fn dummy(width: i32, height: i32) -> Self {
        let len = (width.max(1) as usize) * (height.max(1) as usize) * 4;
        Self {
            x: 0,
            y: 0,
            width,
            height,
            original: PixelBuffer(PixelStorage::Owned(vec![0u8; len])),
            dimmed: vec![0u8; len],
            timings: CaptureTimings::default(),
        }
    }

    /// Captures the full virtual screen across all connected monitors via BitBlt.
    /// Pre-renders a dimmed backdrop buffer so interactive punch-outs are instant.
    pub fn capture_virtual_screen() -> Result<Self> {
        let total_start = Instant::now();
        let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

        // Desktop window DC
        let screen_dc = valid_dc(unsafe { GetDC(HWND::default()) })?;
        let _screen_guard = DcReleaseGuard {
            hwnd: HWND::default(),
            hdc: screen_dc,
        };
        Self::capture_from(total_start, screen_dc, x, y, width, height)
    }

    /// Copies the `width` x `height` area of `source` at `(x, y)` into a new capture
    /// and pre-renders its dimmed backdrop. `total_start` anchors `timings.total`.
    fn capture_from(
        total_start: Instant,
        source: HDC,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Result<Self> {
        let section = DibSection::new(source, width, height)?;
        let setup = total_start.elapsed();

        let bit_blt_start = Instant::now();
        section.copy_from(source, x, y)?;
        let bit_blt = bit_blt_start.elapsed();

        // Dim directly from the captured DIB; no full-screen copy is required.
        let mut dimmed = take_dimmed_buffer(section.len);
        let dim_start = Instant::now();
        Self::fill_dimmed_buffer(section.pixels(), &mut dimmed);
        let dim = dim_start.elapsed();

        Ok(Self {
            x,
            y,
            width,
            height,
            original: PixelBuffer(PixelStorage::Dib(section)),
            dimmed,
            timings: CaptureTimings {
                setup,
                bit_blt,
                dim,
                total: total_start.elapsed(),
            },
        })
    }

    /// Pre-renders a dimmed version of the original BGRA pixel buffer.
    /// Applies a ~45% dimming factor to B, G, R channels and sets Alpha to 255.
    fn fill_dimmed_buffer(original: &[u8], dimmed: &mut [u8]) {
        debug_assert_eq!(original.len(), dimmed.len());
        let dim_lut: [u8; 256] = std::array::from_fn(|index| ((index as u32 * 115) / 255) as u8);
        let (source_pixels, _) = original.as_chunks::<4>();
        let (destination_pixels, _) = dimmed.as_chunks_mut::<4>();
        for (source, destination) in source_pixels.iter().zip(destination_pixels.iter_mut()) {
            destination[0] = dim_lut[source[0] as usize];
            destination[1] = dim_lut[source[1] as usize];
            destination[2] = dim_lut[source[2] as usize];
            destination[3] = 255;
        }
    }

    /// Punches out an undimmed rectangle by copying rows from `source` (e.g. `original`)
    /// into `target` (e.g. `composed`).
    pub fn copy_rect(target: &mut [u8], width: i32, height: i32, rect: &Rect, source: &[u8]) {
        if rect.is_empty() || width <= 0 || height <= 0 {
            return;
        }

        let clamped = rect.clamp(width, height);
        if clamped.is_empty() {
            return;
        }

        let stride = (width as usize) * 4;
        let col_start = clamped.left as usize * 4;
        let col_bytes = (clamped.right - clamped.left) as usize * 4;

        for y in clamped.top..clamped.bottom {
            let row_offset = y as usize * stride;
            let start = row_offset + col_start;
            let end = start + col_bytes;

            if end <= target.len() && end <= source.len() {
                target[start..end].copy_from_slice(&source[start..end]);
            }
        }
    }

    /// Restores a rectangle to the pre-rendered dimmed state.
    pub fn restore_dimmed(&self, target: &mut [u8], rect: &Rect) {
        Self::copy_rect(target, self.width, self.height, rect, &self.dimmed);
    }

    /// Punches out a rectangle with the original undimmed pixels.
    pub fn punch_out(&self, target: &mut [u8], rect: &Rect) {
        Self::copy_rect(target, self.width, self.height, rect, &self.original);
    }

    /// Draws a crisp 1px or 2px rectangle border directly onto the 32-bit BGRA buffer.
    pub fn draw_border(
        target: &mut [u8],
        width: i32,
        height: i32,
        rect: &Rect,
        color_bgra: [u8; 4],
        thickness: i32,
    ) {
        if rect.is_empty() || thickness <= 0 || width <= 0 || height <= 0 {
            return;
        }

        let clamped = rect.clamp(width, height);
        if clamped.is_empty() {
            return;
        }

        let t = thickness.min(clamped.width()).min(clamped.height());
        let stride = width as usize * 4;

        // Every border pixel lies inside `clamped`, which is inside the buffer's
        // `width` x `height`; only a `target` shorter than that needs a guard.
        let fill_pixel = |target: &mut [u8], x: i32, y: i32| {
            let offset = (y as usize * stride) + (x as usize * 4);
            if offset + 4 <= target.len() {
                target[offset..offset + 4].copy_from_slice(&color_bgra);
            }
        };

        // Top horizontal border (full width)
        for y in clamped.top..(clamped.top + t) {
            for x in clamped.left..clamped.right {
                fill_pixel(target, x, y);
            }
        }

        // Bottom horizontal border (full width)
        for y in (clamped.bottom - t)..clamped.bottom {
            for x in clamped.left..clamped.right {
                fill_pixel(target, x, y);
            }
        }

        // Left vertical border (between top and bottom borders)
        for y in (clamped.top + t)..(clamped.bottom - t) {
            for x in clamped.left..(clamped.left + t) {
                fill_pixel(target, x, y);
            }
        }

        // Right vertical border (between top and bottom borders)
        for y in (clamped.top + t)..(clamped.bottom - t) {
            for x in (clamped.right - t)..clamped.right {
                fill_pixel(target, x, y);
            }
        }
    }

    /// Draws a solid filled rectangle onto the 32-bit BGRA buffer.
    pub fn fill_rect(target: &mut [u8], width: i32, height: i32, rect: &Rect, color_bgra: [u8; 4]) {
        if rect.is_empty() || width <= 0 || height <= 0 {
            return;
        }
        let clamped = rect.clamp(width, height);
        if clamped.is_empty() {
            return;
        }
        let stride = width as usize * 4;
        for y in clamped.top..clamped.bottom {
            let row_offset = y as usize * stride;
            let start = row_offset + clamped.left as usize * 4;
            let end = row_offset + clamped.right as usize * 4;
            for px in (start..end).step_by(4) {
                if px + 4 <= target.len() {
                    target[px..px + 4].copy_from_slice(&color_bgra);
                }
            }
        }
    }

    /// Draws a dual-tone high-contrast selection border and eight resize handles,
    /// sized for `dpi` so they match the hit-test geometry. Everything is laid out
    /// around `rect.selection_frame(dpi)`, so the selected pixels themselves are never
    /// covered except by the inner halves of the grips.
    /// Main border: accent ring, `(2 * dpi / 96).max(2)` px, directly outside `rect`.
    /// Outer border: dark outline, `(dpi / 96).max(1)` px, outside the accent ring.
    /// Handles: white squares, `8 * dpi / 96` px (at least 8) with a dark border,
    /// centred on the accent ring's outer edge.
    pub fn draw_contrast_selection(
        target: &mut [u8],
        width: i32,
        height: i32,
        rect: &Rect,
        accent_bgra: [u8; 4],
        dpi: u32,
    ) {
        if rect.is_empty() || width <= 0 || height <= 0 {
            return;
        }

        let dark_border = crate::theme::BRAND_INK_BGRA;
        let handle_fill = [255, 255, 255, 255]; // Crisp white
        let thickness = Rect::selection_frame_thickness(dpi);
        let outline = (dpi / 96).max(1) as i32;
        let frame = rect.selection_frame(dpi);

        // 1. Dual-tone selection rectangle: dark outline around the accent frame.
        let outer = frame.inflate(outline, outline);
        Self::draw_border(target, width, height, &outer, dark_border, outline);
        Self::draw_border(target, width, height, &frame, accent_bgra, thickness);

        // Four corners and four edge midpoints share the hit-test geometry.
        let half_h = (4 * dpi / 96).max(4) as i32;
        let middle_x = frame.left + frame.width() / 2;
        let middle_y = frame.top + frame.height() / 2;
        let handles = [
            (frame.left, frame.top),
            (frame.right, frame.top),
            (frame.left, frame.bottom),
            (frame.right, frame.bottom),
            (middle_x, frame.top),
            (frame.right, middle_y),
            (middle_x, frame.bottom),
            (frame.left, middle_y),
        ];

        for (cx, cy) in handles {
            let handle_rect = Rect::new(cx - half_h, cy - half_h, cx + half_h, cy + half_h);
            Self::fill_rect(target, width, height, &handle_rect, handle_fill);
            Self::draw_border(target, width, height, &handle_rect, dark_border, outline);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{COLORREF, ERROR_INVALID_HANDLE, SetLastError};
    use windows::Win32::Graphics::Gdi::SetPixel;

    #[test]
    fn test_selection_hit_zones() {
        let sel = Rect::new(100, 100, 300, 200);

        // Corner handles (8x8 px centered on vertices, half_h = 4)
        assert_eq!(
            sel.hit_test_selection((100, 100), 4, 8),
            SelectionHitZone::TopLeftCorner
        );
        assert_eq!(
            sel.hit_test_selection((300, 100), 4, 8),
            SelectionHitZone::TopRightCorner
        );
        assert_eq!(
            sel.hit_test_selection((100, 200), 4, 8),
            SelectionHitZone::BottomLeftCorner
        );
        assert_eq!(
            sel.hit_test_selection((300, 200), 4, 8),
            SelectionHitZone::BottomRightCorner
        );

        for (point, zone) in [
            ((200, 100), SelectionHitZone::TopEdge),
            ((300, 150), SelectionHitZone::RightEdge),
            ((200, 200), SelectionHitZone::BottomEdge),
            ((100, 150), SelectionHitZone::LeftEdge),
        ] {
            assert_eq!(sel.hit_test_selection(point, 4, 8), zone);
        }
        assert_eq!(
            sel.hit_test_selection((160, 100), 4, 8),
            SelectionHitZone::BorderEdge
        );

        // Interior (deep inside selection, far from border band)
        assert_eq!(
            sel.hit_test_selection((200, 150), 4, 8),
            SelectionHitZone::Interior
        );

        // Outside (far from selection)
        assert_eq!(
            sel.hit_test_selection((50, 50), 4, 8),
            SelectionHitZone::None
        );
        assert_eq!(
            sel.hit_test_selection((400, 400), 4, 8),
            SelectionHitZone::None
        );
    }

    #[test]
    fn test_contrast_selection_and_fill() {
        let mut buffer = vec![0u8; 100 * 100 * 4];
        let rect = Rect::new(10, 10, 50, 50);
        CaptureBuffer::draw_contrast_selection(
            &mut buffer,
            100,
            100,
            &rect,
            [246, 130, 59, 255],
            96,
        );

        // The grip is centred on the frame's outer corner (8, 8), not on the selection's.
        let offset = (8 * 100 + 8) * 4;
        assert_eq!(&buffer[offset..offset + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn selection_frame_grows_by_the_dpi_scaled_thickness() {
        let rect = Rect::new(30, 40, 90, 100);
        for (dpi, thickness) in [(48, 2), (96, 2), (144, 3), (192, 4)] {
            assert_eq!(Rect::selection_frame_thickness(dpi), thickness, "dpi {dpi}");
            assert_eq!(
                rect.selection_frame(dpi),
                Rect::new(
                    30 - thickness,
                    40 - thickness,
                    90 + thickness,
                    100 + thickness
                ),
                "dpi {dpi}"
            );
        }
    }

    #[test]
    fn selection_frame_leaves_every_selected_pixel_visible() {
        let accent = [246, 130, 59, 255];
        for dpi in [96, 192] {
            let mut buffer = vec![0u8; 200 * 200 * 4];
            let rect = Rect::new(60, 60, 140, 120);
            CaptureBuffer::draw_contrast_selection(&mut buffer, 200, 200, &rect, accent, dpi);
            let thickness = Rect::selection_frame_thickness(dpi);
            let pixel =
                |x: i32, y: i32| buffer[(y as usize * 200 + x as usize) * 4..][..4].to_vec();

            // Each probe starts at the ring pixel next to the selection and runs outward
            // across one side, away from every grip: the ring is `thickness` pixels wide
            // and the selected pixel beside it is untouched.
            for ((x, y), (dx, dy)) in [
                ((rect.left - 1, 72), (-1, 0)),
                ((rect.right, 72), (1, 0)),
                ((76, rect.top - 1), (0, -1)),
                ((76, rect.bottom), (0, 1)),
            ] {
                for step in 0..thickness {
                    let (ring_x, ring_y) = (x + dx * step, y + dy * step);
                    assert_eq!(
                        pixel(ring_x, ring_y),
                        accent,
                        "dpi {dpi} ({ring_x}, {ring_y})"
                    );
                }
                assert_eq!(pixel(x - dx, y - dy), [0; 4], "dpi {dpi} ({x}, {y})");
            }
        }
    }

    #[test]
    fn selection_frame_stays_visible_when_the_selection_touches_the_buffer_edge() {
        let accent = [246, 130, 59, 255];
        let mut buffer = vec![0u8; 100 * 100 * 4];
        let full = Rect::new(0, 0, 100, 100);
        CaptureBuffer::draw_contrast_selection(&mut buffer, 100, 100, &full, accent, 96);
        // Ring sides that fall outside the buffer are clamped onto its outermost
        // pixels; the probes sit between the grips.
        for (x, y) in [(0, 20), (99, 20), (20, 0), (20, 99)] {
            assert_eq!(buffer[(y * 100 + x) * 4..][..4], accent, "({x}, {y})");
        }
    }

    #[test]
    fn grip_pixels_and_frame_hit_zones_agree() {
        let dpi = 96;
        let mut buffer = vec![0u8; 200 * 200 * 4];
        let rect = Rect::new(60, 60, 140, 120);
        let accent = [246, 130, 59, 255];
        CaptureBuffer::draw_contrast_selection(&mut buffer, 200, 200, &rect, accent, dpi);
        let frame = rect.selection_frame(dpi);
        let middle_x = frame.left + frame.width() / 2;
        let middle_y = frame.top + frame.height() / 2;
        for ((x, y), zone) in [
            ((frame.left, frame.top), SelectionHitZone::TopLeftCorner),
            ((frame.right, frame.top), SelectionHitZone::TopRightCorner),
            (
                (frame.left, frame.bottom),
                SelectionHitZone::BottomLeftCorner,
            ),
            (
                (frame.right, frame.bottom),
                SelectionHitZone::BottomRightCorner,
            ),
            ((middle_x, frame.top), SelectionHitZone::TopEdge),
            ((frame.right, middle_y), SelectionHitZone::RightEdge),
            ((middle_x, frame.bottom), SelectionHitZone::BottomEdge),
            ((frame.left, middle_y), SelectionHitZone::LeftEdge),
        ] {
            assert_eq!(buffer[(y as usize * 200 + x as usize) * 4..][..4], [255; 4]);
            assert_eq!(frame.hit_test_selection((x, y), 4, 8), zone);
        }
    }

    #[test]
    fn contrast_selection_grips_scale_with_dpi() {
        let mut buffer = vec![0u8; 200 * 200 * 4];
        let rect = Rect::new(30, 30, 90, 90);
        CaptureBuffer::draw_contrast_selection(
            &mut buffer,
            200,
            200,
            &rect,
            [246, 130, 59, 255],
            192,
        );

        // The top-left grip is centred on the frame's outer corner (26, 26); its top
        // row is dark and spans 2 * (4 * 192 / 96) = 16 px.
        let top_row = 18 * 200;
        let dark: Vec<usize> = (0..45)
            .filter(|&x| buffer[(top_row + x) * 4..][..4] == crate::theme::BRAND_INK_BGRA)
            .collect();
        assert_eq!(dark, (18..34).collect::<Vec<_>>());
        assert_eq!(&buffer[(26 * 200 + 26) * 4..][..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn contrast_selection_frame_scales_with_dpi() {
        let accent = [246, 130, 59, 255];
        // (dpi, outline px, frame px)
        for (dpi, outline, frame) in [(96, 1, 2), (192, 2, 4)] {
            let mut buffer = vec![0u8; 200 * 200 * 4];
            let rect = Rect::new(30, 30, 90, 90);
            CaptureBuffer::draw_contrast_selection(&mut buffer, 200, 200, &rect, accent, dpi);

            // Row 45 crosses only the left edge, between the grips.
            // The ring lies outside the selection; its first selected pixel stays untouched.
            let pixel = |x: i32| buffer[(45 * 200 + x as usize) * 4..][..4].to_vec();
            assert_eq!(pixel(30 - frame - outline - 1), [0; 4], "dpi {dpi}");
            for x in 30 - frame - outline..30 - frame {
                assert_eq!(pixel(x), crate::theme::BRAND_INK_BGRA, "dpi {dpi} x {x}");
            }
            for x in 30 - frame..30 {
                assert_eq!(pixel(x), accent, "dpi {dpi} x {x}");
            }
            assert_eq!(pixel(30), [0; 4], "dpi {dpi}");
        }
    }

    fn english() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::i18n::TEST_LANGUAGE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::i18n::set_preference(crate::i18n::LanguagePreference::English);
        guard
    }

    /// A device context handle that every GDI call rejects.
    fn bogus_dc() -> HDC {
        HDC(std::ptr::dangling_mut::<c_void>())
    }

    /// A real memory device context to build sections compatible with.
    fn compatible_dc() -> MemoryDc {
        MemoryDc(valid_dc(unsafe { CreateCompatibleDC(None) }).unwrap())
    }

    /// Writes `color(x, y)` into every pixel of `section` through its pixel memory.
    fn paint(section: &DibSection, color: impl Fn(i32, i32) -> [u8; 4]) {
        let pixels = unsafe { std::slice::from_raw_parts_mut(section.bits, section.len) };
        for y in 0..section.height {
            for x in 0..section.width {
                let offset = (y * section.width + x) as usize * 4;
                pixels[offset..offset + 4].copy_from_slice(&color(x, y));
            }
        }
    }

    fn pixels_of(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| pixel(x, y))
            .collect()
    }

    fn at(buffer: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        buffer[(y * width + x) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn normalized_orders_two_corners_whichever_way_they_are_dragged() {
        let expected = Rect::new(10, 5, 30, 40);
        assert_eq!(Rect::normalized((10, 40), (30, 5)), expected);
        assert_eq!(Rect::normalized((30, 5), (10, 40)), expected);
        assert_eq!(Rect::normalized((10, 5), (30, 40)), expected);
        assert_eq!(
            Rect::normalized((30, 40), (30, 40)),
            Rect::new(30, 40, 30, 40)
        );
    }

    #[test]
    fn union_is_the_bounding_box_and_ignores_empty_rects() {
        let a = Rect::new(10, 20, 30, 40);
        let b = Rect::new(25, 5, 50, 30);
        assert_eq!(a.union(&b), Rect::new(10, 5, 50, 40));
        assert_eq!(b.union(&a), Rect::new(10, 5, 50, 40));
        let empty = Rect::new(100, 100, 100, 150);
        assert_eq!(empty.union(&a), a);
        assert_eq!(a.union(&empty), a);
        assert_eq!(empty.union(&empty), empty);
    }

    #[test]
    fn an_empty_selection_hits_nothing() {
        let empty = Rect::new(50, 50, 50, 80);
        assert_eq!(
            empty.hit_test_selection((50, 60), 4, 8),
            SelectionHitZone::None
        );
    }

    #[test]
    fn copy_rect_copies_only_the_clamped_rows_and_columns() {
        let source = pixels_of(4, 3, |x, y| [x as u8 + 1, y as u8 + 1, 7, 255]);
        let mut target = vec![0u8; source.len()];
        CaptureBuffer::copy_rect(&mut target, 4, 3, &Rect::new(1, 1, 3, 3), &source);
        for y in 0..3 {
            for x in 0..4 {
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                let expected = if inside { at(&source, 4, x, y) } else { [0; 4] };
                assert_eq!(at(&target, 4, x, y), expected, "({x}, {y})");
            }
        }

        // A rectangle hanging over the buffer is cut at its edges.
        let mut target = vec![0u8; source.len()];
        CaptureBuffer::copy_rect(&mut target, 4, 3, &Rect::new(-2, -2, 2, 1), &source);
        for y in 0..3 {
            for x in 0..4 {
                let expected = if x < 2 && y < 1 {
                    at(&source, 4, x, y)
                } else {
                    [0; 4]
                };
                assert_eq!(at(&target, 4, x, y), expected, "({x}, {y})");
            }
        }
    }

    #[test]
    fn copy_rect_ignores_empty_degenerate_and_out_of_buffer_rects() {
        let source = vec![9u8; 4 * 3 * 4];
        for (width, height, rect) in [
            (4, 3, Rect::new(2, 1, 2, 3)),
            (4, 3, Rect::new(2, 2, 3, 2)),
            (0, 3, Rect::new(0, 0, 4, 3)),
            (4, -1, Rect::new(0, 0, 4, 3)),
            (4, 3, Rect::new(10, 10, 20, 20)),
        ] {
            let mut target = vec![0u8; source.len()];
            CaptureBuffer::copy_rect(&mut target, width, height, &rect, &source);
            assert!(target.iter().all(|&byte| byte == 0), "{rect:?}");
        }
    }

    #[test]
    fn copy_rect_skips_rows_a_short_buffer_does_not_hold() {
        let source = vec![9u8; 4 * 3 * 4];
        // Only the first two rows fit in the target.
        let mut target = vec![0u8; 4 * 2 * 4];
        CaptureBuffer::copy_rect(&mut target, 4, 3, &Rect::new(0, 0, 4, 3), &source);
        assert!(target.iter().all(|&byte| byte == 9));
        // Only the first row fits in the source.
        let mut target = vec![0u8; 4 * 3 * 4];
        CaptureBuffer::copy_rect(&mut target, 4, 3, &Rect::new(0, 0, 4, 3), &source[..16]);
        assert!(target[..16].iter().all(|&byte| byte == 9));
        assert!(target[16..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn punch_out_and_restore_copy_from_the_original_and_the_dimmed_pixels() {
        let capture = CaptureBuffer {
            x: 0,
            y: 0,
            width: 4,
            height: 3,
            original: PixelBuffer(PixelStorage::Owned(vec![200u8; 4 * 3 * 4])),
            dimmed: vec![50u8; 4 * 3 * 4],
            timings: CaptureTimings::default(),
        };
        let rect = Rect::new(1, 0, 3, 2);
        let mut target = vec![0u8; 4 * 3 * 4];
        capture.punch_out(&mut target, &rect);
        for y in 0..3 {
            for x in 0..4 {
                let expected = if (1..3).contains(&x) && y < 2 {
                    [200; 4]
                } else {
                    [0; 4]
                };
                assert_eq!(at(&target, 4, x, y), expected, "({x}, {y})");
            }
        }
        capture.restore_dimmed(&mut target, &Rect::new(0, 0, 2, 1));
        assert_eq!(at(&target, 4, 0, 0), [50; 4]);
        assert_eq!(at(&target, 4, 1, 0), [50; 4]);
        assert_eq!(at(&target, 4, 2, 0), [200; 4]);
        assert_eq!(at(&target, 4, 1, 1), [200; 4]);
    }

    #[test]
    fn dimming_scales_each_color_channel_to_45_percent_and_makes_alpha_opaque() {
        let original = [255, 100, 20, 7, 0, 1, 2, 255];
        let mut dimmed = [0u8; 8];
        CaptureBuffer::fill_dimmed_buffer(&original, &mut dimmed);
        // v * 115 / 255, rounded down: 255 -> 115, 100 -> 45, 20 -> 9, 2 -> 0.
        assert_eq!(dimmed, [115, 45, 9, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn dimmed_buffers_are_pooled_unless_they_are_oversized() {
        assert_eq!(take_dimmed_buffer(24).len(), 24);

        let mut capture = CaptureBuffer::dummy(1, 1);
        capture.dimmed = Vec::with_capacity(MAX_POOLED_BYTES);
        let pooled = capture.dimmed.capacity();
        drop(capture);
        let reused = take_dimmed_buffer(16);
        assert_eq!(reused.len(), 16);
        assert!(reused.capacity() >= pooled);

        let mut capture = CaptureBuffer::dummy(1, 1);
        capture.dimmed = Vec::with_capacity(MAX_POOLED_BYTES + 1);
        drop(capture);
        assert!(take_dimmed_buffer(16).capacity() <= MAX_POOLED_BYTES);
    }

    #[test]
    fn border_and_fill_cover_exactly_their_pixels() {
        let accent = [1, 2, 3, 255];
        let mut buffer = vec![0u8; 10 * 10 * 4];
        CaptureBuffer::draw_border(&mut buffer, 10, 10, &Rect::new(2, 2, 8, 8), accent, 2);
        for y in 0..10 {
            for x in 0..10 {
                let on_ring = (2..8).contains(&x)
                    && (2..8).contains(&y)
                    && !((4..6).contains(&x) && (4..6).contains(&y));
                let expected = if on_ring { accent } else { [0; 4] };
                assert_eq!(at(&buffer, 10, x, y), expected, "({x}, {y})");
            }
        }

        let mut buffer = vec![0u8; 10 * 10 * 4];
        CaptureBuffer::fill_rect(&mut buffer, 10, 10, &Rect::new(3, 4, 7, 6), accent);
        for y in 0..10 {
            for x in 0..10 {
                let inside = (3..7).contains(&x) && (4..6).contains(&y);
                let expected = if inside { accent } else { [0; 4] };
                assert_eq!(at(&buffer, 10, x, y), expected, "({x}, {y})");
            }
        }
    }

    #[test]
    fn a_border_thicker_than_its_rect_fills_it_and_one_off_the_buffer_is_cut() {
        let accent = [1, 2, 3, 255];
        let mut buffer = vec![0u8; 10 * 10 * 4];
        CaptureBuffer::draw_border(&mut buffer, 10, 10, &Rect::new(2, 2, 5, 4), accent, 9);
        for y in 0..10 {
            for x in 0..10 {
                let inside = (2..5).contains(&x) && (2..4).contains(&y);
                let expected = if inside { accent } else { [0; 4] };
                assert_eq!(at(&buffer, 10, x, y), expected, "({x}, {y})");
            }
        }

        // A frame hanging over the buffer is drawn on its outermost pixels.
        let mut buffer = vec![0u8; 10 * 10 * 4];
        CaptureBuffer::draw_border(&mut buffer, 10, 10, &Rect::new(6, 6, 14, 14), accent, 1);
        assert_eq!(at(&buffer, 10, 6, 8), accent);
        assert_eq!(at(&buffer, 10, 8, 6), accent);
        assert_eq!(at(&buffer, 10, 9, 9), accent);
        assert_eq!(at(&buffer, 10, 7, 7), [0; 4]);
    }

    #[test]
    fn drawing_ignores_degenerate_requests() {
        let accent = [1, 2, 3, 255];
        let untouched = vec![0u8; 10 * 10 * 4];
        let full = Rect::new(0, 0, 10, 10);
        let off_buffer = Rect::new(20, 20, 30, 30);
        let empty = Rect::new(4, 4, 4, 8);

        // (rect, thickness, width, height)
        for (rect, thickness, width, height) in [
            (empty, 1, 10, 10),
            (full, 0, 10, 10),
            (full, 1, 0, 10),
            (full, 1, 10, -3),
            (off_buffer, 1, 10, 10),
        ] {
            let mut buffer = untouched.clone();
            CaptureBuffer::draw_border(&mut buffer, width, height, &rect, accent, thickness);
            assert_eq!(buffer, untouched, "{rect:?} {thickness} {width}x{height}");
        }
        for (rect, width, height) in [
            (empty, 10, 10),
            (full, 0, 10),
            (full, 10, -3),
            (off_buffer, 10, 10),
        ] {
            let mut buffer = untouched.clone();
            CaptureBuffer::fill_rect(&mut buffer, width, height, &rect, accent);
            assert_eq!(buffer, untouched, "{rect:?} {width}x{height}");
        }
        let mut buffer = untouched.clone();
        for (rect, width, height) in [(empty, 10, 10), (full, 0, 10), (full, 10, 0)] {
            CaptureBuffer::draw_contrast_selection(&mut buffer, width, height, &rect, accent, 96);
        }
        assert_eq!(buffer, untouched);
    }

    #[test]
    fn drawing_skips_rows_a_short_buffer_does_not_hold() {
        let accent = [1, 2, 3, 255];
        let full = Rect::new(0, 0, 10, 10);
        // The buffer holds only the first two of the ten rows.
        let mut short = vec![0u8; 10 * 2 * 4];
        CaptureBuffer::draw_border(&mut short, 10, 10, &full, accent, 1);
        for x in 0..10 {
            assert_eq!(at(&short, 10, x, 0), accent, "top row {x}");
            let on_side = x == 0 || x == 9;
            assert_eq!(
                at(&short, 10, x, 1),
                if on_side { accent } else { [0; 4] },
                "second row {x}"
            );
        }
        CaptureBuffer::fill_rect(&mut short, 10, 10, &full, accent);
        assert!(
            short
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == accent)
        );
    }

    #[test]
    fn pixel_bytes_is_four_bytes_per_pixel_and_rejects_overflow() {
        assert_eq!(pixel_bytes(1920, 1080).unwrap(), 8_294_400);
        for (width, height) in [(usize::MAX, 2), (usize::MAX / 4 + 1, 1)] {
            let error = pixel_bytes(width, height).unwrap_err();
            assert_eq!(error.code(), windows::core::HRESULT::from_win32(534));
        }
    }

    #[test]
    fn gdi_failures_carry_error_31_and_their_own_message() {
        let _english = english();
        for (failure, message) in [
            (
                GdiFailure::NoBitmap,
                "Windows did not allocate a usable screenshot bitmap.",
            ),
            (
                GdiFailure::SelectBitmap,
                "Windows could not select the screenshot bitmap.",
            ),
            (
                GdiFailure::Flush,
                "Windows could not finish drawing the captured screen.",
            ),
        ] {
            let error = Error::from(failure);
            assert_eq!(error.code(), windows::core::HRESULT::from_win32(31));
            assert!(error.message().contains(message), "{}", error.message());
        }
        crate::i18n::set_preference(crate::i18n::LanguagePreference::System);
    }

    #[test]
    fn valid_dc_passes_a_handle_and_turns_a_null_one_into_the_last_error() {
        let dc = compatible_dc();
        assert_eq!(valid_dc(dc.0).unwrap(), dc.0);
        unsafe { SetLastError(ERROR_INVALID_HANDLE) };
        let error = valid_dc(HDC::default()).unwrap_err();
        assert_eq!(error.code(), windows::core::HRESULT::from_win32(6));
    }

    #[test]
    fn dib_sections_are_top_down_32_bit_pixel_memory_that_gdi_draws_into() {
        let compatible = compatible_dc();
        let section = DibSection::new(compatible.0, 5, 3).unwrap();
        assert_eq!(section.pixels().len(), 5 * 3 * 4);
        // COLORREF 0x00BBGGRR lands in the DIB as B, G, R: row 1 is the second row from the top.
        unsafe {
            SetPixel(section.memory_dc.0, 2, 1, COLORREF(0x0030_2010));
            let _ = GdiFlush();
        }
        assert_eq!(at(section.pixels(), 5, 2, 1)[..3], [0x30, 0x20, 0x10]);
        assert_eq!(at(section.pixels(), 5, 2, 0)[..3], [0, 0, 0]);
        assert_eq!(at(section.pixels(), 5, 1, 1)[..3], [0, 0, 0]);
    }

    #[test]
    fn dib_sections_reject_what_gdi_cannot_provide() {
        let _english = english();
        let compatible = compatible_dc();
        for (width, height) in [(0, 5), (5, 0), (-3, 5), (5, -3)] {
            let error = DibSection::new(compatible.0, width, height).err().unwrap();
            assert_eq!(
                error.code(),
                windows::core::HRESULT::from_win32(87),
                "{width}x{height}"
            );
            assert!(
                error
                    .message()
                    .contains("The virtual screen has no capturable area."),
                "{}",
                error.message()
            );
        }
        assert!(DibSection::new(bogus_dc(), 4, 4).is_err());
        // 40 GB of pixels: the memory device context exists, the bitmap cannot.
        assert!(DibSection::new(compatible.0, 100_000, 100_000).is_err());
        crate::i18n::set_preference(crate::i18n::LanguagePreference::System);
    }

    #[test]
    fn copying_from_an_unusable_source_fails_and_leaves_the_section_intact() {
        let compatible = compatible_dc();
        let section = DibSection::new(compatible.0, 4, 4).unwrap();
        paint(&section, |_, _| [5, 6, 7, 255]);
        assert!(section.copy_from(bogus_dc(), 0, 0).is_err());
        assert!(
            section
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [5, 6, 7, 255])
        );
    }

    #[test]
    fn capture_from_copies_the_requested_area_and_pre_renders_the_dimmed_copy() {
        let compatible = compatible_dc();
        let source = DibSection::new(compatible.0, 8, 6).unwrap();
        paint(&source, |x, y| [x as u8 * 10, y as u8 * 10, 100, 255]);

        let capture =
            CaptureBuffer::capture_from(Instant::now(), source.memory_dc.0, 2, 1, 4, 3).unwrap();
        assert_eq!((capture.x, capture.y), (2, 1));
        assert_eq!((capture.width, capture.height), (4, 3));
        assert_eq!(capture.original.len(), 4 * 3 * 4);
        assert_eq!(capture.dimmed.len(), 4 * 3 * 4);
        for row in 0..3usize {
            for col in 0..4usize {
                let blue = (2 + col as u32) * 10;
                let green = (1 + row as u32) * 10;
                assert_eq!(
                    at(&capture.original, 4, col, row)[..3],
                    [blue as u8, green as u8, 100],
                    "original ({col}, {row})"
                );
                // 115 / 255 of each channel, alpha opaque.
                assert_eq!(
                    at(&capture.dimmed, 4, col, row),
                    [(blue * 115 / 255) as u8, (green * 115 / 255) as u8, 45, 255],
                    "dimmed ({col}, {row})"
                );
            }
        }
        let timings = capture.timings;
        assert!(timings.total >= timings.setup + timings.bit_blt + timings.dim);
    }

    #[test]
    fn capture_from_reports_the_failure_instead_of_a_partial_capture() {
        let compatible = compatible_dc();
        let source = DibSection::new(compatible.0, 8, 6).unwrap();
        assert!(
            CaptureBuffer::capture_from(Instant::now(), source.memory_dc.0, 0, 0, 0, 6).is_err()
        );
        assert!(CaptureBuffer::capture_from(Instant::now(), bogus_dc(), 0, 0, 4, 3).is_err());
    }

    #[test]
    fn virtual_screen_capture_matches_the_desktop_geometry_and_dims_every_pixel() {
        let capture = CaptureBuffer::capture_virtual_screen().unwrap();
        let metric = |index| unsafe { GetSystemMetrics(index) };
        assert_eq!(
            (capture.x, capture.y, capture.width, capture.height),
            (
                metric(SM_XVIRTUALSCREEN),
                metric(SM_YVIRTUALSCREEN),
                metric(SM_CXVIRTUALSCREEN),
                metric(SM_CYVIRTUALSCREEN)
            )
        );
        let bytes = capture.width as usize * capture.height as usize * 4;
        assert_eq!(capture.original.len(), bytes);
        assert_eq!(capture.dimmed.len(), bytes);
        for (source, dimmed) in capture
            .original
            .as_chunks::<4>()
            .0
            .iter()
            .zip(capture.dimmed.as_chunks::<4>().0.iter())
        {
            assert_eq!(dimmed[3], 255);
            for channel in 0..3 {
                assert_eq!(dimmed[channel] as u32, source[channel] as u32 * 115 / 255);
            }
        }
    }
}
