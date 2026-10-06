use super::*;
use std::time::Instant;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::UpdateWindow;

fn register_overlay_class() -> Result<()> {
    static REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REGISTERED.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(());
    }

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW
            | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW
            | CS_DBLCLKS,
        lpfnWndProc: Some(overlay_wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: HINSTANCE::default(),
        hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
        hCursor: unsafe { LoadCursorW(HINSTANCE::default(), IDC_CROSS).unwrap_or_default() },
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
        lpszMenuName: windows::core::PCWSTR::null(),
        lpszClassName: OVERLAY_CLASS_NAME,
        hIconSm: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
    };

    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
        return Err(windows::core::Error::from_win32());
    }
    REGISTERED.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

pub fn show_overlay_session(
    capture: Rc<CaptureBuffer>,
    triggered: Instant,
) -> Result<Option<Rect>> {
    run_session(capture, triggered, false).map(|(selection, _)| selection)
}

/// Diagnostic calibration uses the same capture/window/presentation path, then closes its own window.
pub fn measure_overlay(
    capture: Rc<CaptureBuffer>,
    triggered: Instant,
) -> Result<std::time::Duration> {
    run_session(capture, triggered, true).map(|(_, duration)| duration)
}

fn run_session(
    capture: Rc<CaptureBuffer>,
    triggered: Instant,
    measure: bool,
) -> Result<(Option<Rect>, std::time::Duration)> {
    let overlay_start = Instant::now();
    register_overlay_class()?;

    let width = capture.width;
    let height = capture.height;

    let screen_dc = unsafe { GetDC(HWND::default()) };
    if screen_dc.is_invalid() {
        return Err(windows::core::Error::from_win32());
    }
    let screen_guard = ScreenDcGuard(screen_dc);

    let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
    if mem_dc.is_invalid() {
        return Err(windows::core::Error::from_win32());
    }
    let mem_guard = DeleteDcGuard(mem_dc);

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
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
    let dib = unsafe { CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)? };
    // Own the bitmap immediately so every later error path frees it.
    let bitmap_guard = DeleteBitmapGuard(dib);
    if dib.is_invalid() || bits_ptr.is_null() {
        return Err(windows::core::Error::from_win32());
    }
    drop(screen_guard);

    let old_bmp = unsafe { SelectObject(mem_dc, HGDIOBJ(dib.0)) };

    let buffer_bytes = (width as usize) * (height as usize) * 4;
    unsafe {
        std::ptr::copy_nonoverlapping(capture.dimmed.as_ptr(), bits_ptr as *mut u8, buffer_bytes);
    }

    let settings = Settings::load().map_err(|error| {
        let hresult = match error.raw_os_error() {
            Some(code) => windows::core::HRESULT::from_win32(code as u32),
            None => windows::core::HRESULT(-1),
        };
        windows::core::Error::new(
            hresult,
            format!("could not load settings for the capture overlay: {error}"),
        )
    })?;
    crate::i18n::set_preference(settings.language);
    let active_color = settings.default_color;
    let active_thickness = settings.default_thickness;
    let active_tool = settings.last_tool;

    let mut state = Box::new(OverlayState {
        capture,
        mode: OverlayMode::Hovering,
        drag_start: None,
        committed_selection: None,
        hover_snap_rect: None,
        visible_windows: Vec::new(),
        dpi: 96,

        settings,
        active_tool,
        active_color,
        active_thickness,
        thickness_input: None,
        thickness_drag: None,
        thickness_wheel_remainder: 0,
        thickness_drag_original: None,
        objects: Vec::new(),
        selected_id: None,
        next_id: 1,
        history: HistoryManager::default(),

        drawing_shape: None,
        dragging_object: None,
        dragging_selection: None,
        text_edit: None,
        toolbar: None,
        tools_expanded: false,

        mem_dc,
        dib,
        old_bmp,
        bits_ptr: bits_ptr as *mut u8,

        committed_result: false,
        setup_pending: false,
        scene_dirty: false,
        base_cache: Vec::new(),
        cache_requested: false,
        pointer: (0, 0),
        preferences_dirty: false,
    });
    std::mem::forget(mem_guard);
    std::mem::forget(bitmap_guard);

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            OVERLAY_CLASS_NAME,
            w!("isolmaSS Overlay"),
            WS_POPUP,
            state.capture.x,
            state.capture.y,
            width,
            height,
            None,
            None,
            HINSTANCE::default(),
            None,
        )?
    };

    if hwnd.is_invalid() {
        return Err(windows::core::Error::from_win32());
    }
    let _window = crate::ui::OwnedWindow(hwnd);
    state.dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }.max(96);

    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            state.as_mut() as *mut OverlayState as isize,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        let _ = UpdateWindow(hwnd);
        let _ = DwmFlush();
    }
    if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool() } {
        return Err(windows::core::Error::new(
            windows::core::HRESULT::from_win32(1400),
            crate::i18n::t("The capture overlay could not be made visible."),
        ));
    }
    let overlay_time = overlay_start.elapsed();
    let visible = triggered.elapsed();
    if measure {
        return Ok((None, visible));
    }
    crate::diagnostics::record(
        "capture",
        &format!(
            "event_to_visible_us={} capture_us={} setup_us={} bitblt_us={} dim_us={} overlay_us={}",
            triggered.elapsed().as_micros(),
            state.capture.timings.total.as_micros(),
            state.capture.timings.setup.as_micros(),
            state.capture.timings.bit_blt.as_micros(),
            state.capture.timings.dim.as_micros(),
            overlay_time.as_micros()
        ),
    );
    println!(
        "[isolmaSS] Overlay visible={}us; capture-to-visible={}us.",
        overlay_time.as_micros(),
        (state.capture.timings.total + overlay_time).as_micros()
    );
    register_overlay(hwnd)?;
    state.visible_windows = get_visible_windows(Some(hwnd));

    let loop_result = crate::ui::window_loop(hwnd, crate::ui::WindowKind::Overlay);
    unregister_overlay();
    if state.preferences_dirty
        && let Err(error) = state.settings.save_editor_preferences()
    {
        crate::tray::show_notification(
            crate::i18n::t("Could not save preferences"),
            &error.to_string(),
        );
    }

    loop_result?;
    let committed = if state.committed_result || state.committed_selection.is_some() {
        state.committed_selection
    } else {
        None
    };

    Ok((committed, visible))
}
