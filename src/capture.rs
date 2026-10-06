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
    Dib {
        pointer: *mut u8,
        length: usize,
        memory_dc: HDC,
        bitmap: HBITMAP,
        old_bitmap: HGDIOBJ,
    },
}

pub struct PixelBuffer(PixelStorage);

impl std::ops::Deref for PixelBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            PixelStorage::Owned(bytes) => bytes,
            PixelStorage::Dib {
                pointer, length, ..
            } => unsafe { std::slice::from_raw_parts(*pointer, *length) },
        }
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        if let PixelStorage::Dib {
            memory_dc,
            bitmap,
            old_bitmap,
            ..
        } = self.0
        {
            unsafe {
                SelectObject(memory_dc, old_bitmap);
                let _ = DeleteDC(memory_dc);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
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

// RAII cleanup helper for HDC deleted via DeleteDC
struct DcDeleteGuard(HDC);

impl Drop for DcDeleteGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

// RAII cleanup helper for GDI object deleted via DeleteObject
struct GdiObjectGuard(HGDIOBJ);

impl Drop for GdiObjectGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
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

        if width <= 0 || height <= 0 {
            return Err(Error::new(
                windows::core::HRESULT::from_win32(87),
                "The virtual screen has no capturable area.",
            ));
        }

        let buffer_size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| {
                Error::new(
                    windows::core::HRESULT::from_win32(534),
                    "The virtual screen exceeds the supported pixel-buffer size.",
                )
            })?;

        // 1. Get desktop window DC
        let screen_dc = unsafe { GetDC(HWND::default()) };
        if screen_dc.is_invalid() {
            return Err(Error::from_win32());
        }
        let _screen_guard = DcReleaseGuard {
            hwnd: HWND::default(),
            hdc: screen_dc,
        };

        // 2. Create compatible memory DC
        let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
        if mem_dc.is_invalid() {
            return Err(Error::from_win32());
        }
        let _mem_guard = DcDeleteGuard(mem_dc);

        // 3. Create a 32-bit top-down DIB section
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

        let mut bits_ptr: *mut c_void = std::ptr::null_mut();
        let hbitmap: HBITMAP =
            unsafe { CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)? };
        let _bitmap_guard = GdiObjectGuard(HGDIOBJ(hbitmap.0));
        if hbitmap.is_invalid() || bits_ptr.is_null() {
            return Err(Error::new(
                windows::core::HRESULT::from_win32(31),
                "Windows did not allocate a usable screenshot bitmap.",
            ));
        }

        // 4. Select DIB section into memory DC
        let old_obj = unsafe { SelectObject(mem_dc, HGDIOBJ(hbitmap.0)) };
        if old_obj.is_invalid() {
            return Err(Error::new(
                windows::core::HRESULT::from_win32(31),
                "Windows could not select the screenshot bitmap.",
            ));
        }

        let setup = total_start.elapsed();
        // 5. BitBlt from screen DC to memory DC with CAPTUREBLT
        let bit_blt_start = Instant::now();
        let blt_res = unsafe {
            BitBlt(
                mem_dc,
                0,
                0,
                width,
                height,
                screen_dc,
                x,
                y,
                SRCCOPY | CAPTUREBLT,
            )
        };

        if let Err(error) = blt_res {
            unsafe {
                SelectObject(mem_dc, old_obj);
            }
            return Err(error);
        }
        // GDI batches writes to DIB sections. Complete them before reading pixels on the CPU.
        if !unsafe { GdiFlush() }.as_bool() {
            unsafe {
                SelectObject(mem_dc, old_obj);
            }
            return Err(Error::new(
                windows::core::HRESULT::from_win32(31),
                "Windows could not finish drawing the captured screen.",
            ));
        }
        let bit_blt = bit_blt_start.elapsed();

        // 6. Dim directly from the captured DIB; no full-screen copy is required.
        let original_slice =
            unsafe { std::slice::from_raw_parts(bits_ptr.cast::<u8>(), buffer_size) };
        let mut dimmed = take_dimmed_buffer(buffer_size);
        let dim_start = Instant::now();
        Self::fill_dimmed_buffer(original_slice, &mut dimmed);
        let dim = dim_start.elapsed();
        let original = PixelBuffer(PixelStorage::Dib {
            pointer: bits_ptr.cast::<u8>(),
            length: buffer_size,
            memory_dc: mem_dc,
            bitmap: hbitmap,
            old_bitmap: old_obj,
        });
        std::mem::forget(_mem_guard);
        std::mem::forget(_bitmap_guard);

        Ok(Self {
            x,
            y,
            width,
            height,
            original,
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

        let fill_pixel = |target: &mut [u8], x: i32, y: i32| {
            if x >= 0 && x < width && y >= 0 && y < height {
                let offset = (y as usize * stride) + (x as usize * 4);
                if offset + 4 <= target.len() {
                    target[offset..offset + 4].copy_from_slice(&color_bgra);
                }
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

    /// Draws a dual-tone high-contrast selection border and eight resize handles.
    /// Outer border: 1px black outline.
    /// Main border: 2px accent outline.
    /// Corner handles: 8x8 px squares with white fill and 1px black border.
    pub fn draw_contrast_selection(
        target: &mut [u8],
        width: i32,
        height: i32,
        rect: &Rect,
        accent_bgra: [u8; 4],
    ) {
        if rect.is_empty() || width <= 0 || height <= 0 {
            return;
        }

        let dark_border = [15, 15, 15, 255]; // Deep charcoal/black
        let handle_fill = [255, 255, 255, 255]; // Crisp white

        // 1. Dual-tone selection rectangle:
        // Outer 1px dark border
        let outer = Rect::new(rect.left - 1, rect.top - 1, rect.right + 1, rect.bottom + 1);
        Self::draw_border(target, width, height, &outer, dark_border, 1);

        // Inner 2px accent border
        Self::draw_border(target, width, height, rect, accent_bgra, 2);

        // Four corners and four edge midpoints share the hit-test geometry.
        let half_h = 4;
        let middle_x = rect.left + rect.width() / 2;
        let middle_y = rect.top + rect.height() / 2;
        let handles = [
            (rect.left, rect.top),
            (rect.right, rect.top),
            (rect.left, rect.bottom),
            (rect.right, rect.bottom),
            (middle_x, rect.top),
            (rect.right, middle_y),
            (middle_x, rect.bottom),
            (rect.left, middle_y),
        ];

        for (cx, cy) in handles {
            let handle_rect = Rect::new(cx - half_h, cy - half_h, cx + half_h, cy + half_h);
            Self::fill_rect(target, width, height, &handle_rect, handle_fill);
            Self::draw_border(target, width, height, &handle_rect, dark_border, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        CaptureBuffer::draw_contrast_selection(&mut buffer, 100, 100, &rect, [246, 130, 59, 255]);

        // Corner handle should be filled with white [255, 255, 255, 255]
        let offset = (10 * 100 + 10) * 4;
        assert_eq!(&buffer[offset..offset + 4], &[255, 255, 255, 255]);
    }
}
