//! Pixel tests for the annotation renderers: each drawing call goes into a 32-bit DIB
//! section selected into a memory device context, and the pixels are read back.
use super::*;
use std::ffi::c_void;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACK_PEN, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetCurrentObject, HBITMAP, HGDIOBJ, OBJ_PEN,
    RGBQUAD, SetBkMode, TRANSPARENT,
};

const BACKGROUND: [u8; 3] = [200, 200, 200];
const COLOR: [u8; 4] = [49, 49, 224, 255];
/// `COLOR` as a canvas pixel: B, G, R.
const INK: [u8; 3] = [49, 49, 224];
const AMBER: [u8; 3] = [
    crate::theme::BRAND_AMBER_BGRA[0],
    crate::theme::BRAND_AMBER_BGRA[1],
    crate::theme::BRAND_AMBER_BGRA[2],
];
const WHITE: [u8; 3] = [255, 255, 255];

/// A `BACKGROUND`-filled 32-bit canvas behind a memory device context. Like the editor's
/// overlay, the context draws transparently.
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Self {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                bmiColors: [RGBQUAD::default()],
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            let _ = SetBkMode(dc, TRANSPARENT);
            let canvas = Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
                width,
                height,
            };
            let pixels = std::slice::from_raw_parts_mut(canvas.bits, (width * height * 4) as usize);
            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = [BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 255];
            }
            canvas
        }
    }

    /// The canvas pixels after GDI has finished drawing.
    fn pixels(&self) -> &[u8] {
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts(self.bits, (self.width * self.height * 4) as usize)
        }
    }

    fn pixel(&self, x: i32, y: i32) -> [u8; 3] {
        let offset = ((y * self.width + x) * 4) as usize;
        self.pixels()[offset..offset + 3].try_into().unwrap()
    }

    /// Every pixel that is not `BACKGROUND`.
    fn changed(&self) -> Vec<(i32, i32)> {
        (0..self.height)
            .flat_map(|y| (0..self.width).map(move |x| (x, y)))
            .filter(|&(x, y)| self.pixel(x, y) != BACKGROUND)
            .collect()
    }

    fn count(&self, color: [u8; 3]) -> usize {
        (0..self.height)
            .flat_map(|y| (0..self.width).map(move |x| (x, y)))
            .filter(|&(x, y)| self.pixel(x, y) == color)
            .count()
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

fn drawn(width: i32, height: i32, draw: impl FnOnce(HDC)) -> Canvas {
    let canvas = Canvas::new(width, height);
    draw(canvas.dc);
    canvas
}

/// A device context handle that every GDI and GDI+ call rejects.
fn bogus_dc() -> HDC {
    HDC(std::ptr::dangling_mut::<c_void>())
}

fn rectangle(rect: Rect, thickness: i32) -> AnnotationObject {
    AnnotationObject::new(
        1,
        AnnotationKind::Rectangle {
            rect,
            color: COLOR,
            thickness,
        },
    )
}

fn arrow(start: (i32, i32), end: (i32, i32), thickness: i32) -> AnnotationObject {
    AnnotationObject::new(
        1,
        AnnotationKind::Arrow {
            start,
            end,
            color: COLOR,
            thickness,
        },
    )
}

fn pen(points: &[(i32, i32)], thickness: i32) -> AnnotationObject {
    AnnotationObject::new(
        1,
        AnnotationKind::Pen {
            points: points.to_vec(),
            color: COLOR,
            thickness,
        },
    )
}

fn step(pos: (i32, i32), radius: i32, color: [u8; 4]) -> AnnotationObject {
    AnnotationObject::new(
        1,
        AnnotationKind::Step {
            pos,
            number: 8,
            color,
            radius,
        },
    )
}

#[test]
fn smooth_frames_cover_exactly_the_pixels_of_a_plain_gdi_frame() {
    let rect = Rect::new(10, 12, 40, 34);
    for thickness in 1..=5 {
        let smooth = drawn(60, 50, |dc| {
            assert!(smooth::frame(dc, rect, COLOR, thickness));
        });
        let plain = drawn(60, 50, |dc| plain_frame(dc, rect, COLOR, thickness));
        let covered = smooth.changed();
        assert!(!covered.is_empty(), "thickness {thickness}");
        assert_eq!(covered, plain.changed(), "thickness {thickness}");
        // The inside of the frame stays untouched.
        assert_eq!(smooth.pixel(25, 23), BACKGROUND);
        assert_eq!(plain.pixel(25, 23), BACKGROUND);
        // Mitred corners and whole-pixel edges: no blended pixels beyond a 1 px stroke.
        if thickness > 1 {
            assert!(covered.iter().all(|&(x, y)| smooth.pixel(x, y) == INK));
        }
        assert!(covered.iter().all(|&(x, y)| plain.pixel(x, y) == INK));
    }
}

#[test]
fn smooth_frames_decline_slivers_and_unusable_devices() {
    let canvas = Canvas::new(60, 50);
    assert!(!smooth::frame(
        canvas.dc,
        Rect::new(10, 10, 11, 30),
        COLOR,
        2
    ));
    assert!(!smooth::frame(
        canvas.dc,
        Rect::new(10, 10, 30, 11),
        COLOR,
        2
    ));
    assert!(!smooth::frame(
        bogus_dc(),
        Rect::new(10, 10, 30, 30),
        COLOR,
        2
    ));
    assert!(canvas.changed().is_empty());
}

#[test]
fn rectangles_render_through_gdi_plus_when_it_is_available() {
    let rect = Rect::new(10, 12, 40, 34);
    let rendered = drawn(60, 50, |dc| rectangle(rect, 3).render_gdi(dc));
    let direct = drawn(60, 50, |dc| {
        assert!(smooth::frame(dc, rect, COLOR, 3));
    });
    assert!(!rendered.changed().is_empty());
    assert_eq!(rendered.pixels(), direct.pixels());
}

#[test]
fn arrows_have_a_round_capped_shaft_and_a_wider_filled_head() {
    let (start, end, thickness) = ((10, 30), (60, 30), 3);
    let geometry = arrow_geometry(start, end, thickness).unwrap();
    let smooth = drawn(80, 60, |dc| {
        assert!(smooth::arrow(dc, start, &geometry, COLOR, thickness));
    });
    let plain = drawn(80, 60, |dc| {
        plain_arrow(dc, start, end, &geometry, COLOR, thickness);
    });
    let rendered = drawn(80, 60, |dc| {
        arrow(start, end, thickness).render_gdi(dc);
    });
    assert_eq!(rendered.pixels(), smooth.pixels());

    for canvas in [&smooth, &plain] {
        // Shaft: three rows thick, with the start end covered.
        for point in [(25, 29), (25, 30), (25, 31), (10, 30)] {
            assert_eq!(canvas.pixel(point.0, point.1), INK, "{point:?}");
        }
        // Head: filled and wider than the shaft, between the base (44) and the tip (60).
        for point in [(52, 30), (45, 35), (45, 26), (55, 30)] {
            assert_eq!(canvas.pixel(point.0, point.1), INK, "{point:?}");
        }
        // Outside: beside the shaft, beside the head, past the tip, before the start.
        for point in [(25, 28), (25, 32), (52, 15), (52, 45), (63, 30), (5, 30)] {
            assert_eq!(canvas.pixel(point.0, point.1), BACKGROUND, "{point:?}");
        }
    }
}

#[test]
fn arrows_shorter_than_two_pixels_draw_nothing() {
    let canvas = drawn(40, 40, |dc| {
        arrow((20, 20), (21, 21), 3).render_gdi(dc);
        arrow((20, 20), (20, 20), 3).render_gdi(dc);
    });
    assert!(canvas.changed().is_empty());
}

#[test]
fn smooth_arrows_and_strokes_decline_unusable_devices() {
    let geometry = arrow_geometry((10, 30), (60, 30), 3).unwrap();
    assert!(!smooth::arrow(bogus_dc(), (10, 30), &geometry, COLOR, 3));
    assert!(!smooth::polyline(bogus_dc(), &[(1, 1), (9, 9)], COLOR, 3));
}

#[test]
fn pen_strokes_are_as_thick_as_asked_in_both_renderers() {
    let points = [(5, 10), (35, 10), (35, 30)];
    for thickness in 1..=4 {
        let smooth = drawn(60, 50, |dc| {
            assert!(smooth::polyline(dc, &points, COLOR, thickness));
        });
        let plain = drawn(60, 50, |dc| plain_polyline(dc, &points, COLOR, thickness));
        let rows = |canvas: &Canvas| -> Vec<i32> {
            (0..50)
                .filter(|&y| canvas.pixel(20, y) != BACKGROUND)
                .collect()
        };
        let columns = |canvas: &Canvas| -> Vec<i32> {
            (0..60)
                .filter(|&x| canvas.pixel(x, 20) != BACKGROUND)
                .collect()
        };
        // Across the horizontal run (x = 20) and across the vertical run (y = 20).
        assert_eq!(
            rows(&smooth).len(),
            thickness as usize,
            "thickness {thickness}"
        );
        assert_eq!(rows(&smooth), rows(&plain), "thickness {thickness}");
        assert_eq!(
            columns(&smooth).len(),
            thickness as usize,
            "thickness {thickness}"
        );
        assert_eq!(columns(&smooth), columns(&plain), "thickness {thickness}");
    }
}

#[test]
fn pen_strokes_join_and_end_in_the_stroke_color() {
    let points = [(5, 10), (35, 10), (35, 30)];
    let smooth = drawn(60, 50, |dc| {
        assert!(smooth::polyline(dc, &points, COLOR, 3));
    });
    let plain = drawn(60, 50, |dc| plain_polyline(dc, &points, COLOR, 3));
    let preview = drawn(60, 50, |dc| render_pen_preview(dc, &points, COLOR, 3));
    let rendered = drawn(60, 50, |dc| pen(&points, 3).render_gdi(dc));
    assert_eq!(preview.pixels(), smooth.pixels());
    assert_eq!(rendered.pixels(), smooth.pixels());

    for canvas in [&smooth, &plain] {
        for point in [
            (20, 9),
            (20, 10),
            (20, 11),
            (35, 10),
            (34, 20),
            (36, 20),
            (5, 10),
        ] {
            assert_eq!(canvas.pixel(point.0, point.1), INK, "{point:?}");
        }
        for point in [(20, 8), (20, 12), (33, 20), (37, 20), (20, 20)] {
            assert_eq!(canvas.pixel(point.0, point.1), BACKGROUND, "{point:?}");
        }
    }
}

#[test]
fn pen_previews_need_two_points() {
    let canvas = drawn(40, 40, |dc| {
        render_pen_preview(dc, &[], COLOR, 3);
        render_pen_preview(dc, &[(20, 20)], COLOR, 3);
    });
    assert!(canvas.changed().is_empty());
}

#[test]
fn highlights_blurs_and_redactions_are_not_drawn_through_gdi() {
    let rect = Rect::new(5, 5, 30, 30);
    let canvas = drawn(40, 40, |dc| {
        for kind in [
            AnnotationKind::Highlight {
                points: vec![(5, 5), (30, 30)],
                color: COLOR,
                thickness: 4,
            },
            AnnotationKind::Blur {
                rect,
                block_size: 8,
            },
            AnnotationKind::Redact { rect },
        ] {
            AnnotationObject::new(1, kind).render_gdi(dc);
        }
    });
    assert!(canvas.changed().is_empty());
}

#[test]
fn step_badges_are_a_filled_circle_with_a_contrasting_number() {
    // (badge color, label color): the number is white on dark badges, black on light ones.
    for (color, label) in [([30, 30, 200, 255], WHITE), ([0, 200, 255, 255], [0, 0, 0])] {
        let canvas = drawn(130, 130, |dc| step((60, 60), 40, color).render_gdi(dc));
        let badge = [color[0], color[1], color[2]];
        // The disc fills its bounds (20..=100) at the middle of each side but not the corners.
        assert_eq!(canvas.pixel(24, 60), badge, "{color:?}");
        assert_eq!(canvas.pixel(96, 60), badge, "{color:?}");
        assert_eq!(canvas.pixel(21, 21), BACKGROUND, "{color:?}");
        assert_eq!(canvas.pixel(99, 99), BACKGROUND, "{color:?}");
        assert!(canvas.count(label) > 0, "{color:?}");
    }
}

#[test]
fn text_is_drawn_in_its_color_inside_its_measured_box() {
    let text = AnnotationObject::new(
        1,
        AnnotationKind::Text {
            pos: (10, 10),
            text: "HI".to_string(),
            color: COLOR,
            font_size: 40,
        },
    );
    let canvas = drawn(160, 90, |dc| text.render_gdi(dc));
    let allowed = text.bounds().inflate(3, 3);
    let changed = canvas.changed();
    assert!(!changed.is_empty());
    assert!(
        changed.iter().all(|&(x, y)| allowed.contains(x, y)),
        "{allowed:?}"
    );
    assert!(canvas.count(INK) > 0);
}

#[test]
fn a_rectangle_selection_gets_a_dashed_box_and_four_corner_grips() {
    let object = rectangle(Rect::new(30, 30, 90, 70), 2);
    for (dpi, half) in [(96, 4), (192, 8)] {
        let canvas = drawn(140, 120, |dc| object.render_selection_indicator(dc, dpi));
        // The box is the geometry grown by the gap, which is also the grip radius.
        let (left, top, right, bottom) = (30 - half, 30 - half, 90 + half, 70 + half);
        // Each grip: a white square (side 2 * half) with an amber outline.
        for ((cx, cy), outside) in [
            ((left, top), (left - half - 1, top)),
            ((right, top), (right + half, top)),
            ((left, bottom), (left - half - 1, bottom)),
            ((right, bottom), (right + half, bottom)),
        ] {
            assert_eq!(canvas.pixel(cx, cy), WHITE, "dpi {dpi} ({cx}, {cy})");
            assert_eq!(canvas.pixel(cx - half, cy), AMBER, "dpi {dpi} ({cx}, {cy})");
            assert_eq!(
                canvas.pixel(cx + half - 1, cy),
                AMBER,
                "dpi {dpi} ({cx}, {cy})"
            );
            assert_eq!(canvas.pixel(outside.0, outside.1), BACKGROUND, "dpi {dpi}");
        }
        let interior = ((2 * half - 2) * (2 * half - 2)) as usize;
        assert_eq!(canvas.count(WHITE), 4 * interior, "dpi {dpi}");

        // Between the grips the box is dotted: some pixels amber, some not, none inside.
        let dotted = (left + 3 * half..right - 3 * half)
            .filter(|&x| canvas.pixel(x, top) == AMBER)
            .count();
        let run = (right - 3 * half - (left + 3 * half)) as usize;
        assert!(
            dotted > run / 4 && dotted < run,
            "dpi {dpi}: {dotted} of {run}"
        );
        assert_eq!(canvas.pixel(60, top - 1), BACKGROUND, "dpi {dpi}");
        assert_eq!(canvas.pixel(60, top + 3), BACKGROUND, "dpi {dpi}");
    }
}

#[test]
fn an_arrow_selection_has_a_grip_on_each_end_only() {
    let object = arrow((20, 20), (80, 50), 2);
    let canvas = drawn(120, 100, |dc| object.render_selection_indicator(dc, 96));
    assert_eq!(canvas.pixel(20, 20), WHITE);
    assert_eq!(canvas.pixel(80, 50), WHITE);
    // Two grips with a 6 x 6 white interior each.
    assert_eq!(canvas.count(WHITE), 2 * 36);
    assert!(canvas.count(AMBER) > 0);
}

#[test]
fn a_step_badge_selection_is_only_a_dashed_box() {
    let canvas = drawn(100, 100, |dc| {
        step((50, 50), 14, COLOR).render_selection_indicator(dc, 96)
    });
    assert_eq!(canvas.count(WHITE), 0);
    assert!(canvas.count(AMBER) > 0);
    // Badge bounds are (36, 36)..(65, 65); the box sits 4 px outside: x = 32 and 68.
    assert!(canvas.changed().iter().all(|&(x, y)| {
        (32..=68).contains(&x)
            && (32..=68).contains(&y)
            && (x == 32 || x == 68 || y == 32 || y == 68)
    }));
}

#[test]
fn without_anti_aliasing_strokes_use_plain_gdi_only() {
    let rect = Rect::new(10, 12, 40, 34);
    let points = [(5, 10), (35, 10), (35, 30)];
    let (start, end) = ((10, 30), (60, 30));
    let geometry = arrow_geometry(start, end, 3).unwrap();

    let frame = drawn(60, 50, |dc| {
        rectangle(rect, 3).render_gdi_with(dc, false);
    });
    let primitive = drawn(60, 50, |dc| plain_frame(dc, rect, COLOR, 3));
    assert!(!frame.changed().is_empty());
    assert_eq!(frame.pixels(), primitive.pixels());

    let shaft_and_head = drawn(80, 60, |dc| {
        arrow(start, end, 3).render_gdi_with(dc, false);
    });
    let primitive = drawn(80, 60, |dc| {
        plain_arrow(dc, start, end, &geometry, COLOR, 3);
    });
    assert!(!shaft_and_head.changed().is_empty());
    assert_eq!(shaft_and_head.pixels(), primitive.pixels());

    let stroke = drawn(60, 50, |dc| {
        pen(&points, 3).render_gdi_with(dc, false);
    });
    let primitive = drawn(60, 50, |dc| plain_polyline(dc, &points, COLOR, 3));
    assert!(!stroke.changed().is_empty());
    assert_eq!(stroke.pixels(), primitive.pixels());

    // Too short to draw in either mode.
    let nothing = drawn(40, 40, |dc| {
        arrow((20, 20), (21, 21), 3).render_gdi_with(dc, false);
        pen(&[(20, 20)], 3).render_gdi_with(dc, false);
    });
    assert!(nothing.changed().is_empty());
}

#[test]
fn a_sliver_frame_falls_back_to_plain_gdi() {
    // GDI+ declines a 1 px wide frame; plain GDI still draws it.
    let sliver = Rect::new(20, 10, 21, 30);
    let rendered = drawn(60, 50, |dc| rectangle(sliver, 2).render_gdi(dc));
    let plain = drawn(60, 50, |dc| plain_frame(dc, sliver, COLOR, 2));
    assert!(!rendered.changed().is_empty());
    assert_eq!(rendered.pixels(), plain.pixels());
}

#[test]
fn a_stock_object_is_selected_only_while_drawing() {
    let canvas = Canvas::new(8, 8);
    unsafe {
        SelectObject(canvas.dc, GetStockObject(BLACK_PEN));
        let current = || GetCurrentObject(canvas.dc, OBJ_PEN);
        let inside = with_stock_object(canvas.dc, NULL_PEN, || {
            (current() == GetStockObject(NULL_PEN), 7)
        });
        assert_eq!(inside, (true, 7));
        assert_eq!(current(), GetStockObject(BLACK_PEN));
    }
}
