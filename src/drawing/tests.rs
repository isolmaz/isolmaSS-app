use super::canvas::{Canvas, GdiQuota, Rgb, assert_color, distance, rgb};
use super::*;
use crate::capture::Rect;
use crate::theme::tests::ForcedTheme;
use crate::theme::{Theme, ThemePreference, tokens_for};
use std::ffi::c_void;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Controls::{
    CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDIS_SHOWKEYBOARDCUES, NMCUSTOMDRAW,
    NMCUSTOMDRAW_DRAW_STATE_FLAGS, NMHDR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_SETCHECK, BS_AUTOCHECKBOX, CreateWindowExW, DestroyWindow, SendMessageW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WS_POPUP,
};
use windows::core::w;

const WHITE: COLORREF = COLORREF(0x00FF_FFFF);
const RED: COLORREF = COLORREF(0x0000_00FF);
const GREEN: COLORREF = COLORREF(0x0000_FF00);
const BLUE: COLORREF = COLORREF(0x00FF_0000);
/// A canvas colour no theme token resembles.
const CANVAS: COLORREF = COLORREF(0x0040_8040);
/// The colour behind a control, different from `CANVAS` and from every token.
const BACKDROP: COLORREF = COLORREF(0x0080_4040);
/// 3x scale: strokes and stems are wide enough for exact-colour checks.
const DPI: u32 = 288;

const THEMES: [(ThemePreference, Theme); 2] = [
    (ThemePreference::Light, Theme::Light),
    (ThemePreference::Dark, Theme::Dark),
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Runs `test` on a fresh thread, so it gets its own GDI object and metrics caches.
fn on_thread(test: impl FnOnce() + Send + 'static) {
    std::thread::spawn(test)
        .join()
        .expect("the test thread panicked");
}

fn current(hdc: HDC, kind: OBJ_TYPE) -> HGDIOBJ {
    unsafe { GetCurrentObject(hdc, kind) }
}

fn stock(object: GET_STOCK_OBJECT_FLAGS) -> HGDIOBJ {
    unsafe { GetStockObject(object) }
}

fn alive(object: HGDIOBJ) -> bool {
    unsafe { GetObjectType(object) != 0 }
}

fn get_object<T: Default>(object: HGDIOBJ) -> T {
    let mut out = T::default();
    let copied = unsafe {
        GetObjectW(
            object,
            size_of::<T>() as i32,
            Some((&mut out as *mut T).cast::<c_void>()),
        )
    };
    assert!(copied > 0, "GetObjectW");
    out
}

fn face_of(font: &LOGFONTW) -> String {
    let end = font.lfFaceName.iter().position(|&unit| unit == 0);
    String::from_utf16_lossy(&font.lfFaceName[..end.unwrap_or(font.lfFaceName.len())])
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn has_color(pixels: &[Rgb], color: Rgb, tolerance: u32) -> bool {
    pixels
        .iter()
        .any(|&pixel| distance(pixel, color) <= tolerance)
}

/// Runs `draw` on `canvas` and returns the box around what it changed.
fn drawn(canvas: &Canvas, draw: impl FnOnce(HDC)) -> (i32, i32, i32, i32) {
    let blank = canvas.pixels();
    draw(canvas.hdc);
    canvas.changed_since(&blank).expect("something was drawn")
}

fn logged_failures() -> usize {
    LOGGED_FAILURES.with(|seen| seen.borrow().len())
}

fn cached_objects() -> usize {
    CACHE.with(|cache| cache.borrow().0.len())
}

fn cached_measurements() -> usize {
    METRICS.with(|cache| cache.borrow().len())
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

#[test]
fn icon_face_is_an_installed_icon_font() {
    assert!(crate::theme::face_installed(icon_face()));
}

#[test]
fn corner_diameter_is_twice_the_scaled_radius() {
    assert_eq!(corner_diameter(4, 96), 8);
    assert_eq!(corner_diameter(8, 96), 16);
    assert_eq!(corner_diameter(4, 144), 12);
    assert_eq!(corner_diameter(8, 192), 32);
}

#[test]
fn elevation_layers_widen_sink_and_fade_and_scale_with_dpi() {
    let panel = Rect::new(100, 200, 400, 300);
    let layers = elevation_layers(panel, 8, 96);
    for (layer, diameter, _) in layers {
        assert!(layer.left < panel.left && layer.right > panel.right);
        assert!(layer.bottom > panel.bottom);
        assert!(diameter > 16);
    }
    for pair in layers.windows(2) {
        assert!(pair[1].0.bottom > pair[0].0.bottom, "{pair:?}");
        assert!(pair[1].0.left < pair[0].0.left, "{pair:?}");
        assert!(pair[1].2 < pair[0].2, "{pair:?}");
    }
    let doubled = elevation_layers(panel, 16, 192);
    for (single, double) in layers.iter().zip(doubled) {
        assert_eq!(
            double.0.bottom - panel.bottom,
            (single.0.bottom - panel.bottom) * 2
        );
        assert_eq!(panel.left - double.0.left, (panel.left - single.0.left) * 2);
        assert_eq!(double.1, single.1 * 2);
    }
}

#[test]
fn focus_strokes_scale_with_dpi_and_never_vanish() {
    assert_eq!(focus_widths(96), (1, 2));
    assert_eq!(focus_widths(192), (2, 4));
    assert_eq!(focus_extent(96), 3);
    assert_eq!(focus_extent(192), 6);
    assert_eq!(focus_widths(48), (1, 1));
}

#[test]
fn text_alignment_maps_to_the_draw_text_flags() {
    assert_eq!(Align::Left.format(), DT_LEFT);
    assert_eq!(Align::Center.format(), DT_CENTER);
    assert_eq!(Align::Right.format(), DT_RIGHT);
}

#[test]
fn gdiplus_starts_for_the_supported_version_only() {
    assert!(gdiplus_ready());
    assert!(!start_gdiplus(99));
}

// ---------------------------------------------------------------------------
// Cached GDI objects
// ---------------------------------------------------------------------------

#[test]
fn cached_objects_are_selected_for_the_action_and_restored_afterwards() {
    on_thread(|| {
        let canvas = Canvas::new(8, 8, COLORREF(0));
        let hdc = canvas.hdc;
        let selection = || {
            (
                current(hdc, OBJ_BRUSH),
                current(hdc, OBJ_PEN),
                current(hdc, OBJ_FONT),
            )
        };
        let before = selection();
        let inside = with_brush(hdc, RED, || {
            with_pen(hdc, PS_SOLID, 3, BLUE, || {
                with_font(hdc, -20, 700, selection)
            })
        });
        assert_ne!(inside.0, before.0);
        assert_ne!(inside.1, before.1);
        assert_ne!(inside.2, before.2);
        assert_eq!(selection(), before);

        let brush: LOGBRUSH = get_object(inside.0);
        assert_eq!(brush.lbColor, RED);
        let pen: LOGPEN = get_object(inside.1);
        assert_eq!(
            (pen.lopnStyle, pen.lopnWidth.x, pen.lopnColor),
            (PS_SOLID.0 as u32, 3, BLUE)
        );
        let font: LOGFONTW = get_object(inside.2);
        assert_eq!((font.lfHeight, font.lfWeight), (-20, 700));
        assert_eq!(face_of(&font), crate::theme::ui_face());
        assert_eq!(font.lfQuality, CLEARTYPE_QUALITY);

        // The same request reuses the same objects, which outlive the draw.
        let again = with_brush(hdc, RED, || current(hdc, OBJ_BRUSH));
        assert_eq!(again, inside.0);
        assert!(alive(again));
        let thicker = with_pen(hdc, PS_SOLID, 4, BLUE, || current(hdc, OBJ_PEN));
        assert_ne!(thicker, inside.1);
    });
}

#[test]
fn fonts_are_cached_per_face_and_icon_glyphs_use_grayscale_smoothing() {
    on_thread(|| {
        let canvas = Canvas::new(8, 8, COLORREF(0));
        let hdc = canvas.hdc;
        let pick =
            |face: &'static str| with_font_face(hdc, face, -16, 400, || current(hdc, OBJ_FONT));
        let (text_font, icon_font) = (pick(crate::theme::ui_face()), pick(icon_face()));
        assert_ne!(text_font, icon_font);
        assert_eq!(pick(icon_face()), icon_font);
        assert_eq!(
            get_object::<LOGFONTW>(text_font).lfQuality,
            CLEARTYPE_QUALITY
        );
        let icon_logfont: LOGFONTW = get_object(icon_font);
        assert_eq!(icon_logfont.lfQuality, ANTIALIASED_QUALITY);
        assert_eq!(face_of(&icon_logfont), icon_face());
    });
}

#[test]
fn a_failed_creation_selects_a_stock_object_and_is_logged_once_per_key() {
    on_thread(|| {
        let canvas = Canvas::new(4, 4, COLORREF(0));
        let hdc = canvas.hdc;
        let failing = HGDIOBJ::default;
        with_brush(hdc, RED, || {
            with_pen(hdc, PS_SOLID, 2, BLUE, || {
                with_font(hdc, -12, 400, || {
                    let own = (
                        current(hdc, OBJ_BRUSH),
                        current(hdc, OBJ_PEN),
                        current(hdc, OBJ_FONT),
                    );
                    assert_ne!(own.0, stock(WHITE_BRUSH));
                    assert_ne!(own.1, stock(BLACK_PEN));
                    assert_ne!(own.2, stock(SYSTEM_FONT));
                    let stand_ins = (
                        with_object(hdc, Key::Brush(1), failing, || current(hdc, OBJ_BRUSH)),
                        with_object(hdc, Key::Pen(PS_SOLID.0, 1, 2), failing, || {
                            current(hdc, OBJ_PEN)
                        }),
                        with_object(hdc, Key::Font("No Such Face", 12, 400), failing, || {
                            current(hdc, OBJ_FONT)
                        }),
                    );
                    assert_eq!(
                        stand_ins,
                        (stock(WHITE_BRUSH), stock(BLACK_PEN), stock(SYSTEM_FONT))
                    );
                    // Each stand-in gives the caller's own selection back.
                    assert_eq!(
                        (
                            current(hdc, OBJ_BRUSH),
                            current(hdc, OBJ_PEN),
                            current(hdc, OBJ_FONT)
                        ),
                        own
                    );
                })
            })
        });
        assert_eq!(logged_failures(), 3);
        with_object(hdc, Key::Brush(1), failing, || ());
        assert_eq!(logged_failures(), 3, "a key already reported stays quiet");
        for index in 0..70u32 {
            with_object(hdc, Key::Brush(100 + index), failing, || ());
        }
        assert_eq!(logged_failures(), 64, "the failure log is bounded");
        assert_eq!(cached_objects(), 3, "failures are never cached");
    });
}

#[test]
fn a_full_cache_hands_out_temporary_objects_that_are_deleted_after_use() {
    on_thread(|| {
        let canvas = Canvas::new(4, 4, COLORREF(0));
        let hdc = canvas.hdc;
        for color in 0..48 {
            with_brush(hdc, COLORREF(color), || ());
        }
        assert_eq!(cached_objects(), 48);
        let temporary = with_brush(hdc, RED, || current(hdc, OBJ_BRUSH));
        assert!(!alive(temporary), "deleted once the draw is over");
        assert_eq!(cached_objects(), 48, "the cache stays bounded");
        let kept = with_brush(hdc, COLORREF(5), || current(hdc, OBJ_BRUSH));
        assert!(alive(kept), "cached brushes are not deleted");
    });
}

#[test]
fn cached_objects_are_deleted_when_their_thread_ends() {
    let handle = std::thread::spawn(|| {
        let canvas = Canvas::new(4, 4, COLORREF(0));
        let brush = with_brush(canvas.hdc, COLORREF(0x00AB_CDE1), || {
            current(canvas.hdc, OBJ_BRUSH)
        });
        assert!(alive(brush));
        brush.0 as usize
    })
    .join()
    .expect("the test thread panicked");
    assert!(!alive(HGDIOBJ(handle as *mut c_void)));
}

#[test]
fn select_if_valid_never_hands_an_invalid_handle_to_the_dc() {
    let canvas = Canvas::new(4, 4, COLORREF(0));
    let hdc = canvas.hdc;
    let before = current(hdc, OBJ_BRUSH);
    assert!(select_if_valid(hdc, HGDIOBJ::default()).is_invalid());
    assert_eq!(current(hdc, OBJ_BRUSH), before);

    let brush = unsafe { CreateSolidBrush(RED) };
    let previous = select_if_valid(hdc, HGDIOBJ(brush.0));
    assert_eq!(previous, before);
    assert_eq!(current(hdc, OBJ_BRUSH), HGDIOBJ(brush.0));
    unsafe {
        SelectObject(hdc, previous);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

// ---------------------------------------------------------------------------
// Text measurement
// ---------------------------------------------------------------------------

#[test]
fn ui_text_measurement_grows_with_the_text() {
    let (narrow, height) = measure_ui_text("W");
    let (wide, _) = measure_ui_text("WWWWWWWW");
    assert!(height > 0);
    assert!(wide > narrow * 4);
}

#[test]
fn measurements_are_cached_bounded_and_skip_large_text() {
    on_thread(|| {
        let (width, height) = measure_text("cache me", 20);
        assert!(width > 20 && height >= 20, "{width}x{height}");
        assert!(
            measure_text("cache me", 40).0 > width,
            "bigger text is wider"
        );
        assert_eq!(cached_measurements(), 2);
        assert_eq!(measure_text("cache me", 20), (width, height));
        assert_eq!(cached_measurements(), 2, "the second ask is a cache hit");

        assert_eq!(measure_text("", 12).0, 1, "empty text is one pixel wide");
        let negative = measure_text("Hello", -13);
        assert!(
            negative.0 > 13 && negative.1 >= 13,
            "character height: {negative:?}"
        );

        let before = cached_measurements();
        let large = "W".repeat(1025);
        assert!(measure_text(&large, 10).0 > 1025);
        assert_eq!(
            cached_measurements(),
            before,
            "large text is never retained"
        );

        for index in 0..40 {
            measure_text(&format!("entry {index}"), 12);
        }
        assert_eq!(cached_measurements(), 32, "the cache is bounded");
        let remembered = |entry: &str| {
            METRICS.with(|cache| cache.borrow().iter().any(|(text, _, _)| text == entry))
        };
        assert!(
            !remembered("entry 0"),
            "the oldest entries are evicted first"
        );
        assert!(remembered("entry 39"));

        clear_text_metrics();
        assert_eq!(cached_measurements(), 0);
    });
}

#[test]
fn measurement_falls_back_to_the_font_size_when_gdi_has_no_handles_left() {
    let quota = GdiQuota::exhaust();
    assert_eq!(measure_text("quota probe", 17), (17, 17));
    let body = crate::theme::FONT_BODY_PX;
    assert_eq!(measure_ui_text("quota probe"), (body, body));
    drop(quota);
    // The fallback was not cached: with handles back, the real measurement is used.
    let (width, height) = measure_text("quota probe", 17);
    assert!(width > 17 && height >= 17, "{width}x{height}");
    assert!(measure_ui_text("quota probe").0 > body);
}

// ---------------------------------------------------------------------------
// Text and icons
// ---------------------------------------------------------------------------

#[test]
fn text_draws_bold_from_its_origin_in_the_given_colour() {
    let canvas = Canvas::new(240, 100, WHITE);
    let (left, top, right, bottom) = drawn(&canvas, |hdc| text(hdc, (30, 20), "HH", 48, RED));
    assert!(
        (30..45).contains(&left) && (20..45).contains(&top),
        "{left},{top}"
    );
    assert!(right > left + 35 && bottom > top + 20, "{right},{bottom}");
    assert_eq!(canvas.closest(left, top, right, bottom, rgb(RED)), 0);
}

#[test]
fn label_centers_or_left_aligns_and_ellipsizes_inside_its_rectangle() {
    let rect = Rect::new(20, 10, 220, 90);
    let draw = |text: &str, centered: bool| {
        let canvas = Canvas::new(240, 100, WHITE);
        let bounds = drawn(&canvas, |hdc| label(hdc, rect, text, 40, RED, centered));
        assert_eq!(
            canvas.closest(bounds.0, bounds.1, bounds.2, bounds.3, rgb(RED)),
            0
        );
        bounds
    };
    let (left, top, right, bottom) = draw("HH", true);
    assert!(((left + right) / 2 - 120).abs() <= 3, "{left}..{right}");
    assert!(((top + bottom) / 2 - 50).abs() <= 12, "{top}..{bottom}");

    let (left, _, right, _) = draw("HH", false);
    assert!((20..30).contains(&left), "{left}");
    assert!(right < 120, "{right}");

    let (left, _, right, _) = draw("HHHHHHHHHHHHHHHHHHHH", false);
    assert!(
        left >= 18 && right <= 222,
        "cut inside the rectangle: {left}..{right}"
    );
    assert!(right > 180, "the ellipsis fills the width: {right}");

    let canvas = Canvas::new(240, 100, WHITE);
    let blank = canvas.pixels();
    label(canvas.hdc, rect, "", 40, RED, true);
    assert!(
        canvas.changed_since(&blank).is_none(),
        "empty text draws nothing"
    );
}

#[test]
fn icon_draws_the_glyph_in_its_colour_and_honours_alignment() {
    let rect = Rect::new(0, 0, 160, 100);
    let draw = |codepoint: u16, center: bool| {
        let canvas = Canvas::new(160, 100, WHITE);
        let bounds = drawn(&canvas, |hdc| icon(hdc, rect, codepoint, 60, BLUE, center));
        (bounds, canvas.pixels())
    };
    let (gear, gear_pixels) = draw(0xE713, true);
    assert!(has_color(&gear_pixels, rgb(BLUE), 0));
    assert!(
        ((gear.0 + gear.2) / 2 - 80).abs() <= 6,
        "centered: {gear:?}"
    );
    let (aligned, _) = draw(0xE713, false);
    assert!(
        aligned.0 < 15 && aligned.2 < 80,
        "left aligned: {aligned:?}"
    );
    let (_, cross_pixels) = draw(0xE711, true);
    assert_ne!(
        gear_pixels, cross_pixels,
        "each codepoint has its own glyph"
    );
}

#[test]
fn ui_text_aligns_inside_the_rect_in_the_body_font() {
    let area = RECT {
        left: 20,
        top: 10,
        right: 380,
        bottom: 130,
    };
    let draw = |content: &str, align: Align| {
        let canvas = Canvas::new(400, 140, WHITE);
        let blank = canvas.pixels();
        let mut rect = area;
        ui_text(canvas.hdc, &mut utf16(content), &mut rect, DPI, RED, align);
        let bounds = canvas.changed_since(&blank).expect("text was drawn");
        assert_eq!(rect, area, "the rectangle is only read");
        (bounds, canvas.pixels())
    };
    let ((left, top, right, bottom), pixels) = draw("Hello", Align::Left);
    assert!((20..30).contains(&left) && right < 200, "{left}..{right}");
    assert!(top > 10 && bottom < 130, "{top}..{bottom}");
    assert!(has_color(&pixels, rgb(RED), 0));
    let ((left, _, right, _), _) = draw("Hello", Align::Center);
    assert!(((left + right) / 2 - 200).abs() <= 4, "{left}..{right}");
    let ((left, _, right, _), _) = draw("Hello", Align::Right);
    assert!(right > 370 && right <= 381 && left > 200, "{left}..{right}");
    let ((left, _, right, _), _) = draw(&"Hello ".repeat(12), Align::Left);
    assert!(
        left >= 18 && right <= 382,
        "cut inside the rect: {left}..{right}"
    );

    let canvas = Canvas::new(400, 140, WHITE);
    let blank = canvas.pixels();
    let mut rect = area;
    let mut nothing: [u16; 0] = [];
    ui_text(canvas.hdc, &mut nothing, &mut rect, DPI, RED, Align::Left);
    assert!(
        canvas.changed_since(&blank).is_none(),
        "empty text draws nothing"
    );
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

#[test]
fn rounded_fills_and_outlines_with_transparent_corners() {
    let canvas = Canvas::new(60, 50, WHITE);
    rounded(canvas.hdc, Rect::new(10, 10, 50, 40), 16, RED, BLUE);
    assert_color(canvas.pixel(30, 25), RED, 2, "fill");
    for (x, y) in [(10, 25), (49, 25), (30, 10), (30, 39)] {
        assert_color(canvas.pixel(x, y), BLUE, 4, "border");
    }
    for (x, y) in [(10, 10), (49, 10), (10, 39), (49, 39)] {
        assert_eq!(canvas.pixel(x, y), rgb(WHITE), "corner ({x}, {y})");
    }
    for (x, y) in [(9, 25), (50, 25), (30, 9), (30, 40)] {
        assert_eq!(canvas.pixel(x, y), rgb(WHITE), "outside ({x}, {y})");
    }
}

#[test]
fn rounded_with_no_radius_has_square_corners() {
    let canvas = Canvas::new(60, 50, WHITE);
    rounded(canvas.hdc, Rect::new(10, 10, 50, 40), 0, RED, BLUE);
    for (x, y) in [(10, 10), (49, 10), (10, 39), (49, 39)] {
        let corner = canvas.pixel(x, y);
        assert!(
            distance(corner, rgb(BLUE)) < distance(corner, rgb(WHITE)),
            "({x}, {y}) belongs to the border: {corner:?}"
        );
    }
    assert_color(canvas.pixel(30, 25), RED, 2, "fill");
}

#[test]
fn a_border_matching_the_fill_adds_no_outline_so_the_fill_stops_half_a_pixel_inside() {
    let canvas = Canvas::new(60, 50, WHITE);
    rounded(canvas.hdc, Rect::new(10, 10, 50, 40), 16, RED, RED);
    // Red at 50% over white on the edge pixels, full red one pixel further in.
    assert_color(canvas.pixel(10, 25), COLORREF(0x007F_7FFF), 4, "left edge");
    assert_color(canvas.pixel(49, 25), COLORREF(0x007F_7FFF), 4, "right edge");
    assert_color(canvas.pixel(11, 25), RED, 2, "inside the edge");
}

#[test]
fn rounded_falls_back_to_plain_gdi_for_slivers() {
    let canvas = Canvas::new(20, 20, WHITE);
    rounded(canvas.hdc, Rect::new(4, 4, 5, 10), 8, GREEN, BLUE);
    for y in 4..10 {
        assert_eq!(canvas.pixel(4, y), rgb(BLUE), "column pixel {y}");
        assert_eq!(canvas.pixel(3, y), rgb(WHITE));
        assert_eq!(canvas.pixel(5, y), rgb(WHITE));
    }
    let canvas = Canvas::new(20, 20, WHITE);
    rounded(canvas.hdc, Rect::new(4, 4, 10, 5), 8, GREEN, BLUE);
    for x in 4..10 {
        assert_eq!(canvas.pixel(x, 4), rgb(BLUE), "row pixel {x}");
        assert_eq!(canvas.pixel(x, 3), rgb(WHITE));
        assert_eq!(canvas.pixel(x, 5), rgb(WHITE));
    }
}

#[test]
fn smooth_drawing_reports_when_it_cannot_draw() {
    let canvas = Canvas::new(20, 20, WHITE);
    let frame = RECT {
        left: 0,
        top: 0,
        right: 20,
        bottom: 20,
    };
    assert!(smooth_rounded(
        canvas.hdc,
        Rect::new(2, 2, 18, 18),
        4,
        RED,
        BLUE
    ));
    assert!(smooth_stroke(canvas.hdc, frame, 4, 2, RED));
    assert!(!smooth_rounded(
        HDC::default(),
        Rect::new(2, 2, 18, 18),
        4,
        RED,
        BLUE
    ));
    assert!(!smooth_stroke(HDC::default(), frame, 4, 2, RED));
    assert!(!smooth_rounded(
        canvas.hdc,
        Rect::new(2, 2, 3, 18),
        4,
        RED,
        BLUE
    ));
    assert!(!smooth_rounded(
        canvas.hdc,
        Rect::new(2, 2, 18, 3),
        4,
        RED,
        BLUE
    ));
    let thin = RECT { right: 4, ..frame };
    assert!(!smooth_stroke(canvas.hdc, thin, 0, 2, RED));
}

#[test]
fn stroke_frame_draws_a_ring_inside_the_frame() {
    let frame = RECT {
        left: 10,
        top: 10,
        right: 50,
        bottom: 50,
    };
    let canvas = Canvas::new(60, 60, WHITE);
    stroke_frame(canvas.hdc, frame, 16, 2, RED);
    for (x, y) in [
        (10, 30),
        (11, 30),
        (49, 30),
        (48, 30),
        (30, 10),
        (30, 11),
        (30, 49),
    ] {
        assert_color(canvas.pixel(x, y), RED, 4, "ring");
    }
    for (x, y) in [(9, 30), (12, 30), (30, 12), (30, 30), (10, 10), (49, 49)] {
        assert_eq!(
            canvas.pixel(x, y),
            rgb(WHITE),
            "({x}, {y}) is not on the ring"
        );
    }

    let square = Canvas::new(60, 60, WHITE);
    stroke_frame(square.hdc, frame, 0, 2, RED);
    assert_color(square.pixel(10, 10), RED, 4, "square corner");
}

#[test]
fn stroke_frame_falls_back_to_gdi_when_the_frame_is_too_thin_for_the_stroke() {
    let canvas = Canvas::new(20, 20, WHITE);
    let frame = RECT {
        left: 6,
        top: 6,
        right: 12,
        bottom: 12,
    };
    stroke_frame(canvas.hdc, frame, 0, 3, RED);
    for y in 6..12 {
        for x in 6..12 {
            assert_eq!(canvas.pixel(x, y), rgb(RED), "({x}, {y})");
        }
    }
    assert_eq!(canvas.pixel(5, 8), rgb(WHITE));
    assert_eq!(canvas.pixel(12, 8), rgb(WHITE));
    assert_eq!(canvas.pixel(8, 12), rgb(WHITE));
}

#[test]
fn elevation_casts_a_soft_shadow_below_and_beside_a_panel_but_not_above_it() {
    let canvas = Canvas::new(120, 100, WHITE);
    let panel = Rect::new(30, 20, 90, 60);
    elevation(HDC::default(), panel, 8, 96);
    assert!(
        canvas.changed_since(&vec![rgb(WHITE); 12000]).is_none(),
        "an unusable DC draws nothing"
    );
    elevation(canvas.hdc, panel, 8, 96);
    let shade = |x, y| canvas.pixel(x, y)[0];
    let column = [60, 62, 65, 70, 75].map(|y| shade(60, y));
    assert!(
        column.windows(2).all(|pair| pair[0] < pair[1]),
        "{column:?}"
    );
    assert!(column[0] < 215, "darkest at the panel: {column:?}");
    assert_eq!(column[4], 255, "nothing beyond the shadow's reach");
    assert_eq!(shade(60, 19), 255, "no shadow above the panel");
    let grey = canvas.pixel(60, 61);
    assert!(
        grey[0] == grey[1] && grey[1] == grey[2],
        "black shadow: {grey:?}"
    );
    for offset in 0..6 {
        assert!(
            distance(canvas.pixel(29 - offset, 40), canvas.pixel(90 + offset, 40)) <= 2,
            "symmetric at {offset}"
        );
    }
    assert!(shade(29, 40) < 255 && shade(20, 40) == 255);
}

#[test]
fn elevation_reach_scales_with_the_dpi() {
    let reach = |dpi: u32| {
        let canvas = Canvas::new(120, 140, WHITE);
        elevation(canvas.hdc, Rect::new(30, 20, 90, 60), 8, dpi);
        (60..140)
            .take_while(|&y| canvas.pixel(60, y)[0] < 255)
            .count()
    };
    assert!(
        reach(192) > reach(96) * 3 / 2,
        "{} vs {}",
        reach(192),
        reach(96)
    );
}

#[test]
fn focus_ring_draws_a_contrasting_inner_and_an_outer_text_stroke_around_the_rect() {
    let rect = RECT {
        left: 30,
        top: 20,
        right: 70,
        bottom: 60,
    };
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let tokens = tokens_for(theme);
        let ring = |dpi: u32| {
            let canvas = Canvas::new(100, 80, CANVAS);
            focus_ring(canvas.hdc, rect, 4, dpi);
            canvas
        };
        // 96 DPI: a 1 px inner and a 2 px outer stroke, outside the rect.
        let canvas = ring(96);
        assert_color(canvas.pixel(29, 40), tokens.focus_inner, 4, "inner left");
        assert_color(canvas.pixel(50, 19), tokens.focus_inner, 4, "inner top");
        for (x, y) in [
            (28, 40),
            (27, 40),
            (50, 18),
            (50, 17),
            (70 + 1, 40),
            (50, 60 + 2),
        ] {
            assert_color(canvas.pixel(x, y), tokens.text, 4, "outer stroke");
        }
        for (x, y) in [(26, 40), (30, 40), (50, 16), (50, 20), (50, 40)] {
            assert_eq!(canvas.pixel(x, y), rgb(CANVAS), "({x}, {y}) stays clear");
        }
        // 192 DPI: both strokes twice as thick.
        let canvas = ring(192);
        for x in [28, 29] {
            assert_color(canvas.pixel(x, 40), tokens.focus_inner, 4, "inner, 2x");
        }
        for x in [24, 25, 26, 27] {
            assert_color(canvas.pixel(x, 40), tokens.text, 4, "outer, 2x");
        }
        assert_eq!(canvas.pixel(23, 40), rgb(CANVAS));
    }
}

// ---------------------------------------------------------------------------
// Owner-drawn controls
// ---------------------------------------------------------------------------

/// A control's rectangle inside a 400x160 canvas.
const CONTROL: RECT = RECT {
    left: 20,
    top: 20,
    right: 380,
    bottom: 140,
};

/// Draws a control on a fresh canvas with `BACKDROP` as the background brush;
/// returns the canvas and its pixels from before the control was drawn.
fn paint(size: (i32, i32), draw: impl FnOnce(HDC, HBRUSH)) -> (Canvas, Vec<Rgb>) {
    let canvas = Canvas::new(size.0, size.1, CANVAS);
    let blank = canvas.pixels();
    let brush = unsafe { CreateSolidBrush(BACKDROP) };
    draw(canvas.hdc, brush);
    unsafe {
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
    (canvas, blank)
}

fn button(look: Look) -> (Canvas, Vec<Rgb>) {
    paint((400, 160), |hdc, brush| {
        fluent_button(hdc, CONTROL, &mut utf16("Hello"), DPI, look, brush)
    })
}

fn tab(look: Look) -> (Canvas, Vec<Rgb>) {
    paint((400, 160), |hdc, brush| {
        fluent_tab(hdc, CONTROL, &mut utf16("Hello"), DPI, look, brush)
    })
}

/// Everything a control drew must lie inside its rectangle.
#[track_caller]
fn assert_within(canvas: &Canvas, blank: &[Rgb], rect: RECT) {
    let (left, top, right, bottom) = canvas.changed_since(blank).expect("the control drew");
    assert!(
        left >= rect.left && top >= rect.top && right <= rect.right && bottom <= rect.bottom,
        "drew outside {rect:?}: {left},{top},{right},{bottom}"
    );
}

#[test]
fn button_fill_border_and_text_follow_the_look_and_the_theme() {
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let default = Look::default();
        let looks = [
            ("normal", default, t.control_fill, t.stroke, t.text),
            (
                "hot",
                Look {
                    hot: true,
                    ..default
                },
                t.control_hover,
                t.stroke,
                t.text,
            ),
            (
                "pressed",
                Look {
                    pressed: true,
                    ..default
                },
                t.control_hover,
                t.stroke,
                t.text,
            ),
            (
                "disabled",
                Look {
                    disabled: true,
                    hot: true,
                    ..default
                },
                t.control_fill,
                t.stroke,
                t.text_disabled,
            ),
            (
                "primary",
                Look {
                    primary: true,
                    ..default
                },
                t.accent,
                t.accent,
                t.accent_text,
            ),
            (
                "primary hot",
                Look {
                    primary: true,
                    hot: true,
                    ..default
                },
                t.accent_hover,
                t.accent_hover,
                t.accent_text,
            ),
            (
                "primary pressed",
                Look {
                    primary: true,
                    pressed: true,
                    hot: true,
                    ..default
                },
                t.accent_pressed,
                t.accent_pressed,
                t.accent_text,
            ),
            (
                "primary disabled",
                Look {
                    primary: true,
                    disabled: true,
                    ..default
                },
                t.control_fill,
                t.stroke,
                t.text_disabled,
            ),
        ];
        for (name, look, fill, border, ink) in looks {
            let (canvas, blank) = button(look);
            let what = format!("{theme:?} {name}");
            assert_eq!(canvas.pixel(5, 80), rgb(CANVAS), "{what}: outside the rect");
            assert_color(canvas.pixel(20, 20), BACKDROP, 0, &what);
            assert_color(canvas.pixel(32, 80), fill, 2, &what);
            if border == fill {
                // No outline: the fill itself starts half a pixel inside the edge.
                assert_color(canvas.pixel(22, 80), fill, 2, &what);
                assert_color(canvas.pixel(200, 22), fill, 2, &what);
            } else {
                assert_color(canvas.pixel(21, 80), border, 4, &what);
                assert_color(canvas.pixel(200, 21), border, 4, &what);
                assert_color(canvas.pixel(22, 80), fill, 2, &what);
            }
            assert!(
                canvas.closest(60, 30, 340, 130, rgb(ink)) <= 4,
                "{what}: the label is not drawn in {:?}",
                rgb(ink)
            );
            assert_within(&canvas, &blank, CONTROL);
        }
    }
}

#[test]
fn button_focus_ring_covers_the_outermost_pixels_only_with_keyboard_focus() {
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let (plain, _) = button(Look::default());
        let (focused, blank) = button(Look {
            focus: true,
            ..Look::default()
        });
        assert_color(plain.pixel(20, 80), BACKDROP, 0, "no ring without focus");
        // 6 px outer text stroke, then 3 px inner stroke, on the rect's own edge.
        for x in 20..26 {
            assert_color(focused.pixel(x, 80), t.text, 4, "outer stroke");
        }
        for x in 26..29 {
            assert_color(focused.pixel(x, 80), t.focus_inner, 4, "inner stroke");
        }
        assert_color(focused.pixel(200, 22), t.text, 4, "top edge");
        assert_within(&focused, &blank, CONTROL);
    }
}

#[test]
fn tab_label_is_dim_until_selected_or_hot_and_the_selected_tab_gets_an_accent_pill() {
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let default = Look::default();
        for (name, look, ink, pill) in [
            ("idle", default, t.text_secondary, false),
            (
                "hot",
                Look {
                    hot: true,
                    ..default
                },
                t.text,
                false,
            ),
            (
                "selected",
                Look {
                    selected: true,
                    ..default
                },
                t.text,
                true,
            ),
        ] {
            let (canvas, blank) = tab(look);
            let what = format!("{theme:?} {name}");
            assert_color(canvas.pixel(30, 25), BACKDROP, 0, &what);
            assert!(
                canvas.closest(40, 30, 360, 118, rgb(ink)) <= 4,
                "{what}: label colour"
            );
            // The pill: 16 x 3 design px, centred 2..5 px above the bottom edge.
            let pill_centre = canvas.pixel(200, 129);
            if pill {
                assert_color(pill_centre, t.accent_ink, 2, &what);
                assert_color(canvas.pixel(170, 129), BACKDROP, 0, &what);
            } else {
                assert_color(pill_centre, BACKDROP, 0, &what);
            }
            assert_within(&canvas, &blank, CONTROL);
        }
        let (focused, _) = tab(Look {
            focus: true,
            ..default
        });
        assert_color(focused.pixel(20, 80), t.text, 4, "focus ring");
        let (plain, _) = tab(default);
        assert_color(plain.pixel(20, 80), BACKDROP, 0, "no ring without focus");
    }
}

/// The toggle's geometry at `DPI`: track left/top and size, per the 40 x 20 design.
const TOGGLE: RECT = RECT {
    left: 0,
    top: 0,
    right: 600,
    bottom: 120,
};

fn toggle(look: Look) -> (Canvas, Vec<Rgb>) {
    paint((600, 120), |hdc, brush| {
        fluent_toggle(hdc, TOGGLE, &mut utf16("Hello"), DPI, look, brush)
    })
}

/// Pixels in row `y`, within `[from, to)`, close to `color`.
fn run_length(canvas: &Canvas, y: i32, from: i32, to: i32, color: COLORREF) -> i32 {
    (from..to)
        .filter(|&x| distance(canvas.pixel(x, y), rgb(color)) <= 12)
        .count() as i32
}

#[test]
fn toggle_track_knob_and_labels_follow_the_state() {
    let scale = (DPI / 96) as i32;
    let (track_width, track_height) = (40 * scale, 20 * scale);
    let x = TOGGLE.right - track_width - focus_extent(DPI);
    let y = (TOGGLE.top + TOGGLE.bottom - track_height) / 2;
    let middle = y + track_height / 2;
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let default = Look::default();
        // (look, track fill, border, knob colour, knob on the right)
        let states = [
            (
                "off",
                default,
                t.page,
                t.text_secondary,
                t.text_secondary,
                false,
            ),
            (
                "on",
                Look {
                    selected: true,
                    ..default
                },
                t.accent,
                t.accent,
                t.accent_text,
                true,
            ),
            (
                "on hot",
                Look {
                    selected: true,
                    hot: true,
                    ..default
                },
                t.accent_hover,
                t.accent_hover,
                t.accent_text,
                true,
            ),
            (
                "on pressed",
                Look {
                    selected: true,
                    pressed: true,
                    ..default
                },
                t.accent_pressed,
                t.accent_pressed,
                t.accent_text,
                true,
            ),
            (
                "disabled off",
                Look {
                    disabled: true,
                    ..default
                },
                t.page,
                t.text_disabled,
                t.text_secondary,
                false,
            ),
            (
                "disabled on",
                Look {
                    disabled: true,
                    selected: true,
                    ..default
                },
                t.page,
                t.text_disabled,
                t.accent_text,
                true,
            ),
        ];
        for (name, look, track, border, knob, right) in states {
            let (canvas, _) = toggle(look);
            let what = format!("{theme:?} {name}");
            // The knob's centre sits half a track height from the end the state
            // points to; the opposite stretch of track is left empty.
            let (knob_x, empty_x) = if right {
                (x + track_width - track_height / 2, x + 15 * scale)
            } else {
                (x + track_height / 2, x + track_width - 15 * scale)
            };
            assert_color(
                canvas.pixel(knob_x, middle),
                knob,
                2,
                &format!("{what}: knob"),
            );
            assert_color(
                canvas.pixel(empty_x, middle),
                track,
                2,
                &format!("{what}: track"),
            );
            // Without an outline (the border matches the track) the fill starts half a
            // pixel inside the edge; otherwise the 1 px border sits on the edge itself.
            let edge = if border == track { y + 1 } else { y };
            assert_color(
                canvas.pixel(x + track_width / 2, edge),
                border,
                4,
                &format!("{what}: edge"),
            );
            assert_color(
                canvas.pixel(x + track_width / 2, y - 3),
                BACKDROP,
                0,
                &format!("{what}: no ring"),
            );
        }

        // The knob grows under the pointer and while pressed.
        let width = |look: Look| {
            let (canvas, _) = toggle(look);
            run_length(
                &canvas,
                middle,
                x + 2,
                x + track_width - 2,
                t.text_secondary,
            )
        };
        let resting = width(default);
        assert!(
            width(Look {
                hot: true,
                ..default
            }) >= resting + 3 * scale / 2,
            "hot"
        );
        assert!(
            width(Look {
                pressed: true,
                ..default
            }) >= resting + 3 * scale / 2,
            "pressed"
        );

        // Label left, state word right-aligned a fixed gap before the switch.
        let (canvas, _) = toggle(Look {
            selected: true,
            ..default
        });
        let blank = vec![rgb(BACKDROP); (TOGGLE.right * TOGGLE.bottom) as usize];
        let state_area = (x - 58 * scale, 0, x - 8 * scale, TOGGLE.bottom);
        let word = canvas
            .changed_within(&blank, state_area)
            .expect("the state word");
        assert!(
            word.2 > x - 8 * scale - 12 && word.2 <= x - 8 * scale + 2,
            "{word:?}"
        );
        assert!(canvas.closest(state_area.0, 0, state_area.2, 120, rgb(t.text_secondary)) <= 6);
        let label_area = (0, 0, x - 58 * scale - 8 * scale, TOGGLE.bottom);
        let label = canvas
            .changed_within(&blank, label_area)
            .expect("the label");
        assert!((0..12).contains(&label.0), "{label:?}");
        assert!(canvas.closest(0, 0, label_area.2, 120, rgb(t.text)) <= 6);
    }
}

#[test]
fn toggle_focus_ring_surrounds_the_switch_only_with_keyboard_focus() {
    let scale = (DPI / 96) as i32;
    let x = TOGGLE.right - 40 * scale - focus_extent(DPI);
    let y = (TOGGLE.top + TOGGLE.bottom - 20 * scale) / 2;
    let top_middle = x + 20 * scale;
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let (focused, _) = toggle(Look {
            focus: true,
            ..Look::default()
        });
        for offset in 1..=3 {
            assert_color(
                focused.pixel(top_middle, y - offset),
                t.focus_inner,
                4,
                "inner stroke",
            );
        }
        for offset in 4..=9 {
            assert_color(
                focused.pixel(top_middle, y - offset),
                t.text,
                4,
                "outer stroke",
            );
        }
        let (plain, _) = toggle(Look::default());
        assert_color(
            plain.pixel(top_middle, y - 2),
            BACKDROP,
            0,
            "no ring without focus",
        );
    }
}

#[test]
fn custom_draw_states_map_to_the_look() {
    let draw = |from: HWND, state: NMCUSTOMDRAW_DRAW_STATE_FLAGS| NMCUSTOMDRAW {
        hdr: NMHDR {
            hwndFrom: from,
            ..Default::default()
        },
        uItemState: state,
        ..Default::default()
    };
    let none = Look::from_custom_draw(&draw(HWND::default(), NMCUSTOMDRAW_DRAW_STATE_FLAGS(0)));
    assert!(!none.primary && !none.selected && !none.disabled);
    assert!(!none.hot && !none.pressed && !none.focus);

    let all = Look::from_custom_draw(&draw(
        HWND::default(),
        CDIS_DISABLED | CDIS_HOT | CDIS_SELECTED | CDIS_FOCUS | CDIS_SHOWKEYBOARDCUES,
    ));
    assert!(all.disabled && all.hot && all.pressed && all.focus);
    assert!(!all.selected, "only a checked check box is selected");
    assert!(!all.primary);

    // Focus is a keyboard cue: it needs both flags.
    assert!(!Look::from_custom_draw(&draw(HWND::default(), CDIS_FOCUS)).focus);
    assert!(!Look::from_custom_draw(&draw(HWND::default(), CDIS_SHOWKEYBOARDCUES)).focus);

    let check_box = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!(""),
            WS_POPUP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            0,
            0,
            20,
            20,
            HWND::default(),
            None,
            HINSTANCE::default(),
            None,
        )
    }
    .expect("check box");
    let idle = NMCUSTOMDRAW_DRAW_STATE_FLAGS(0);
    assert!(!Look::from_custom_draw(&draw(check_box, idle)).selected);
    unsafe { SendMessageW(check_box, BM_SETCHECK, WPARAM(1), LPARAM(0)) };
    assert!(Look::from_custom_draw(&draw(check_box, idle)).selected);
    unsafe { SendMessageW(check_box, BM_SETCHECK, WPARAM(0), LPARAM(0)) };
    assert!(!Look::from_custom_draw(&draw(check_box, idle)).selected);
    let _ = unsafe { DestroyWindow(check_box) };
}
