use super::*;

impl OverlayState {
    /// Opens the keyboard command menu, which runs its own modal loop, and applies the
    /// command the user picks.
    pub(super) fn open_command_menu(&mut self, hwnd: HWND) {
        let Some(buttons) = self.command_menu_buttons() else {
            return;
        };
        let anchor = self.command_menu_anchor(hwnd);
        let choice = crate::toolbar::show_command_menu(hwnd, &buttons, anchor);
        self.apply_command_menu_choice(hwnd, &buttons, choice);
    }

    /// Starts the upload of the selection: the first one opens Cloudflare setup instead.
    /// The encoded image goes to the upload toast, which sends it from a background
    /// thread and shows the link, and the editor closes.
    pub(super) fn upload_selection(&mut self, hwnd: HWND) -> std::result::Result<(), String> {
        let Some((prepared, anchor)) = self.begin_upload(hwnd)? else {
            return Ok(());
        };
        crate::toast::show_upload(prepared, anchor);
        self.finish_upload(hwnd)
    }
}

/// Hides the editor while another top-level flow needs the screen, and brings
/// it back topmost and focused afterwards if it still exists.
struct HiddenOverlay(HWND, bool);
impl HiddenOverlay {
    fn new(hwnd: HWND) -> Self {
        unsafe {
            let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
        }
        Self(hwnd, true)
    }
    /// The editor is about to close (the upload continues in its toast), so it
    /// is not shown again.
    fn stay_hidden(mut self) {
        self.1 = false;
    }
}
impl Drop for HiddenOverlay {
    fn drop(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{
            HWND_TOPMOST, IsWindow, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos,
        };
        if !self.1 || !unsafe { IsWindow(self.0).as_bool() } {
            return;
        }
        unsafe {
            let _ = SetWindowPos(
                self.0,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
        }
        crate::ui::bring_to_front(self.0);
        unsafe {
            let _ = SetFocus(self.0);
        }
    }
}

pub(super) unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut OverlayState;

    if crate::hotkey::overlay_input_suspended()
        && matches!(
            msg,
            WM_OVERLAY_KEYDOWN
                | WM_OVERLAY_CHAR
                | WM_CHAR
                | WM_KEYDOWN
                | WM_LBUTTONDOWN
                | WM_LBUTTONUP
                | WM_MOUSEMOVE
                | WM_MOUSEWHEEL
                | WM_LBUTTONDBLCLK
                | WM_RBUTTONUP
                | WM_TIMER
                | WM_EDITOR_SETTINGS
                | WM_EDITOR_PICK_COLOR
        )
    {
        return LRESULT(0);
    }
    if msg == WM_EDITOR_PICK_COLOR {
        if !state_ptr.is_null() {
            let initial = unsafe { (*state_ptr).active_color };
            match crate::ui::choose_color(hwnd, initial) {
                Ok(Some(color))
                    if unsafe {
                        windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool()
                    } =>
                {
                    let state = unsafe { &mut *state_ptr };
                    state.settings.last_custom_color = color;
                    state.change_color(color);
                    if let Some(edit) = &mut state.text_edit {
                        edit.color = color;
                        edit.caret_visible = true;
                    }
                    state.redraw(hwnd);
                }
                Ok(_) => {}
                Err(error) => crate::ui::error(
                    hwnd,
                    crate::i18n::t("Could not open the color picker"),
                    &error.to_string(),
                ),
            }
            // The dialog took the keyboard; typing continues in the same text box.
            if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() }
                && unsafe { (*state_ptr).text_edit.is_some() }
            {
                unsafe {
                    let _ = SetForegroundWindow(hwnd);
                    let _ = SetFocus(hwnd);
                }
            }
        }
        return LRESULT(0);
    }
    match msg {
        WM_EDITOR_SAVE_AS => {
            if state_ptr.is_null() {
                return LRESULT(0);
            }
            let format = unsafe { (*state_ptr).settings.save_format };
            let _suspension = crate::hotkey::OverlayInputSuspension::new();
            let path = crate::save::choose_output_path(hwnd, format);
            if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() } {
                return LRESULT(0);
            }
            match path {
                Ok(Some(path)) => {
                    let state = unsafe { &mut *state_ptr };
                    if let Some(selection) = state.committed_selection {
                        state.composite_scene_with(CompositionPolicy::EXPORT);
                        let capture = &state.capture;
                        let pixels = unsafe {
                            std::slice::from_raw_parts(
                                state.bits_ptr,
                                capture.width as usize * capture.height as usize * 4,
                            )
                        };
                        let result = crate::save::save_buffer_to_image(
                            pixels,
                            capture.width,
                            capture.height,
                            &selection,
                            &path,
                            format,
                            state.settings.jpeg_quality,
                        );
                        match result {
                            Ok(path) => {
                                crate::save::recent::saved(&path);
                                state.committed_result = true;
                                if state.settings.close_after_action {
                                    unsafe {
                                        let _ = DestroyWindow(hwnd);
                                    }
                                } else {
                                    state.redraw(hwnd);
                                }
                            }
                            Err(error) => {
                                state.redraw(hwnd);
                                OverlayState::show_action_error(
                                    hwnd,
                                    crate::i18n::t("Save failed"),
                                    &error,
                                );
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => crate::ui::error(
                    hwnd,
                    crate::i18n::t("Could not open the save dialog"),
                    &error.to_string(),
                ),
            }
            LRESULT(0)
        }
        WM_EDITOR_SETTINGS => {
            if state_ptr.is_null() {
                return LRESULT(0);
            }
            let settings = unsafe { (*state_ptr).settings.clone() };
            let result = show_settings_dialog(&settings, Some(hwnd));
            if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() } {
                match result {
                    Ok(Some(settings)) => {
                        let state = unsafe { &mut *state_ptr };
                        state.active_color = settings.default_color;
                        state.active_thickness = settings.default_thickness;
                        state.settings = settings;
                        state.preferences_dirty = false;
                        state.redraw(hwnd);
                    }
                    Ok(None) => {}
                    Err(error) => crate::ui::error(
                        hwnd,
                        crate::i18n::t("Could not open Settings"),
                        &error.to_string(),
                    ),
                }
            }
            LRESULT(0)
        }
        WM_EDITOR_CLOUD_SETUP => {
            if state_ptr.is_null() {
                return LRESULT(0);
            }
            let settings = unsafe { (*state_ptr).settings.clone() };
            // The full-screen topmost editor would cover the browser's Cloudflare
            // consent page, so it steps aside (keeping the capture and edits in
            // memory) until setup finishes, then returns and continues the upload.
            let hidden = HiddenOverlay::new(hwnd);
            let result = crate::cloud_settings_window::show_for_upload(&settings);
            if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() } {
                return LRESULT(0);
            }
            let state = unsafe { &mut *state_ptr };
            state.setup_pending = false;
            match result {
                Ok(Some(settings)) => {
                    state.settings = settings;
                    match state.upload_selection(hwnd) {
                        Ok(()) => hidden.stay_hidden(),
                        Err(error) => {
                            drop(hidden);
                            OverlayState::show_action_error(
                                hwnd,
                                crate::i18n::t("Upload failed"),
                                &error,
                            );
                        }
                    }
                }
                Ok(None) => drop(hidden),
                Err(error) => {
                    drop(hidden);
                    crate::ui::error(
                        hwnd,
                        crate::i18n::t("Could not open Cloudflare setup"),
                        &error.to_string(),
                    )
                }
            }
            LRESULT(0)
        }
        WM_EDITOR_ERROR => {
            let error = ACTION_ERROR.with(|pending| pending.borrow_mut().take());
            if let Some((title, message)) = error {
                crate::ui::error(hwnd, &title, &message);
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),

        WM_SETCURSOR => {
            if !state_ptr.is_null() {
                let state = unsafe { &*state_ptr };

                let mut pt_screen = windows::Win32::Foundation::POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt_screen);
                }
                let mut pt_client = pt_screen;
                unsafe {
                    let _ = ScreenToClient(hwnd, &mut pt_client);
                }
                let pt = (pt_client.x, pt_client.y);

                let cur_id = if let Some(toolbar) = &state.toolbar {
                    if toolbar.contains_point(pt) {
                        if toolbar.hit_test(pt).is_some() {
                            IDC_HAND
                        } else {
                            IDC_ARROW
                        }
                    } else {
                        state.editor_cursor(pt)
                    }
                } else {
                    state.editor_cursor(pt)
                };

                let cur = unsafe { LoadCursorW(HINSTANCE::default(), cur_id).unwrap_or_default() };
                unsafe {
                    SetCursor(cur);
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_DPICHANGED => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                state.dpi = (wparam.0 & 0xffff) as u32;
                state.redraw(hwnd);
            }
            LRESULT(0)
        }

        WM_TIMER => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                match wparam.0 {
                    CARET_TIMER => {
                        if let Some(edit) = &mut state.text_edit {
                            edit.caret_visible = !edit.caret_visible;
                            state.redraw_chrome(hwnd);
                        }
                    }
                    FADE_TIMER => state.advance_fade(hwnd),
                    _ => {}
                }
            }
            LRESULT(0)
        }

        WM_LBUTTONDBLCLK => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                let client_x = (lparam.0 as i32) as i16 as i32;
                let client_y = ((lparam.0 >> 16) as i32) as i16 as i32;
                let pt = (client_x, client_y);

                unsafe {
                    let _ = SetForegroundWindow(hwnd);
                    let _ = SetFocus(hwnd);
                }

                let over_toolbar = state
                    .toolbar
                    .as_ref()
                    .is_some_and(|toolbar| toolbar.contains_point(pt));
                // Double-clicking a committed text object reopens it, caret after the last
                // character. A text box already open takes the click as a caret placement.
                if state.mode == OverlayMode::SelectionActive
                    && state.active_tool == ToolKind::Select
                    && state.text_edit.is_none()
                    && !over_toolbar
                    && state.begin_text_reedit(hwnd, pt, false)
                {
                    return LRESULT(0);
                }
                // A fast second press is an ordinary press, unless the first one belonged
                // to another mode (double-clicking a hovered window to snap it).
                if state.last_down_mode == Some(state.mode) {
                    return unsafe { overlay_wnd_proc(hwnd, WM_LBUTTONDOWN, wparam, lparam) };
                }
            }
            LRESULT(0)
        }

        WM_PAINT => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                if state.scene_dirty {
                    state.composite_scene();
                    state.scene_dirty = false;
                }
                let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
                let hdc = unsafe { BeginPaint(hwnd, &mut ps) };

                let dirty_w = ps.rcPaint.right - ps.rcPaint.left;
                let dirty_h = ps.rcPaint.bottom - ps.rcPaint.top;

                if dirty_w > 0 && dirty_h > 0 {
                    let _ = unsafe {
                        BitBlt(
                            hdc,
                            ps.rcPaint.left,
                            ps.rcPaint.top,
                            dirty_w,
                            dirty_h,
                            state.mem_dc,
                            ps.rcPaint.left,
                            ps.rcPaint.top,
                            SRCCOPY,
                        )
                    };
                }

                let _ = unsafe { EndPaint(hwnd, &ps) };
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_ERASEBKGND => LRESULT(1),

        WM_MOUSEWHEEL => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                if state.mode == OverlayMode::SelectionActive {
                    let mut point = windows::Win32::Foundation::POINT {
                        x: lparam.0 as i32 as i16 as i32,
                        y: (lparam.0 >> 16) as i16 as i32,
                    };
                    if unsafe { ScreenToClient(hwnd, &mut point) }.as_bool()
                        && matches!(
                            state
                                .toolbar
                                .as_ref()
                                .and_then(|tb| tb.hit_test((point.x, point.y))),
                            Some(ToolbarItem::Thickness | ToolbarItem::ThicknessOption(_))
                        )
                    {
                        state.thickness_wheel_remainder += (wparam.0 >> 16) as i16 as i32;
                        let steps = state.thickness_wheel_remainder / 120;
                        state.thickness_wheel_remainder %= 120;
                        if steps != 0 {
                            if let Some(digits) = &mut state.thickness_input {
                                digits.clear();
                            }
                            // Like a list: scrolling down brings the thicker rows up.
                            let value = (state.active_thickness - steps).clamp(1, 64);
                            if value != state.active_thickness {
                                state.change_thickness(value, true);
                                state.redraw(hwnd);
                            }
                        }
                        return LRESULT(0);
                    }
                    state.thickness_wheel_remainder = 0;
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_MOUSEMOVE => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                let client_x = (lparam.0 as i32) as i16 as i32;
                let client_y = ((lparam.0 >> 16) as i32) as i16 as i32;
                let pt = (client_x, client_y);
                let previous_pointer = state.pointer;
                state.pointer = pt;
                if let Some(hud) = state.hud_rect
                    && state.mode == OverlayMode::SelectionActive
                    && hud.contains(previous_pointer.0, previous_pointer.1)
                        != hud.contains(pt.0, pt.1)
                {
                    state.redraw_chrome(hwnd);
                }

                // A press outside the selection only becomes a new region once it drags.
                if let Some(origin) = state.pending_reselect {
                    if !exceeds_drag_threshold(origin, pt, drag_threshold(state.dpi)) {
                        return LRESULT(0);
                    }
                    state.pending_reselect = None;
                    state.begin_reselect(origin);
                }

                let shift_down = (unsafe { GetKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000) != 0;

                match state.mode {
                    OverlayMode::Hovering => {
                        if state.settings.enable_window_snap {
                            let screen_x = client_x + state.capture.x;
                            let screen_y = client_y + state.capture.y;

                            let new_snap = if let Some(win) =
                                find_window_in_list(&state.visible_windows, (screen_x, screen_y))
                            {
                                Some(
                                    Rect::new(
                                        win.bounds.left - state.capture.x,
                                        win.bounds.top - state.capture.y,
                                        win.bounds.right - state.capture.x,
                                        win.bounds.bottom - state.capture.y,
                                    )
                                    .clamp(state.capture.width, state.capture.height),
                                )
                            } else {
                                None
                            };

                            if state.hover_snap_rect != new_snap {
                                let old_snap = state.hover_snap_rect;
                                state.hover_snap_rect = new_snap;
                                let dirty = match (old_snap, new_snap) {
                                    (Some(old), Some(new)) => old.union(&new),
                                    (Some(old), None) => old,
                                    (None, Some(new)) => new,
                                    (None, None) => Rect::default(),
                                };
                                state.redraw_region(hwnd, dirty.inflate(4, 4));
                            }
                        } else if let Some(old_snap) = state.hover_snap_rect.take() {
                            state.redraw_region(hwnd, old_snap.inflate(4, 4));
                        }
                    }
                    OverlayMode::DraggingSelection => {
                        // Jitter within the click threshold keeps the window highlight.
                        if let Some(start) = state.drag_start
                            && (state.committed_selection.is_some()
                                || exceeds_drag_threshold(start, pt, drag_threshold(state.dpi)))
                        {
                            let adjusted_pt = if shift_down {
                                snap_square(start, pt)
                            } else {
                                pt
                            };
                            let new_sel = Rect::normalized(start, adjusted_pt)
                                .clamp(state.capture.width, state.capture.height);
                            let dirty = state
                                .committed_selection
                                .map_or(new_sel, |old| old.union(&new_sel))
                                .inflate(4, 4);
                            state.committed_selection = Some(new_sel);
                            state.redraw_region(hwnd, dirty);
                        }
                    }
                    OverlayMode::SelectionActive => {
                        if let Some(drag) = &mut state.thickness_drag {
                            let rise = drag.start_y - pt.1;
                            let row = state
                                .toolbar
                                .as_ref()
                                .map_or(1, |toolbar| toolbar.wheel_row_height());
                            drag.moved |= rise.abs() > row / 4;
                            // Dragging the wheel up brings the thicker rows to the middle.
                            let value = (drag.start_value + rise / row).clamp(1, 64);
                            if drag.moved && value != state.active_thickness {
                                if let Some(digits) = &mut state.thickness_input {
                                    digits.clear();
                                }
                                state.change_thickness(value, false);
                                state.redraw(hwnd);
                            }
                            return LRESULT(0);
                        }
                        // 1. Dragging selection (Move or Resize)
                        if let Some(drag) = state.dragging_selection {
                            let dx = client_x - drag.last_pos.0;
                            let dy = client_y - drag.last_pos.1;
                            let max_w = state.capture.width;
                            let max_h = state.capture.height;
                            let old_selection = state.committed_selection;

                            if let Some(mut sel) = state.committed_selection {
                                match drag.action {
                                    DragSelectionAction::Move => {
                                        let w = sel.width();
                                        let h = sel.height();
                                        let target_left = (sel.left + dx).clamp(0, max_w - w);
                                        let target_top = (sel.top + dy).clamp(0, max_h - h);
                                        let actual_dx = target_left - sel.left;
                                        let actual_dy = target_top - sel.top;

                                        if actual_dx != 0 || actual_dy != 0 {
                                            sel.left += actual_dx;
                                            sel.right += actual_dx;
                                            sel.top += actual_dy;
                                            sel.bottom += actual_dy;
                                            state.committed_selection = Some(sel);

                                            for obj in &mut state.objects {
                                                obj.translate(actual_dx, actual_dy);
                                            }
                                        }
                                    }
                                    DragSelectionAction::Resize(zone) => {
                                        // The edge keeps the offset it was grabbed at.
                                        let edge_x = client_x - drag.grab.0;
                                        let edge_y = client_y - drag.grab.1;
                                        match zone {
                                            SelectionHitZone::TopLeftCorner => {
                                                sel.left = edge_x.clamp(0, (sel.right - 8).max(0));
                                                sel.top = edge_y.clamp(0, (sel.bottom - 8).max(0));
                                            }
                                            SelectionHitZone::TopRightCorner => {
                                                sel.right =
                                                    edge_x.clamp((sel.left + 8).min(max_w), max_w);
                                                sel.top = edge_y.clamp(0, (sel.bottom - 8).max(0));
                                            }
                                            SelectionHitZone::BottomLeftCorner => {
                                                sel.left = edge_x.clamp(0, (sel.right - 8).max(0));
                                                sel.bottom =
                                                    edge_y.clamp((sel.top + 8).min(max_h), max_h);
                                            }
                                            SelectionHitZone::BottomRightCorner => {
                                                sel.right =
                                                    edge_x.clamp((sel.left + 8).min(max_w), max_w);
                                                sel.bottom =
                                                    edge_y.clamp((sel.top + 8).min(max_h), max_h);
                                            }
                                            SelectionHitZone::TopEdge => {
                                                sel.top = edge_y.clamp(0, (sel.bottom - 8).max(0));
                                            }
                                            SelectionHitZone::RightEdge => {
                                                sel.right =
                                                    edge_x.clamp((sel.left + 8).min(max_w), max_w);
                                            }
                                            SelectionHitZone::BottomEdge => {
                                                sel.bottom =
                                                    edge_y.clamp((sel.top + 8).min(max_h), max_h);
                                            }
                                            SelectionHitZone::LeftEdge => {
                                                sel.left = edge_x.clamp(0, (sel.right - 8).max(0));
                                            }
                                            _ => {}
                                        }
                                        state.committed_selection = Some(sel);
                                    }
                                }
                            }

                            state.dragging_selection = Some(DragSelectionState {
                                last_pos: pt,
                                ..drag
                            });
                            let dirty = match (old_selection, state.committed_selection) {
                                (Some(old), Some(new)) => old.union(&new).inflate(8, 8),
                                (Some(old), None) => old.inflate(8, 8),
                                (None, Some(new)) => new.inflate(8, 8),
                                (None, None) => Rect::default(),
                            };
                            state.redraw_region(hwnd, dirty);
                            return LRESULT(0);
                        }

                        // 2. Moving or resizing the selected object. The preview mutates
                        // freely, while history receives one Modify command on button-up.
                        if let Some(drag) = state.dragging_object.as_ref() {
                            let drag_id = drag.id;
                            let action = drag.action;
                            let last_pos = drag.last_pos;
                            let original_kind = &drag.original_kind;
                            let selection = state.committed_selection.unwrap_or_default();
                            let dirty = if let Some(object) =
                                state.objects.iter_mut().find(|object| object.id == drag_id)
                            {
                                let old_bounds = object.bounds();
                                match action {
                                    DragObjectAction::Move => {
                                        let bounds = object.geometry_bounds();
                                        let requested_dx = client_x - last_pos.0;
                                        let requested_dy = client_y - last_pos.1;
                                        let min_dx = selection.left - bounds.left;
                                        let max_dx = selection.right - bounds.right;
                                        let min_dy = selection.top - bounds.top;
                                        let max_dy = selection.bottom - bounds.bottom;
                                        let dx = if min_dx <= max_dx {
                                            requested_dx.clamp(min_dx, max_dx)
                                        } else {
                                            0
                                        };
                                        let dy = if min_dy <= max_dy {
                                            requested_dy.clamp(min_dy, max_dy)
                                        } else {
                                            0
                                        };
                                        object.translate(dx, dy);
                                    }
                                    DragObjectAction::Resize(handle, grab) => {
                                        let cursor = (pt.0 - grab.0, pt.1 - grab.1);
                                        object.resize_from(
                                            original_kind,
                                            handle,
                                            cursor,
                                            selection,
                                        );
                                    }
                                }
                                old_bounds.union(&object.bounds()).inflate(8, 8)
                            } else {
                                Rect::default()
                            };
                            if let Some(drag) = state.dragging_object.as_mut() {
                                drag.last_pos = pt;
                            }
                            state.redraw_region(hwnd, dirty);
                            return LRESULT(0);
                        }

                        // 3. Drawing in-progress shape, kept inside the exported region
                        let region = state.committed_selection.unwrap_or_default();
                        if let Some(shape) = &mut state.drawing_shape {
                            match shape {
                                InProgressDrawing::Rectangle { start, current }
                                | InProgressDrawing::Ellipse { start, current } => {
                                    *current = frame_drag_end(*start, pt, shift_down, region);
                                }
                                InProgressDrawing::Arrow { start, current }
                                | InProgressDrawing::Line { start, current } => {
                                    *current = segment_drag_end(*start, pt, shift_down, region);
                                }
                                InProgressDrawing::Pen { points } => {
                                    let pt = clamp_point(pt, pixel_bounds(region));
                                    if points.len() < 32_768
                                        && points.last().is_none_or(|last| {
                                            (last.0 - pt.0).abs() + (last.1 - pt.1).abs() >= 2
                                        })
                                    {
                                        points.push(pt);
                                    }
                                }
                                InProgressDrawing::Blur { start, current, .. } => {
                                    *current = clamp_point(
                                        if shift_down {
                                            snap_square(*start, pt)
                                        } else {
                                            pt
                                        },
                                        region,
                                    );
                                }
                            }
                            state.redraw_chrome(hwnd);
                            return LRESULT(0);
                        }

                        // 4. Selecting text with the pointer
                        if state.text_dragging {
                            if let Some(edit) = &mut state.text_edit {
                                let at = edit.caret_at(pt);
                                if at != edit.caret {
                                    edit.move_to(at, true);
                                    start_caret_timer(hwnd);
                                    state.redraw_chrome(hwnd);
                                }
                            }
                            return LRESULT(0);
                        }

                        // 5. Toolbar hover update
                        if state.toolbar.as_mut().is_some_and(|tb| tb.update_hover(pt)) {
                            state.redraw_chrome(hwnd);
                        }
                    }
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_LBUTTONDOWN => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                let client_x = (lparam.0 as i32) as i16 as i32;
                let client_y = ((lparam.0 >> 16) as i32) as i16 as i32;
                let pt = (client_x, client_y);

                unsafe {
                    let _ = SetForegroundWindow(hwnd);
                    let _ = SetFocus(hwnd);
                    SetCapture(hwnd);
                }
                state.last_down_mode = Some(state.mode);
                state.pending_reselect = None;

                match state.mode {
                    OverlayMode::Hovering => {
                        state.drag_start = Some(pt);
                        state.mode = OverlayMode::DraggingSelection;
                    }
                    OverlayMode::DraggingSelection => {}
                    OverlayMode::SelectionActive => {
                        // A press that never got its release must not fire on the next one.
                        if let Some(toolbar) = &mut state.toolbar {
                            toolbar.pressed_item = None;
                        }

                        // 1. Toolbar button: pressed now, executed when released over it.
                        if let Some(item) = state.toolbar.as_ref().and_then(|tb| tb.hit_test(pt)) {
                            if matches!(item, ToolbarItem::ThicknessOption(_)) {
                                state.activate_toolbar_item(hwnd, item, pt);
                            } else {
                                if let Some(toolbar) = &mut state.toolbar {
                                    toolbar.pressed_item = Some(item);
                                }
                                state.redraw_chrome(hwnd);
                            }
                            return LRESULT(0);
                        }

                        // Panel padding, gaps and disabled buttons swallow the click.
                        if state
                            .toolbar
                            .as_ref()
                            .is_some_and(|toolbar| toolbar.contains_point(pt))
                        {
                            return LRESULT(0);
                        }

                        if state.thickness_input.take().is_some() {
                            set_overlay_text_editing(false);
                        }
                        // A press inside the text box places the caret, Shift extends the
                        // selection, and dragging selects; the text stays open.
                        if state
                            .text_edit
                            .as_ref()
                            .is_some_and(|edit| edit.contains(pt, state.dpi))
                        {
                            if let Some(edit) = &mut state.text_edit {
                                let at = edit.caret_at(pt);
                                edit.move_to(at, wparam.0 & MK_SHIFT != 0);
                            }
                            state.text_dragging = true;
                            start_caret_timer(hwnd);
                            state.redraw_chrome(hwnd);
                            return LRESULT(0);
                        }
                        // If text edit active, commit it when clicked elsewhere
                        if state.text_edit.is_some() {
                            state.commit_text(hwnd);
                        }

                        let Some(sel) = state.committed_selection else {
                            return LRESULT(0);
                        };

                        let selection_zone = hit_selection(&sel, pt, state.dpi);
                        if matches!(
                            selection_zone,
                            SelectionHitZone::TopLeftCorner
                                | SelectionHitZone::TopRightCorner
                                | SelectionHitZone::BottomLeftCorner
                                | SelectionHitZone::BottomRightCorner
                                | SelectionHitZone::TopEdge
                                | SelectionHitZone::RightEdge
                                | SelectionHitZone::BottomEdge
                                | SelectionHitZone::LeftEdge
                        ) {
                            state.dragging_selection = Some(DragSelectionState::new(
                                sel,
                                DragSelectionAction::Resize(selection_zone),
                                pt,
                            ));
                            state.selected_id = None;
                            state.redraw(hwnd);
                            return LRESULT(0);
                        }

                        if state.hud_rect.is_some_and(|hud| hud.contains(pt.0, pt.1)) {
                            state.dragging_selection =
                                Some(DragSelectionState::new(sel, DragSelectionAction::Move, pt));
                            state.selected_id = None;
                            state.redraw(hwnd);
                            return LRESULT(0);
                        }

                        if state.active_tool == ToolKind::Select {
                            if let Some(object) = state
                                .selected_id
                                .and_then(|id| state.objects.iter().find(|object| object.id == id))
                                && let Some(handle) = object.hit_resize_handle(
                                    pt,
                                    object_grip_radius(state.dpi),
                                    state.dpi,
                                )
                            {
                                state.dragging_object = Some(DragObjectState {
                                    id: object.id,
                                    action: DragObjectAction::Resize(
                                        handle,
                                        object.resize_grab(handle, pt),
                                    ),
                                    last_pos: pt,
                                    original_kind: object.kind.clone(),
                                });
                                return LRESULT(0);
                            }

                            match selection_zone {
                                SelectionHitZone::TopLeftCorner
                                | SelectionHitZone::TopRightCorner
                                | SelectionHitZone::BottomLeftCorner
                                | SelectionHitZone::BottomRightCorner
                                | SelectionHitZone::TopEdge
                                | SelectionHitZone::RightEdge
                                | SelectionHitZone::BottomEdge
                                | SelectionHitZone::LeftEdge => unreachable!(),
                                SelectionHitZone::BorderEdge => {
                                    state.dragging_selection = Some(DragSelectionState::new(
                                        sel,
                                        DragSelectionAction::Move,
                                        pt,
                                    ));
                                    state.selected_id = None;
                                    state.redraw(hwnd);
                                    return LRESULT(0);
                                }
                                SelectionHitZone::Interior => {
                                    // A selected object's whole bounding interior is draggable;
                                    // unselected line work still requires a precise first hit.
                                    let hit_object = state
                                        .selected_id
                                        .and_then(|id| state.objects.iter().find(|o| o.id == id))
                                        .filter(|object| {
                                            object
                                                .geometry_bounds()
                                                .inflate(4, 4)
                                                .contains(pt.0, pt.1)
                                        })
                                        .or_else(|| state.topmost_object_at(pt));
                                    if let Some(object) = hit_object {
                                        let id = object.id;
                                        let original_kind = object.kind.clone();
                                        state.selected_id = Some(id);
                                        state.dragging_object = Some(DragObjectState {
                                            id,
                                            action: DragObjectAction::Move,
                                            last_pos: pt,
                                            original_kind,
                                        });
                                        state.redraw(hwnd);
                                        return LRESULT(0);
                                    }
                                    state.selected_id = None;
                                    state.redraw(hwnd);
                                }
                                SelectionHitZone::None => {
                                    if state.selected_id.is_some() {
                                        state.selected_id = None;
                                        state.redraw(hwnd);
                                    } else {
                                        // The session is only replaced once the pointer drags.
                                        state.pending_reselect = Some(pt);
                                    }
                                }
                            }
                        } else if sel.contains(pt.0, pt.1) {
                            state.selected_id = None;
                            match state.active_tool {
                                ToolKind::Rectangle => {
                                    state.drawing_shape = Some(InProgressDrawing::Rectangle {
                                        start: pt,
                                        current: pt,
                                    });
                                }
                                ToolKind::Ellipse => {
                                    state.drawing_shape = Some(InProgressDrawing::Ellipse {
                                        start: pt,
                                        current: pt,
                                    });
                                }
                                ToolKind::Arrow => {
                                    state.drawing_shape = Some(InProgressDrawing::Arrow {
                                        start: pt,
                                        current: pt,
                                    });
                                }
                                ToolKind::Line => {
                                    state.drawing_shape = Some(InProgressDrawing::Line {
                                        start: pt,
                                        current: pt,
                                    });
                                }
                                ToolKind::Pen | ToolKind::Highlight => {
                                    state.drawing_shape =
                                        Some(InProgressDrawing::Pen { points: vec![pt] });
                                }
                                ToolKind::Step => {
                                    let number = state
                                        .objects
                                        .iter()
                                        .filter_map(|object| {
                                            if let AnnotationKind::Step { number, .. } = object.kind
                                            {
                                                Some(number)
                                            } else {
                                                None
                                            }
                                        })
                                        .max()
                                        .unwrap_or(0)
                                        .saturating_add(1);
                                    let radius = (14 * state.dpi as i32 / 96).max(14);
                                    let center = (
                                        if sel.width() > 2 * radius {
                                            pt.0.clamp(sel.left + radius, sel.right - radius - 1)
                                        } else {
                                            (sel.left + sel.right) / 2
                                        },
                                        if sel.height() > 2 * radius {
                                            pt.1.clamp(sel.top + radius, sel.bottom - radius - 1)
                                        } else {
                                            (sel.top + sel.bottom) / 2
                                        },
                                    );
                                    let object = AnnotationObject::new(
                                        state.next_id,
                                        AnnotationKind::Step {
                                            pos: center,
                                            number,
                                            color: state.active_color,
                                            radius,
                                        },
                                    );
                                    state.next_id += 1;
                                    state.history.record(EditCommand::Add(object.clone()));
                                    state.objects.push(object);
                                    state.redraw(hwnd);
                                }
                                ToolKind::Text => {
                                    // Clicking existing text edits it instead of stacking a box.
                                    if state.begin_text_reedit(hwnd, pt, true) {
                                        state.text_dragging = true;
                                        return LRESULT(0);
                                    }
                                    state.text_edit = Some(
                                        TextEditState::new(
                                            pt,
                                            String::new(),
                                            state.active_color,
                                            DEFAULT_FONT_SIZE * state.dpi as i32 / 96,
                                            None,
                                        )
                                        .with_background(state.settings.text_background),
                                    );
                                    start_caret_timer(hwnd);
                                    set_overlay_text_editing(true);
                                    state.redraw(hwnd);
                                }
                                ToolKind::Blur | ToolKind::Redact => {
                                    state.drawing_shape = Some(InProgressDrawing::Blur {
                                        redact: state.active_tool == ToolKind::Redact,
                                        start: pt,
                                        current: pt,
                                    });
                                }
                                ToolKind::Select => unreachable!(),
                            }
                        }
                    }
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_LBUTTONUP => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                let client_x = (lparam.0 as i32) as i16 as i32;
                let client_y = ((lparam.0 >> 16) as i32) as i16 as i32;
                let pt = (client_x, client_y);

                let _ = unsafe { ReleaseCapture() };
                let shift_down = (unsafe { GetKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000) != 0;
                state.pending_reselect = None;
                state.text_dragging = false;
                // A toolbar button acts only when the button is released over it.
                if let Some(item) = state
                    .toolbar
                    .as_mut()
                    .and_then(|toolbar| toolbar.pressed_item.take())
                {
                    let released_over = state.toolbar.as_ref().and_then(|tb| tb.hit_test(pt));
                    if released_over == Some(item) {
                        state.activate_toolbar_item(hwnd, item, pt);
                    } else {
                        state.redraw_chrome(hwnd);
                    }
                    return LRESULT(0);
                }
                if let Some(drag) = state.thickness_drag.take() {
                    if !drag.moved {
                        state.change_thickness(drag.picked, false);
                        if state.thickness_input.take().is_some() {
                            set_overlay_text_editing(false);
                        }
                    }
                    if let Some((id, before)) = state.thickness_drag_original.take()
                        && let Some(object) = state.objects.iter().find(|object| object.id == id)
                        && before != object.kind
                    {
                        state.history.record(EditCommand::Modify {
                            id,
                            old_kind: before,
                            new_kind: object.kind.clone(),
                        });
                    }
                    state.persist_editor_preferences();
                    state.redraw(hwnd);
                    return LRESULT(0);
                }

                match state.mode {
                    OverlayMode::DraggingSelection => {
                        state.reselecting = false;
                        if let Some(start) = state.drag_start.take() {
                            if !exceeds_drag_threshold(start, pt, drag_threshold(state.dpi)) {
                                if state.settings.enable_window_snap {
                                    let screen_x = client_x + state.capture.x;
                                    let screen_y = client_y + state.capture.y;

                                    if let Some(win) = find_window_in_list(
                                        &state.visible_windows,
                                        (screen_x, screen_y),
                                    ) {
                                        let snap_rect = Rect::new(
                                            win.bounds.left - state.capture.x,
                                            win.bounds.top - state.capture.y,
                                            win.bounds.right - state.capture.x,
                                            win.bounds.bottom - state.capture.y,
                                        )
                                        .clamp(state.capture.width, state.capture.height);

                                        state.commit_selection(hwnd, snap_rect);
                                    } else {
                                        state.mode = OverlayMode::Hovering;
                                        state.committed_selection = None;
                                        state.redraw(hwnd);
                                    }
                                } else {
                                    state.mode = OverlayMode::Hovering;
                                    state.committed_selection = None;
                                    state.redraw(hwnd);
                                }
                            } else {
                                let final_pt = if shift_down {
                                    snap_square(start, pt)
                                } else {
                                    pt
                                };
                                let drag_rect = Rect::normalized(start, final_pt)
                                    .clamp(state.capture.width, state.capture.height);

                                if drag_rect.width() >= 8 && drag_rect.height() >= 8 {
                                    state.commit_selection(hwnd, drag_rect);
                                } else {
                                    state.mode = OverlayMode::Hovering;
                                    state.committed_selection = None;
                                    state.redraw(hwnd);
                                }
                            }
                        }
                    }
                    OverlayMode::SelectionActive => {
                        if let Some(drag) = state.dragging_selection.take() {
                            if let Some(after) = state
                                .committed_selection
                                .filter(|after| *after != drag.original_selection)
                            {
                                let before = drag.original_selection;
                                let shift = if drag.action == DragSelectionAction::Move {
                                    (after.left - before.left, after.top - before.top)
                                } else {
                                    (0, 0)
                                };
                                state.history.record(EditCommand::Selection {
                                    before,
                                    after,
                                    shift,
                                });
                            }
                            state.redraw(hwnd);
                            return LRESULT(0);
                        }

                        if let Some(drag) = state.dragging_object.take() {
                            if let Some(object) =
                                state.objects.iter().find(|object| object.id == drag.id)
                            {
                                let new_kind = object.kind.clone();
                                if new_kind != drag.original_kind {
                                    state.history.record(EditCommand::Modify {
                                        id: drag.id,
                                        old_kind: drag.original_kind,
                                        new_kind,
                                    });
                                }
                            }
                            state.selected_id = Some(drag.id);
                            state.redraw(hwnd);
                            return LRESULT(0);
                        }

                        if let Some(shape) = state.drawing_shape.take() {
                            // Annotations never leave the region that will be exported.
                            let region = state.committed_selection.unwrap_or_default();
                            let (color, thickness) = (state.active_color, state.active_thickness);
                            let make = |kind| AnnotationObject::new(state.next_id, kind);
                            let new_obj = match shape {
                                InProgressDrawing::Rectangle { start, current } => {
                                    dragged_frame(start, current, shift_down, region).map(|rect| {
                                        make(AnnotationKind::Rectangle {
                                            rect,
                                            color,
                                            thickness,
                                        })
                                    })
                                }
                                InProgressDrawing::Ellipse { start, current } => {
                                    dragged_frame(start, current, shift_down, region).map(|rect| {
                                        make(AnnotationKind::Ellipse {
                                            rect,
                                            color,
                                            thickness,
                                        })
                                    })
                                }
                                InProgressDrawing::Arrow { start, current } => dragged_segment(
                                    start, current, shift_down, region,
                                )
                                .map(|(start, end)| {
                                    make(AnnotationKind::Arrow {
                                        start,
                                        end,
                                        color,
                                        thickness,
                                    })
                                }),
                                InProgressDrawing::Line { start, current } => dragged_segment(
                                    start, current, shift_down, region,
                                )
                                .map(|(start, end)| {
                                    make(AnnotationKind::Line {
                                        start,
                                        end,
                                        color,
                                        thickness,
                                    })
                                }),
                                InProgressDrawing::Pen { points } => {
                                    let min_x = points.iter().map(|p| p.0).min().unwrap_or(0);
                                    let max_x = points.iter().map(|p| p.0).max().unwrap_or(0);
                                    let min_y = points.iter().map(|p| p.1).min().unwrap_or(0);
                                    let max_y = points.iter().map(|p| p.1).max().unwrap_or(0);
                                    if points.len() >= 2
                                        && ((max_x - min_x) >= 3 || (max_y - min_y) >= 3)
                                    {
                                        Some(AnnotationObject::new(
                                            state.next_id,
                                            if state.active_tool == ToolKind::Highlight {
                                                AnnotationKind::Highlight {
                                                    points,
                                                    color: state.active_color,
                                                    thickness: state.active_thickness,
                                                }
                                            } else {
                                                AnnotationKind::Pen {
                                                    points,
                                                    color: state.active_color,
                                                    thickness: state.active_thickness,
                                                }
                                            },
                                        ))
                                    } else {
                                        None
                                    }
                                }
                                InProgressDrawing::Blur {
                                    start,
                                    current,
                                    redact,
                                } => {
                                    let final_cur = clamp_point(
                                        if shift_down {
                                            snap_square(start, current)
                                        } else {
                                            current
                                        },
                                        region,
                                    );
                                    let r = Rect::normalized(start, final_cur);
                                    if r.width() >= 6 && r.height() >= 6 {
                                        Some(AnnotationObject::new(
                                            state.next_id,
                                            if redact {
                                                AnnotationKind::Redact { rect: r }
                                            } else {
                                                AnnotationKind::Blur {
                                                    rect: r,
                                                    block_size: DEFAULT_BLUR_BLOCK,
                                                }
                                            },
                                        ))
                                    } else {
                                        None
                                    }
                                }
                            };

                            if let Some(obj) = new_obj {
                                state.next_id += 1;
                                state.history.record(EditCommand::Add(obj.clone()));
                                state.selected_id = None;
                                state.objects.push(obj);
                            }

                            state.redraw(hwnd);
                        }
                    }
                    _ => {}
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_OVERLAY_CHAR | WM_CHAR => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                return state.handle_char(hwnd, wparam.0 as u32);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_OVERLAY_KEYDOWN | WM_KEYDOWN => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                let modifiers = if msg == WM_OVERLAY_KEYDOWN {
                    lparam.0
                } else {
                    crate::hotkey::get_current_modifiers() as isize
                };
                return state.handle_key_down(hwnd, wparam.0, modifiers);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        // F10 and Shift+F10 arrive as system keys. Handling them here opens the command
        // menu and keeps Windows from entering its own menu mode.
        WM_SYSKEYDOWN if wparam.0 == 0x79 => {
            let repeat = lparam.0 & (1 << 30) != 0;
            if !state_ptr.is_null() && !repeat && !crate::hotkey::overlay_input_suspended() {
                let state = unsafe { &mut *state_ptr };
                return state.handle_key_down(
                    hwnd,
                    0x79,
                    crate::hotkey::get_current_modifiers() as isize,
                );
            }
            LRESULT(0)
        }

        // While an IME composes, the keyboard hook must leave every key to the IME.
        WM_IME_STARTCOMPOSITION => {
            if !state_ptr.is_null()
                && let Some(edit) = unsafe { &(*state_ptr).text_edit }
            {
                set_overlay_ime_composing(true);
                edit.place_ime_window(hwnd);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_IME_ENDCOMPOSITION => {
            set_overlay_ime_composing(false);
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_RBUTTONDOWN => {
            unsafe {
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(hwnd);
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            if !state_ptr.is_null() {
                let state = unsafe { &mut *state_ptr };
                match state.handle_escape_action() {
                    EscapeAction::CancelledTextEdit => {
                        stop_text_input(hwnd);
                        state.redraw(hwnd);
                        return LRESULT(0);
                    }
                    EscapeAction::CancelledGesture
                    | EscapeAction::CancelledThicknessEdit
                    | EscapeAction::DeselectedObject(_)
                    | EscapeAction::CancelledSelection => {
                        state.redraw(hwnd);
                        return LRESULT(0);
                    }
                    EscapeAction::CloseOverlay => {
                        let _ = unsafe { DestroyWindow(hwnd) };
                        return LRESULT(0);
                    }
                }
            }
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }

        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }

        WM_DESTROY => {
            unregister_overlay();
            set_overlay_ime_composing(false);
            LRESULT(0)
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
