use crate::capture::Rect;
use std::ffi::c_void;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetLayeredWindowAttributes, GetWindowLongW,
    GetWindowRect, GetWindowTextLengthW, IsIconic, IsWindowVisible,
    LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

/// Information about an enumerated top-level window.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: HWND,
    pub class_name: String,
    /// True visible bounds in screen coordinates (via DWMWA_EXTENDED_FRAME_BOUNDS).
    pub bounds: Rect,
}

/// Enumerates all visible, uncloaked top-level application windows in top-to-bottom Z-order.
pub fn get_visible_windows(exclude_hwnd: Option<HWND>) -> Vec<WindowInfo> {
    struct EnumContext {
        exclude: Option<HWND>,
        windows: Vec<WindowInfo>,
    }

    let mut context = EnumContext {
        exclude: exclude_hwnd,
        windows: Vec::new(),
    };

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut EnumContext) };

        if ctx.exclude == Some(hwnd) {
            return BOOL(1);
        }

        // 1. Must be visible
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return BOOL(1);
        }

        // 2. Must not be minimized (iconic)
        if unsafe { IsIconic(hwnd) }.as_bool() {
            return BOOL(1);
        }

        // 3. Must not be cloaked by DWM (e.g. background UWP apps or other virtual desktops)
        let mut cloaked: u32 = 0;
        let dwm_res = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut _ as *mut c_void,
                std::mem::size_of::<u32>() as u32,
            )
        };
        if dwm_res.is_ok() && cloaked != 0 {
            return BOOL(1);
        }

        // 4. Exclude click-through overlays, invisible layered windows and
        // tool windows without titles
        let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
        let constant_alpha = || {
            let mut alpha = 0u8;
            let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
            let queried = unsafe {
                GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags))
            };
            (queried.is_ok() && flags.0 & LWA_ALPHA.0 != 0).then_some(alpha)
        };
        if is_invisible_to_input(ex_style, constant_alpha) {
            return BOOL(1);
        }
        let is_tool_window = (ex_style & WS_EX_TOOLWINDOW.0) != 0;

        // Only query titles for tool windows, and never copy user document titles.
        if is_tool_window && unsafe { GetWindowTextLengthW(hwnd) } <= 0 {
            return BOOL(1);
        }

        // 5. Get window class name
        let mut class_buf = [0u16; 256];
        let class_len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
        let class_name = if class_len > 0 {
            String::from_utf16_lossy(&class_buf[..class_len as usize])
        } else {
            String::new()
        };

        // 6. Get true visible bounds via DWMWA_EXTENDED_FRAME_BOUNDS
        let mut frame_rect = RECT::default();
        let bounds_res = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut frame_rect as *mut _ as *mut c_void,
                std::mem::size_of::<RECT>() as u32,
            )
        };

        let rect = if bounds_res.is_ok() {
            Rect::new(
                frame_rect.left,
                frame_rect.top,
                frame_rect.right,
                frame_rect.bottom,
            )
        } else {
            let mut win_rect = RECT::default();
            let _ = unsafe { GetWindowRect(hwnd, &mut win_rect) };
            Rect::new(win_rect.left, win_rect.top, win_rect.right, win_rect.bottom)
        };

        // Ignore zero-area or inverted windows
        if rect.is_empty() {
            return BOOL(1);
        }

        ctx.windows.push(WindowInfo {
            hwnd,
            class_name,
            bounds: rect,
        });

        BOOL(1)
    }

    if let Err(error) = unsafe {
        EnumWindows(
            Some(enum_proc),
            LPARAM(&mut context as *mut EnumContext as isize),
        )
    } {
        crate::diagnostics::record("window enumeration", &error.to_string());
    }

    context.windows
}

/// True for windows that cannot be a snap target: click-through windows, and
/// layered windows with a constant alpha of 0 (vendor in-game overlays span
/// the whole monitor but show nothing). `constant_alpha` runs only for
/// layered windows and yields `None` when no `LWA_ALPHA` value is set.
fn is_invisible_to_input(ex_style: u32, constant_alpha: impl FnOnce() -> Option<u8>) -> bool {
    if ex_style & WS_EX_TRANSPARENT.0 != 0 {
        return true;
    }
    ex_style & WS_EX_LAYERED.0 != 0 && constant_alpha() == Some(0)
}

pub fn find_window_in_list(
    windows: &[WindowInfo],
    screen_point: (i32, i32),
) -> Option<&WindowInfo> {
    windows
        .iter()
        .find(|window| window.bounds.contains(screen_point.0, screen_point.1))
}

/// Finds the topmost visible window under the given screen coordinate `(x, y)`.
pub fn find_window_at_point(
    screen_point: (i32, i32),
    exclude_hwnd: Option<HWND>,
) -> Option<WindowInfo> {
    let windows = get_visible_windows(exclude_hwnd);
    find_window_in_list(&windows, screen_point).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_through_windows_are_skipped() {
        let style = WS_EX_TRANSPARENT.0;
        assert!(is_invisible_to_input(style, || unreachable!()));
        assert!(is_invisible_to_input(style | WS_EX_LAYERED.0, || Some(255)));
    }

    #[test]
    fn layered_windows_are_skipped_only_at_zero_alpha() {
        let style = WS_EX_LAYERED.0;
        assert!(is_invisible_to_input(style, || Some(0)));
        assert!(!is_invisible_to_input(style, || Some(1)));
        assert!(!is_invisible_to_input(style, || Some(255)));
        // Alpha not set via LWA_ALPHA (colour key or per-pixel alpha).
        assert!(!is_invisible_to_input(style, || None));
    }

    #[test]
    fn ordinary_windows_never_query_alpha() {
        assert!(!is_invisible_to_input(0, || unreachable!()));
        assert!(!is_invisible_to_input(
            WS_EX_TOOLWINDOW.0,
            || unreachable!()
        ));
    }
}
