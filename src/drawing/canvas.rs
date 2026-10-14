//! Test-only helpers: an in-memory 32-bit canvas to draw into and read pixels back
//! from, and a device-context hoard that exhausts the process's GDI handle quota.
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::*;

/// Pixel colour as `[red, green, blue]`.
pub(crate) type Rgb = [u8; 3];

/// The channels of a GDI colour.
pub(crate) fn rgb(color: COLORREF) -> Rgb {
    let [red, green, blue, _] = color.0.to_le_bytes();
    [red, green, blue]
}

/// A DIB pixel (`0x00RRGGBB` in a little-endian `u32`) as its channels.
fn unpack(value: u32) -> Rgb {
    let [blue, green, red, _] = value.to_le_bytes();
    [red, green, blue]
}

/// Sum of the per-channel differences: 0 for identical colours.
pub(crate) fn distance(a: Rgb, b: Rgb) -> u32 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(y) as u32).sum()
}

/// Asserts that `actual` lies within `tolerance` (summed channel difference) of `expected`.
#[track_caller]
pub(crate) fn assert_color(actual: Rgb, expected: COLORREF, tolerance: u32, what: &str) {
    let wanted = rgb(expected);
    assert!(
        distance(actual, wanted) <= tolerance,
        "{what}: {actual:?} is not {wanted:?} (±{tolerance})"
    );
}

/// A top-down 32-bit DIB section selected into a memory DC, cleared to one colour.
pub(crate) struct Canvas {
    pub hdc: HDC,
    pub width: i32,
    pub height: i32,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *const u32,
}

impl Canvas {
    pub fn new(width: i32, height: i32, background: COLORREF) -> Self {
        unsafe {
            let hdc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
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
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(hdc, &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .expect("DIB section");
            let previous = SelectObject(hdc, bitmap);
            let canvas = Self {
                hdc,
                width,
                height,
                bitmap,
                previous,
                bits: bits.cast(),
            };
            canvas.fill(0, 0, width, height, background);
            canvas
        }
    }

    /// Paints `[left, right) × [top, bottom)` with a solid colour.
    pub fn fill(&self, left: i32, top: i32, right: i32, bottom: i32, color: COLORREF) {
        unsafe {
            let brush = CreateSolidBrush(color);
            let rect = windows::Win32::Foundation::RECT {
                left,
                top,
                right,
                bottom,
            };
            FillRect(self.hdc, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
    }

    /// Every pixel, row by row, after pending GDI drawing has landed.
    pub fn pixels(&self) -> Vec<Rgb> {
        let count = (self.width * self.height) as usize;
        let raw = unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts(self.bits, count)
        };
        raw.iter().map(|&value| unpack(value)).collect()
    }

    pub fn pixel(&self, x: i32, y: i32) -> Rgb {
        assert!(
            x >= 0 && y >= 0 && x < self.width && y < self.height,
            "({x}, {y})"
        );
        unpack(unsafe {
            let _ = GdiFlush();
            *self.bits.add((y * self.width + x) as usize)
        })
    }

    /// How close any pixel inside `[left, right) × [top, bottom)` comes to `color`.
    pub fn closest(&self, left: i32, top: i32, right: i32, bottom: i32, color: Rgb) -> u32 {
        let pixels = self.pixels();
        (top..bottom)
            .flat_map(|y| (left..right).map(move |x| (x, y)))
            .map(|(x, y)| distance(pixels[(y * self.width + x) as usize], color))
            .min()
            .expect("a non-empty region")
    }

    /// The tight `(left, top, right, bottom)` box around every pixel that differs from
    /// `reference` (a [`Canvas::pixels`] snapshot), if any does.
    pub fn changed_since(&self, reference: &[Rgb]) -> Option<(i32, i32, i32, i32)> {
        self.changed_within(reference, (0, 0, self.width, self.height))
    }

    /// Like [`Canvas::changed_since`], looking only inside `(left, top, right, bottom)`.
    pub fn changed_within(
        &self,
        reference: &[Rgb],
        (left, top, right, bottom): (i32, i32, i32, i32),
    ) -> Option<(i32, i32, i32, i32)> {
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for (index, (now, before)) in self.pixels().iter().zip(reference).enumerate() {
            let (x, y) = (index as i32 % self.width, index as i32 / self.width);
            if now != before && x >= left && x < right && y >= top && y < bottom {
                bounds = Some(match bounds {
                    Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x + 1), b.max(y + 1)),
                    None => (x, y, x + 1, y + 1),
                });
            }
        }
        bounds
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.hdc);
        }
    }
}

/// Holds device contexts until the process hits its GDI handle quota, so the next
/// GDI creation fails the way it does under real resource exhaustion.
pub(crate) struct GdiQuota(Vec<HDC>);

impl GdiQuota {
    pub fn exhaust() -> Self {
        let mut held = Vec::new();
        loop {
            let hdc = unsafe { CreateCompatibleDC(None) };
            if hdc.is_invalid() {
                break;
            }
            held.push(hdc);
            assert!(held.len() < 200_000, "the GDI handle quota never ran out");
        }
        Self(held)
    }

    /// Frees one handle, so exactly one more GDI object can be created.
    pub fn release_one(&mut self) {
        let hdc = self.0.pop().expect("a held device context");
        unsafe {
            let _ = DeleteDC(hdc);
        }
    }
}

impl Drop for GdiQuota {
    fn drop(&mut self) {
        for hdc in self.0.drain(..) {
            unsafe {
                let _ = DeleteDC(hdc);
            }
        }
    }
}
