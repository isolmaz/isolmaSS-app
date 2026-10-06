//! Upload toast: a small Windows 11 card in the bottom-right corner that shows
//! progress, then the share link with **Copy** / **Open** (the link is also
//! copied automatically), or the error with **Try again**. It never takes
//! focus and lives on the UI thread after the editor has closed.
use crate::upload::{Prepared, WM_UPLOAD_DONE};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, GetMonitorInfoW, HDC, HGDIOBJ, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, SRCCOPY, SelectObject,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const CLASS: PCWSTR = w!("isolmaSS_UploadToast");
const WIDTH: i32 = 380;
const HEIGHT: i32 = 112;
const TIMER_ANIMATE: usize = 1;
const TIMER_CLOSE: usize = 2;
/// How long a finished toast stays while the pointer is elsewhere.
const DONE_MS: u32 = 7000;
const FAILED_MS: u32 = 15000;
/// `WM_MOUSELEAVE`, delivered after `TrackMouseEvent(TME_LEAVE)`.
const WM_MOUSELEAVE: u32 = 0x02A3;

thread_local! { static CURRENT: Cell<isize> = const { Cell::new(0) }; }

/// The outcome of one upload attempt, tagged with its attempt number.
type AttemptResult = (u64, Result<String, String>);

enum Phase {
    Uploading,
    Done { url: String, copied: bool },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Close,
    Copy,
    Open,
    Retry,
}

struct State {
    prepared: Prepared,
    phase: Phase,
    /// Identifies the current attempt; results of abandoned attempts are ignored.
    attempt: u64,
    result: Arc<Mutex<Option<AttemptResult>>>,
    frame: u32,
    dpi: u32,
    hot: Option<Action>,
    pressed: Option<Action>,
    hovering: bool,
}

fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// Starts uploading `prepared` and shows its toast near `anchor` (screen
/// coordinates, used to pick the monitor). A previous toast is replaced.
pub fn show_upload(prepared: Prepared, anchor: POINT) {
    if let Err(error) = create(prepared, anchor) {
        crate::tray::show_notification(crate::i18n::t("Could not start the upload"), &error);
    }
}

/// Pumps messages until the current toast has closed. Used by one-shot
/// command-line captures, whose process would otherwise end mid-upload.
pub fn wait_until_closed() {
    let mut msg = MSG::default();
    while CURRENT.with(Cell::get) != 0 {
        let status = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if status <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn create(prepared: Prepared, anchor: POINT) -> Result<(), String> {
    register().map_err(|error| error.to_string())?;
    let previous = CURRENT.with(|current| current.replace(0));
    if previous != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(previous as *mut _));
        }
    }
    let state = Box::new(State {
        prepared,
        phase: Phase::Uploading,
        attempt: 0,
        result: Arc::new(Mutex::new(None)),
        frame: 0,
        dpi: 96,
        hot: None,
        pressed: None,
        hovering: false,
    });
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS,
            &windows::core::HSTRING::from(crate::i18n::t("isolmaSS upload")),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            HINSTANCE::default(),
            None,
        )
    }
    .map_err(|error| error.to_string())?;
    let state = Box::into_raw(state);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
    }
    CURRENT.with(|current| current.set(hwnd.0 as isize));
    let state = unsafe { &mut *state };
    // Size and place on the work area of the capture's monitor.
    let monitor = unsafe { MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let work = if unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        }
    };
    let mut dpi_x = 96;
    let mut dpi_y = 96;
    if unsafe {
        windows::Win32::UI::HiDpi::GetDpiForMonitor(
            monitor,
            windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
            &mut dpi_x,
            &mut dpi_y,
        )
    }
    .is_ok()
    {
        state.dpi = dpi_x.max(96);
    }
    let width = scale(WIDTH, state.dpi);
    let height = scale(HEIGHT, state.dpi);
    let margin = scale(12, state.dpi);
    unsafe {
        let dark: u32 = (crate::theme::theme() == crate::theme::Theme::Dark) as u32;
        let round: u32 = 2; // DWMWCP_ROUND
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20),
            std::ptr::from_ref(&dark).cast(),
            4,
        );
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(33),
            std::ptr::from_ref(&round).cast(),
            4,
        );
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            work.right - width - margin,
            work.bottom - height - margin,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    start_attempt(hwnd, state);
    Ok(())
}

fn start_attempt(hwnd: HWND, state: &mut State) {
    state.attempt += 1;
    state.phase = Phase::Uploading;
    state.frame = 0;
    unsafe {
        let _ = KillTimer(hwnd, TIMER_CLOSE);
        SetTimer(hwnd, TIMER_ANIMATE, 30, None);
    }
    let attempt = state.attempt;
    let prepared = state.prepared.clone();
    let output = state.result.clone();
    let window = hwnd.0 as usize;
    let spawned = std::thread::Builder::new()
        .name("isolmass-upload".into())
        .spawn(move || {
            let outcome = crate::upload::send(&prepared);
            if let Ok(mut slot) = output.lock() {
                *slot = Some((attempt, outcome));
            }
            // The toast may already be closed; then there is nobody to tell.
            unsafe {
                let _ = PostMessageW(HWND(window as *mut _), WM_UPLOAD_DONE, WPARAM(0), LPARAM(0));
            }
        });
    if let Err(error) = spawned {
        finish(
            hwnd,
            state,
            Err(crate::i18n::tf("Could not start the upload: {}", &[&error])),
        );
    }
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn finish(hwnd: HWND, state: &mut State, outcome: Result<String, String>) {
    unsafe {
        let _ = KillTimer(hwnd, TIMER_ANIMATE);
    }
    state.phase = match outcome {
        Ok(url) => {
            let copied = crate::clipboard::copy_text_to_clipboard(Some(hwnd), &url).is_ok();
            Phase::Done { url, copied }
        }
        Err(error) => {
            crate::diagnostics::record("upload", &error);
            Phase::Failed(error)
        }
    };
    arm_close(hwnd, state);
    unsafe {
        // Another topmost window may have been activated meanwhile; the result
        // must be visible without taking focus.
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn arm_close(hwnd: HWND, state: &State) {
    let delay = match state.phase {
        Phase::Uploading => return,
        Phase::Done { .. } => DONE_MS,
        Phase::Failed(_) => FAILED_MS,
    };
    unsafe {
        if state.hovering {
            let _ = KillTimer(hwnd, TIMER_CLOSE);
        } else {
            SetTimer(hwnd, TIMER_CLOSE, delay, None);
        }
    }
}

/// Button rectangles for the current phase, right to left.
fn buttons(state: &State, client: RECT) -> Vec<(Action, &'static str, RECT)> {
    let dpi = state.dpi;
    let close = RECT {
        left: client.right - scale(40, dpi),
        top: scale(8, dpi),
        right: client.right - scale(8, dpi),
        bottom: scale(40, dpi),
    };
    let mut list = vec![(Action::Close, "", close)];
    let row = |right: i32, width: i32| RECT {
        left: right - scale(width, dpi),
        top: client.bottom - scale(44, dpi),
        right,
        bottom: client.bottom - scale(12, dpi),
    };
    let right = client.right - scale(16, dpi);
    match state.phase {
        Phase::Uploading => {}
        Phase::Done { .. } => {
            let open = row(right, 72);
            let copy = row(open.left - scale(8, dpi), 96);
            list.push((Action::Open, crate::i18n::t("Open"), open));
            list.push((Action::Copy, crate::i18n::t("Copy"), copy));
        }
        Phase::Failed(_) => {
            list.push((Action::Retry, crate::i18n::t("Try again"), row(right, 112)));
        }
    }
    list
}

fn hit(state: &State, hwnd: HWND, point: POINT) -> Option<Action> {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    buttons(state, client)
        .into_iter()
        .find(|(_, _, rect)| {
            point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
        })
        .map(|(action, _, _)| action)
}

fn run(hwnd: HWND, state: &mut State, action: Action) {
    match action {
        Action::Close => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        Action::Copy => {
            if let Phase::Done { url, copied } = &mut state.phase {
                *copied = crate::clipboard::copy_text_to_clipboard(Some(hwnd), url).is_ok();
            }
            arm_close(hwnd, state);
        }
        Action::Open => {
            if let Phase::Done { url, .. } = &state.phase {
                if let Err(error) = crate::upload::open_link(url) {
                    crate::tray::show_notification(
                        crate::i18n::t("Could not open the link"),
                        &error,
                    );
                }
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
        Action::Retry => start_attempt(hwnd, state),
    }
}

fn paint(hwnd: HWND, state: &State, hdc: HDC) {
    use crate::drawing::{Look, fluent_button, icon, rounded, with_font};
    let tokens = crate::theme::tokens();
    let dpi = state.dpi;
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let background = unsafe { CreateSolidBrush(tokens.card) };
    unsafe {
        let _ = FillRect(hdc, &client, background);
    }
    let (glyph, glyph_color, title, detail) = match &state.phase {
        Phase::Uploading => (
            0xe898u16,
            tokens.accent,
            crate::i18n::t("Uploading…").to_string(),
            crate::i18n::t("Sending to the Worker in your Cloudflare account").to_string(),
        ),
        Phase::Done { url, copied } => (
            0xe73e,
            tokens.accent,
            if *copied {
                crate::i18n::t("Uploaded · link copied").to_string()
            } else {
                crate::i18n::t("Uploaded").to_string()
            },
            url.trim_start_matches("https://").to_string(),
        ),
        Phase::Failed(error) => (
            0xe783,
            windows::Win32::Foundation::COLORREF(0x004f_4fe8),
            crate::i18n::t("Upload failed").to_string(),
            error.clone(),
        ),
    };
    icon(
        hdc,
        crate::capture::Rect::new(
            scale(16, dpi),
            scale(14, dpi),
            scale(40, dpi),
            scale(38, dpi),
        ),
        glyph,
        scale(18, dpi),
        glyph_color,
        true,
    );
    let mut title_wide: Vec<u16> = title.encode_utf16().collect();
    let mut title_rect = RECT {
        left: scale(48, dpi),
        top: scale(12, dpi),
        right: client.right - scale(48, dpi),
        bottom: scale(36, dpi),
    };
    with_font(
        hdc,
        -scale(crate::theme::FONT_BODY_PX, dpi),
        600,
        || unsafe {
            use windows::Win32::Graphics::Gdi::*;
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, tokens.text);
            let _ = DrawTextW(
                hdc,
                &mut title_wide,
                &mut title_rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        },
    );
    let mut detail_wide: Vec<u16> = detail.encode_utf16().collect();
    let mut detail_rect = RECT {
        left: scale(48, dpi),
        top: scale(36, dpi),
        right: client.right - scale(16, dpi),
        bottom: scale(58, dpi),
    };
    // A link keeps its distinctive end visible ("isolmass-share-…/i/<id>").
    let link = matches!(state.phase, Phase::Done { .. });
    with_font(
        hdc,
        -scale(crate::theme::FONT_BODY_PX, dpi),
        400,
        || unsafe {
            use windows::Win32::Graphics::Gdi::*;
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, tokens.text_secondary);
            if link {
                detail_wide = middle_ellipsis(hdc, &detail, detail_rect.right - detail_rect.left);
            }
            if !detail_wide.is_empty() {
                let _ = DrawTextW(
                    hdc,
                    &mut detail_wide,
                    &mut detail_rect,
                    DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
                );
            }
        },
    );
    if matches!(state.phase, Phase::Uploading) {
        // Indeterminate progress, as in Windows 11 transfer dialogs.
        let left = scale(48, dpi);
        let right = client.right - scale(16, dpi);
        let top = client.bottom - scale(30, dpi);
        let bottom = top + scale(3, dpi).max(2);
        rounded(
            hdc,
            crate::capture::Rect::new(left, top, right, bottom),
            4,
            tokens.stroke,
            tokens.stroke,
        );
        let span = right - left;
        let segment = (span / 3).max(1);
        let position = (state.frame as i32 * span / 60) % (span + segment) - segment;
        let start = (left + position).max(left);
        let end = (left + position + segment).min(right);
        if end > start {
            rounded(
                hdc,
                crate::capture::Rect::new(start, top, end, bottom),
                4,
                tokens.accent,
                tokens.accent,
            );
        }
    }
    for (action, label, rect) in buttons(state, client) {
        let look = Look {
            primary: matches!(action, Action::Copy | Action::Retry),
            hot: state.hot == Some(action),
            pressed: state.pressed == Some(action),
            ..Look::default()
        };
        if action == Action::Close {
            if look.hot {
                rounded(
                    hdc,
                    crate::capture::Rect::new(rect.left, rect.top, rect.right, rect.bottom),
                    scale(6, dpi),
                    tokens.control_hover,
                    tokens.control_hover,
                );
            }
            icon(
                hdc,
                crate::capture::Rect::new(rect.left, rect.top, rect.right, rect.bottom),
                0xe711,
                scale(12, dpi),
                tokens.text_secondary,
                true,
            );
            continue;
        }
        let mut text: Vec<u16> = label.encode_utf16().collect();
        fluent_button(hdc, rect, &mut text, dpi, look, background);
    }
    unsafe {
        let _ = DeleteObject(HGDIOBJ(background.0));
    }
}

/// Shortens `text` to `width` pixels in the selected font, keeping everything
/// from the last "/i/" on and trimming the host in the middle.
fn middle_ellipsis(hdc: HDC, text: &str, width: i32) -> Vec<u16> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::GetTextExtentPoint32W;
    let measure = |candidate: &[u16]| {
        let mut size = SIZE::default();
        unsafe {
            let _ = GetTextExtentPoint32W(hdc, candidate, &mut size);
        }
        size.cx
    };
    let full: Vec<u16> = text.encode_utf16().collect();
    if measure(&full) <= width {
        return full;
    }
    let split = text.rfind("/i/").unwrap_or(text.len());
    let head: Vec<char> = text[..split].chars().collect();
    let tail = &text[split..];
    for keep in (1..head.len()).rev() {
        let candidate: String = head[..keep].iter().collect::<String>() + "…" + tail;
        let wide: Vec<u16> = candidate.encode_utf16().collect();
        if measure(&wide) <= width {
            return wide;
        }
    }
    full
}

fn point_from(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam.0 & 0xffff) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *pointer };
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint_struct = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut paint_struct) };
            let mut client = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut client);
                // Double-buffered so the progress animation never flickers.
                let memory = CreateCompatibleDC(hdc);
                let bitmap = CreateCompatibleBitmap(hdc, client.right, client.bottom);
                let previous = SelectObject(memory, HGDIOBJ(bitmap.0));
                paint(hwnd, state, memory);
                let _ = BitBlt(
                    hdc,
                    0,
                    0,
                    client.right,
                    client.bottom,
                    memory,
                    0,
                    0,
                    SRCCOPY,
                );
                SelectObject(memory, previous);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(memory);
                let _ = EndPaint(hwnd, &paint_struct);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_ANIMATE => {
            state.frame = state.frame.wrapping_add(1);
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_UPLOAD_DONE => {
            let outcome = state.result.lock().ok().and_then(|mut slot| slot.take());
            if let Some((attempt, outcome)) = outcome
                && attempt == state.attempt
                && matches!(state.phase, Phase::Uploading)
            {
                finish(hwnd, state, outcome);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if !state.hovering {
                state.hovering = true;
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut track);
                }
                arm_close(hwnd, state);
            }
            let hot = hit(state, hwnd, point_from(lparam));
            if hot != state.hot {
                state.hot = hot;
                unsafe {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            state.hovering = false;
            state.hot = None;
            state.pressed = None;
            arm_close(hwnd, state);
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            state.pressed = hit(state, hwnd, point_from(lparam));
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let action = hit(state, hwnd, point_from(lparam));
            let pressed = state.pressed.take();
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            if let Some(action) = action.filter(|action| Some(*action) == pressed) {
                run(hwnd, state, action);
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED => {
            crate::theme::invalidate_theme_cache();
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_ANIMATE);
                let _ = KillTimer(hwnd, TIMER_CLOSE);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                // Drops the prepared file unless an attempt still holds it.
                drop(Box::from_raw(pointer));
            }
            CURRENT.with(|current| {
                if current.get() == hwnd.0 as isize {
                    current.set(0);
                }
            });
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register() -> windows::core::Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DROPSHADOW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE::default(),
        hCursor: unsafe { LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default() },
        lpszClassName: CLASS,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0
        && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
    {
        return Err(windows::core::Error::from_win32());
    }
    REGISTERED.store(true, Ordering::Release);
    Ok(())
}
