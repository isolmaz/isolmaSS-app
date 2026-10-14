//! The pin windows: creating them, painting the image, and the mouse and key handling that
//! moves, zooms, copies and closes them. Window glue that only a desktop can exercise; where
//! a pin is, how it zooms, which keys it answers to and what its menu holds is decided and
//! tested in the parent module.
use super::{
    PinCommand, PinImage, PinView, command_for_id, command_for_key, menu_labels, wheel_steps,
};
use crate::capture::Rect;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, COLORONCOLOR, CreateSolidBrush,
    DIB_RGB_COLORS, DeleteObject, EndPaint, FillRect, HALFTONE, HGDIOBJ, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MonitorFromPoint, PAINTSTRUCT, RGBQUAD, SRCCOPY, SetStretchBltMode,
    StretchDIBits,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const CLASS: PCWSTR = w!("isolmaSS_Pin");

struct State {
    image: PinImage,
    view: PinView,
    wheel_remainder: i32,
    /// Offset from the pointer to the image's corner while the pin is being dragged.
    grab: Option<(i32, i32)>,
}

thread_local! { static PINS: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) }; }

/// Destroys a popup menu when dropped.
struct Menu(HMENU);

impl Drop for Menu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

/// Shows `image` as a pin over the place it was selected. The pin lives on this thread's
/// message loop until the user closes it or the app quits.
pub fn show(image: PinImage) -> Result<(), String> {
    register().map_err(|error| error.to_string())?;
    let selection = Rect::new(
        image.origin.0,
        image.origin.1,
        image.origin.0 + image.width,
        image.origin.1 + image.height,
    );
    let middle = POINT {
        x: (selection.left + selection.right) / 2,
        y: (selection.top + selection.bottom) / 2,
    };
    let view = PinView::over(selection, dpi_at(middle));
    let frame = view.window_rect();
    let state = Box::new(State {
        image,
        view,
        wheel_remainder: 0,
        grab: None,
    });
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("isolmaSS pin"),
            WS_POPUP,
            frame.left,
            frame.top,
            frame.width(),
            frame.height(),
            None,
            None,
            HINSTANCE::default(),
            None,
        )
    }
    .map_err(|error| crate::i18n::tf("Could not open the pin window: {}", &[&error]))?;
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    }
    PINS.with(|pins| pins.borrow_mut().push(hwnd.0 as isize));
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

/// Closes every pin and frees their images; the app is quitting.
pub fn close_all() {
    let open: Vec<isize> = PINS.with(|pins| pins.borrow().clone());
    for hwnd in open {
        unsafe {
            let _ = DestroyWindow(HWND(hwnd as *mut _));
        }
    }
}

/// Pumps messages until every pin has closed. Used by one-shot command-line captures,
/// whose process would otherwise end and take the pins with it.
pub fn wait_until_closed() {
    let mut msg = MSG::default();
    while PINS.with(|pins| !pins.borrow().is_empty()) {
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Effective DPI of the display that holds `point`.
fn dpi_at(point: POINT) -> u32 {
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let (mut x, mut y) = (96, 96);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_ok() {
        x.max(96)
    } else {
        96
    }
}

fn cursor_position() -> (i32, i32) {
    let mut point = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut point);
    }
    (point.x, point.y)
}

/// The signed x and y packed into a mouse message's `lparam`.
fn point_from(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xffff) as i16 as i32,
        ((lparam.0 >> 16) & 0xffff) as i16 as i32,
    )
}

/// Moves and sizes the window to the pin's view.
fn place(hwnd: HWND, view: &PinView) {
    let frame = view.window_rect();
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            frame.left,
            frame.top,
            frame.width(),
            frame.height(),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn state_of<'a>(hwnd: HWND) -> Option<&'a mut State> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    unsafe { pointer.as_mut() }
}

fn paint(hwnd: HWND, state: &State) {
    let mut area = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut area) };
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let image = state.view.image_rect();
    // The border is what is left of the window around the image: four strips.
    let amber = unsafe { CreateSolidBrush(crate::theme::BRAND_AMBER) };
    for strip in [
        RECT {
            left: 0,
            top: 0,
            right: client.right,
            bottom: image.top,
        },
        RECT {
            left: 0,
            top: image.bottom,
            right: client.right,
            bottom: client.bottom,
        },
        RECT {
            left: 0,
            top: image.top,
            right: image.left,
            bottom: image.bottom,
        },
        RECT {
            left: image.right,
            top: image.top,
            right: client.right,
            bottom: image.bottom,
        },
    ] {
        unsafe {
            FillRect(hdc, &strip, amber);
        }
    }
    unsafe {
        let _ = DeleteObject(HGDIOBJ(amber.0));
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: state.image.width,
            biHeight: -state.image.height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        bmiColors: [RGBQUAD::default()],
    };
    // Shrinking blends neighbouring pixels; magnifying keeps every pixel crisp.
    let mode = if image.width() < state.image.width {
        HALFTONE
    } else {
        COLORONCOLOR
    };
    unsafe {
        SetStretchBltMode(hdc, mode);
        StretchDIBits(
            hdc,
            image.left,
            image.top,
            image.width(),
            image.height(),
            0,
            0,
            state.image.width,
            state.image.height,
            Some(state.image.pixels.as_ptr().cast()),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        let _ = EndPaint(hwnd, &area);
    }
}

/// Opens the right-click menu at the screen position `at`; the command picked, if any.
fn pick_command(hwnd: HWND, at: (i32, i32)) -> Option<PinCommand> {
    let menu = Menu(unsafe { CreatePopupMenu() }.ok()?);
    for (index, label) in menu_labels().iter().enumerate() {
        let text: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
        unsafe { AppendMenuW(menu.0, MF_STRING, index + 1, PCWSTR(text.as_ptr())) }.ok()?;
    }
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
    let chosen = unsafe {
        TrackPopupMenuEx(
            menu.0,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            at.0,
            at.1,
            hwnd,
            None,
        )
    };
    // A popup menu needs one message afterwards to let go of the foreground.
    unsafe {
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    }
    command_for_id(chosen.0)
}

fn run(hwnd: HWND, command: PinCommand) {
    match command {
        PinCommand::Copy => copy(hwnd),
        PinCommand::SaveAs => save_as(hwnd),
        PinCommand::Close => unsafe {
            let _ = DestroyWindow(hwnd);
        },
    }
}

fn copy(hwnd: HWND) {
    let Some(state) = state_of(hwnd) else {
        return;
    };
    let image = &state.image;
    let whole = Rect::new(0, 0, image.width, image.height);
    let copied = crate::clipboard::flatten_selection_to_dib(
        &image.pixels,
        image.width,
        image.height,
        &whole,
    )
    .map_err(|error| crate::i18n::tf("Could not prepare the clipboard image: {}", &[&error]))
    .and_then(|dib| {
        crate::clipboard::copy_dib_to_clipboard(Some(hwnd), &dib)
            .map_err(|error| crate::i18n::tf("Windows rejected the clipboard image: {}", &[&error]))
    });
    if let Err(error) = copied {
        crate::ui::error(hwnd, crate::i18n::t("Copy failed"), &error);
    }
}

fn save_as(hwnd: HWND) {
    let Some(format) = state_of(hwnd).map(|state| state.image.format) else {
        return;
    };
    let path = match crate::save::choose_output_path(hwnd, format) {
        Ok(Some(path)) => path,
        Ok(None) => return,
        Err(error) => {
            crate::ui::error(
                hwnd,
                crate::i18n::t("Could not open the save dialog"),
                &error.to_string(),
            );
            return;
        }
    };
    // The dialog runs a message loop; the pin may be gone by now.
    let Some(state) = state_of(hwnd) else {
        return;
    };
    let image = &state.image;
    let saved = crate::save::save_buffer_to_image(
        &image.pixels,
        image.width,
        image.height,
        &Rect::new(0, 0, image.width, image.height),
        &path,
        image.format,
        image.jpeg_quality,
    );
    match saved {
        Ok(path) => crate::save::recent::saved(&path),
        Err(error) => crate::ui::error(hwnd, crate::i18n::t("Save failed"), &error),
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(state) = state_of(hwnd) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd, state);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            state.grab = Some(state.view.grab_offset(cursor_position()));
            unsafe {
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(offset) = state.grab {
                state.view.move_with(cursor_position(), offset);
                place(hwnd, &state.view);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            state.grab = None;
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam.0 >> 16) as i16 as i32;
            let steps = wheel_steps(&mut state.wheel_remainder, delta);
            if steps != 0 && state.view.zoom_by(steps, point_from(lparam)) {
                place(hwnd, &state.view);
                unsafe {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            state.view.set_dpi((wparam.0 & 0xffff) as u32);
            place(hwnd, &state.view);
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            match command_for_key(wparam.0 as u32, ctrl) {
                Some(command) => {
                    run(hwnd, command);
                    LRESULT(0)
                }
                None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }
        WM_CONTEXTMENU => {
            // A key press (Menu key, Shift+F10) reports no position; use the corner.
            let at = match point_from(lparam) {
                (-1, -1) => {
                    let frame = state.view.window_rect();
                    (frame.left, frame.top)
                }
                point => point,
            };
            if let Some(command) = pick_command(hwnd, at) {
                run(hwnd, command);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let pointer = state as *mut State;
            PINS.with(|pins| pins.borrow_mut().retain(|open| *open != hwnd.0 as isize));
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                // The image and its pixels go with the window.
                drop(Box::from_raw(pointer));
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
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
        style: CS_DBLCLKS | CS_DROPSHADOW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE::default(),
        hCursor: unsafe { LoadCursorW(HINSTANCE::default(), IDC_SIZEALL).unwrap_or_default() },
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
