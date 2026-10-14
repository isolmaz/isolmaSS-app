//! Upload toast: a small Windows 11 card in the bottom-right corner that shows
//! progress, then the share link with **Copy** / **Open** (the link is also
//! copied automatically), or the error with **Try again**. It never takes
//! focus and lives on the UI thread after the editor has closed.
use crate::upload::{Prepared, WM_UPLOAD_DONE, WM_UPLOAD_PROGRESS};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmSetWindowAttribute,
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
/// Height of the failed card with a two-line detail, before it grows further.
const HEIGHT_FAILED: i32 = 132;
const TIMER_ANIMATE: usize = 1;
const TIMER_CLOSE: usize = 2;
const TIMER_COPIED: usize = 3;
/// How long a finished toast stays while the pointer is elsewhere.
const DONE_MS: u32 = 7000;
const FAILED_MS: u32 = 15000;
/// How long the Copy button reads "Copied".
const COPIED_MS: u32 = 1500;
const FADE_IN_MS: u32 = 150;
const FADE_OUT_MS: u32 = 100;
/// The toast rises this far (96-DPI pixels) while it fades in.
const SLIDE_PX: i32 = 8;
/// `WM_MOUSELEAVE`, delivered after `TrackMouseEvent(TME_LEAVE)`.
const WM_MOUSELEAVE: u32 = 0x02A3;

thread_local! { static CURRENT: Cell<isize> = const { Cell::new(0) }; }

/// Upload attempts whose thread is still running. Several can overlap when a
/// new capture replaces a toast that is still uploading.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// True while an upload is on the wire; quitting or installing an update then
/// would cut it off and leave nothing but a half-stored image.
pub fn upload_in_flight() -> bool {
    IN_FLIGHT.load(Ordering::Acquire) > 0
}

/// Counts one running upload attempt until dropped, then wakes the main loop
/// so that a quit deferred for the upload can finish.
struct InFlight;

impl InFlight {
    fn begin() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
        crate::tray::notify_tray_wakeup();
    }
}

/// The outcome of one upload attempt, tagged with its attempt number.
type AttemptResult = (u64, Result<String, String>);

enum Phase {
    /// `permille` is the share of the file already sent, once the first
    /// chunk is out; 1000 means the response is awaited.
    Uploading {
        permille: Option<u16>,
    },
    Done {
        url: String,
        copied: bool,
    },
    Failed(String),
}

/// A fade (and, when entering, a slide) in progress.
#[derive(Clone, Copy)]
struct Motion {
    started: Instant,
    leaving: bool,
    /// Opacity the fade-out starts from.
    from: u8,
}

impl Motion {
    fn elapsed_ms(&self) -> u32 {
        u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX)
    }
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
    /// Left and bottom edge the toast rests at, in screen pixels. The bottom
    /// stays put when the card grows for a failure.
    left: i32,
    bottom: i32,
    /// Current height in device pixels.
    height: i32,
    /// Created layered, so it fades and slides; false when the user turned
    /// client-area animation off.
    animated: bool,
    motion: Option<Motion>,
    /// The Copy button reads "Copied" until `TIMER_COPIED` fires.
    copied_flash: bool,
}

impl State {
    /// A fade-out is running; the card no longer reacts to the pointer.
    fn closing(&self) -> bool {
        self.motion.is_some_and(|motion| motion.leaving)
    }

    /// The progress bar is the moving segment: before the first chunk is out
    /// and while the response is awaited. Only then do frames need repaints.
    fn indeterminate(&self) -> bool {
        matches!(
            self.phase,
            Phase::Uploading { permille } if permille.is_none_or(|sent| sent >= 1000)
        )
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// Ease-out cubic: share of an animation done `elapsed_ms` into `duration_ms`.
fn eased(elapsed_ms: u32, duration_ms: u32) -> f32 {
    let progress = (elapsed_ms as f32 / duration_ms as f32).clamp(0.0, 1.0);
    1.0 - (1.0 - progress).powi(3)
}

/// Opacity, 0..=255, of a fade-in `elapsed_ms` after it started.
fn fade_in_alpha(elapsed_ms: u32) -> u8 {
    (255.0 * eased(elapsed_ms, FADE_IN_MS)).round() as u8
}

/// Opacity, 0..=255, of a fade-out that started at `from`.
fn fade_out_alpha(from: u8, elapsed_ms: u32) -> u8 {
    let remaining = 1.0 - (elapsed_ms as f32 / FADE_OUT_MS as f32).clamp(0.0, 1.0);
    (from as f32 * remaining).round() as u8
}

/// Pixels a fade-in still has to rise `elapsed_ms` after it started.
fn slide_offset(elapsed_ms: u32, distance: i32) -> i32 {
    (distance as f32 * (1.0 - eased(elapsed_ms, FADE_IN_MS))).round() as i32
}

/// Per-mille of `total` that `sent` covers; an empty body counts as done.
fn permille(sent: u64, total: u64) -> u16 {
    if total == 0 {
        return 1000;
    }
    (sent.min(total) * 1000 / total) as u16
}

/// Whether the user allows animation inside windows (Settings > Accessibility).
fn animations_enabled() -> bool {
    use windows::Win32::Foundation::BOOL;
    let mut enabled = BOOL(1);
    let queried = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(std::ptr::from_mut(&mut enabled).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    queried.is_err() || enabled.as_bool()
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
    let animated = animations_enabled();
    let state = Box::new(State {
        prepared,
        phase: Phase::Uploading { permille: None },
        attempt: 0,
        result: Arc::new(Mutex::new(None)),
        frame: 0,
        dpi: 96,
        hot: None,
        pressed: None,
        hovering: false,
        left: 0,
        bottom: 0,
        height: HEIGHT,
        animated,
        motion: None,
        copied_flash: false,
    });
    let mut ex_style = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
    if animated {
        ex_style |= WS_EX_LAYERED;
    }
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
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
    let margin = scale(12, state.dpi);
    state.height = scale(HEIGHT, state.dpi);
    state.left = work.right - scale(WIDTH, state.dpi) - margin;
    state.bottom = work.bottom - margin;
    apply_frame_theme(hwnd);
    unsafe {
        // Rounded corners need Windows 11; Windows 10 rejects the attribute
        // and keeps its square frame.
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            std::ptr::from_ref(&DWMWCP_ROUND).cast(),
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
    }
    if animated {
        set_alpha(hwnd, 0);
        state.motion = Some(Motion {
            started: Instant::now(),
            leaving: false,
            from: 0,
        });
    }
    place(hwnd, state, SWP_SHOWWINDOW);
    start_attempt(hwnd, state);
    Ok(())
}

fn set_alpha(hwnd: HWND, alpha: u8) {
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}

/// Moves and sizes the toast to its resting place, raised by whatever is left
/// of the entrance slide.
fn place(hwnd: HWND, state: &State, flags: SET_WINDOW_POS_FLAGS) {
    let lift = state
        .motion
        .filter(|motion| !motion.leaving)
        .map_or(0, |motion| {
            slide_offset(motion.elapsed_ms(), scale(SLIDE_PX, state.dpi))
        });
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            state.left,
            state.bottom - state.height + lift,
            scale(WIDTH, state.dpi),
            state.height,
            SWP_NOACTIVATE | flags,
        );
    }
}

/// Matches the DWM frame (border and shadow tint) to the current app theme.
fn apply_frame_theme(hwnd: HWND) {
    let dark = (crate::theme::theme() == crate::theme::Theme::Dark) as u32;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            std::ptr::from_ref(&dark).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

fn start_attempt(hwnd: HWND, state: &mut State) {
    state.attempt += 1;
    state.phase = Phase::Uploading { permille: None };
    state.frame = 0;
    unsafe {
        let _ = KillTimer(hwnd, TIMER_CLOSE);
        SetTimer(hwnd, TIMER_ANIMATE, 30, None);
    }
    // Back from a failure's taller card; the bottom edge stays put.
    let height = scale(HEIGHT, state.dpi);
    if state.height != height {
        state.height = height;
        place(hwnd, state, SET_WINDOW_POS_FLAGS(0));
    }
    let attempt = state.attempt;
    let prepared = state.prepared.clone();
    let output = state.result.clone();
    let window = hwnd.0 as usize;
    let in_flight = InFlight::begin();
    let spawned = std::thread::Builder::new()
        .name("isolmass-upload".into())
        .spawn(move || {
            let report = move |sent: u64, total: u64| unsafe {
                let _ = PostMessageW(
                    HWND(window as *mut _),
                    WM_UPLOAD_PROGRESS,
                    WPARAM(permille(sent, total) as usize),
                    LPARAM(attempt as isize),
                );
            };
            let outcome = crate::upload::send(&prepared, Some(&report));
            if let Ok(mut slot) = output.lock() {
                *slot = Some((attempt, outcome));
            }
            // The toast may already be closed; then there is nobody to tell.
            unsafe {
                let _ = PostMessageW(HWND(window as *mut _), WM_UPLOAD_DONE, WPARAM(0), LPARAM(0));
            }
            // Release this copy of the file before announcing that nothing is
            // in flight, so a quit deferred for the upload cannot outrun the
            // cleanup when the toast is already gone.
            drop(prepared);
            drop(in_flight);
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
    // The entrance animation still needs the timer.
    if state.motion.is_none() {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_ANIMATE);
        }
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
    state.height = match &state.phase {
        Phase::Failed(error) => failed_height(hwnd, state.dpi, error),
        _ => scale(HEIGHT, state.dpi),
    };
    arm_close(hwnd, state);
    // Another topmost window may have been activated meanwhile; the result
    // must be visible without taking focus.
    place(hwnd, state, SWP_SHOWWINDOW);
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// Height of the failed card. The detail gets the lines its message needs,
/// two to four, and the card grows by the lines beyond two.
fn failed_height(hwnd: HWND, dpi: u32, detail: &str) -> i32 {
    use windows::Win32::Graphics::Gdi::{
        DRAW_TEXT_FORMAT, DT_CALCRECT, DT_EDITCONTROL, DT_NOPREFIX, DT_SINGLELINE, DT_WORDBREAK,
        DrawTextW, GetDC, ReleaseDC,
    };
    let base = scale(HEIGHT_FAILED, dpi);
    let mut client = RECT::default();
    let hdc = unsafe {
        let _ = GetClientRect(hwnd, &mut client);
        GetDC(hwnd)
    };
    if hdc.is_invalid() {
        return base;
    }
    let width = client.right - scale(48, dpi) - scale(16, dpi);
    let measure = |text: &str, format: DRAW_TEXT_FORMAT| {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: 0,
        };
        unsafe {
            DrawTextW(
                hdc,
                &mut wide,
                &mut rect,
                format | DT_CALCRECT | DT_NOPREFIX,
            );
        }
        rect.bottom
    };
    let (line, wrapped) =
        crate::drawing::with_font(hdc, -scale(crate::theme::FONT_BODY_PX, dpi), 400, || {
            (
                measure("Ag", DT_SINGLELINE),
                measure(detail, DT_WORDBREAK | DT_EDITCONTROL),
            )
        });
    unsafe {
        ReleaseDC(hwnd, hdc);
    }
    if line <= 0 {
        return base;
    }
    let lines = ((wrapped + line / 2) / line).clamp(2, 4);
    base + (lines - 2) * line
}

/// Fades the toast out and destroys it afterwards; at once when it is not
/// animated. Callers must not touch `state` after a direct destroy.
fn close(hwnd: HWND, state: &mut State) {
    if !state.animated {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    if state.closing() {
        return;
    }
    let from = match state.motion {
        Some(motion) => fade_in_alpha(motion.elapsed_ms()),
        None => 255,
    };
    state.motion = Some(Motion {
        started: Instant::now(),
        leaving: true,
        from,
    });
    unsafe {
        let _ = KillTimer(hwnd, TIMER_CLOSE);
        SetTimer(hwnd, TIMER_ANIMATE, 30, None);
    }
    place(hwnd, state, SET_WINDOW_POS_FLAGS(0));
}

/// Steps the fade and slide. Destroys the window when a fade-out is over, so
/// the caller must not touch `state` afterwards.
fn advance_motion(hwnd: HWND, state: &mut State) {
    let Some(motion) = state.motion else {
        return;
    };
    let elapsed = motion.elapsed_ms();
    if motion.leaving {
        if elapsed >= FADE_OUT_MS {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        } else {
            set_alpha(hwnd, fade_out_alpha(motion.from, elapsed));
        }
        return;
    }
    if elapsed >= FADE_IN_MS {
        state.motion = None;
        set_alpha(hwnd, 255);
        if !state.indeterminate() {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_ANIMATE);
            }
        }
    } else {
        set_alpha(hwnd, fade_in_alpha(elapsed));
    }
    place(hwnd, state, SET_WINDOW_POS_FLAGS(0));
}

fn arm_close(hwnd: HWND, state: &State) {
    let delay = match state.phase {
        Phase::Uploading { .. } => return,
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
        Phase::Uploading { .. } => {}
        Phase::Done { .. } => {
            let open = row(right, 72);
            let copy = row(open.left - scale(8, dpi), 96);
            let copy_label = if state.copied_flash {
                crate::i18n::t("Copied")
            } else {
                crate::i18n::t("Copy")
            };
            list.push((Action::Open, crate::i18n::t("Open"), open));
            list.push((Action::Copy, copy_label, copy));
        }
        Phase::Failed(_) => {
            list.push((Action::Retry, crate::i18n::t("Try again"), row(right, 112)));
        }
    }
    list
}

fn hit(state: &State, hwnd: HWND, point: POINT) -> Option<Action> {
    if state.closing() {
        return None;
    }
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
        Action::Close => close(hwnd, state),
        Action::Copy => {
            if let Phase::Done { url, copied } = &mut state.phase {
                *copied = crate::clipboard::copy_text_to_clipboard(Some(hwnd), url).is_ok();
                state.copied_flash = *copied;
            }
            if state.copied_flash {
                unsafe {
                    SetTimer(hwnd, TIMER_COPIED, COPIED_MS, None);
                }
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
                close(hwnd, state);
            }
        }
        Action::Retry => start_attempt(hwnd, state),
    }
}

fn paint(hwnd: HWND, state: &State, hdc: HDC) {
    use crate::drawing::{Look, corner_diameter, fluent_button, icon, rounded, with_font};
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
        Phase::Uploading { permille } => (
            0xe898u16,
            tokens.accent_ink,
            match permille {
                None => crate::i18n::t("Uploading…").to_string(),
                Some(1000..) => crate::i18n::t("Finishing…").to_string(),
                Some(sent) => crate::i18n::tf("Uploading… {}%", &[&(sent / 10)]),
            },
            crate::i18n::t("Sending to the Worker in your Cloudflare account").to_string(),
        ),
        Phase::Done { url, copied } => (
            0xe73e,
            tokens.accent_ink,
            if *copied {
                crate::i18n::t("Uploaded · link copied").to_string()
            } else {
                crate::i18n::t("Uploaded").to_string()
            },
            url.trim_start_matches("https://").to_string(),
        ),
        Phase::Failed(error) => (
            0xe783,
            tokens.critical,
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
    let failed = matches!(state.phase, Phase::Failed(_));
    let mut detail_wide: Vec<u16> = detail.encode_utf16().collect();
    let mut detail_rect = RECT {
        left: scale(48, dpi),
        top: scale(36, dpi),
        right: client.right - scale(16, dpi),
        // A failure's message wraps over the lines the taller card makes room for.
        bottom: if failed {
            client.bottom - scale(56, dpi)
        } else {
            scale(58, dpi)
        },
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
                let format = if failed {
                    DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS | DT_NOPREFIX
                } else {
                    DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS
                };
                let _ = DrawTextW(hdc, &mut detail_wide, &mut detail_rect, format);
            }
        },
    );
    if let Phase::Uploading { permille } = state.phase {
        let left = scale(48, dpi);
        let right = client.right - scale(16, dpi);
        let top = client.bottom - scale(30, dpi);
        let bottom = top + scale(3, dpi).max(2);
        let corner = corner_diameter(crate::theme::RADIUS_CONTROL, dpi);
        rounded(
            hdc,
            crate::capture::Rect::new(left, top, right, bottom),
            corner,
            tokens.stroke,
            tokens.stroke,
        );
        let span = right - left;
        let (start, end) = match permille.filter(|&sent| sent < 1000) {
            // Determinate while the file is written.
            Some(sent) => (left, left + span * i32::from(sent) / 1000),
            // Indeterminate, as in Windows 11 transfer dialogs, until the
            // first chunk is out and while the response is awaited.
            None => {
                let segment = (span / 3).max(1);
                let position = (state.frame as i32 * span / 60) % (span + segment) - segment;
                (
                    (left + position).max(left),
                    (left + position + segment).min(right),
                )
            }
        };
        if end > start {
            rounded(
                hdc,
                crate::capture::Rect::new(start, top, end, bottom),
                corner,
                tokens.accent_ink,
                tokens.accent_ink,
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
                let fill = if look.pressed {
                    tokens.subtle_pressed
                } else {
                    tokens.subtle_hover
                };
                rounded(
                    hdc,
                    crate::capture::Rect::new(rect.left, rect.top, rect.right, rect.bottom),
                    corner_diameter(crate::theme::RADIUS_CONTROL, dpi),
                    fill,
                    fill,
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
            // Fading and sliding are composited; only the moving bar repaints.
            let repaint = state.indeterminate();
            state.frame = state.frame.wrapping_add(1);
            // May destroy the window at the end of a fade-out.
            advance_motion(hwnd, state);
            if repaint {
                unsafe {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_CLOSE => {
            close(hwnd, state);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_COPIED => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_COPIED);
                let _ = InvalidateRect(hwnd, None, false);
            }
            state.copied_flash = false;
            LRESULT(0)
        }
        WM_UPLOAD_PROGRESS => {
            if lparam.0 as u64 == state.attempt && matches!(state.phase, Phase::Uploading { .. }) {
                let was_moving = state.indeterminate();
                let sent = wparam.0.min(1000) as u16;
                if let Phase::Uploading { permille } = &mut state.phase
                    && permille.is_none_or(|current| sent > current)
                {
                    *permille = Some(sent);
                    unsafe {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                // The timer only runs while the bar moves or a fade is going.
                let is_moving = state.indeterminate();
                if was_moving != is_moving {
                    unsafe {
                        if is_moving {
                            SetTimer(hwnd, TIMER_ANIMATE, 30, None);
                        } else if state.motion.is_none() {
                            let _ = KillTimer(hwnd, TIMER_ANIMATE);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_UPLOAD_DONE => {
            let outcome = state.result.lock().ok().and_then(|mut slot| slot.take());
            if let Some((attempt, outcome)) = outcome
                && attempt == state.attempt
                && matches!(state.phase, Phase::Uploading { .. })
                && !state.closing()
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
        WM_SETTINGCHANGE => {
            crate::theme::invalidate_theme_cache();
            apply_frame_theme(hwnd);
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_ANIMATE);
                let _ = KillTimer(hwnd, TIMER_CLOSE);
                let _ = KillTimer(hwnd, TIMER_COPIED);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permille_covers_the_body_and_clamps() {
        assert_eq!(permille(0, 100), 0);
        assert_eq!(permille(50, 100), 500);
        assert_eq!(permille(1, 3), 333);
        assert_eq!(permille(100, 100), 1000);
        assert_eq!(permille(150, 100), 1000);
        assert_eq!(permille(5, 0), 1000);
    }

    #[test]
    fn entrance_fades_in_and_settles_with_an_ease_out() {
        assert_eq!(fade_in_alpha(0), 0);
        assert_eq!(fade_in_alpha(FADE_IN_MS), 255);
        assert_eq!(fade_in_alpha(FADE_IN_MS * 4), 255);
        assert_eq!(slide_offset(0, 8), 8);
        assert_eq!(slide_offset(FADE_IN_MS, 8), 0);
        // Ease-out: most of the way is covered in the first half.
        assert!(fade_in_alpha(FADE_IN_MS / 2) > 200);
        assert_eq!(slide_offset(FADE_IN_MS / 2, 8), 1);
        let steps: Vec<u8> = (0..=FADE_IN_MS).step_by(30).map(fade_in_alpha).collect();
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]), "{steps:?}");
    }

    #[test]
    fn exit_fades_linearly_from_where_it_started() {
        assert_eq!(fade_out_alpha(255, 0), 255);
        assert_eq!(fade_out_alpha(100, FADE_OUT_MS / 2), 50);
        assert_eq!(fade_out_alpha(255, FADE_OUT_MS), 0);
        assert_eq!(fade_out_alpha(255, FADE_OUT_MS * 3), 0);
    }

    #[test]
    fn overlapping_uploads_keep_the_flag_until_the_last_one_ends() {
        assert!(!upload_in_flight());
        let first = InFlight::begin();
        let second = InFlight::begin();
        assert!(upload_in_flight());
        drop(first);
        assert!(upload_in_flight());
        drop(second);
        assert!(!upload_in_flight());
    }
}
