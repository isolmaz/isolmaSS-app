//! Bounded, UI-thread-local GDI resources shared by previews, output and controls.
use std::cell::RefCell;
use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::core::PCWSTR;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    /// (face, pixel height, weight): the face keeps the Segoe UI text cache
    /// and the Segoe Fluent Icons glyph cache as distinct entries.
    Font(&'static str, i32, i32),
    Pen(i32, i32, u32),
    Brush(u32),
}
struct Cache(Vec<(Key, HGDIOBJ)>);
impl Drop for Cache {
    fn drop(&mut self) {
        for (_, object) in self.0.drain(..) {
            unsafe {
                let _ = DeleteObject(object);
            }
        }
    }
}
type TextMetrics = Vec<(String, i32, (i32, i32))>;
thread_local! { static METRICS: RefCell<TextMetrics> = const { RefCell::new(Vec::new()) }; }
thread_local! { static CACHE: RefCell<Cache> = const { RefCell::new(Cache(Vec::new())) }; }
thread_local! { static LOGGED_FAILURES: RefCell<Vec<Key>> = const { RefCell::new(Vec::new()) }; }

/// Records a GDI creation failure once per object key so the paint loop cannot
/// flood the bounded diagnostic log, while the failure stays observable.
fn note_create_failure(key: Key) {
    LOGGED_FAILURES.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.len() < 64 && !seen.contains(&key) {
            seen.push(key);
            // Consoleless app: go through the rotating diagnostic log, not stderr.
            crate::diagnostics::record(
                "gdi",
                &format!("GDI object creation failed for {key:?}; using stock fallback"),
            );
        }
    });
}

/// Stock stand-ins for failed creations: visibly degraded (black strokes, white
/// fills, system font) instead of a silent no-op, and never deleted by owners.
fn stock_fallback(key: Key) -> HGDIOBJ {
    unsafe {
        match key {
            Key::Font(..) => GetStockObject(SYSTEM_FONT),
            Key::Pen(..) => GetStockObject(BLACK_PEN),
            Key::Brush(..) => GetStockObject(WHITE_BRUSH),
        }
    }
}

fn with_object<T>(
    hdc: HDC,
    key: Key,
    create: impl FnOnce() -> HGDIOBJ,
    action: impl FnOnce() -> T,
) -> T {
    let (object, owned) = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, object)) = cache.0.iter().find(|(existing, _)| *existing == key) {
            return (*object, false);
        }
        let object = create();
        if cache.0.len() < 48 && !object.is_invalid() {
            cache.0.push((key, object));
            (object, false)
        } else {
            (object, true)
        }
    });
    // A failed creation must never be selected: substitute a stock object for
    // this draw only (the next call retries the real creation) and log it. The
    // stock object is not owned, so Drop only restores the previous selection.
    let (object, owned) = if object.is_invalid() {
        note_create_failure(key);
        (stock_fallback(key), false)
    } else {
        (object, owned)
    };
    struct Selection {
        hdc: HDC,
        previous: HGDIOBJ,
        object: HGDIOBJ,
        owned: bool,
    }
    impl Drop for Selection {
        fn drop(&mut self) {
            unsafe {
                // A failed SelectObject leaves the DC unchanged and returns an
                // invalid sentinel; restoring that sentinel would corrupt the DC.
                if !self.previous.is_invalid() {
                    SelectObject(self.hdc, self.previous);
                }
                // Stock fallbacks are never owned, so they are never deleted.
                if self.owned && !self.object.is_invalid() {
                    let _ = DeleteObject(self.object);
                }
            }
        }
    }
    // Never hand an invalid handle (failed creation or failed stock fallback)
    // to SelectObject: skip the selection and leave the DC untouched instead.
    let previous = if object.is_invalid() {
        HGDIOBJ::default()
    } else {
        unsafe { SelectObject(hdc, object) }
    };
    let _selection = Selection {
        hdc,
        previous,
        object,
        owned,
    };
    action()
}

/// Windows 11 ships Segoe Fluent Icons; Windows 10 ships Segoe MDL2 Assets with the
/// same E-range codepoints. GDI never substitutes one for the other, so the
/// installed face is chosen once.
fn icon_face() -> &'static str {
    use std::sync::LazyLock;
    static FACE: LazyLock<&'static str> = LazyLock::new(|| {
        const FLUENT: &str = "Segoe Fluent Icons";
        const MDL2: &str = "Segoe MDL2 Assets";
        if crate::theme::face_installed(FLUENT) {
            FLUENT
        } else {
            MDL2
        }
    });
    *FACE
}

pub fn with_font<T>(hdc: HDC, height: i32, weight: i32, action: impl FnOnce() -> T) -> T {
    with_font_face(hdc, crate::theme::ui_face(), height, weight, action)
}

/// Like [`with_font`], but for an arbitrary typeface — e.g.
/// `"Segoe Fluent Icons"` for toolbar glyphs. Fonts are cached per
/// `(face, height, weight)` so both faces coexist in the same cache.
pub fn with_font_face<T>(
    hdc: HDC,
    face: &'static str,
    height: i32,
    weight: i32,
    action: impl FnOnce() -> T,
) -> T {
    with_object(
        hdc,
        Key::Font(face, height, weight),
        || unsafe {
            // The wide conversion allocates only on a cache miss; the created
            // HFONT is then reused for every later draw with this face.
            let wide: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
            HGDIOBJ(
                CreateFontW(
                    height,
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET.0 as u32,
                    OUT_DEFAULT_PRECIS.0 as u32,
                    CLIP_DEFAULT_PRECIS.0 as u32,
                    // Icon glyphs use grayscale anti-aliasing like the shell:
                    // ClearType's colour fringes show around thin outlines.
                    if face == icon_face() {
                        ANTIALIASED_QUALITY.0 as u32
                    } else {
                        CLEARTYPE_QUALITY.0 as u32
                    },
                    DEFAULT_PITCH.0 as u32,
                    PCWSTR(wide.as_ptr()),
                )
                .0,
            )
        },
        action,
    )
}

pub fn with_pen<T>(
    hdc: HDC,
    style: PEN_STYLE,
    width: i32,
    color: COLORREF,
    action: impl FnOnce() -> T,
) -> T {
    with_object(
        hdc,
        Key::Pen(style.0, width, color.0),
        || unsafe { HGDIOBJ(CreatePen(style, width, color).0) },
        action,
    )
}

pub fn with_brush<T>(hdc: HDC, color: COLORREF, action: impl FnOnce() -> T) -> T {
    with_object(
        hdc,
        Key::Brush(color.0),
        || unsafe { HGDIOBJ(CreateSolidBrush(color).0) },
        action,
    )
}

pub fn measure_text(text: &str, font_size: i32) -> (i32, i32) {
    if let Some(size) = METRICS.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(value, size, _)| value == text && *size == font_size)
            .map(|(_, _, result)| *result)
    }) {
        return size;
    }
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.is_invalid() {
        return (font_size.max(1), font_size.max(1));
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    let measured_ok = with_font(dc, font_size, 700, || unsafe {
        GetTextExtentPoint32W(dc, &wide, &mut size).as_bool()
    });
    unsafe {
        let _ = DeleteDC(dc);
    }
    let measured = (size.cx.max(1), size.cy.max(font_size.max(1)));
    // Bounded and session-local: never retain large user text or grow with every caret move.
    // Only a real measurement enters the cache; after a failed
    // GetTextExtentPoint32W a later successful measurement can still replace
    // the fallback for this string.
    if measured_ok && text.len() <= 1024 {
        METRICS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() == 32 {
                cache.remove(0);
            }
            cache.push((text.to_owned(), font_size, measured));
        });
    }
    measured
}

pub fn text(hdc: HDC, pos: (i32, i32), text: &str, font_size: i32, color: COLORREF) {
    with_font(hdc, font_size, 700, || unsafe {
        let _ = SetTextColor(hdc, color);
        let _ = SetBkMode(hdc, TRANSPARENT);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let _ = TextOutW(hdc, pos.0, pos.1, &wide);
    });
}

pub fn clear_text_metrics() {
    METRICS.with(|cache| cache.borrow_mut().clear());
}

/// Fills and outlines a rounded rectangle (`radius` is the corner ellipse
/// diameter, as with GDI `RoundRect`). GDI+ draws it anti-aliased so small
/// shapes — switches, check boxes, swatches, toolbar pills — stay smooth at
/// every scale; plain GDI remains the fallback if GDI+ is unavailable.
pub fn rounded(
    hdc: HDC,
    rect: crate::capture::Rect,
    radius: i32,
    fill: COLORREF,
    border: COLORREF,
) {
    if smooth_rounded(hdc, rect, radius, fill, border) {
        return;
    }
    with_brush(hdc, fill, || {
        with_pen(hdc, PS_SOLID, 1, border, || unsafe {
            let _ = RoundRect(
                hdc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                radius,
                radius,
            );
        })
    });
}

fn gdiplus_ready() -> bool {
    use std::sync::LazyLock;
    use windows::Win32::Graphics::GdiPlus::{GdiplusStartup, GdiplusStartupInput, Ok};
    static READY: LazyLock<bool> = LazyLock::new(|| {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let mut token = 0usize;
        // The token lives for the whole process; GDI+ is torn down with it.
        let status = unsafe { GdiplusStartup(&mut token, &input, std::ptr::null_mut()) };
        if status != Ok {
            crate::diagnostics::record("drawing", &format!("GDI+ unavailable: {}", status.0));
        }
        status == Ok
    });
    *READY
}

fn argb(color: COLORREF) -> u32 {
    let r = color.0 & 0xff;
    let g = (color.0 >> 8) & 0xff;
    let b = (color.0 >> 16) & 0xff;
    0xff00_0000 | (r << 16) | (g << 8) | b
}

fn smooth_rounded(
    hdc: HDC,
    rect: crate::capture::Rect,
    radius: i32,
    fill: COLORREF,
    border: COLORREF,
) -> bool {
    use windows::Win32::Graphics::GdiPlus::*;
    if !gdiplus_ready() || rect.right - rect.left < 2 || rect.bottom - rect.top < 2 {
        return false;
    }
    unsafe {
        let mut graphics = std::ptr::null_mut();
        if GdipCreateFromHDC(hdc, &mut graphics) != Ok {
            return false;
        }
        GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias8x8);
        GdipSetPixelOffsetMode(graphics, PixelOffsetModeHalf);
        let mut path = std::ptr::null_mut();
        let mut drawn = false;
        if GdipCreatePath(FillModeAlternate, &mut path) == Ok {
            // Match RoundRect: the outline sits inside [left, right) × [top, bottom).
            let x = rect.left as f32 + 0.5;
            let y = rect.top as f32 + 0.5;
            let w = (rect.right - rect.left) as f32 - 1.0;
            let h = (rect.bottom - rect.top) as f32 - 1.0;
            let d = (radius as f32).clamp(0.0, w.min(h));
            if d < 1.0 {
                GdipAddPathRectangle(path, x, y, w, h);
            } else {
                GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
                GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
                GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
                GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
                GdipClosePathFigure(path);
            }
            let mut brush = std::ptr::null_mut();
            if GdipCreateSolidFill(argb(fill), &mut brush) == Ok {
                GdipFillPath(graphics, brush.cast(), path);
                GdipDeleteBrush(brush.cast());
                drawn = true;
            }
            if border != fill {
                let mut pen = std::ptr::null_mut();
                if GdipCreatePen1(argb(border), 1.0, UnitPixel, &mut pen) == Ok {
                    GdipDrawPath(graphics, pen, path);
                    GdipDeletePen(pen);
                }
            }
            GdipDeletePath(path);
        }
        GdipDeleteGraphics(graphics);
        drawn
    }
}

pub fn label(
    hdc: HDC,
    rect: crate::capture::Rect,
    text: &str,
    size: i32,
    color: COLORREF,
    centered: bool,
) {
    with_font(hdc, -size.max(1), 600, || unsafe {
        let _ = SetTextColor(hdc, color);
        let _ = SetBkMode(hdc, TRANSPARENT);
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }
        let mut bounds = windows::Win32::Foundation::RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        let _ = DrawTextW(
            hdc,
            &mut wide,
            &mut bounds,
            DT_SINGLELINE
                | DT_VCENTER
                | DT_NOPREFIX
                | DT_END_ELLIPSIS
                | if centered { DT_CENTER } else { DT_LEFT },
        );
    });
}

/// Draws one icon-font glyph (a single UTF-16 PUA codepoint) inside `rect`,
/// using the same layout rules as [`label`]. The chosen E-range codepoints
/// exist in both Segoe Fluent Icons and Segoe MDL2 Assets (see [`icon_face`]).
pub fn icon(
    hdc: HDC,
    rect: crate::capture::Rect,
    codepoint: u16,
    size_px: i32,
    color: COLORREF,
    center: bool,
) {
    with_font_face(hdc, icon_face(), -size_px.max(1), 400, || unsafe {
        let _ = SetTextColor(hdc, color);
        let _ = SetBkMode(hdc, TRANSPARENT);
        let mut wide = [codepoint];
        let mut bounds = windows::Win32::Foundation::RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        let _ = DrawTextW(
            hdc,
            &mut wide,
            &mut bounds,
            DT_SINGLELINE
                | DT_VCENTER
                | DT_NOPREFIX
                | DT_END_ELLIPSIS
                | if center { DT_CENTER } else { DT_LEFT },
        );
    });
}

/// Interaction state of an owner-drawn Windows 11 control.
#[derive(Clone, Copy, Default)]
pub struct Look {
    pub primary: bool,
    pub selected: bool,
    pub disabled: bool,
    pub hot: bool,
    pub pressed: bool,
    /// Keyboard focus while keyboard cues are shown (not after a mouse click).
    pub focus: bool,
}

impl Look {
    /// Reads the state flags of an `NM_CUSTOMDRAW` paint request.
    pub fn from_custom_draw(draw: &windows::Win32::UI::Controls::NMCUSTOMDRAW) -> Self {
        use windows::Win32::UI::Controls::*;
        let state = draw.uItemState;
        let checked = unsafe {
            windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                draw.hdr.hwndFrom,
                windows::Win32::UI::WindowsAndMessaging::BM_GETCHECK,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            )
            .0
        } == BST_CHECKED.0 as isize;
        Self {
            primary: false,
            selected: checked,
            disabled: state.contains(CDIS_DISABLED),
            hot: state.contains(CDIS_HOT),
            pressed: state.contains(CDIS_SELECTED),
            focus: state.contains(CDIS_FOCUS) && state.contains(CDIS_SHOWKEYBOARDCUES),
        }
    }
}

fn scaled(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// Draws single-line UI text in the body font.
pub fn ui_text(
    hdc: HDC,
    text: &mut [u16],
    rect: &mut windows::Win32::Foundation::RECT,
    dpi: u32,
    color: COLORREF,
    centered: bool,
) {
    // An empty slice carries a dangling pointer that DrawTextW dereferences.
    if text.is_empty() {
        return;
    }
    with_font(
        hdc,
        -scaled(crate::theme::FONT_BODY_PX, dpi),
        400,
        || unsafe {
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, color);
            let _ = DrawTextW(
                hdc,
                text,
                rect,
                DT_VCENTER
                    | DT_SINGLELINE
                    | DT_NOPREFIX
                    | DT_END_ELLIPSIS
                    | if centered { DT_CENTER } else { DT_LEFT },
            );
        },
    );
}

/// Windows 11 push button (accent-filled when `look.primary`) on `background`.
pub fn fluent_button(
    hdc: HDC,
    rect: windows::Win32::Foundation::RECT,
    text: &mut [u16],
    dpi: u32,
    look: Look,
    background: HBRUSH,
) {
    let tokens = crate::theme::tokens();
    unsafe {
        let _ = FillRect(hdc, &rect, background);
    }
    let fill = if look.disabled {
        tokens.control_fill
    } else if look.primary {
        tokens.accent
    } else if look.pressed || look.hot {
        tokens.control_hover
    } else {
        tokens.control_fill
    };
    let border = if look.focus {
        tokens.text
    } else if look.primary && !look.disabled {
        tokens.accent
    } else {
        tokens.stroke
    };
    rounded(
        hdc,
        crate::capture::Rect::new(rect.left + 1, rect.top + 1, rect.right - 1, rect.bottom - 1),
        scaled(8, dpi),
        fill,
        border,
    );
    let color = if look.disabled {
        tokens.text_disabled
    } else if look.primary {
        tokens.accent_text
    } else {
        tokens.text
    };
    let mut inner = windows::Win32::Foundation::RECT {
        left: rect.left + scaled(8, dpi),
        right: rect.right - scaled(8, dpi),
        ..rect
    };
    ui_text(hdc, text, &mut inner, dpi, color, true);
}

/// Windows 11 SelectorBar item: the label, with a short accent pill under
/// the selected item.
pub fn fluent_tab(
    hdc: HDC,
    rect: windows::Win32::Foundation::RECT,
    text: &mut [u16],
    dpi: u32,
    look: Look,
    background: HBRUSH,
) {
    let tokens = crate::theme::tokens();
    unsafe {
        let _ = FillRect(hdc, &rect, background);
    }
    let color = if look.selected || look.hot {
        tokens.text
    } else {
        tokens.text_secondary
    };
    let mut label = windows::Win32::Foundation::RECT {
        bottom: rect.bottom - scaled(4, dpi),
        ..rect
    };
    ui_text(hdc, text, &mut label, dpi, color, true);
    if look.selected {
        let half = scaled(8, dpi);
        let middle = (rect.left + rect.right) / 2;
        rounded(
            hdc,
            crate::capture::Rect::new(
                middle - half,
                rect.bottom - scaled(5, dpi),
                middle + half,
                rect.bottom - scaled(2, dpi),
            ),
            scaled(3, dpi),
            tokens.accent,
            tokens.accent,
        );
    }
    if look.focus {
        let focus = windows::Win32::Foundation::RECT {
            left: rect.left + 2,
            top: rect.top + 2,
            right: rect.right - 2,
            bottom: rect.bottom - 2,
        };
        unsafe {
            let _ = DrawFocusRect(hdc, &focus);
        }
    }
}

/// Windows 11 ToggleSwitch row: the label on the left, "On/Off" and a
/// 40×20 switch on the right.
pub fn fluent_toggle(
    hdc: HDC,
    rect: windows::Win32::Foundation::RECT,
    text: &mut [u16],
    dpi: u32,
    look: Look,
    background: HBRUSH,
) {
    let tokens = crate::theme::tokens();
    unsafe {
        let _ = FillRect(hdc, &rect, background);
    }
    let on = look.selected;
    let track_width = scaled(40, dpi);
    let track_height = scaled(20, dpi);
    let x = rect.right - track_width - scaled(2, dpi);
    let y = (rect.top + rect.bottom - track_height) / 2;
    let (track, border) = if look.disabled {
        (tokens.page, tokens.text_disabled)
    } else if on {
        (tokens.accent, tokens.accent)
    } else {
        (tokens.page, tokens.text_secondary)
    };
    rounded(
        hdc,
        crate::capture::Rect::new(x, y, x + track_width, y + track_height),
        track_height,
        track,
        border,
    );
    let knob = scaled(if look.hot || look.pressed { 14 } else { 12 }, dpi);
    let inset = (track_height - knob) / 2;
    let knob_x = if on {
        x + track_width - knob - inset
    } else {
        x + inset
    };
    let knob_color = if on {
        tokens.accent_text
    } else {
        tokens.text_secondary
    };
    rounded(
        hdc,
        crate::capture::Rect::new(knob_x, y + inset, knob_x + knob, y + inset + knob),
        knob,
        knob_color,
        knob_color,
    );
    let mut state_text: Vec<u16> = if on {
        crate::i18n::t("On")
    } else {
        crate::i18n::t("Off")
    }
    .encode_utf16()
    .collect();
    let mut state_rect = windows::Win32::Foundation::RECT {
        left: x - scaled(60, dpi),
        right: x - scaled(10, dpi),
        ..rect
    };
    ui_text(
        hdc,
        &mut state_text,
        &mut state_rect,
        dpi,
        tokens.text_secondary,
        false,
    );
    let mut label = windows::Win32::Foundation::RECT {
        right: state_rect.left - scaled(8, dpi),
        ..rect
    };
    ui_text(hdc, text, &mut label, dpi, tokens.text, false);
    if look.focus {
        let focus = windows::Win32::Foundation::RECT {
            left: x - 3,
            top: y - 3,
            right: x + track_width + 3,
            bottom: y + track_height + 3,
        };
        unsafe {
            let _ = DrawFocusRect(hdc, &focus);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_face_is_an_installed_icon_font() {
        assert!(crate::theme::face_installed(super::icon_face()));
    }
}
