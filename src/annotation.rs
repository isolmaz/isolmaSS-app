use crate::capture::Rect;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, POINT};
use windows::Win32::Graphics::Gdi::{
    Ellipse as GdiEllipse, GET_STOCK_OBJECT_FLAGS, GetStockObject, HDC, NULL_BRUSH, NULL_PEN,
    PS_DOT, PS_SOLID, Polygon, Polyline, Rectangle as GdiRectangle, SelectObject, WHITE_BRUSH,
};

mod smooth;

#[cfg(test)]
mod render_tests;

/// Available annotation tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ToolKind {
    #[default]
    Rectangle,
    Arrow,
    Ellipse,
    Line,
    Pen,
    Highlight,
    Step,
    Text,
    Blur,
    Redact,
    Select,
}

/// Converts a 32-bit BGRA color `[B, G, R, A]` to a Win32 `COLORREF` (0x00bbggrr).
#[inline]
pub fn bgra_to_colorref(bgra: [u8; 4]) -> COLORREF {
    COLORREF((bgra[2] as u32) | ((bgra[1] as u32) << 8) | ((bgra[0] as u32) << 16))
}

/// Runs `draw` with the stock object `stock` selected into `hdc`, then restores the
/// previous selection. Stock objects are never created or deleted.
fn with_stock_object<T>(hdc: HDC, stock: GET_STOCK_OBJECT_FLAGS, draw: impl FnOnce() -> T) -> T {
    let previous = unsafe { SelectObject(hdc, GetStockObject(stock)) };
    let drawn = draw();
    unsafe {
        SelectObject(hdc, previous);
    }
    drawn
}

/// The largest size in `1..=max` for which `fits` holds, given that `fits` is true up to
/// some size and false beyond it. Binary search: `fits` runs O(log max) times.
fn largest_fitting(max: i32, mut fits: impl FnMut(i32) -> bool) -> Option<i32> {
    let (mut low, mut high) = (1, max);
    let mut best = None;
    while low <= high {
        let mid = low + (high - low) / 2;
        if fits(mid) {
            best = Some(mid);
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }
    best
}

/// The specific data for each annotation kind.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationKind {
    Rectangle {
        rect: Rect,
        color: [u8; 4],
        thickness: i32,
    },
    Arrow {
        start: (i32, i32),
        end: (i32, i32),
        color: [u8; 4],
        thickness: i32,
    },
    /// Hollow ellipse inscribed in `rect`.
    Ellipse {
        rect: Rect,
        color: [u8; 4],
        thickness: i32,
    },
    /// Straight segment with round caps.
    Line {
        start: (i32, i32),
        end: (i32, i32),
        color: [u8; 4],
        thickness: i32,
    },
    Pen {
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        thickness: i32,
    },
    Highlight {
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        thickness: i32,
    },
    Step {
        pos: (i32, i32),
        number: u32,
        color: [u8; 4],
        radius: i32,
    },
    Text {
        /// Top-left corner of the first line; a background box extends past it.
        pos: (i32, i32),
        /// Lines separated by `\n`.
        text: String,
        color: [u8; 4],
        font_size: i32,
        /// A rounded box in `color` behind the text, which is then drawn black or white.
        background: bool,
    },
    Blur {
        rect: Rect,
        block_size: i32,
    },
    Redact {
        rect: Rect,
    },
}

/// Black or white, whichever reads better on a `fill` colour.
fn contrast_bgra(fill: [u8; 4]) -> [u8; 4] {
    let brightness = (fill[2] as u32 * 299 + fill[1] as u32 * 587 + fill[0] as u32 * 114) / 1000;
    if brightness > 140 {
        [0, 0, 0, 255]
    } else {
        [255, 255, 255, 255]
    }
}

/// Air between text and the edge of its background box; none without a box.
pub(crate) fn text_padding(font_size: i32, background: bool) -> i32 {
    if background {
        (font_size / 4).max(2)
    } else {
        0
    }
}

/// The area a text annotation covers: its lines, plus the padding of the background box.
fn text_extent(pos: (i32, i32), text: &str, font_size: i32, background: bool) -> Rect {
    let (width, height) = crate::drawing::measure_text(text, font_size);
    let padding = text_padding(font_size, background);
    Rect::new(
        pos.0 - padding,
        pos.1 - padding,
        pos.0 + width + padding,
        pos.1 + height + padding,
    )
}

/// The colour text is drawn in: its own, or black/white over its background box.
pub(crate) fn text_ink_bgra(color: [u8; 4], background: bool) -> [u8; 4] {
    if background {
        contrast_bgra(color)
    } else {
        color
    }
}

/// [`text_ink_bgra`] as a GDI colour.
pub(crate) fn text_ink(color: [u8; 4], background: bool) -> COLORREF {
    bgra_to_colorref(text_ink_bgra(color, background))
}

/// Draws a text annotation: the background box first when there is one, then every line.
pub(crate) fn render_text(
    hdc: HDC,
    pos: (i32, i32),
    text: &str,
    color: [u8; 4],
    font_size: i32,
    background: bool,
) {
    if background {
        let fill = bgra_to_colorref(color);
        // `rounded` takes the corner diameter: a radius of a fifth of the font, at least 4.
        crate::drawing::rounded(
            hdc,
            text_extent(pos, text, font_size, true),
            2 * (font_size / 5).max(4),
            fill,
            fill,
        );
    }
    crate::drawing::text(hdc, pos, text, font_size, text_ink(color, background));
}

/// A standalone annotation object with an ID and geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationObject {
    pub id: usize,
    pub kind: AnnotationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationResizeHandle {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// The first and last point of an arrow or a line.
    Start,
    End,
}

type ResizeHandlePoints = ([(AnnotationResizeHandle, (i32, i32)); 4], usize);

/// Half the grip size, and the gap between an object and its dashed indicator.
fn grip_half(dpi: u32) -> i32 {
    (4 * dpi / 96).max(4) as i32
}

/// Shaft end and head triangle of an arrow, in client coordinates.
struct ArrowGeometry {
    /// Where the shaft meets the head.
    base: (f64, f64),
    /// Head triangle: tip, left corner, right corner.
    head: [(f64, f64); 3],
}

/// Lays out an arrow from `start` to `end`; `None` when it is too short to draw.
fn arrow_geometry(start: (i32, i32), end: (i32, i32), thickness: i32) -> Option<ArrowGeometry> {
    let dx = (end.0 - start.0) as f64;
    let dy = (end.1 - start.1) as f64;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 2.0 {
        return None;
    }

    let u = (dx / len, dy / len);
    let perp = (-u.1, u.0);
    let head_len = (thickness as f64 * 2.5).clamp(16.0, 96.0).min(len * 0.5);
    let head_width = head_len * 0.5 + thickness as f64 * 0.25;

    let base = (end.0 as f64 - u.0 * head_len, end.1 as f64 - u.1 * head_len);
    Some(ArrowGeometry {
        base,
        head: [
            (end.0 as f64, end.1 as f64),
            (base.0 + perp.0 * head_width, base.1 + perp.1 * head_width),
            (base.0 - perp.0 * head_width, base.1 - perp.1 * head_width),
        ],
    })
}

/// Pixels a `thickness` px stroke reaches beyond the path it follows.
fn stroke_reach(thickness: i32) -> i32 {
    (thickness.max(1) + 1) / 2
}

/// The pixels a round-capped stroke from `a` to `b` covers, plus one pixel of
/// anti-aliasing on every side.
fn segment_extent(a: (i32, i32), b: (i32, i32), thickness: i32) -> Rect {
    let reach = stroke_reach(thickness) + 1;
    Rect::normalized(a, b).inflate(reach, reach)
}

/// The pixels an arrow covers: the round-capped shaft and the head triangle, plus one pixel
/// of anti-aliasing on every side.
fn arrow_extent(start: (i32, i32), end: (i32, i32), thickness: i32) -> Rect {
    let Some(arrow) = arrow_geometry(start, end, thickness) else {
        return segment_extent(start, end, thickness);
    };
    // The shaft stops at the head's base; the tip is a point of the triangle.
    let base = (arrow.base.0.round() as i32, arrow.base.1.round() as i32);
    let mut extent = segment_extent(start, base, thickness);
    for (x, y) in arrow.head {
        let corner = Rect::new(
            x.floor() as i32 - 1,
            y.floor() as i32 - 1,
            x.ceil() as i32 + 2,
            y.ceil() as i32 + 2,
        );
        extent = extent.union(&corner);
    }
    extent
}

/// Hollow frame with plain GDI: `rect` outlined by a `thickness` px pen.
fn plain_frame(hdc: HDC, rect: Rect, color: [u8; 4], thickness: i32) {
    crate::drawing::with_pen(hdc, PS_SOLID, thickness, bgra_to_colorref(color), || {
        with_stock_object(hdc, NULL_BRUSH, || unsafe {
            let _ = GdiRectangle(hdc, rect.left, rect.top, rect.right, rect.bottom);
        })
    });
}

/// Hollow ellipse with plain GDI: the ellipse inscribed in `rect`, outlined by a
/// `thickness` px pen.
fn plain_ellipse(hdc: HDC, rect: Rect, color: [u8; 4], thickness: i32) {
    crate::drawing::with_pen(hdc, PS_SOLID, thickness, bgra_to_colorref(color), || {
        with_stock_object(hdc, NULL_BRUSH, || unsafe {
            let _ = GdiEllipse(hdc, rect.left, rect.top, rect.right, rect.bottom);
        })
    });
}

/// Arrow with plain GDI: a pen shaft from `start` to the head's base and a filled head.
fn plain_arrow(
    hdc: HDC,
    start: (i32, i32),
    end: (i32, i32),
    arrow: &ArrowGeometry,
    color: [u8; 4],
    thickness: i32,
) {
    let colorref = bgra_to_colorref(color);
    let [_, p_left, p_right] = arrow.head;
    let shaft_points = [
        POINT {
            x: start.0,
            y: start.1,
        },
        POINT {
            x: arrow.base.0.round() as i32,
            y: arrow.base.1.round() as i32,
        },
    ];
    crate::drawing::with_pen(hdc, PS_SOLID, thickness, colorref, || unsafe {
        let _ = Polyline(hdc, &shaft_points);
    });

    let arrow_points = [
        POINT { x: end.0, y: end.1 },
        POINT {
            x: p_left.0.round() as i32,
            y: p_left.1.round() as i32,
        },
        POINT {
            x: p_right.0.round() as i32,
            y: p_right.1.round() as i32,
        },
    ];
    // Reusing the wide shaft pen to outline the triangle inflates every edge by half
    // the stroke width and deforms the head as width changes.
    crate::drawing::with_brush(hdc, colorref, || {
        with_stock_object(hdc, NULL_PEN, || unsafe {
            let _ = Polygon(hdc, &arrow_points);
        })
    });
}

/// Freehand stroke with plain GDI through every input point.
fn plain_polyline(hdc: HDC, points: &[(i32, i32)], color: [u8; 4], thickness: i32) {
    thread_local! { static POINTS: std::cell::RefCell<Vec<POINT>> = const { std::cell::RefCell::new(Vec::new()) }; }
    POINTS.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.clear();
        scratch.extend(points.iter().map(|point| POINT {
            x: point.0,
            y: point.1,
        }));
        crate::drawing::with_pen(
            hdc,
            PS_SOLID,
            thickness,
            bgra_to_colorref(color),
            || unsafe {
                let _ = Polyline(hdc, &scratch);
            },
        );
    });
}

pub fn render_pen_preview(hdc: HDC, points: &[(i32, i32)], color: [u8; 4], thickness: i32) {
    draw_polyline(hdc, points, color, thickness, true);
}

/// Draws the stroke through GDI+ when `anti_aliased`, and falls back to plain GDI when GDI+
/// cannot draw it (or is not asked to).
fn draw_polyline(
    hdc: HDC,
    points: &[(i32, i32)],
    color: [u8; 4],
    thickness: i32,
    anti_aliased: bool,
) {
    if points.len() < 2 {
        return;
    }
    if anti_aliased && smooth::polyline(hdc, points, color, thickness) {
        return;
    }
    plain_polyline(hdc, points, color, thickness);
}

impl AnnotationObject {
    pub fn new(id: usize, kind: AnnotationKind) -> Self {
        Self { id, kind }
    }

    /// Returns the bounding box of the annotation in client coordinates.
    pub fn bounds(&self) -> Rect {
        match &self.kind {
            AnnotationKind::Rectangle {
                rect, thickness, ..
            }
            | AnnotationKind::Ellipse {
                rect, thickness, ..
            } => rect.inflate(*thickness, *thickness),
            AnnotationKind::Arrow {
                start,
                end,
                thickness,
                ..
            } => arrow_extent(*start, *end, *thickness),
            AnnotationKind::Line {
                start,
                end,
                thickness,
                ..
            } => segment_extent(*start, *end, *thickness),
            AnnotationKind::Pen {
                points, thickness, ..
            }
            | AnnotationKind::Highlight {
                points, thickness, ..
            } => {
                if points.is_empty() {
                    return Rect::default();
                }
                let mut left = points[0].0;
                let mut right = points[0].0;
                let mut top = points[0].1;
                let mut bottom = points[0].1;
                for p in points {
                    left = left.min(p.0);
                    right = right.max(p.0);
                    top = top.min(p.1);
                    bottom = bottom.max(p.1);
                }
                let radius = if matches!(&self.kind, AnnotationKind::Highlight { .. }) {
                    thickness.saturating_mul(2)
                } else {
                    *thickness
                };
                Rect::new(left, top, right + 1, bottom + 1).inflate(radius, radius)
            }
            AnnotationKind::Step { pos, radius, .. } => Rect::new(
                pos.0 - radius,
                pos.1 - radius,
                pos.0 + radius + 1,
                pos.1 + radius + 1,
            ),
            AnnotationKind::Text {
                pos,
                text,
                font_size,
                background,
                ..
            } => text_extent(*pos, text, *font_size, *background),
            AnnotationKind::Blur { rect, .. } | AnnotationKind::Redact { rect } => *rect,
        }
    }

    /// Tests if point `pt` hits this annotation object. A hollow frame or ellipse counts
    /// as hit across its whole interior; `hit_test_stroke` is the outline band only.
    pub fn hit_test(&self, pt: (i32, i32)) -> bool {
        match &self.kind {
            AnnotationKind::Rectangle {
                rect, thickness, ..
            } => {
                let tol = (*thickness).max(6);
                rect.inflate(tol, tol).contains(pt.0, pt.1)
            }
            AnnotationKind::Ellipse {
                rect, thickness, ..
            } => within_ellipse(*rect, (*thickness).max(6), pt),
            AnnotationKind::Arrow {
                start,
                end,
                thickness,
                ..
            }
            | AnnotationKind::Line {
                start,
                end,
                thickness,
                ..
            } => {
                let tol = (*thickness as f64 + 6.0).max(8.0);
                dist_to_segment(
                    (pt.0 as f64, pt.1 as f64),
                    (start.0 as f64, start.1 as f64),
                    (end.0 as f64, end.1 as f64),
                ) <= tol
            }
            AnnotationKind::Pen {
                points, thickness, ..
            }
            | AnnotationKind::Highlight {
                points, thickness, ..
            } => {
                let tol = if matches!(&self.kind, AnnotationKind::Highlight { .. }) {
                    (*thickness as f64 * 2.0).max(8.0)
                } else {
                    (*thickness as f64 + 6.0).max(8.0)
                };
                if points.len() == 1 {
                    let d = ((pt.0 - points[0].0).pow(2) + (pt.1 - points[0].1).pow(2)) as f64;
                    return d.sqrt() <= tol;
                }
                for i in 0..points.len().saturating_sub(1) {
                    let d = dist_to_segment(
                        (pt.0 as f64, pt.1 as f64),
                        (points[i].0 as f64, points[i].1 as f64),
                        (points[i + 1].0 as f64, points[i + 1].1 as f64),
                    );
                    if d <= tol {
                        return true;
                    }
                }
                false
            }
            AnnotationKind::Step { .. } => self.bounds().contains(pt.0, pt.1),
            AnnotationKind::Text { .. } => self.bounds().inflate(4, 4).contains(pt.0, pt.1),
            AnnotationKind::Blur { rect, .. } | AnnotationKind::Redact { rect } => {
                rect.contains(pt.0, pt.1)
            }
        }
    }

    /// Like `hit_test`, but a hollow frame or ellipse only hits on its outline band, so
    /// objects drawn inside it stay selectable.
    pub fn hit_test_stroke(&self, pt: (i32, i32)) -> bool {
        match &self.kind {
            AnnotationKind::Rectangle {
                rect, thickness, ..
            } => {
                let tol = (*thickness).max(6);
                rect.inflate(tol, tol).contains(pt.0, pt.1)
                    && !rect.inflate(-tol, -tol).contains(pt.0, pt.1)
            }
            AnnotationKind::Ellipse {
                rect, thickness, ..
            } => {
                let tol = (*thickness).max(6);
                within_ellipse(*rect, tol, pt) && !within_ellipse(*rect, -tol, pt)
            }
            _ => self.hit_test(pt),
        }
    }

    /// Translates this annotation object by `(dx, dy)`.
    pub fn translate(&mut self, dx: i32, dy: i32) {
        match &mut self.kind {
            AnnotationKind::Rectangle { rect, .. } | AnnotationKind::Ellipse { rect, .. } => {
                rect.left += dx;
                rect.right += dx;
                rect.top += dy;
                rect.bottom += dy;
            }
            AnnotationKind::Arrow { start, end, .. } | AnnotationKind::Line { start, end, .. } => {
                start.0 += dx;
                start.1 += dy;
                end.0 += dx;
                end.1 += dy;
            }
            AnnotationKind::Pen { points, .. } | AnnotationKind::Highlight { points, .. } => {
                for p in points {
                    p.0 += dx;
                    p.1 += dy;
                }
            }
            AnnotationKind::Text { pos, .. } | AnnotationKind::Step { pos, .. } => {
                pos.0 += dx;
                pos.1 += dy;
            }
            AnnotationKind::Blur { rect, .. } | AnnotationKind::Redact { rect } => {
                rect.left += dx;
                rect.right += dx;
                rect.top += dy;
                rect.bottom += dy;
            }
        }
    }

    pub fn geometry_bounds(&self) -> Rect {
        match &self.kind {
            AnnotationKind::Rectangle { rect, .. }
            | AnnotationKind::Ellipse { rect, .. }
            | AnnotationKind::Blur { rect, .. }
            | AnnotationKind::Redact { rect } => *rect,
            AnnotationKind::Arrow { start, end, .. } | AnnotationKind::Line { start, end, .. } => {
                Rect::normalized(*start, *end)
            }
            AnnotationKind::Pen { points, .. } | AnnotationKind::Highlight { points, .. } => {
                let Some(first) = points.first() else {
                    return Rect::default();
                };
                let (mut left, mut top) = *first;
                let (mut right, mut bottom) = *first;
                for &(x, y) in &points[1..] {
                    left = left.min(x);
                    right = right.max(x);
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
                Rect::new(left, top, right, bottom)
            }
            AnnotationKind::Text { .. } | AnnotationKind::Step { .. } => self.bounds(),
        }
    }

    /// The dashed box around the object, just outside what is drawn: the geometry grown by
    /// the stroke's reach and the gap. An arrow's or line's box is its drawn extent, so it
    /// holds the head and the round caps.
    fn selection_box(&self, dpi: u32) -> Rect {
        let (extent, reach) = match &self.kind {
            AnnotationKind::Arrow { .. } | AnnotationKind::Line { .. } => (self.bounds(), 0),
            AnnotationKind::Rectangle { thickness, .. }
            | AnnotationKind::Ellipse { thickness, .. }
            | AnnotationKind::Pen { thickness, .. } => {
                (self.geometry_bounds(), stroke_reach(*thickness))
            }
            // The marker radius `bounds` and `render_highlight` use.
            AnnotationKind::Highlight { thickness, .. } => {
                (self.geometry_bounds(), thickness.saturating_mul(2))
            }
            AnnotationKind::Step { .. }
            | AnnotationKind::Text { .. }
            | AnnotationKind::Blur { .. }
            | AnnotationKind::Redact { .. } => (self.geometry_bounds(), 0),
        };
        let margin = reach + grip_half(dpi);
        extent.inflate(margin, margin)
    }

    fn resize_handle_points(&self, dpi: u32) -> ResizeHandlePoints {
        if let AnnotationKind::Arrow { start, end, .. } | AnnotationKind::Line { start, end, .. } =
            &self.kind
        {
            return (
                [
                    (AnnotationResizeHandle::Start, *start),
                    (AnnotationResizeHandle::End, *end),
                    (AnnotationResizeHandle::End, *end),
                    (AnnotationResizeHandle::End, *end),
                ],
                2,
            );
        }
        if matches!(&self.kind, AnnotationKind::Step { .. }) {
            let empty = (AnnotationResizeHandle::TopLeft, (0, 0));
            return ([empty; 4], 0);
        }
        // The dashed box itself, so handles sit on what the user sees; hit-testing
        // follows these points automatically.
        let b = self.selection_box(dpi);
        (
            [
                (AnnotationResizeHandle::TopLeft, (b.left, b.top)),
                (AnnotationResizeHandle::TopRight, (b.right, b.top)),
                (AnnotationResizeHandle::BottomLeft, (b.left, b.bottom)),
                (AnnotationResizeHandle::BottomRight, (b.right, b.bottom)),
            ],
            4,
        )
    }

    pub fn hit_resize_handle(
        &self,
        pt: (i32, i32),
        radius: i32,
        dpi: u32,
    ) -> Option<AnnotationResizeHandle> {
        let (handles, count) = self.resize_handle_points(dpi);
        handles[..count]
            .iter()
            .find(|(_, (x, y))| (pt.0 - *x).abs() <= radius && (pt.1 - *y).abs() <= radius)
            .map(|(handle, _)| *handle)
    }

    /// How far the pointer is from the geometry the grip `handle` drags when it presses
    /// the grip at `press`: a corner of the geometry (the grip itself sits outside it, past
    /// the stroke and the gap) or an end of an arrow or line. A resize keeps this offset,
    /// so the grabbed point follows the pointer without jumping.
    pub fn resize_grab(&self, handle: AnnotationResizeHandle, press: (i32, i32)) -> (i32, i32) {
        let bounds = self.geometry_bounds();
        let anchor = match (&self.kind, handle) {
            (
                AnnotationKind::Arrow { start, .. } | AnnotationKind::Line { start, .. },
                AnnotationResizeHandle::Start,
            ) => *start,
            (
                AnnotationKind::Arrow { end, .. } | AnnotationKind::Line { end, .. },
                AnnotationResizeHandle::End,
            ) => *end,
            (_, AnnotationResizeHandle::TopLeft) => (bounds.left, bounds.top),
            (_, AnnotationResizeHandle::TopRight) => (bounds.right, bounds.top),
            (_, AnnotationResizeHandle::BottomLeft) => (bounds.left, bounds.bottom),
            (_, AnnotationResizeHandle::BottomRight) => (bounds.right, bounds.bottom),
            // Only arrows and lines have end grips, and they are matched above.
            (_, AnnotationResizeHandle::Start | AnnotationResizeHandle::End) => press,
        };
        (press.0 - anchor.0, press.1 - anchor.1)
    }

    fn resized_rect(
        source: Rect,
        handle: AnnotationResizeHandle,
        cursor: (i32, i32),
        limit: Rect,
        minimum: i32,
    ) -> Rect {
        let cursor = (
            cursor.0.clamp(limit.left, limit.right),
            cursor.1.clamp(limit.top, limit.bottom),
        );
        let fixed_left = source.left.clamp(limit.left, limit.right);
        let fixed_top = source.top.clamp(limit.top, limit.bottom);
        let fixed_right = source.right.clamp(limit.left, limit.right);
        let fixed_bottom = source.bottom.clamp(limit.top, limit.bottom);
        let moving_left = cursor
            .0
            .min((fixed_right - minimum).max(limit.left))
            .max(limit.left);
        let moving_top = cursor
            .1
            .min((fixed_bottom - minimum).max(limit.top))
            .max(limit.top);
        let moving_right = cursor
            .0
            .max((fixed_left + minimum).min(limit.right))
            .min(limit.right);
        let moving_bottom = cursor
            .1
            .max((fixed_top + minimum).min(limit.bottom))
            .min(limit.bottom);
        match handle {
            AnnotationResizeHandle::TopLeft => {
                Rect::new(moving_left, moving_top, fixed_right, fixed_bottom)
            }
            AnnotationResizeHandle::TopRight => {
                Rect::new(fixed_left, moving_top, moving_right, fixed_bottom)
            }
            AnnotationResizeHandle::BottomLeft => {
                Rect::new(moving_left, fixed_top, fixed_right, moving_bottom)
            }
            AnnotationResizeHandle::BottomRight => {
                Rect::new(fixed_left, fixed_top, moving_right, moving_bottom)
            }
            AnnotationResizeHandle::Start | AnnotationResizeHandle::End => source,
        }
    }

    /// The ends of a segment after the grip `handle` was dragged to `cursor`; unchanged when
    /// that would leave the ends within 3 px of each other on both axes.
    fn resized_segment(
        start: (i32, i32),
        end: (i32, i32),
        handle: AnnotationResizeHandle,
        cursor: (i32, i32),
    ) -> ((i32, i32), (i32, i32)) {
        let (candidate_start, candidate_end) = match handle {
            AnnotationResizeHandle::Start => (cursor, end),
            AnnotationResizeHandle::End => (start, cursor),
            _ => (start, end),
        };
        let long_enough = (candidate_end.0 - candidate_start.0).abs() >= 4
            || (candidate_end.1 - candidate_start.1).abs() >= 4;
        if long_enough {
            (candidate_start, candidate_end)
        } else {
            (start, end)
        }
    }

    pub fn resize_from(
        &mut self,
        original: &AnnotationKind,
        handle: AnnotationResizeHandle,
        cursor: (i32, i32),
        limit: Rect,
    ) {
        let cursor = (
            cursor.0.clamp(limit.left, limit.right),
            cursor.1.clamp(limit.top, limit.bottom),
        );
        self.kind = match original {
            AnnotationKind::Rectangle {
                rect,
                color,
                thickness,
            } => AnnotationKind::Rectangle {
                rect: Self::resized_rect(*rect, handle, cursor, limit, 6),
                color: *color,
                thickness: *thickness,
            },
            AnnotationKind::Ellipse {
                rect,
                color,
                thickness,
            } => AnnotationKind::Ellipse {
                rect: Self::resized_rect(*rect, handle, cursor, limit, 6),
                color: *color,
                thickness: *thickness,
            },
            AnnotationKind::Redact { rect } => AnnotationKind::Redact {
                rect: Self::resized_rect(*rect, handle, cursor, limit, 1),
            },
            AnnotationKind::Blur { rect, block_size } => AnnotationKind::Blur {
                rect: Self::resized_rect(*rect, handle, cursor, limit, 6),
                block_size: *block_size,
            },
            AnnotationKind::Arrow {
                start,
                end,
                color,
                thickness,
            } => {
                let (start, end) = Self::resized_segment(*start, *end, handle, cursor);
                AnnotationKind::Arrow {
                    start,
                    end,
                    color: *color,
                    thickness: *thickness,
                }
            }
            AnnotationKind::Line {
                start,
                end,
                color,
                thickness,
            } => {
                let (start, end) = Self::resized_segment(*start, *end, handle, cursor);
                AnnotationKind::Line {
                    start,
                    end,
                    color: *color,
                    thickness: *thickness,
                }
            }
            AnnotationKind::Pen {
                points,
                color,
                thickness,
            }
            | AnnotationKind::Highlight {
                points,
                color,
                thickness,
            } => {
                let source = AnnotationObject::new(0, original.clone()).geometry_bounds();
                let target = Self::resized_rect(source, handle, cursor, limit, 2);
                let source_w = (source.right - source.left).max(1) as f64;
                let source_h = (source.bottom - source.top).max(1) as f64;
                let target_w = target.right - target.left;
                let target_h = target.bottom - target.top;
                let scaled = points
                    .iter()
                    .map(|&(x, y)| {
                        let nx = target.left
                            + (((x - source.left) as f64 / source_w) * target_w as f64).round()
                                as i32;
                        let ny = target.top
                            + (((y - source.top) as f64 / source_h) * target_h as f64).round()
                                as i32;
                        (nx, ny)
                    })
                    .collect();
                if matches!(original, AnnotationKind::Highlight { .. }) {
                    AnnotationKind::Highlight {
                        points: scaled,
                        color: *color,
                        thickness: *thickness,
                    }
                } else {
                    AnnotationKind::Pen {
                        points: scaled,
                        color: *color,
                        thickness: *thickness,
                    }
                }
            }
            AnnotationKind::Text {
                pos: _,
                text,
                color,
                font_size,
                background,
            } => {
                let source = AnnotationObject::new(0, original.clone()).geometry_bounds();
                let anchor = match handle {
                    AnnotationResizeHandle::TopLeft => (source.right, source.bottom),
                    AnnotationResizeHandle::TopRight => (source.left, source.bottom),
                    AnnotationResizeHandle::BottomLeft => (source.right, source.top),
                    _ => (source.left, source.top),
                };
                let source_w = source.width().max(1) as f64;
                let source_h = source.height().max(1) as f64;
                let requested_w = (cursor.0 - anchor.0).abs().max(1) as f64;
                let requested_h = (cursor.1 - anchor.1).abs().max(1) as f64;
                let scale = (requested_w / source_w).min(requested_h / source_h);
                let requested_font = (*font_size as f64 * scale).round() as i32;
                let available_width = limit.width().max(1);
                let available_height = limit.height().max(1);
                let width_limited_font =
                    ((available_width as f64 / source_w) * *font_size as f64).floor() as i32;
                let max_font = width_limited_font
                    .min(available_height.saturating_sub(4))
                    .max(1);
                let requested_font = requested_font.clamp(8.min(max_font), max_font);
                // Extent grows with font size: binary-search the largest fitting
                // size (O(log n) measures) instead of a per-pixel decrement loop
                // re-measuring on every mouse move.
                let new_font = largest_fitting(requested_font, |size| {
                    let extent = text_extent((0, 0), text, size, *background);
                    extent.width() <= available_width && extent.height() <= available_height
                })
                .unwrap_or(1);
                let extent = text_extent((0, 0), text, new_font, *background);
                let (width, height) = (extent.width(), extent.height());
                // Top-left corner of the whole box, background padding included.
                let corner = match handle {
                    AnnotationResizeHandle::TopLeft => (anchor.0 - width, anchor.1 - height),
                    AnnotationResizeHandle::TopRight => (anchor.0, anchor.1 - height),
                    AnnotationResizeHandle::BottomLeft => (anchor.0 - width, anchor.1),
                    _ => anchor,
                };
                let padding = text_padding(new_font, *background);
                AnnotationKind::Text {
                    pos: (
                        corner
                            .0
                            .clamp(limit.left, (limit.right - width).max(limit.left))
                            + padding,
                        corner
                            .1
                            .clamp(limit.top, (limit.bottom - height).max(limit.top))
                            + padding,
                    ),
                    text: text.clone(),
                    color: *color,
                    font_size: new_font,
                    background: *background,
                }
            }
            AnnotationKind::Step { .. } => original.clone(),
        };
    }

    /// Updates the color of this annotation if applicable.
    pub fn set_color(&mut self, new_color: [u8; 4]) {
        match &mut self.kind {
            AnnotationKind::Rectangle { color, .. }
            | AnnotationKind::Ellipse { color, .. }
            | AnnotationKind::Arrow { color, .. }
            | AnnotationKind::Line { color, .. } => *color = new_color,
            AnnotationKind::Pen { color, .. }
            | AnnotationKind::Highlight { color, .. }
            | AnnotationKind::Step { color, .. }
            | AnnotationKind::Text { color, .. } => *color = new_color,
            AnnotationKind::Blur { .. } | AnnotationKind::Redact { .. } => {}
        }
    }

    /// Updates the stroke thickness of this annotation if applicable.
    pub fn set_thickness(&mut self, new_thickness: i32) {
        match &mut self.kind {
            AnnotationKind::Rectangle { thickness, .. }
            | AnnotationKind::Ellipse { thickness, .. }
            | AnnotationKind::Arrow { thickness, .. }
            | AnnotationKind::Line { thickness, .. }
            | AnnotationKind::Pen { thickness, .. }
            | AnnotationKind::Highlight { thickness, .. } => *thickness = new_thickness,
            AnnotationKind::Text { .. }
            | AnnotationKind::Step { .. }
            | AnnotationKind::Blur { .. }
            | AnnotationKind::Redact { .. } => {}
        }
    }

    /// Returns the color of this annotation if applicable.
    pub fn get_color(&self) -> Option<[u8; 4]> {
        match &self.kind {
            AnnotationKind::Rectangle { color, .. }
            | AnnotationKind::Ellipse { color, .. }
            | AnnotationKind::Arrow { color, .. }
            | AnnotationKind::Line { color, .. }
            | AnnotationKind::Pen { color, .. }
            | AnnotationKind::Highlight { color, .. }
            | AnnotationKind::Step { color, .. }
            | AnnotationKind::Text { color, .. } => Some(*color),
            AnnotationKind::Blur { .. } | AnnotationKind::Redact { .. } => None,
        }
    }

    /// Returns the thickness of this annotation if applicable.
    pub fn get_thickness(&self) -> Option<i32> {
        match &self.kind {
            AnnotationKind::Rectangle { thickness, .. }
            | AnnotationKind::Ellipse { thickness, .. }
            | AnnotationKind::Arrow { thickness, .. }
            | AnnotationKind::Line { thickness, .. }
            | AnnotationKind::Pen { thickness, .. }
            | AnnotationKind::Highlight { thickness, .. } => Some(*thickness),
            AnnotationKind::Text { .. }
            | AnnotationKind::Step { .. }
            | AnnotationKind::Blur { .. }
            | AnnotationKind::Redact { .. } => None,
        }
    }

    /// Whether this text annotation has a background box; `None` for every other kind.
    pub fn text_background(&self) -> Option<bool> {
        match &self.kind {
            AnnotationKind::Text { background, .. } => Some(*background),
            _ => None,
        }
    }

    /// Turns the background box of a text annotation on or off; other kinds are unchanged.
    pub fn set_text_background(&mut self, on: bool) {
        if let AnnotationKind::Text { background, .. } = &mut self.kind {
            *background = on;
        }
    }

    /// Renders vector shapes (Rectangle, Ellipse, Arrow, Line, Pen, Text) onto `hdc`.
    /// Strokes are anti-aliased through GDI+ when it is available and drawn with plain GDI
    /// otherwise.
    pub fn render_gdi(&self, hdc: HDC) {
        self.render_gdi_with(hdc, true);
    }

    /// [`render_gdi`](Self::render_gdi), with GDI+ strokes only when `anti_aliased`.
    fn render_gdi_with(&self, hdc: HDC, anti_aliased: bool) {
        match &self.kind {
            AnnotationKind::Rectangle {
                rect,
                color,
                thickness,
            } => {
                if anti_aliased && smooth::frame(hdc, *rect, *color, *thickness) {
                    return;
                }
                plain_frame(hdc, *rect, *color, *thickness);
            }
            AnnotationKind::Ellipse {
                rect,
                color,
                thickness,
            } => {
                if anti_aliased && smooth::ellipse(hdc, *rect, *color, *thickness) {
                    return;
                }
                plain_ellipse(hdc, *rect, *color, *thickness);
            }
            AnnotationKind::Arrow {
                start,
                end,
                color,
                thickness,
            } => {
                let Some(arrow) = arrow_geometry(*start, *end, *thickness) else {
                    return;
                };
                if anti_aliased && smooth::arrow(hdc, *start, &arrow, *color, *thickness) {
                    return;
                }
                plain_arrow(hdc, *start, *end, &arrow, *color, *thickness);
            }
            AnnotationKind::Line {
                start,
                end,
                color,
                thickness,
            } => draw_polyline(hdc, &[*start, *end], *color, *thickness, anti_aliased),
            AnnotationKind::Pen {
                points,
                color,
                thickness,
            } => draw_polyline(hdc, points, *color, *thickness, anti_aliased),
            AnnotationKind::Step {
                number,
                color,
                radius,
                ..
            } => {
                let circle = self.bounds();
                let ink = bgra_to_colorref(*color);
                crate::drawing::rounded(hdc, circle, 2 * radius, ink, ink);
                crate::drawing::label(
                    hdc,
                    circle,
                    &number.to_string(),
                    *radius,
                    bgra_to_colorref(contrast_bgra(*color)),
                    true,
                );
            }
            AnnotationKind::Text {
                pos,
                text,
                color,
                font_size,
                background,
            } => render_text(hdc, *pos, text, *color, *font_size, *background),
            AnnotationKind::Highlight { .. }
            | AnnotationKind::Blur { .. }
            | AnnotationKind::Redact { .. } => {
                // CPU pixel effects render through `render_blur`, not opaque GDI.
            }
        }
    }

    /// Renders the blur / pixelate effect directly onto the 32-bit BGRA buffer.
    pub fn render_blur(&self, buffer: &mut [u8], width: i32, height: i32) {
        match &self.kind {
            AnnotationKind::Highlight {
                points,
                color,
                thickness,
            } => {
                render_highlight(buffer, width, height, points, *color, *thickness);
            }
            AnnotationKind::Blur { rect, block_size } => {
                apply_pixelate_blur(buffer, width, height, rect, *block_size)
            }
            AnnotationKind::Redact { rect } => {
                let rect = rect.clamp(width, height);
                if width <= 0 || height <= 0 {
                    return;
                }
                for y in rect.top..rect.bottom {
                    for x in rect.left..rect.right {
                        let offset = (y as usize * width as usize + x as usize) * 4;
                        if let Some(pixel) = buffer.get_mut(offset..offset + 4) {
                            pixel.copy_from_slice(&crate::theme::BRAND_INK_BGRA);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Renders selection bounding indicators (dashed rectangle + corner handles) around this object.
    pub fn render_selection_indicator(&self, hdc: HDC, dpi: u32) {
        let half = grip_half(dpi);
        let b = self.selection_box(dpi);
        crate::drawing::with_pen(hdc, PS_DOT, 1, crate::theme::BRAND_AMBER, || {
            with_stock_object(hdc, NULL_BRUSH, || unsafe {
                let _ = GdiRectangle(hdc, b.left, b.top, b.right, b.bottom);
            })
        });

        // Handles: white squares with a solid amber outline, readable on any image.
        let (handles, count) = self.resize_handle_points(dpi);
        crate::drawing::with_pen(hdc, PS_SOLID, 1, crate::theme::BRAND_AMBER, || {
            with_stock_object(hdc, WHITE_BRUSH, || {
                for (_, (cx, cy)) in &handles[..count] {
                    unsafe {
                        let _ = GdiRectangle(hdc, cx - half, cy - half, cx + half, cy + half);
                    }
                }
            })
        });
    }
}

/// Blend each marker stroke once, including its self-intersections, over the preceding image.
pub(crate) fn render_highlight(
    buffer: &mut [u8],
    width: i32,
    height: i32,
    points: &[(i32, i32)],
    color: [u8; 4],
    thickness: i32,
) {
    if width <= 0
        || height <= 0
        || points.is_empty()
        || (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .is_none_or(|bytes| bytes > buffer.len())
    {
        return;
    }
    let radius = thickness.clamp(1, 64) * 2;
    let mut left = points[0].0;
    let mut top = points[0].1;
    let mut right = left;
    let mut bottom = top;
    for &(x, y) in &points[1..] {
        left = left.min(x);
        top = top.min(y);
        right = right.max(x);
        bottom = bottom.max(y);
    }
    let bounds = Rect::new(
        left.saturating_sub(radius),
        top.saturating_sub(radius),
        right.saturating_add(radius + 1),
        bottom.saturating_add(radius + 1),
    )
    .clamp(width, height);
    if bounds.is_empty() {
        return;
    }
    let bw = bounds.width() as usize;
    let bh = bounds.height() as usize;
    thread_local! { static MASK: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) }; }
    MASK.with(|scratch| {
        let mut mask = scratch.borrow_mut();
        let area = bw * bh;
        mask.resize(area, 0);
        mask[..area].fill(0);
        let segments = points.iter().zip(points.iter().skip(1)).chain(
            points
                .first()
                .filter(|_| points.len() == 1)
                .map(|point| (point, point)),
        );
        for (start, end) in segments {
            let dx = end.0 - start.0;
            let dy = end.1 - start.1;
            let count = (dx.abs().max(dy.abs()) / (radius / 2).max(1)).max(1);
            for step in 0..=count {
                let cx = start.0 + ((dx as i64 * step as i64) / count as i64) as i32;
                let cy = start.1 + ((dy as i64 * step as i64) / count as i64) as i32;
                for y in (cy - radius).max(bounds.top)..=(cy + radius).min(bounds.bottom - 1) {
                    let vertical = y - cy;
                    let half = ((radius * radius - vertical * vertical) as f64).sqrt() as i32;
                    let row_left = (cx - half).max(bounds.left);
                    let row_right = (cx + half + 1).min(bounds.right);
                    if row_left < row_right {
                        let offset =
                            (y - bounds.top) as usize * bw + (row_left - bounds.left) as usize;
                        mask[offset..offset + (row_right - row_left) as usize].fill(1);
                    }
                }
            }
        }
        const ALPHA: u32 = 96;
        for y in 0..bh {
            for x in 0..bw {
                if mask[y * bw + x] == 0 {
                    continue;
                }
                let offset =
                    ((bounds.top as usize + y) * width as usize + bounds.left as usize + x) * 4;
                for channel in 0..3 {
                    buffer[offset + channel] = ((buffer[offset + channel] as u32 * (255 - ALPHA)
                        + color[channel] as u32 * ALPHA
                        + 127)
                        / 255) as u8;
                }
                buffer[offset + 3] = 255;
            }
        }
    });
}

/// Applies a fast mosaic pixelation / box-average blur directly to a 32-bit BGRA buffer.
/// Makes underlying text completely unreadable while keeping smooth performance.
pub fn apply_pixelate_blur(
    buffer: &mut [u8],
    width: i32,
    height: i32,
    rect: &Rect,
    block_size: i32,
) {
    if rect.is_empty() || width <= 0 || height <= 0 {
        return;
    }
    let clamped = rect.clamp(width, height);
    if clamped.is_empty() {
        return;
    }

    let b_size = block_size.max(4);
    let stride = width as usize * 4;

    let mut by = clamped.top;
    while by < clamped.bottom {
        let by_end = (by + b_size).min(clamped.bottom);
        let mut bx = clamped.left;
        while bx < clamped.right {
            let bx_end = (bx + b_size).min(clamped.right);

            // 1. Compute average color for this block
            let mut sum_b = 0u64;
            let mut sum_g = 0u64;
            let mut sum_r = 0u64;
            let mut count = 0u64;

            for y in by..by_end {
                let row_start = y as usize * stride;
                for x in bx..bx_end {
                    let offset = row_start + x as usize * 4;
                    if offset + 4 <= buffer.len() {
                        sum_b += buffer[offset] as u64;
                        sum_g += buffer[offset + 1] as u64;
                        sum_r += buffer[offset + 2] as u64;
                        count += 1;
                    }
                }
            }

            if let Some(non_zero_count) = std::num::NonZeroU64::new(count) {
                let avg_b = (sum_b / non_zero_count) as u8;
                let avg_g = (sum_g / non_zero_count) as u8;
                let avg_r = (sum_r / non_zero_count) as u8;

                // 2. Fill all pixels in block with average color
                for y in by..by_end {
                    let row_start = y as usize * stride;
                    for x in bx..bx_end {
                        let offset = row_start + x as usize * 4;
                        if offset + 4 <= buffer.len() {
                            buffer[offset] = avg_b;
                            buffer[offset + 1] = avg_g;
                            buffer[offset + 2] = avg_r;
                            buffer[offset + 3] = 255;
                        }
                    }
                }
            }

            bx += b_size;
        }
        by += b_size;
    }
}

/// Helper: calculates the shortest distance from point `p` to line segment `(a, b)`.
fn dist_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let l2 = (b.0 - a.0).powi(2) + (b.1 - a.1).powi(2);
    if l2 == 0.0 {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    let t = (((p.0 - a.0) * (b.0 - a.0) + (p.1 - a.1) * (b.1 - a.1)) / l2).clamp(0.0, 1.0);
    let proj = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
    ((p.0 - proj.0).powi(2) + (p.1 - proj.1).powi(2)).sqrt()
}

/// Whether `pt` lies inside the ellipse inscribed in `rect` once that is grown by `grow` px
/// on every side (shrunk when negative). An ellipse shrunk to nothing holds no point.
fn within_ellipse(rect: Rect, grow: i32, pt: (i32, i32)) -> bool {
    let radius_x = (rect.right - rect.left) as f64 / 2.0 + grow as f64;
    let radius_y = (rect.bottom - rect.top) as f64 / 2.0 + grow as f64;
    if radius_x <= 0.0 || radius_y <= 0.0 {
        return false;
    }
    let dx = pt.0 as f64 - (rect.left + rect.right) as f64 / 2.0;
    let dy = pt.1 as f64 - (rect.top + rect.bottom) as f64 / 2.0;
    (dx / radius_x).powi(2) + (dy / radius_y).powi(2) <= 1.0
}

/// Snaps a rectangle drag end point to form a 1:1 aspect ratio square.
pub fn snap_square(start: (i32, i32), current: (i32, i32)) -> (i32, i32) {
    let dx = current.0 - start.0;
    let dy = current.1 - start.1;
    let side = dx.abs().max(dy.abs());
    let sign_x = if dx >= 0 { 1 } else { -1 };
    let sign_y = if dy >= 0 { 1 } else { -1 };
    (start.0 + side * sign_x, start.1 + side * sign_y)
}

/// Snaps an arrow end point to the nearest 45-degree angle increment.
pub fn snap_angle_45(start: (i32, i32), current: (i32, i32)) -> (i32, i32) {
    let dx = (current.0 - start.0) as f64;
    let dy = (current.1 - start.1) as f64;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 2.0 {
        return current;
    }
    let angle = dy.atan2(dx);
    let step = std::f64::consts::PI / 4.0; // 45 degrees
    let snapped_angle = (angle / step).round() * step;
    (
        start.0 + (len * snapped_angle.cos()).round() as i32,
        start.1 + (len * snapped_angle.sin()).round() as i32,
    )
}

/// Command for Undo/Redo history.
#[derive(Debug, Clone)]
pub enum EditCommand {
    Add(AnnotationObject),
    Delete {
        object: AnnotationObject,
        index: usize,
    },
    Selection {
        before: Rect,
        after: Rect,
        shift: (i32, i32),
    },
    Modify {
        id: usize,
        old_kind: AnnotationKind,
        new_kind: AnnotationKind,
    },
}

/// Source of a repeatable edit; consecutive edits from one source merge into
/// a single undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeKey {
    Nudge,
    Thickness,
}

/// Longest pause between two edits that still merge into the same undo step.
const MERGE_WINDOW: Duration = Duration::from_secs(1);

/// Manages the Undo / Redo history stack (capped at 50 commands).
pub struct HistoryManager {
    undo_stack: Vec<EditCommand>,
    redo_stack: Vec<EditCommand>,
    max_entries: usize,
    selection_update: Option<Rect>,
    /// Source and time of the most recent edit when it sits on top of the undo stack.
    merge_marker: Option<(MergeKey, Instant)>,
}

impl Default for HistoryManager {
    fn default() -> Self {
        Self::new(50)
    }
}

impl HistoryManager {
    pub fn new(max_entries: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_entries,
            selection_update: None,
            merge_marker: None,
        }
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.selection_update = None;
        self.merge_marker = None;
    }

    pub fn take_selection_update(&mut self) -> Option<Rect> {
        self.selection_update.take()
    }

    pub fn record(&mut self, cmd: EditCommand) {
        self.undo_stack.push(cmd);
        if self.undo_stack.len() > self.max_entries {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.merge_marker = None;
    }

    /// Records `cmd`, folding it into the top undo entry when that entry came
    /// from the same `key` less than a second ago and edits the same object
    /// (`Modify`) or the same selection (`Selection`). The merged entry keeps
    /// its original `old_kind`/`before`, so one undo reverts the whole run.
    pub fn record_merge(&mut self, cmd: EditCommand, key: MergeKey) {
        self.record_merge_at(cmd, key, Instant::now());
    }

    fn record_merge_at(&mut self, cmd: EditCommand, key: MergeKey, now: Instant) {
        let recent = self.merge_marker.is_some_and(|(marker_key, at)| {
            marker_key == key && now.saturating_duration_since(at) < MERGE_WINDOW
        });
        if recent {
            match (self.undo_stack.last_mut(), &cmd) {
                (
                    Some(EditCommand::Modify { id, new_kind, .. }),
                    EditCommand::Modify {
                        id: new_id,
                        new_kind: latest,
                        ..
                    },
                ) if *id == *new_id => {
                    *new_kind = latest.clone();
                    self.redo_stack.clear();
                    self.merge_marker = Some((key, now));
                    return;
                }
                (
                    Some(EditCommand::Selection { after, shift, .. }),
                    EditCommand::Selection {
                        after: latest,
                        shift: step,
                        ..
                    },
                ) => {
                    *after = *latest;
                    shift.0 += step.0;
                    shift.1 += step.1;
                    self.redo_stack.clear();
                    self.merge_marker = Some((key, now));
                    return;
                }
                _ => {}
            }
        }
        self.record(cmd);
        self.merge_marker = Some((key, now));
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Reverses the last action, updating `objects`.
    pub fn undo(&mut self, objects: &mut Vec<AnnotationObject>) -> bool {
        let Some(cmd) = self.undo_stack.pop() else {
            return false;
        };
        self.merge_marker = None;

        match cmd {
            EditCommand::Add(obj) => {
                // To undo Add: remove the object and save to redo stack
                if let Some(pos) = objects.iter().position(|o| o.id == obj.id) {
                    let removed = objects.remove(pos);
                    self.redo_stack.push(EditCommand::Add(removed));
                }
            }
            EditCommand::Delete { object, index } => {
                objects.insert(index.min(objects.len()), object.clone());
                self.redo_stack.push(EditCommand::Delete { object, index });
            }
            EditCommand::Selection {
                before,
                after,
                shift,
            } => {
                for object in objects.iter_mut() {
                    object.translate(-shift.0, -shift.1);
                }
                self.selection_update = Some(before);
                self.redo_stack.push(EditCommand::Selection {
                    before,
                    after,
                    shift,
                });
            }
            EditCommand::Modify {
                id,
                old_kind,
                new_kind,
            } => {
                if let Some(obj) = objects.iter_mut().find(|o| o.id == id) {
                    obj.kind = old_kind.clone();
                    self.redo_stack.push(EditCommand::Modify {
                        id,
                        old_kind,
                        new_kind,
                    });
                }
            }
        }
        true
    }

    /// Re-applies the previously undone action, updating `objects`.
    pub fn redo(&mut self, objects: &mut Vec<AnnotationObject>) -> bool {
        let Some(cmd) = self.redo_stack.pop() else {
            return false;
        };
        self.merge_marker = None;

        match cmd {
            EditCommand::Add(obj) => {
                // The local object is the pushed one: clone it directly instead
                // of re-finding by id (unwrap breaks if ids ever collide).
                self.undo_stack.push(EditCommand::Add(obj.clone()));
                objects.push(obj);
            }
            EditCommand::Delete { object, index } => {
                if let Some(pos) = objects.iter().position(|o| o.id == object.id) {
                    let removed = objects.remove(pos);
                    self.undo_stack.push(EditCommand::Delete {
                        object: removed,
                        index,
                    });
                }
            }
            EditCommand::Selection {
                before,
                after,
                shift,
            } => {
                for object in objects.iter_mut() {
                    object.translate(shift.0, shift.1);
                }
                self.selection_update = Some(after);
                self.undo_stack.push(EditCommand::Selection {
                    before,
                    after,
                    shift,
                });
            }
            EditCommand::Modify {
                id,
                old_kind,
                new_kind,
            } => {
                if let Some(obj) = objects.iter_mut().find(|o| o.id == id) {
                    obj.kind = new_kind.clone();
                    self.undo_stack.push(EditCommand::Modify {
                        id,
                        old_kind,
                        new_kind,
                    });
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::AnnotationResizeHandle::{BottomLeft, BottomRight, End, Start, TopLeft, TopRight};
    use super::*;

    const COLOR: [u8; 4] = [49, 49, 224, 255];

    fn assert_within(rect: Rect, limit: Rect) {
        assert!(rect.left >= limit.left && rect.top >= limit.top, "{rect:?}");
        assert!(
            rect.right <= limit.right && rect.bottom <= limit.bottom,
            "{rect:?}"
        );
    }

    fn object(kind: AnnotationKind) -> AnnotationObject {
        AnnotationObject::new(1, kind)
    }

    fn rectangle_kind(rect: Rect, thickness: i32) -> AnnotationKind {
        AnnotationKind::Rectangle {
            rect,
            color: COLOR,
            thickness,
        }
    }

    fn arrow_kind(start: (i32, i32), end: (i32, i32), thickness: i32) -> AnnotationKind {
        AnnotationKind::Arrow {
            start,
            end,
            color: COLOR,
            thickness,
        }
    }

    fn pen_kind(points: &[(i32, i32)], thickness: i32) -> AnnotationKind {
        AnnotationKind::Pen {
            points: points.to_vec(),
            color: COLOR,
            thickness,
        }
    }

    fn highlight_kind(points: &[(i32, i32)], thickness: i32) -> AnnotationKind {
        AnnotationKind::Highlight {
            points: points.to_vec(),
            color: COLOR,
            thickness,
        }
    }

    fn step_kind(pos: (i32, i32), radius: i32) -> AnnotationKind {
        AnnotationKind::Step {
            pos,
            number: 1,
            color: COLOR,
            radius,
        }
    }

    fn text_kind(pos: (i32, i32), text: &str, font_size: i32) -> AnnotationKind {
        AnnotationKind::Text {
            pos,
            text: text.to_string(),
            color: COLOR,
            font_size,
            background: false,
        }
    }

    fn boxed_text_kind(pos: (i32, i32), text: &str, font_size: i32) -> AnnotationKind {
        AnnotationKind::Text {
            pos,
            text: text.to_string(),
            color: COLOR,
            font_size,
            background: true,
        }
    }

    fn blur_kind(rect: Rect) -> AnnotationKind {
        AnnotationKind::Blur {
            rect,
            block_size: 8,
        }
    }

    fn redact_kind(rect: Rect) -> AnnotationKind {
        AnnotationKind::Redact { rect }
    }

    /// The font size of a text annotation; `None` for every other kind.
    fn font_size_of(object: &AnnotationObject) -> Option<i32> {
        match &object.kind {
            AnnotationKind::Text { font_size, .. } => Some(*font_size),
            _ => None,
        }
    }

    /// 8-bit BGRA gradient: blue grows by 16 per column, green by 16 per row.
    fn gradient(width: usize, height: usize) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [x as u8 * 16, y as u8 * 16, 100, 7]))
            .collect()
    }

    fn pixel_at(buffer: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        buffer[(y * width + x) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn rectangle_and_tiny_edge_blur_resize_stay_bounded() {
        let limit = Rect::new(0, 0, 20, 20);
        let rectangle = AnnotationKind::Rectangle {
            rect: Rect::new(5, 5, 10, 10),
            color: COLOR,
            thickness: 2,
        };
        let mut object = AnnotationObject::new(1, rectangle.clone());
        object.resize_from(
            &rectangle,
            AnnotationResizeHandle::BottomRight,
            (18, 19),
            limit,
        );
        assert_eq!(object.geometry_bounds(), Rect::new(5, 5, 18, 19));

        let blur = AnnotationKind::Blur {
            rect: Rect::new(0, 0, 3, 3),
            block_size: 8,
        };
        let mut object = AnnotationObject::new(2, blur.clone());
        object.resize_from(&blur, AnnotationResizeHandle::TopLeft, (-20, -20), limit);
        assert_eq!(object.geometry_bounds(), Rect::new(0, 0, 3, 3));
        assert_within(object.geometry_bounds(), limit);
    }

    #[test]
    fn arrow_endpoint_resize_clamps_to_selection() {
        let limit = Rect::new(0, 0, 20, 20);
        let arrow = AnnotationKind::Arrow {
            start: (5, 5),
            end: (10, 10),
            color: COLOR,
            thickness: 2,
        };
        let mut object = AnnotationObject::new(1, arrow.clone());
        object.resize_from(&arrow, AnnotationResizeHandle::End, (100, -5), limit);
        assert_eq!(object.kind, arrow_kind((5, 5), (20, 0), 2));
    }

    #[test]
    fn pen_resize_scales_points_from_fixed_corner() {
        let pen = AnnotationKind::Pen {
            points: vec![(10, 10), (15, 15), (20, 20)],
            color: COLOR,
            thickness: 2,
        };
        let mut object = AnnotationObject::new(1, pen.clone());
        object.resize_from(
            &pen,
            AnnotationResizeHandle::BottomRight,
            (30, 40),
            Rect::new(0, 0, 50, 50),
        );
        assert_eq!(object.kind, pen_kind(&[(10, 10), (20, 25), (30, 40)], 2));
    }

    #[test]
    fn text_resize_changes_font_proportionally_and_stays_bounded() {
        let limit = Rect::new(0, 0, 60, 40);
        let text = text_kind((10, 10), "Hi", 20);
        let mut object = AnnotationObject::new(1, text.clone());
        object.resize_from(
            &text,
            AnnotationResizeHandle::BottomRight,
            (1000, 1000),
            limit,
        );
        let font_size = font_size_of(&object).unwrap();
        assert!((20..=40).contains(&font_size), "{font_size}");
        let bounds = object.geometry_bounds();
        assert_eq!((bounds.left, bounds.top), (10, 10));
        assert_within(bounds, limit);
        assert_eq!(font_size_of(&AnnotationObject::new(2, frame_at(0))), None);
    }

    #[test]
    fn modify_transform_is_one_undoable_and_redoable_command() {
        let old_kind = AnnotationKind::Rectangle {
            rect: Rect::new(2, 2, 12, 12),
            color: COLOR,
            thickness: 2,
        };
        let new_kind = AnnotationKind::Rectangle {
            rect: Rect::new(4, 5, 18, 19),
            color: COLOR,
            thickness: 2,
        };
        let mut objects = vec![AnnotationObject::new(7, new_kind.clone())];
        let mut history = HistoryManager::default();
        history.record(EditCommand::Modify {
            id: 7,
            old_kind: old_kind.clone(),
            new_kind: new_kind.clone(),
        });

        assert!(history.undo(&mut objects));
        assert_eq!(objects[0].kind, old_kind);
        assert!(!history.can_undo());
        assert!(history.can_redo());
        assert!(history.redo(&mut objects));
        assert_eq!(objects[0].kind, new_kind);
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }
    #[test]
    fn deletion_and_selection_translation_restore_exact_history_order() {
        let kind = AnnotationKind::Rectangle {
            rect: Rect::new(10, 10, 30, 30),
            color: COLOR,
            thickness: 2,
        };
        let mut objects: Vec<_> = (1..=3)
            .map(|id| AnnotationObject::new(id, kind.clone()))
            .collect();
        let mut history = HistoryManager::default();
        let object = objects.remove(1);
        history.record(EditCommand::Delete { object, index: 1 });
        for object in &mut objects {
            object.translate(20, 5);
        }
        let before = Rect::new(0, 0, 50, 50);
        let after = Rect::new(20, 5, 70, 55);
        history.record(EditCommand::Selection {
            before,
            after,
            shift: (20, 5),
        });
        assert!(history.undo(&mut objects));
        assert_eq!(history.take_selection_update(), Some(before));
        assert!(history.undo(&mut objects));
        assert_eq!(
            objects.iter().map(|object| object.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(objects.iter().all(|object| object.kind == kind));
        assert!(history.redo(&mut objects));
        assert!(history.redo(&mut objects));
        assert_eq!(history.take_selection_update(), Some(after));
        history.clear();
        assert!(!history.undo(&mut objects));
        assert!(!history.redo(&mut objects));
    }

    #[test]
    fn opaque_redaction_replaces_pixels_and_text_uses_real_glyph_width() {
        let mut pixels = vec![200u8; 12 * 12 * 4];
        AnnotationObject::new(
            1,
            AnnotationKind::Redact {
                rect: Rect::new(2, 3, 9, 10),
            },
        )
        .render_blur(&mut pixels, 12, 12);
        assert_eq!(
            &pixels[(3 * 12 + 2) * 4..(3 * 12 + 2) * 4 + 4],
            &crate::theme::BRAND_INK_BGRA
        );
        assert_eq!(&pixels[..4], &[200; 4]);
        let narrow = crate::drawing::measure_text("iiii", 22);
        let wide = crate::drawing::measure_text("WWWW", 22);
        assert!(wide.0 > narrow.0);
        let text = "A&B İıŞşĞğ🙂";
        let object = AnnotationObject::new(2, text_kind((4, 5), text, 22));
        assert_eq!(
            object.geometry_bounds().width(),
            crate::drawing::measure_text(text, 22).0
        );
    }
    #[test]
    fn highlighter_blends_once_per_stroke_and_leaves_outside_pixels_unchanged() {
        let original = [100, 100, 100, 255];
        let mut pixels = original.repeat(40 * 20);
        AnnotationObject::new(
            1,
            AnnotationKind::Highlight {
                points: vec![(4, 10), (20, 10), (8, 10), (20, 10)],
                color: [200, 0, 0, 255],
                thickness: 2,
            },
        )
        .render_blur(&mut pixels, 40, 20);
        assert_eq!(
            &pixels[(10 * 40 + 10) * 4..(10 * 40 + 10) * 4 + 4],
            &[138, 62, 62, 255]
        );
        assert_eq!(
            &pixels[(10 * 40 + 30) * 4..(10 * 40 + 30) * 4 + 4],
            &original
        );
    }

    fn frame_at(x: i32) -> AnnotationKind {
        AnnotationKind::Rectangle {
            rect: Rect::new(x, 0, x + 10, 10),
            color: COLOR,
            thickness: 2,
        }
    }

    fn nudge(id: usize, from: i32, to: i32) -> EditCommand {
        EditCommand::Modify {
            id,
            old_kind: frame_at(from),
            new_kind: frame_at(to),
        }
    }

    fn undo_steps(history: &mut HistoryManager, objects: &mut Vec<AnnotationObject>) -> usize {
        let mut steps = 0;
        while history.undo(objects) {
            steps += 1;
        }
        steps
    }

    #[test]
    fn record_merge_folds_repeats_into_one_undo_step() {
        let t0 = Instant::now();
        let mut history = HistoryManager::default();
        // Each gap is under the window even though the run lasts 1.7 s overall.
        history.record_merge_at(nudge(7, 0, 1), MergeKey::Nudge, t0);
        history.record_merge_at(
            nudge(7, 1, 2),
            MergeKey::Nudge,
            t0 + Duration::from_millis(900),
        );
        history.record_merge_at(
            nudge(7, 2, 3),
            MergeKey::Nudge,
            t0 + Duration::from_millis(1700),
        );
        let mut objects = vec![AnnotationObject::new(7, frame_at(3))];

        assert!(history.undo(&mut objects));
        assert_eq!(objects[0].kind, frame_at(0));
        assert!(!history.can_undo());
        assert!(history.redo(&mut objects));
        assert_eq!(objects[0].kind, frame_at(3));
    }

    #[test]
    fn record_merge_starts_a_new_step_after_the_window() {
        let t0 = Instant::now();
        let mut history = HistoryManager::default();
        history.record_merge_at(nudge(7, 0, 1), MergeKey::Nudge, t0);
        history.record_merge_at(nudge(7, 1, 2), MergeKey::Nudge, t0 + MERGE_WINDOW);
        let mut objects = vec![AnnotationObject::new(7, frame_at(2))];

        assert!(history.undo(&mut objects));
        assert_eq!(objects[0].kind, frame_at(1));
        assert_eq!(undo_steps(&mut history, &mut objects), 1);
        assert_eq!(objects[0].kind, frame_at(0));
    }

    #[test]
    fn record_merge_keeps_different_objects_and_sources_apart() {
        let t0 = Instant::now();
        let mut history = HistoryManager::default();
        history.record_merge_at(nudge(1, 0, 1), MergeKey::Nudge, t0);
        history.record_merge_at(
            nudge(2, 0, 1),
            MergeKey::Nudge,
            t0 + Duration::from_millis(10),
        );
        history.record_merge_at(
            nudge(2, 1, 2),
            MergeKey::Thickness,
            t0 + Duration::from_millis(20),
        );
        let mut objects = vec![
            AnnotationObject::new(1, frame_at(1)),
            AnnotationObject::new(2, frame_at(2)),
        ];

        assert_eq!(undo_steps(&mut history, &mut objects), 3);
        assert_eq!(objects[0].kind, frame_at(0));
        assert_eq!(objects[1].kind, frame_at(0));
    }

    #[test]
    fn record_merge_accumulates_selection_nudges() {
        let t0 = Instant::now();
        let before = Rect::new(0, 0, 50, 50);
        let mut history = HistoryManager::default();
        history.record_merge_at(
            EditCommand::Selection {
                before,
                after: Rect::new(1, 0, 51, 50),
                shift: (1, 0),
            },
            MergeKey::Nudge,
            t0,
        );
        history.record_merge_at(
            EditCommand::Selection {
                before: Rect::new(1, 0, 51, 50),
                after: Rect::new(2, 1, 52, 51),
                shift: (1, 1),
            },
            MergeKey::Nudge,
            t0 + Duration::from_millis(50),
        );
        // The object moved by the combined shift of both nudges.
        let mut objects = vec![AnnotationObject::new(1, frame_at(0))];
        objects[0].translate(2, 1);

        assert!(history.undo(&mut objects));
        assert_eq!(history.take_selection_update(), Some(before));
        assert_eq!(objects[0].kind, frame_at(0));
        assert!(!history.can_undo());
        assert!(history.redo(&mut objects));
        assert_eq!(
            history.take_selection_update(),
            Some(Rect::new(2, 1, 52, 51))
        );
    }

    #[test]
    fn plain_record_undo_and_redo_end_a_merge_run() {
        let t0 = Instant::now();
        let soon = t0 + Duration::from_millis(10);
        let mut objects = vec![
            AnnotationObject::new(7, frame_at(1)),
            AnnotationObject::new(8, frame_at(50)),
        ];

        let mut history = HistoryManager::default();
        history.record_merge_at(nudge(7, 0, 1), MergeKey::Nudge, t0);
        history.record(EditCommand::Add(objects[1].clone()));
        history.record_merge_at(nudge(7, 1, 2), MergeKey::Nudge, soon);
        assert_eq!(undo_steps(&mut history, &mut objects), 3);

        let mut history = HistoryManager::default();
        history.record_merge_at(nudge(7, 0, 1), MergeKey::Nudge, t0);
        assert!(history.undo(&mut objects));
        assert!(history.can_redo());
        history.record_merge_at(nudge(7, 0, 1), MergeKey::Nudge, soon);
        assert!(!history.can_redo());
        history.record_merge_at(nudge(7, 1, 2), MergeKey::Nudge, soon);
        assert_eq!(undo_steps(&mut history, &mut objects), 1);

        let mut history = HistoryManager::default();
        history.record_merge_at(nudge(7, 0, 1), MergeKey::Nudge, t0);
        assert!(history.undo(&mut objects));
        assert!(history.redo(&mut objects));
        history.record_merge_at(nudge(7, 1, 2), MergeKey::Nudge, soon);
        assert_eq!(undo_steps(&mut history, &mut objects), 2);
    }

    #[test]
    fn frame_stroke_hit_test_excludes_the_interior() {
        let frame = AnnotationObject::new(
            1,
            AnnotationKind::Rectangle {
                rect: Rect::new(100, 100, 300, 200),
                color: COLOR,
                thickness: 2,
            },
        );
        // Band is `tol = 6` px either side of the frame edge.
        assert!(frame.hit_test_stroke((100, 150)));
        assert!(frame.hit_test_stroke((200, 105)));
        assert!(frame.hit_test_stroke((200, 195)));
        assert!(!frame.hit_test_stroke((200, 150)));
        assert!(!frame.hit_test_stroke((50, 50)));
        // `hit_test` keeps counting the interior.
        assert!(frame.hit_test((200, 150)));
        assert!(frame.hit_test((100, 150)));
        assert!(!frame.hit_test((50, 50)));

        let tiny = AnnotationObject::new(
            2,
            AnnotationKind::Rectangle {
                rect: Rect::new(10, 10, 14, 14),
                color: COLOR,
                thickness: 2,
            },
        );
        assert!(tiny.hit_test_stroke((12, 12)));
    }

    #[test]
    fn stroke_hit_test_matches_hit_test_for_non_frames() {
        let kinds = [
            AnnotationKind::Arrow {
                start: (0, 0),
                end: (100, 0),
                color: COLOR,
                thickness: 2,
            },
            AnnotationKind::Redact {
                rect: Rect::new(10, 10, 60, 60),
            },
            AnnotationKind::Step {
                pos: (30, 30),
                number: 1,
                color: COLOR,
                radius: 14,
            },
        ];
        for kind in kinds {
            let object = AnnotationObject::new(1, kind);
            for pt in [(30, 30), (50, 0), (50, 40), (500, 500)] {
                assert_eq!(object.hit_test_stroke(pt), object.hit_test(pt), "{pt:?}");
            }
        }
    }

    #[test]
    fn step_geometry_follows_radius() {
        let step = |radius| {
            AnnotationObject::new(
                1,
                AnnotationKind::Step {
                    pos: (50, 60),
                    number: 3,
                    color: COLOR,
                    radius,
                },
            )
        };
        assert_eq!(step(14).bounds(), Rect::new(36, 46, 65, 75));
        let large = step(28);
        assert_eq!(large.bounds(), Rect::new(22, 32, 79, 89));
        assert!(large.hit_test((22, 60)));
        assert!(!step(14).hit_test((22, 60)));
    }

    #[test]
    fn object_grips_and_indicator_gap_scale_with_dpi() {
        let rectangle = AnnotationObject::new(
            1,
            AnnotationKind::Rectangle {
                rect: Rect::new(100, 100, 200, 200),
                color: COLOR,
                thickness: 2,
            },
        );
        let top_left = |dpi| rectangle.resize_handle_points(dpi).0[0].1;
        // The 1 px reach of the 2 px stroke plus the gap.
        assert_eq!(top_left(96), (95, 95));
        assert_eq!(top_left(192), (91, 91));
        // Never smaller than the 96-DPI size.
        assert_eq!(top_left(48), (95, 95));

        // Same pointer and radius: the grip sits at (91, 91) only at 192 DPI.
        assert_eq!(
            rectangle.hit_resize_handle((91, 91), 3, 192),
            Some(AnnotationResizeHandle::TopLeft)
        );
        assert_eq!(rectangle.hit_resize_handle((91, 91), 3, 96), None);
    }

    #[test]
    fn arrow_head_follows_thickness_and_is_capped_by_the_arrow_length() {
        // Arrows start at the origin: (end, thickness, head length, head half-width).
        for (end, thickness, head_len, half_width) in [
            ((100, 0), 3, 16.0, 8.75),
            ((200, 0), 40, 96.0, 58.0),
            // Half the length caps the head of a short arrow.
            ((20, 0), 3, 10.0, 5.75),
        ] {
            let arrow = arrow_geometry((0, 0), end, thickness).unwrap();
            let tip = (end.0 as f64, 0.0);
            assert_eq!(arrow.head[0], tip);
            assert_eq!(arrow.base, (tip.0 - head_len, 0.0));
            assert_eq!(arrow.head[1], (tip.0 - head_len, half_width));
            assert_eq!(arrow.head[2], (tip.0 - head_len, -half_width));
        }
    }

    #[test]
    fn arrow_head_is_a_symmetric_triangle_on_the_shaft_axis_when_diagonal() {
        let (start, end) = ((0, 0), (30, 40));
        let arrow = arrow_geometry(start, end, 3).unwrap();
        let [tip, left, right] = arrow.head;
        let length = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
        let axis = (end.0 as f64 - start.0 as f64, end.1 as f64 - start.1 as f64);

        assert!((length(tip, arrow.base) - 16.0).abs() < 1e-9);
        assert!((length(left, right) - 17.5).abs() < 1e-9);
        assert!((length(left, arrow.base) - length(right, arrow.base)).abs() < 1e-9);
        // The base lies on the shaft axis and the head's width is square to it.
        let on_axis = axis.0 * arrow.base.1 - axis.1 * arrow.base.0;
        assert!(on_axis.abs() < 1e-9);
        let across = axis.0 * (left.0 - right.0) + axis.1 * (left.1 - right.1);
        assert!(across.abs() < 1e-9);
    }

    #[test]
    fn arrows_shorter_than_two_pixels_have_no_geometry() {
        assert!(arrow_geometry((5, 5), (5, 5), 3).is_none());
        assert!(arrow_geometry((5, 5), (6, 6), 3).is_none());
        assert!(arrow_geometry((5, 5), (7, 5), 3).is_some());
    }

    #[test]
    fn bgra_packs_into_a_colorref_with_red_in_the_low_byte() {
        assert_eq!(
            bgra_to_colorref([0x11, 0x22, 0x33, 0x44]),
            COLORREF(0x0011_2233)
        );
    }

    #[test]
    fn bounds_include_the_stroke_of_each_kind() {
        let points = [(10, 20), (30, 5), (20, 40)];
        let (text_width, text_height) = crate::drawing::measure_text("Hi", 20);
        let rect = Rect::new(1, 2, 31, 42);
        for (kind, expected) in [
            (
                rectangle_kind(Rect::new(10, 20, 50, 40), 3),
                Rect::new(7, 17, 53, 43),
            ),
            // Arrow: the shaft to the head's base grown by half its width plus a pixel,
            // joined with the head triangle (tip included) plus a pixel.
            (arrow_kind((50, 10), (20, 40), 2), Rect::new(19, 8, 52, 42)),
            // A thick arrow's head (half-width 58) reaches past the shaft on both sides,
            // and nothing extends beyond the tip.
            (
                arrow_kind((0, 0), (200, 0), 40),
                Rect::new(-21, -59, 202, 60),
            ),
            // Too short for a head: just the stroke around the points.
            (arrow_kind((5, 5), (6, 5), 3), Rect::new(2, 2, 9, 8)),
            // Pen: the points' box including the last pixel, grown by the stroke width.
            (pen_kind(&points, 3), Rect::new(7, 2, 34, 44)),
            // Highlight: the marker is twice as thick as the stroke width.
            (highlight_kind(&points, 3), Rect::new(4, -1, 37, 47)),
            (pen_kind(&[], 3), Rect::default()),
            (highlight_kind(&[], 3), Rect::default()),
            (
                text_kind((12, 15), "Hi", 20),
                Rect::new(12, 15, 12 + text_width, 15 + text_height),
            ),
            (blur_kind(rect), rect),
            (redact_kind(rect), rect),
        ] {
            assert_eq!(object(kind.clone()).bounds(), expected, "{kind:?}");
        }
    }

    #[test]
    fn hit_testing_uses_the_tolerance_of_each_kind() {
        let text = object(text_kind((300, 300), "Hi", 20));
        let text_box = text.bounds();
        let sides = [(0, 0), (0, 50), (50, 50)];
        // (kind, point, expected)
        let mut cases = vec![
            (
                rectangle_kind(Rect::new(100, 100, 200, 160), 2),
                (94, 130),
                true,
            ),
            (
                rectangle_kind(Rect::new(100, 100, 200, 160), 2),
                (93, 130),
                false,
            ),
            (
                rectangle_kind(Rect::new(100, 100, 200, 160), 2),
                (150, 130),
                true,
            ),
            (
                rectangle_kind(Rect::new(100, 100, 200, 160), 2),
                (205, 130),
                true,
            ),
            (
                rectangle_kind(Rect::new(100, 100, 200, 160), 2),
                (206, 130),
                false,
            ),
            (arrow_kind((0, 0), (100, 0), 2), (50, 8), true),
            (arrow_kind((0, 0), (100, 0), 2), (50, 9), false),
            (arrow_kind((0, 0), (100, 0), 2), (108, 0), true),
            (arrow_kind((0, 0), (100, 0), 2), (109, 0), false),
            (arrow_kind((0, 0), (100, 0), 2), (-8, 0), true),
            // An arrow that starts and ends on one point is a disc.
            (arrow_kind((10, 10), (10, 10), 2), (10, 18), true),
            (arrow_kind((10, 10), (10, 10), 2), (10, 19), false),
            (arrow_kind((10, 10), (10, 10), 2), (18, 10), true),
            (pen_kind(&[(10, 10)], 2), (10, 18), true),
            (pen_kind(&[(10, 10)], 2), (10, 19), false),
            (pen_kind(&sides, 2), (2, 25), true),
            (pen_kind(&sides, 2), (25, 52), true),
            (pen_kind(&sides, 2), (25, 25), false),
            (pen_kind(&[(0, 0), (100, 0)], 10), (50, 16), true),
            (pen_kind(&[(0, 0), (100, 0)], 10), (50, 17), false),
            // The marker is wider than a pen of the same thickness.
            (highlight_kind(&[(0, 0), (100, 0)], 10), (50, 20), true),
            (highlight_kind(&[(0, 0), (100, 0)], 10), (50, 21), false),
            (pen_kind(&[], 2), (0, 0), false),
            (step_kind((50, 60), 14), (36, 46), true),
            (step_kind((50, 60), 14), (65, 46), false),
            (blur_kind(Rect::new(10, 10, 60, 60)), (10, 10), true),
            (blur_kind(Rect::new(10, 10, 60, 60)), (60, 10), false),
            (redact_kind(Rect::new(10, 10, 60, 60)), (59, 59), true),
            (redact_kind(Rect::new(10, 10, 60, 60)), (60, 60), false),
        ];
        // Text is grabbed within 4 px of its box.
        for (point, expected) in [
            ((text_box.left - 4, text_box.top), true),
            ((text_box.left - 5, text_box.top), false),
            ((text_box.right + 3, text_box.top), true),
            ((text_box.right + 4, text_box.top), false),
        ] {
            cases.push((text.kind.clone(), point, expected));
        }
        for (kind, point, expected) in cases {
            assert_eq!(
                object(kind.clone()).hit_test(point),
                expected,
                "{kind:?} {point:?}"
            );
        }
    }

    #[test]
    fn translation_moves_every_kind_by_the_offset() {
        let points = [(1, 1), (2, 2)];
        let moved = [(6, -2), (7, -1)];
        for (kind, expected) in [
            (
                rectangle_kind(Rect::new(10, 10, 30, 20), 2),
                rectangle_kind(Rect::new(15, 7, 35, 17), 2),
            ),
            (
                arrow_kind((0, 0), (10, 10), 2),
                arrow_kind((5, -3), (15, 7), 2),
            ),
            (pen_kind(&points, 2), pen_kind(&moved, 2)),
            (highlight_kind(&points, 2), highlight_kind(&moved, 2)),
            (step_kind((50, 60), 14), step_kind((55, 57), 14)),
            (text_kind((50, 60), "Hi", 20), text_kind((55, 57), "Hi", 20)),
            (
                blur_kind(Rect::new(10, 10, 30, 20)),
                blur_kind(Rect::new(15, 7, 35, 17)),
            ),
            (
                redact_kind(Rect::new(10, 10, 30, 20)),
                redact_kind(Rect::new(15, 7, 35, 17)),
            ),
        ] {
            let mut moving = object(kind.clone());
            moving.translate(5, -3);
            assert_eq!(moving.kind, expected, "{kind:?}");
        }
    }

    #[test]
    fn geometry_bounds_are_the_unstroked_extent() {
        let points = [(10, 20), (30, 5), (20, 40)];
        let rect = Rect::new(1, 2, 31, 42);
        for (kind, expected) in [
            (rectangle_kind(rect, 4), rect),
            (arrow_kind((50, 10), (20, 40), 2), Rect::new(20, 10, 50, 40)),
            (pen_kind(&points, 3), Rect::new(10, 5, 30, 40)),
            (highlight_kind(&points, 3), Rect::new(10, 5, 30, 40)),
            (pen_kind(&[], 3), Rect::default()),
            (highlight_kind(&[], 3), Rect::default()),
            (blur_kind(rect), rect),
            (redact_kind(rect), rect),
        ] {
            assert_eq!(object(kind.clone()).geometry_bounds(), expected, "{kind:?}");
        }
        let text = object(text_kind((12, 15), "Hi", 20));
        assert_eq!(text.geometry_bounds(), text.bounds());
        let step = object(step_kind((50, 60), 14));
        assert_eq!(step.geometry_bounds(), step.bounds());
    }

    #[test]
    fn text_bounds_cover_every_line() {
        let content = "Hi\nA wider second line\nx";
        let (width, height) = crate::drawing::measure_text(content, 20);
        let line = crate::drawing::line_height(20);
        assert_eq!(height, 3 * line);
        assert_eq!(
            width,
            crate::drawing::measure_text("A wider second line", 20).0
        );
        assert_eq!(
            object(text_kind((12, 15), content, 20)).bounds(),
            Rect::new(12, 15, 12 + width, 15 + 3 * line)
        );
    }

    #[test]
    fn a_background_box_pads_the_text_by_a_quarter_of_the_font_size() {
        for (font_size, boxed, expected) in
            [(20, true, 5), (60, true, 15), (4, true, 2), (20, false, 0)]
        {
            assert_eq!(
                text_padding(font_size, boxed),
                expected,
                "{font_size} {boxed}"
            );
        }
        let content = "Hi\nthere";
        let (width, height) = crate::drawing::measure_text(content, 20);
        let boxed = object(boxed_text_kind((100, 80), content, 20));
        assert_eq!(
            boxed.bounds(),
            Rect::new(95, 75, 100 + width + 5, 80 + height + 5)
        );
        assert_eq!(boxed.geometry_bounds(), boxed.bounds());
        assert_eq!(
            object(text_kind((100, 80), content, 20)).bounds(),
            Rect::new(100, 80, 100 + width, 80 + height)
        );
    }

    #[test]
    fn text_over_a_background_box_is_black_on_light_fills_and_white_on_dark_ones() {
        let white = [255, 255, 255, 255];
        let black = [0, 0, 0, 255];
        for (fill, ink) in [
            ([0, 0, 0, 255], white),
            ([49, 49, 224, 255], white),
            ([140, 140, 140, 255], white),
            ([141, 141, 141, 255], black),
            ([0, 200, 255, 255], black),
            ([255, 255, 255, 255], black),
            // Green weighs far more than blue: full green is light, full blue is dark.
            ([255, 0, 0, 255], white),
            ([0, 255, 0, 255], black),
        ] {
            assert_eq!(text_ink_bgra(fill, true), ink, "{fill:?}");
            assert_eq!(text_ink(fill, true), bgra_to_colorref(ink), "{fill:?}");
            // Without a box the text keeps its own colour.
            assert_eq!(text_ink_bgra(fill, false), fill, "{fill:?}");
            assert_eq!(text_ink(fill, false), bgra_to_colorref(fill), "{fill:?}");
        }
    }

    #[test]
    fn a_background_box_is_grabbed_across_its_whole_area() {
        let plain = object(text_kind((300, 300), "Hi", 20));
        let boxed = object(boxed_text_kind((300, 300), "Hi", 20));
        let (width, height) = crate::drawing::measure_text("Hi", 20);
        // The box reaches 5 px past the text on every side; grabbing 4 px beyond either.
        for (point, plain_hit, boxed_hit) in [
            ((300, 300), true, true),
            ((297, 297), true, true),
            ((293, 300), false, true),
            ((291, 300), false, true),
            ((290, 300), false, false),
            ((300, 293), false, true),
            ((300, 291), false, true),
            ((300, 290), false, false),
            ((300 + width + 8, 300), false, true),
            ((300 + width + 9, 300), false, false),
            ((300, 300 + height + 8), false, true),
            ((300, 300 + height + 9), false, false),
        ] {
            assert_eq!(plain.hit_test(point), plain_hit, "plain {point:?}");
            assert_eq!(boxed.hit_test(point), boxed_hit, "boxed {point:?}");
        }
    }

    #[test]
    fn moving_a_boxed_text_moves_the_box_with_it() {
        let mut moving = object(boxed_text_kind((50, 60), "Hi\nthere", 20));
        let before = moving.bounds();
        moving.translate(7, -4);
        assert_eq!(
            moving.bounds(),
            Rect::new(
                before.left + 7,
                before.top - 4,
                before.right + 7,
                before.bottom - 4
            )
        );
        assert_eq!(moving.text_background(), Some(true));
    }

    #[test]
    fn the_background_box_is_a_text_only_attribute() {
        let mut text = object(text_kind((5, 5), "A", 20));
        assert_eq!(text.text_background(), Some(false));
        text.set_text_background(true);
        assert_eq!(text.text_background(), Some(true));
        assert_eq!(text.kind, boxed_text_kind((5, 5), "A", 20));
        text.set_text_background(false);
        assert_eq!(text.kind, text_kind((5, 5), "A", 20));

        let frame = rectangle_kind(Rect::new(0, 0, 10, 10), 2);
        let mut other = object(frame.clone());
        assert_eq!(other.text_background(), None);
        other.set_text_background(true);
        assert_eq!(other.kind, frame);
    }

    #[test]
    fn a_boxed_text_resizes_with_its_box_around_the_fixed_corner() {
        let limit = Rect::new(0, 0, 400, 300);
        let original = boxed_text_kind((100, 100), "Hi\nthere", 20);
        let source = object(original.clone()).geometry_bounds();
        let (width, height) = (source.width(), source.height());
        type Case = (
            AnnotationResizeHandle,
            (i32, i32),
            (i32, i32),
            fn(Rect) -> (i32, i32),
        );
        let cases: [Case; 2] = [
            (
                TopLeft,
                (source.right - 2 * width, source.bottom - 2 * height),
                (source.right, source.bottom),
                |b| (b.right, b.bottom),
            ),
            (
                BottomRight,
                (source.left + 2 * width, source.top + 2 * height),
                (source.left, source.top),
                |b| (b.left, b.top),
            ),
        ];
        for (handle, cursor, fixed, corner) in cases {
            let mut resized = object(original.clone());
            resized.resize_from(&original, handle, cursor, limit);
            assert_eq!(corner(resized.geometry_bounds()), fixed, "{handle:?}");
            assert!(font_size_of(&resized).unwrap() > 20, "{handle:?}");
            assert_eq!(resized.text_background(), Some(true), "{handle:?}");
            assert_within(resized.geometry_bounds(), limit);
        }
    }

    #[test]
    fn each_kind_offers_its_own_resize_grips() {
        let rectangle = object(rectangle_kind(Rect::new(100, 100, 200, 200), 2));
        // The grips sit on the corners of the bounds grown by the 1 px reach of the 2 px
        // stroke and the 4 px gap; an ellipse has the same four.
        let ellipse = object(AnnotationKind::Ellipse {
            rect: Rect::new(100, 100, 200, 200),
            color: COLOR,
            thickness: 2,
        });
        for shape in [&rectangle, &ellipse] {
            for (point, handle) in [
                ((95, 95), TopLeft),
                ((205, 95), TopRight),
                ((95, 205), BottomLeft),
                ((205, 205), BottomRight),
            ] {
                assert_eq!(shape.hit_resize_handle(point, 3, 96), Some(handle));
            }
            assert_eq!(shape.hit_resize_handle((150, 150), 3, 96), None);
        }

        // An arrow or a line is resized by its two ends only.
        let line = object(AnnotationKind::Line {
            start: (10, 10),
            end: (60, 40),
            color: COLOR,
            thickness: 2,
        });
        for segment in [object(arrow_kind((10, 10), (60, 40), 2)), line] {
            assert_eq!(segment.hit_resize_handle((11, 9), 3, 96), Some(Start));
            assert_eq!(segment.hit_resize_handle((62, 41), 3, 96), Some(End));
            assert_eq!(segment.hit_resize_handle((35, 25), 3, 96), None);
            assert_eq!(segment.hit_resize_handle((6, 6), 3, 96), None);
        }

        // A step badge cannot be resized.
        let step = object(step_kind((50, 60), 14));
        for point in [(50, 60), (36, 46), (64, 74), (0, 0)] {
            assert_eq!(step.hit_resize_handle(point, 30, 96), None, "{point:?}");
        }
    }

    #[test]
    fn rectangle_like_resizes_move_the_dragged_corner_and_keep_a_minimum_size() {
        let limit = Rect::new(0, 0, 100, 100);
        let source = Rect::new(10, 10, 20, 20);
        for (handle, cursor, expected) in [
            (TopLeft, (5, 5), Rect::new(5, 5, 20, 20)),
            (TopRight, (40, 5), Rect::new(10, 5, 40, 20)),
            (BottomLeft, (5, 40), Rect::new(5, 10, 20, 40)),
            (BottomRight, (40, 50), Rect::new(10, 10, 40, 50)),
            // Dragged past the fixed corner: a redaction stays one pixel wide.
            (BottomRight, (0, 0), Rect::new(10, 10, 11, 11)),
            // Arrow ends do not apply to a rectangle.
            (Start, (50, 50), source),
            (End, (50, 50), source),
        ] {
            let original = redact_kind(source);
            let mut redaction = object(original.clone());
            redaction.resize_from(&original, handle, cursor, limit);
            assert_eq!(
                redaction.kind,
                redact_kind(expected),
                "{handle:?} {cursor:?}"
            );
        }

        // Frames and blurs keep six pixels.
        let frame = rectangle_kind(source, 2);
        let mut resized = object(frame.clone());
        resized.resize_from(&frame, BottomRight, (11, 11), limit);
        assert_eq!(resized.kind, rectangle_kind(Rect::new(10, 10, 16, 16), 2));
        let blur = blur_kind(source);
        let mut resized = object(blur.clone());
        resized.resize_from(&blur, TopLeft, (19, 19), limit);
        assert_eq!(resized.kind, blur_kind(Rect::new(14, 14, 20, 20)));
    }

    #[test]
    fn arrow_resize_moves_one_end_and_ignores_drags_that_collapse_the_arrow() {
        let limit = Rect::new(0, 0, 100, 100);
        let original = arrow_kind((5, 5), (50, 50), 2);
        for (handle, cursor, start, end) in [
            (Start, (20, 10), (20, 10), (50, 50)),
            // Less than 4 px of length on both axes: the arrow stays as it was.
            (End, (7, 7), (5, 5), (50, 50)),
            // 4 px along either axis is long enough.
            (End, (5, 9), (5, 5), (5, 9)),
            (End, (9, 5), (5, 5), (9, 5)),
            // Corner grips do not apply to an arrow.
            (TopLeft, (60, 60), (5, 5), (50, 50)),
        ] {
            let mut arrow = object(original.clone());
            arrow.resize_from(&original, handle, cursor, limit);
            assert_eq!(
                arrow.kind,
                arrow_kind(start, end, 2),
                "{handle:?} {cursor:?}"
            );
        }
    }

    fn ellipse_kind(rect: Rect, thickness: i32) -> AnnotationKind {
        AnnotationKind::Ellipse {
            rect,
            color: COLOR,
            thickness,
        }
    }

    fn line_kind(start: (i32, i32), end: (i32, i32), thickness: i32) -> AnnotationKind {
        AnnotationKind::Line {
            start,
            end,
            color: COLOR,
            thickness,
        }
    }

    #[test]
    fn line_resize_moves_one_end_clamps_and_ignores_collapsing_drags() {
        let limit = Rect::new(0, 0, 100, 100);
        let original = line_kind((5, 5), (50, 50), 2);
        for (handle, cursor, start, end) in [
            (Start, (20, 10), (20, 10), (50, 50)),
            (End, (500, -5), (5, 5), (100, 0)),
            (End, (7, 7), (5, 5), (50, 50)),
            (End, (9, 5), (5, 5), (9, 5)),
            (TopLeft, (60, 60), (5, 5), (50, 50)),
        ] {
            let mut line = object(original.clone());
            line.resize_from(&original, handle, cursor, limit);
            assert_eq!(line.kind, line_kind(start, end, 2), "{handle:?} {cursor:?}");
        }
    }

    #[test]
    fn ellipse_resize_moves_the_dragged_corner_and_keeps_six_pixels() {
        let limit = Rect::new(0, 0, 100, 100);
        let source = Rect::new(10, 10, 20, 20);
        let original = ellipse_kind(source, 3);
        for (handle, cursor, expected) in [
            (TopLeft, (5, 5), Rect::new(5, 5, 20, 20)),
            (TopRight, (40, 5), Rect::new(10, 5, 40, 20)),
            (BottomLeft, (5, 40), Rect::new(5, 10, 20, 40)),
            (BottomRight, (40, 50), Rect::new(10, 10, 40, 50)),
            (BottomRight, (11, 11), Rect::new(10, 10, 16, 16)),
            (Start, (50, 50), source),
        ] {
            let mut ellipse = object(original.clone());
            ellipse.resize_from(&original, handle, cursor, limit);
            assert_eq!(
                ellipse.kind,
                ellipse_kind(expected, 3),
                "{handle:?} {cursor:?}"
            );
        }
    }

    #[test]
    fn a_resize_drag_keeps_the_pointer_offset_from_the_geometry_it_grabbed() {
        let limit = Rect::new(0, 0, 400, 400);
        for (kind, handle, grip) in [
            (
                rectangle_kind(Rect::new(100, 100, 200, 200), 12),
                TopLeft,
                (90, 90),
            ),
            (
                ellipse_kind(Rect::new(100, 100, 200, 200), 12),
                BottomRight,
                (210, 210),
            ),
            (
                pen_kind(&[(100, 100), (200, 200)], 7),
                BottomLeft,
                (92, 208),
            ),
        ] {
            let mut dragged = object(kind.clone());
            // Pressing 2 px inside the grip: the grab offset is relative to the corner.
            let press = (grip.0 + 2, grip.1 - 2);
            let grab = dragged.resize_grab(handle, press);
            let corner = (press.0 - grab.0, press.1 - grab.1);
            // Pointer moves by (+7, +5): the corner follows by exactly that much.
            let pointer = (press.0 + 7, press.1 + 5);
            dragged.resize_from(
                &kind,
                handle,
                (pointer.0 - grab.0, pointer.1 - grab.1),
                limit,
            );
            let before = object(kind.clone()).geometry_bounds();
            let after = dragged.geometry_bounds();
            let (moved, anchor) = match handle {
                TopLeft => (
                    (after.left - before.left, after.top - before.top),
                    (before.left, before.top),
                ),
                BottomRight => (
                    (after.right - before.right, after.bottom - before.bottom),
                    (before.right, before.bottom),
                ),
                _ => (
                    (after.left - before.left, after.bottom - before.bottom),
                    (before.left, before.bottom),
                ),
            };
            assert_eq!(moved, (7, 5), "{kind:?}");
            assert_eq!(corner, anchor, "{kind:?}");
        }
        // Arrow and line grips grab the end point itself.
        for kind in [
            arrow_kind((10, 10), (60, 40), 2),
            line_kind((10, 10), (60, 40), 2),
        ] {
            let segment = object(kind);
            assert_eq!(segment.resize_grab(Start, (12, 9)), (2, -1));
            assert_eq!(segment.resize_grab(End, (58, 43)), (-2, 3));
        }
        // A grip the kind does not have grabs nothing.
        assert_eq!(
            object(rectangle_kind(Rect::new(0, 0, 10, 10), 2)).resize_grab(Start, (5, 5)),
            (0, 0)
        );
        assert_eq!(
            object(rectangle_kind(Rect::new(0, 0, 10, 10), 2)).resize_grab(TopRight, (13, -4)),
            (3, -4)
        );
    }

    #[test]
    fn the_selection_box_clears_the_stroke_of_every_kind_by_the_gap() {
        let rect = Rect::new(100, 100, 200, 180);
        // (kind, reach of the stroke beyond the geometry)
        for (kind, reach) in [
            (rectangle_kind(rect, 12), 6),
            (rectangle_kind(rect, 5), 3),
            (ellipse_kind(rect, 12), 6),
            (pen_kind(&[(100, 100), (200, 180)], 9), 5),
            (highlight_kind(&[(100, 100), (200, 180)], 6), 12),
            (blur_kind(rect), 0),
        ] {
            let shape = object(kind.clone());
            for (dpi, gap) in [(96, 4), (192, 8)] {
                let expected = shape.geometry_bounds().inflate(reach + gap, reach + gap);
                assert_eq!(shape.selection_box(dpi), expected, "{kind:?} dpi {dpi}");
            }
        }
        // An arrow or line is boxed by its drawn extent.
        for kind in [
            arrow_kind((10, 10), (90, 40), 8),
            line_kind((10, 10), (90, 40), 8),
        ] {
            let segment = object(kind);
            assert_eq!(segment.selection_box(96), segment.bounds().inflate(4, 4));
        }
        let step = object(step_kind((50, 60), 14));
        assert_eq!(step.selection_box(96), step.bounds().inflate(4, 4));
    }

    #[test]
    fn ellipse_hits_follow_the_outline_and_the_interior_separately() {
        let ellipse = object(ellipse_kind(Rect::new(100, 100, 300, 200), 2));
        // Band: 6 px either side of the outline. Centre, left and top vertices:
        assert!(ellipse.hit_test_stroke((100, 150)));
        assert!(ellipse.hit_test_stroke((200, 100)));
        assert!(ellipse.hit_test_stroke((200, 203)));
        assert!(ellipse.hit_test_stroke((97, 150)));
        // Inside the band's inner edge and outside its outer edge.
        assert!(!ellipse.hit_test_stroke((200, 150)));
        assert!(!ellipse.hit_test_stroke((250, 150)));
        assert!(!ellipse.hit_test_stroke((90, 150)));
        // The corner of the bounding box is not on the ellipse.
        assert!(!ellipse.hit_test_stroke((102, 102)));
        assert!(!ellipse.hit_test((102, 102)));
        // `hit_test` counts the whole interior.
        assert!(ellipse.hit_test((200, 150)));
        assert!(ellipse.hit_test((100, 150)));
        assert!(!ellipse.hit_test((50, 50)));
        // An ellipse thinner than its band is all outline.
        let sliver = object(ellipse_kind(Rect::new(10, 10, 14, 40), 2));
        assert!(sliver.hit_test_stroke((12, 25)));
        // A wide pen widens the band.
        let thick = object(ellipse_kind(Rect::new(100, 100, 300, 200), 20));
        assert!(thick.hit_test_stroke((100 - 18, 150)));
        assert!(!thick.hit_test_stroke((200, 150)));
    }

    #[test]
    fn lines_hit_by_distance_to_the_segment_and_bound_the_stroke() {
        let line = object(line_kind((100, 100), (200, 100), 3));
        // Tolerance is thickness + 6 px, at least 8.
        assert!(line.hit_test((150, 100)));
        assert!(line.hit_test((150, 109)));
        assert!(!line.hit_test((150, 110)));
        assert!(line.hit_test((92, 100)));
        assert!(!line.hit_test((90, 100)));
        assert_eq!(line.hit_test_stroke((150, 109)), line.hit_test((150, 109)));
        // Box grown by half the stroke and a pixel of anti-aliasing.
        assert_eq!(line.bounds(), Rect::new(97, 97, 203, 103));
        assert_eq!(line.geometry_bounds(), Rect::new(100, 100, 200, 100));
    }

    #[test]
    fn ellipses_and_lines_move_recolor_and_restyle_like_the_other_strokes() {
        let new_color = [1, 2, 3, 255];
        let mut ellipse = object(ellipse_kind(Rect::new(10, 20, 50, 60), 3));
        ellipse.translate(5, -3);
        ellipse.set_color(new_color);
        ellipse.set_thickness(9);
        assert_eq!(ellipse.get_color(), Some(new_color));
        assert_eq!(ellipse.get_thickness(), Some(9));
        assert_eq!(ellipse.geometry_bounds(), Rect::new(15, 17, 55, 57));
        assert_eq!(ellipse.bounds(), Rect::new(6, 8, 64, 66));

        let mut line = object(line_kind((10, 20), (50, 60), 3));
        line.translate(5, -3);
        line.set_color(new_color);
        line.set_thickness(9);
        assert_eq!(line.get_color(), Some(new_color));
        assert_eq!(line.get_thickness(), Some(9));
        assert_eq!(
            line.kind,
            AnnotationKind::Line {
                start: (15, 17),
                end: (55, 57),
                color: new_color,
                thickness: 9,
            }
        );
    }

    #[test]
    fn stroke_resizes_scale_the_points_and_keep_the_kind() {
        let limit = Rect::new(0, 0, 50, 50);
        let marker = highlight_kind(&[(10, 10), (20, 20)], 2);
        let mut resized = object(marker.clone());
        resized.resize_from(&marker, BottomRight, (30, 30), limit);
        assert_eq!(resized.kind, highlight_kind(&[(10, 10), (30, 30)], 2));

        let pen = pen_kind(&[(10, 10), (20, 20)], 2);
        let mut resized = object(pen.clone());
        resized.resize_from(&pen, TopLeft, (0, 0), limit);
        assert_eq!(resized.kind, pen_kind(&[(0, 0), (20, 20)], 2));
    }

    #[test]
    fn step_badges_ignore_resizing() {
        let step = step_kind((50, 60), 14);
        let mut resized = object(step.clone());
        resized.resize_from(&step, BottomRight, (90, 90), Rect::new(0, 0, 100, 100));
        assert_eq!(resized.kind, step);
    }

    #[test]
    fn text_resize_keeps_the_corner_opposite_the_dragged_grip() {
        let limit = Rect::new(0, 0, 400, 300);
        let original = text_kind((100, 100), "Hi", 20);
        let source = object(original.clone()).geometry_bounds();
        let (width, height) = (source.width(), source.height());
        // Each cursor is twice as far from the fixed corner as the text is wide and tall.
        type Case = (
            AnnotationResizeHandle,
            (i32, i32),
            (i32, i32),
            fn(Rect) -> (i32, i32),
        );
        let cases: [Case; 4] = [
            (
                TopLeft,
                (source.right - 2 * width, source.bottom - 2 * height),
                (source.right, source.bottom),
                |b| (b.right, b.bottom),
            ),
            (
                TopRight,
                (source.left + 2 * width, source.bottom - 2 * height),
                (source.left, source.bottom),
                |b| (b.left, b.bottom),
            ),
            (
                BottomLeft,
                (source.right - 2 * width, source.top + 2 * height),
                (source.right, source.top),
                |b| (b.right, b.top),
            ),
            (
                BottomRight,
                (source.left + 2 * width, source.top + 2 * height),
                (source.left, source.top),
                |b| (b.left, b.top),
            ),
        ];
        for (handle, cursor, fixed, corner) in cases {
            let mut resized = object(original.clone());
            resized.resize_from(&original, handle, cursor, limit);
            assert_eq!(corner(resized.geometry_bounds()), fixed, "{handle:?}");
            assert!(font_size_of(&resized).unwrap() > 20, "{handle:?}");
        }
    }

    #[test]
    fn largest_fitting_finds_the_biggest_size_the_predicate_accepts() {
        for (max, limit, expected) in [
            (40, 17, Some(17)),
            (40, 40, Some(40)),
            (10, 1000, Some(10)),
            (40, 1, Some(1)),
            (40, 0, None),
            (0, 5, None),
        ] {
            assert_eq!(
                largest_fitting(max, |size| size <= limit),
                expected,
                "{max} {limit}"
            );
        }
        // The search probes the sizes logarithmically, not one by one.
        let mut probes = 0;
        assert_eq!(
            largest_fitting(1000, |size| {
                probes += 1;
                size <= 321
            }),
            Some(321)
        );
        assert!(probes <= 10, "{probes}");
    }

    #[test]
    fn color_and_thickness_apply_only_to_the_kinds_that_have_them() {
        let new_color = [1, 2, 3, 255];
        let rect = Rect::new(0, 0, 10, 10);
        // (kind, has a color, has a thickness)
        for (kind, has_color, has_thickness) in [
            (rectangle_kind(rect, 2), true, true),
            (arrow_kind((0, 0), (10, 10), 2), true, true),
            (pen_kind(&[(0, 0), (5, 5)], 2), true, true),
            (highlight_kind(&[(0, 0), (5, 5)], 2), true, true),
            (step_kind((5, 5), 14), true, false),
            (text_kind((5, 5), "A", 20), true, false),
            (blur_kind(rect), false, false),
            (redact_kind(rect), false, false),
        ] {
            let mut annotation = object(kind.clone());
            assert_eq!(annotation.get_color(), has_color.then_some(COLOR));
            assert_eq!(annotation.get_thickness(), has_thickness.then_some(2));
            annotation.set_color(new_color);
            annotation.set_thickness(7);
            assert_eq!(annotation.get_color(), has_color.then_some(new_color));
            assert_eq!(annotation.get_thickness(), has_thickness.then_some(7));
            if !has_color && !has_thickness {
                assert_eq!(annotation.kind, kind);
            }
        }
    }

    #[test]
    fn square_snap_keeps_the_longer_side_in_the_drag_direction() {
        for (current, expected) in [
            ((30, 15), (30, 30)),
            ((0, 15), (0, 20)),
            ((14, 2), (18, 2)),
            ((10, 10), (10, 10)),
            ((-5, 40), (-20, 40)),
        ] {
            assert_eq!(snap_square((10, 10), current), expected, "{current:?}");
        }
    }

    #[test]
    fn angle_snap_rounds_the_drag_to_the_nearest_45_degrees_keeping_its_length() {
        for (start, current, expected) in [
            ((0, 0), (100, 10), (100, 0)),
            ((0, 0), (50, 48), (49, 49)),
            ((0, 0), (-3, -100), (0, -100)),
            ((10, 20), (110, 25), (110, 20)),
            // Under two pixels there is no direction to snap.
            ((0, 0), (1, 1), (1, 1)),
        ] {
            assert_eq!(
                snap_angle_45(start, current),
                expected,
                "{start:?} {current:?}"
            );
        }
    }

    #[test]
    fn pixelate_replaces_each_block_with_its_average_and_makes_it_opaque() {
        let mut buffer = gradient(8, 8);
        apply_pixelate_blur(&mut buffer, 8, 8, &Rect::new(0, 0, 8, 8), 4);
        // Blue is the mean of four columns, green the mean of four rows; red is constant.
        for ((x, y), expected) in [
            ((0, 0), [24, 24, 100, 255]),
            ((3, 3), [24, 24, 100, 255]),
            ((4, 0), [88, 24, 100, 255]),
            ((7, 3), [88, 24, 100, 255]),
            ((0, 4), [24, 88, 100, 255]),
            ((7, 7), [88, 88, 100, 255]),
        ] {
            assert_eq!(pixel_at(&buffer, 8, x, y), expected, "({x}, {y})");
        }

        // Blocks below 4 px are raised to 4.
        let mut small = gradient(8, 8);
        apply_pixelate_blur(&mut small, 8, 8, &Rect::new(0, 0, 8, 8), 1);
        assert_eq!(small, buffer);
    }

    #[test]
    fn pixelate_clamps_to_the_buffer_and_leaves_the_rest_alone() {
        let mut buffer = gradient(8, 8);
        apply_pixelate_blur(&mut buffer, 8, 8, &Rect::new(-5, -5, 6, 6), 4);
        // The 2 px wide edge blocks average their own pixels only.
        assert_eq!(pixel_at(&buffer, 8, 0, 0), [24, 24, 100, 255]);
        assert_eq!(pixel_at(&buffer, 8, 5, 0), [72, 24, 100, 255]);
        assert_eq!(pixel_at(&buffer, 8, 0, 5), [24, 72, 100, 255]);
        assert_eq!(pixel_at(&buffer, 8, 5, 5), [72, 72, 100, 255]);
        assert_eq!(pixel_at(&buffer, 8, 6, 0), [96, 0, 100, 7]);
        assert_eq!(pixel_at(&buffer, 8, 7, 7), [112, 112, 100, 7]);
    }

    #[test]
    fn pixelate_ignores_degenerate_requests() {
        let untouched = gradient(8, 8);
        for (rect, width, height) in [
            (Rect::new(2, 2, 2, 6), 8, 8),
            (Rect::new(2, 6, 6, 6), 8, 8),
            (Rect::new(0, 0, 8, 8), 0, 8),
            (Rect::new(0, 0, 8, 8), 8, 0),
            (Rect::new(20, 20, 30, 30), 8, 8),
        ] {
            let mut buffer = untouched.clone();
            apply_pixelate_blur(&mut buffer, width, height, &rect, 4);
            assert_eq!(buffer, untouched, "{rect:?} {width}x{height}");
        }
    }

    #[test]
    fn pixelate_averages_only_the_rows_a_short_buffer_holds() {
        let mut buffer = gradient(8, 8)[..8 * 6 * 4].to_vec();
        apply_pixelate_blur(&mut buffer, 8, 8, &Rect::new(0, 0, 8, 8), 4);
        assert_eq!(pixel_at(&buffer, 8, 0, 0), [24, 24, 100, 255]);
        // Rows 4 and 5 are the only rows of the second block row.
        assert_eq!(pixel_at(&buffer, 8, 0, 4), [24, 72, 100, 255]);
        assert_eq!(pixel_at(&buffer, 8, 0, 5), [24, 72, 100, 255]);
        assert_eq!(pixel_at(&buffer, 8, 7, 5), [88, 72, 100, 255]);
        assert_eq!(buffer.len(), 8 * 6 * 4);

        // A buffer that ends before the second block row: nothing there to average or write.
        let mut top_only = gradient(8, 8)[..8 * 4 * 4].to_vec();
        apply_pixelate_blur(&mut top_only, 8, 8, &Rect::new(0, 0, 8, 8), 4);
        assert_eq!(pixel_at(&top_only, 8, 0, 3), [24, 24, 100, 255]);
        assert_eq!(pixel_at(&top_only, 8, 7, 3), [88, 24, 100, 255]);
        assert_eq!(top_only.len(), 8 * 4 * 4);
    }

    #[test]
    fn render_blur_pixelates_blurs_and_ignores_every_other_kind() {
        let mut buffer = gradient(8, 8);
        object(blur_kind(Rect::new(0, 0, 8, 8))).render_blur(&mut buffer, 8, 8);
        // One 8 px block: blue averages all eight columns, green all eight rows.
        assert!(
            buffer
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [56, 56, 100, 255])
        );

        let untouched = gradient(8, 8);
        let mut buffer = untouched.clone();
        object(redact_kind(Rect::new(2, 2, 6, 6))).render_blur(&mut buffer, 0, 8);
        object(redact_kind(Rect::new(2, 2, 6, 6))).render_blur(&mut buffer, 8, 0);
        for kind in [
            rectangle_kind(Rect::new(1, 1, 6, 6), 2),
            arrow_kind((1, 1), (6, 6), 2),
            pen_kind(&[(1, 1), (6, 6)], 2),
            step_kind((4, 4), 3),
            text_kind((1, 1), "A", 8),
        ] {
            object(kind).render_blur(&mut buffer, 8, 8);
        }
        assert_eq!(buffer, untouched);
    }

    #[test]
    fn a_single_point_highlight_is_a_disc_and_degenerate_requests_change_nothing() {
        let original = [100, 100, 100, 255];
        let mut pixels = original.repeat(40 * 20);
        render_highlight(&mut pixels, 40, 20, &[(20, 10)], [200, 0, 0, 255], 2);
        // Radius 4: the marker blend of the highlighter test, centred on the point.
        assert_eq!(pixel_at(&pixels, 40, 20, 10), [138, 62, 62, 255]);
        assert_eq!(pixel_at(&pixels, 40, 24, 10), [138, 62, 62, 255]);
        assert_eq!(pixel_at(&pixels, 40, 25, 10), original);
        assert_eq!(pixel_at(&pixels, 40, 20, 15), original);

        let untouched = original.repeat(40 * 20);
        let line = [(5, 5), (30, 15)];
        for (width, height, points, buffer_len) in [
            (0, 20, &line[..], untouched.len()),
            (40, 0, &line[..], untouched.len()),
            (40, 20, &[][..], untouched.len()),
            // A buffer too short for width x height pixels.
            (40, 20, &line[..], untouched.len() - 4),
        ] {
            let mut pixels = untouched[..buffer_len].to_vec();
            render_highlight(&mut pixels, width, height, points, [200, 0, 0, 255], 2);
            assert_eq!(
                pixels,
                untouched[..buffer_len],
                "{width}x{height} {points:?}"
            );
        }
    }

    #[test]
    fn a_highlight_entirely_off_the_image_changes_nothing() {
        let mut pixels = vec![100u8; 40 * 20 * 4];
        let original = pixels.clone();
        render_highlight(
            &mut pixels,
            40,
            20,
            &[(-100, -100), (-90, -100)],
            [200, 0, 0, 255],
            2,
        );
        assert_eq!(pixels, original);
    }

    #[test]
    fn history_keeps_only_the_newest_entries() {
        let mut objects: Vec<_> = (1..=4)
            .map(|id| AnnotationObject::new(id, frame_at(id as i32)))
            .collect();
        let mut history = HistoryManager::new(3);
        for object in &objects {
            history.record(EditCommand::Add(object.clone()));
        }
        // The oldest step was dropped: object 1 stays, 4, 3 and 2 are undone.
        assert_eq!(undo_steps(&mut history, &mut objects), 3);
        assert_eq!(
            objects.iter().map(|object| object.id).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn redoing_an_undone_add_restores_the_object_and_can_be_undone_again() {
        let added = AnnotationObject::new(5, frame_at(3));
        let mut objects = vec![added.clone()];
        let mut history = HistoryManager::default();
        history.record(EditCommand::Add(added.clone()));

        assert!(history.undo(&mut objects));
        assert!(objects.is_empty());
        assert!(history.redo(&mut objects));
        assert_eq!(objects, vec![added]);
        assert!(history.can_undo());
        assert!(history.undo(&mut objects));
        assert!(objects.is_empty());
    }

    #[test]
    fn record_merge_folds_edits_made_within_the_merge_window() {
        let mut history = HistoryManager::default();
        history.record_merge(nudge(7, 0, 1), MergeKey::Nudge);
        history.record_merge(nudge(7, 1, 2), MergeKey::Nudge);
        let mut objects = vec![AnnotationObject::new(7, frame_at(2))];

        assert_eq!(undo_steps(&mut history, &mut objects), 1);
        assert_eq!(objects[0].kind, frame_at(0));
    }
}
