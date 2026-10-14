use crate::annotation::{
    AnnotationKind, AnnotationObject, AnnotationResizeHandle, EditCommand, HistoryManager,
    MergeKey, ToolKind, bgra_to_colorref, render_pen_preview, snap_angle_45, snap_square,
};
use crate::capture::{CaptureBuffer, Rect, SelectionHitZone};
use crate::clipboard::{copy_dib_to_clipboard, flatten_selection_to_dib};
use crate::hotkey::{
    WM_OVERLAY_CHAR, WM_OVERLAY_KEYDOWN, register_overlay, set_overlay_ime_composing,
    set_overlay_text_editing, unregister_overlay,
};
use crate::save::save_screenshot;
use crate::settings::{Settings, show_settings_dialog};
use crate::toolbar::{Toolbar, ToolbarAction, ToolbarItem};
use crate::window_snap::{WindowInfo, find_window_in_list, get_visible_windows};
use std::ffi::c_void;
use std::rc::Rc;
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT,
    RECT as WIN_RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, ClientToScreen, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, GdiFlush, GetDC,
    GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, IntersectClipRect, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, RGBQUAD, ReleaseDC, RestoreDC, SRCCOPY,
    SaveDC, ScreenToClient, SelectObject,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MOD_CONTROL, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_CONTROL, VK_DELETE,
    VK_DOWN, VK_ESCAPE, VK_OEM_COMMA, VK_RETURN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetCaretBlinkTime,
    GetCursorPos, GetWindowLongPtrW, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, IDC_SIZEALL,
    IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, KillTimer, LWA_ALPHA, LoadCursorW,
    MA_ACTIVATE, RegisterClassExW, SM_CXDRAG, SM_CYDRAG, SW_SHOW, SetCursor, SetForegroundWindow,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, ShowWindow, WM_CHAR, WM_CLOSE,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SYSKEYDOWN, WM_TIMER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, Result, w};

mod render;
mod session;
pub use session::{measure_overlay, show_overlay_session};

const OVERLAY_CLASS_NAME: windows::core::PCWSTR = w!("isolmaSS_OverlayClass");

const DEFAULT_FONT_SIZE: i32 = 22;
const DEFAULT_BLUR_BLOCK: i32 = 12;
const CARET_TIMER: usize = 1;
const FADE_TIMER: usize = 2;
/// Length of the dimming fade-in when the overlay opens.
const FADE_IN_MS: u32 = 120;
/// Fade timer period, about one display frame.
const FADE_STEP_MS: u32 = 16;
/// `wParam` flag of the mouse messages: Shift is held.
const MK_SHIFT: usize = 0x0004;
const WM_EDITOR_SETTINGS: u32 = 0x8000 + 301;
const WM_EDITOR_ERROR: u32 = 0x8000 + 302;
const WM_EDITOR_SAVE_AS: u32 = 0x8000 + 303;
const WM_EDITOR_PICK_COLOR: u32 = 0x8000 + 304;
const WM_EDITOR_CLOUD_SETUP: u32 = 0x8000 + 305;
thread_local! { static ACTION_ERROR: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) }; }

pub fn tool_for_key(vk: u32) -> Option<ToolKind> {
    match vk {
        0x56 => Some(ToolKind::Select),
        0x52 => Some(ToolKind::Rectangle),
        0x41 => Some(ToolKind::Arrow),
        0x50 => Some(ToolKind::Pen),
        0x54 => Some(ToolKind::Text),
        0x48 => Some(ToolKind::Highlight),
        0x4e => Some(ToolKind::Step),
        0x42 => Some(ToolKind::Blur),
        0x4d => Some(ToolKind::Redact),
        _ => None,
    }
}

pub fn is_editor_shortcut(vk: u32, modifiers: u32, editing: bool) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_WIN};
    if modifiers & (MOD_ALT.0 | MOD_WIN.0) != 0 {
        return false;
    }
    if modifiers & MOD_CONTROL.0 != 0 {
        matches!(vk, 0x43 | 0x53 | 0x55 | 0x5a | 0x59 | 0xbc | 37..=40)
            || editing && matches!(vk, 0x41 | 0x56 | 0x58 | 8 | 46)
    } else {
        matches!(vk, 8 | 13 | 27 | 35..=40 | 46) || !editing && tool_for_key(vk).is_some()
    }
}

fn rects_intersect(a: &Rect, b: &Rect) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.left < b.right
        && b.left < a.right
        && a.top < b.bottom
        && b.top < a.bottom
}

fn intersect_rects(a: Rect, b: Rect) -> Rect {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    Rect::new(
        left,
        top,
        a.right.min(b.right).max(left),
        a.bottom.min(b.bottom).max(top),
    )
}

/// Edge length of a selection grip; `Rect::hit_test_selection` uses half of it.
fn selection_handle_size(dpi: u32) -> i32 {
    (8 * dpi as i32 / 96).max(8)
}

/// Grips sit on the accent frame drawn around the selection, so the zones follow it.
fn hit_selection(selection: &Rect, pt: (i32, i32), dpi: u32) -> SelectionHitZone {
    selection.selection_frame(dpi).hit_test_selection(
        pt,
        4 * dpi as i32 / 96,
        selection_handle_size(dpi),
    )
}

/// Half size, in pixels, of an object's resize grip and its hit tolerance.
fn object_grip_radius(dpi: u32) -> i32 {
    4 * dpi as i32 / 96
}

fn selection_grip_boxes(selection: &Rect, half: i32) -> [Rect; 8] {
    let middle_x = selection.left + selection.width() / 2;
    let middle_y = selection.top + selection.height() / 2;
    [
        (selection.left, selection.top),
        (selection.right, selection.top),
        (selection.left, selection.bottom),
        (selection.right, selection.bottom),
        (middle_x, selection.top),
        (selection.right, middle_y),
        (middle_x, selection.bottom),
        (selection.left, middle_y),
    ]
    .map(|(x, y)| Rect::new(x - half, y - half, x + half, y + half))
}

/// Places the "W × H px" readout: above the selection, else below it, else inside its
/// top-left corner past the grip. The first candidate that fits the viewport and clears
/// every obstacle (toolbar panels) and selection grip wins; the inside one is the fallback.
fn place_capture_hud(selection: Rect, viewport: Rect, dpi: u32, obstacles: &[Rect]) -> Rect {
    let scale = |value: i32| (value * dpi as i32 / 96).max(1);
    let width = scale(136).min(viewport.width().max(1));
    let height = scale(28);
    let grip = selection_handle_size(dpi);
    let frame = Rect::selection_frame_thickness(dpi);
    let gap = frame + grip / 2 + scale(4);
    let max_x = (viewport.right - width).max(viewport.left);
    let x = selection.left.clamp(viewport.left, max_x);
    let above = Rect::new(
        x,
        selection.top - gap - height,
        x + width,
        selection.top - gap,
    );
    let below = Rect::new(
        x,
        selection.bottom + gap,
        x + width,
        selection.bottom + gap + height,
    );
    let inside_x = (selection.left + scale(8) + grip).clamp(viewport.left, max_x);
    let inside_y = (selection.top + scale(8) + grip)
        .clamp(viewport.top, (viewport.bottom - height).max(viewport.top));
    let inside = Rect::new(inside_x, inside_y, inside_x + width, inside_y + height);
    let grips = selection_grip_boxes(&selection.selection_frame(dpi), grip / 2 + scale(1));
    [above, below, inside]
        .into_iter()
        .filter(|rect| rect.top >= viewport.top && rect.bottom <= viewport.bottom)
        .find(|rect| {
            !obstacles
                .iter()
                .chain(&grips)
                .any(|blocked| rects_intersect(rect, blocked))
        })
        .unwrap_or(inside)
}

/// Pointer travel, in pixels, that turns a click into a drag (`SM_CXDRAG`/`SM_CYDRAG`).
fn drag_threshold(dpi: u32) -> (i32, i32) {
    unsafe {
        (
            windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CXDRAG, dpi).max(1),
            windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CYDRAG, dpi).max(1),
        )
    }
}

fn exceeds_drag_threshold(origin: (i32, i32), pt: (i32, i32), threshold: (i32, i32)) -> bool {
    (pt.0 - origin.0).abs() > threshold.0 || (pt.1 - origin.1).abs() > threshold.1
}

/// Distance from the pressed point to the edge being resized, so the edge keeps that
/// offset from the pointer instead of snapping to it.
fn resize_grab_offset(zone: SelectionHitZone, selection: Rect, press: (i32, i32)) -> (i32, i32) {
    let edge_x = match zone {
        SelectionHitZone::TopLeftCorner
        | SelectionHitZone::BottomLeftCorner
        | SelectionHitZone::LeftEdge => selection.left,
        SelectionHitZone::TopRightCorner
        | SelectionHitZone::BottomRightCorner
        | SelectionHitZone::RightEdge => selection.right,
        _ => press.0,
    };
    let edge_y = match zone {
        SelectionHitZone::TopLeftCorner
        | SelectionHitZone::TopRightCorner
        | SelectionHitZone::TopEdge => selection.top,
        SelectionHitZone::BottomLeftCorner
        | SelectionHitZone::BottomRightCorner
        | SelectionHitZone::BottomEdge => selection.bottom,
        _ => press.1,
    };
    (press.0 - edge_x, press.1 - edge_y)
}

/// Clamps `pt` into `bounds`, edges included.
fn clamp_point(pt: (i32, i32), bounds: Rect) -> (i32, i32) {
    (
        pt.0.clamp(bounds.left, bounds.right.max(bounds.left)),
        pt.1.clamp(bounds.top, bounds.bottom.max(bounds.top)),
    )
}

/// The last pixel column and row of `selection`: inclusive bounds for pixel positions.
fn pixel_bounds(selection: Rect) -> Rect {
    Rect::new(
        selection.left,
        selection.top,
        selection.right - 1,
        selection.bottom - 1,
    )
}

fn selection_screen_rect(capture: &CaptureBuffer, selection: Rect) -> WIN_RECT {
    WIN_RECT {
        left: selection.left + capture.x,
        top: selection.top + capture.y,
        right: selection.right + capture.x,
        bottom: selection.bottom + capture.y,
    }
}

fn selection_monitor_dpi(capture: &CaptureBuffer, selection: Rect, fallback: u32) -> u32 {
    let screen_selection = selection_screen_rect(capture, selection);
    let monitor = unsafe { MonitorFromRect(&screen_selection, MONITOR_DEFAULTTONEAREST) };
    let mut dpi_x = fallback;
    let mut dpi_y = fallback;
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
        dpi_x.max(48)
    } else {
        fallback
    }
}

fn selection_work_viewport(capture: &CaptureBuffer, selection: Rect) -> Rect {
    let screen_selection = selection_screen_rect(capture, selection);
    let monitor = unsafe { MonitorFromRect(&screen_selection, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } {
        Rect::new(
            info.rcWork.left - capture.x,
            info.rcWork.top - capture.y,
            info.rcWork.right - capture.x,
            info.rcWork.bottom - capture.y,
        )
        .clamp(capture.width, capture.height)
    } else {
        Rect::new(0, 0, capture.width, capture.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    Hovering,
    DraggingSelection,
    SelectionActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompositionPolicy {
    selection_frame: bool,
    selected_handles: bool,
    toolbar_and_tooltip: bool,
    caret: bool,
    in_progress_preview: bool,
}

impl CompositionPolicy {
    const EDITOR: Self = Self {
        selection_frame: true,
        selected_handles: true,
        toolbar_and_tooltip: true,
        caret: true,
        in_progress_preview: true,
    };
    const EXPORT: Self = Self {
        selection_frame: false,
        selected_handles: false,
        toolbar_and_tooltip: false,
        caret: false,
        in_progress_preview: false,
    };
}

#[derive(Debug, Clone)]
enum InProgressDrawing {
    Rectangle {
        start: (i32, i32),
        current: (i32, i32),
    },
    Arrow {
        start: (i32, i32),
        current: (i32, i32),
    },
    Pen {
        points: Vec<(i32, i32)>,
    },
    Blur {
        start: (i32, i32),
        current: (i32, i32),
        redact: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragSelectionAction {
    Move,
    Resize(SelectionHitZone),
}

#[derive(Debug, Clone, Copy)]
struct DragSelectionState {
    original_selection: Rect,
    action: DragSelectionAction,
    last_pos: (i32, i32),
    /// Pointer offset from the grabbed edge at press time (resizes only).
    grab: (i32, i32),
}

impl DragSelectionState {
    fn new(selection: Rect, action: DragSelectionAction, press: (i32, i32)) -> Self {
        let grab = match action {
            DragSelectionAction::Move => (0, 0),
            DragSelectionAction::Resize(zone) => resize_grab_offset(zone, selection, press),
        };
        Self {
            original_selection: selection,
            action,
            last_pos: press,
            grab,
        }
    }
}

/// Everything a discarded selection held, kept so Ctrl+Z can bring it back.
struct DiscardedSession {
    selection: Rect,
    objects: Vec<AnnotationObject>,
    history: HistoryManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeAction {
    CancelledGesture,
    CancelledTextEdit,
    CancelledThicknessEdit,
    DeselectedObject(usize),
    CancelledSelection,
    CloseOverlay,
}

mod text;
pub use text::TextEditState;

#[derive(Debug, Clone, Copy)]
enum DragObjectAction {
    Move,
    Resize(AnnotationResizeHandle),
}

#[derive(Debug, Clone)]
struct DragObjectState {
    id: usize,
    action: DragObjectAction,
    last_pos: (i32, i32),
    original_kind: AnnotationKind,
}

/// Blink period for the system caret blink time: `INFINITE` turns blinking off, 0 is a
/// failed query and falls back to the Windows default.
fn caret_blink_interval(blink_time: u32) -> Option<u32> {
    match blink_time {
        u32::MAX => None,
        0 => Some(500),
        milliseconds => Some(milliseconds),
    }
}

/// Starts or restarts the caret blink cycle at the system rate. Restarting makes the
/// caret solid for a whole period after every edit; with blinking off it stays solid.
fn start_caret_timer(hwnd: HWND) {
    unsafe {
        match caret_blink_interval(GetCaretBlinkTime()) {
            Some(interval) => {
                let _ = SetTimer(hwnd, CARET_TIMER, interval, None);
            }
            None => {
                let _ = KillTimer(hwnd, CARET_TIMER);
            }
        }
    }
}

/// Window opacity `elapsed_ms` into the opening fade: eases out from nearly clear to opaque.
fn fade_alpha(elapsed_ms: u32) -> u8 {
    let progress = elapsed_ms.min(FADE_IN_MS) as f32 / FADE_IN_MS as f32;
    let eased = 1.0 - (1.0 - progress) * (1.0 - progress);
    (1.0 + 254.0 * eased).round() as u8
}

/// Stops the caret timer and hands the keyboard back to shortcuts and the IME.
fn stop_text_input(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(hwnd, CARET_TIMER);
    }
    set_overlay_text_editing(false);
    set_overlay_ime_composing(false);
}

struct ScreenDcGuard(HDC);

impl Drop for ScreenDcGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(HWND::default(), self.0);
        }
    }
}

struct DeleteDcGuard(HDC);

impl Drop for DeleteDcGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct DeleteBitmapGuard(HBITMAP);

impl Drop for DeleteBitmapGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}

pub struct OverlayState {
    capture: Rc<CaptureBuffer>,
    mode: OverlayMode,
    drag_start: Option<(i32, i32)>,
    committed_selection: Option<Rect>,
    hover_snap_rect: Option<Rect>,
    visible_windows: Vec<WindowInfo>,
    dpi: u32,

    settings: Settings,
    active_tool: ToolKind,
    active_color: [u8; 4],
    active_thickness: i32,
    /// `Some` while the line-width wheel is open; holds digits typed into it.
    thickness_input: Option<String>,
    thickness_drag: Option<ThicknessDrag>,
    thickness_wheel_remainder: i32,
    thickness_drag_original: Option<(usize, AnnotationKind)>,
    objects: Vec<AnnotationObject>,
    selected_id: Option<usize>,
    next_id: usize,
    history: HistoryManager,

    drawing_shape: Option<InProgressDrawing>,
    dragging_object: Option<DragObjectState>,
    dragging_selection: Option<DragSelectionState>,
    text_edit: Option<TextEditState>,
    /// The button went down inside the text box: moving the pointer extends the selection.
    text_dragging: bool,
    /// Start of the opening fade; `None` once the overlay is fully opaque.
    fade_started: Option<std::time::Instant>,
    /// A press outside the selection becomes a new region only once the pointer drags.
    pending_reselect: Option<(i32, i32)>,
    /// A new region was started outside the previous session; Esc puts that session back.
    reselecting: bool,
    discarded_session: Option<DiscardedSession>,
    last_down_mode: Option<OverlayMode>,

    toolbar: Option<Toolbar>,
    /// Size readout placed after the toolbar layout of the latest frame.
    hud_rect: Option<Rect>,
    tools_expanded: bool,

    mem_dc: HDC,
    dib: HBITMAP,
    old_bmp: HGDIOBJ,
    bits_ptr: *mut u8,

    committed_result: bool,
    setup_pending: bool,
    scene_dirty: bool,
    base_cache: Vec<u8>,
    cache_requested: bool,
    pointer: (i32, i32),
    preferences_dirty: bool,
}

/// A press on the line-width wheel: dragging scrolls it, a plain click picks the row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ThicknessDrag {
    start_y: i32,
    start_value: i32,
    picked: i32,
    moved: bool,
}

impl Drop for OverlayState {
    fn drop(&mut self) {
        unregister_overlay();
        set_overlay_ime_composing(false);
        crate::drawing::clear_text_metrics();
        unsafe {
            if !self.mem_dc.0.is_null() {
                SelectObject(self.mem_dc, self.old_bmp);
                let _ = DeleteDC(self.mem_dc);
            }
            if !self.dib.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(self.dib.0));
            }
        }
    }
}

impl OverlayState {
    /// Aborts the gesture in progress and puts back the state it started from.
    fn cancel_active_gesture(&mut self) -> bool {
        let mut cancelled = self.pending_reselect.take().is_some();
        cancelled |= self
            .toolbar
            .as_mut()
            .and_then(|toolbar| toolbar.pressed_item.take())
            .is_some();
        cancelled |= self.drawing_shape.take().is_some();
        if let Some(drag) = self.dragging_object.take() {
            if let Some(object) = self.objects.iter_mut().find(|object| object.id == drag.id) {
                object.kind = drag.original_kind;
            }
            cancelled = true;
        }
        if let Some(drag) = self.dragging_selection.take() {
            if drag.action == DragSelectionAction::Move
                && let Some(current) = self.committed_selection
            {
                let dx = drag.original_selection.left - current.left;
                let dy = drag.original_selection.top - current.top;
                for object in &mut self.objects {
                    object.translate(dx, dy);
                }
            }
            self.committed_selection = Some(drag.original_selection);
            cancelled = true;
        }
        if let Some(drag) = self.thickness_drag.take() {
            if let Some((id, kind)) = self.thickness_drag_original.take()
                && let Some(object) = self.objects.iter_mut().find(|object| object.id == id)
            {
                object.kind = kind;
            }
            self.active_thickness = drag.start_value;
            cancelled = true;
        }
        if self.reselecting {
            cancelled |= self.restore_discarded_session();
        }
        if cancelled {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        cancelled
    }

    /// Drops the selection and every annotation. An active selection is kept as one
    /// snapshot so Ctrl+Z in `Hovering` can bring the session back.
    fn discard_session(&mut self) {
        if self.mode == OverlayMode::SelectionActive
            && let Some(selection) = self.committed_selection
        {
            self.discarded_session = Some(DiscardedSession {
                selection,
                objects: std::mem::take(&mut self.objects),
                history: std::mem::take(&mut self.history),
            });
        }
        self.mode = OverlayMode::Hovering;
        self.drag_start = None;
        self.hover_snap_rect = None;
        self.committed_selection = None;
        self.toolbar = None;
        self.hud_rect = None;
        self.objects.clear();
        self.history.clear();
        self.selected_id = None;
        self.drawing_shape = None;
        self.dragging_object = None;
        self.dragging_selection = None;
        self.pending_reselect = None;
        self.reselecting = false;
    }

    pub fn restore_discarded_session(&mut self) -> bool {
        let Some(session) = self.discarded_session.take() else {
            return false;
        };
        self.mode = OverlayMode::SelectionActive;
        self.committed_selection = Some(session.selection);
        self.objects = session.objects;
        self.history = session.history;
        self.hover_snap_rect = None;
        self.drag_start = None;
        self.reselecting = false;
        self.selected_id = None;
        true
    }

    /// Starts a new region from `origin`; the previous session stays restorable.
    fn begin_reselect(&mut self, origin: (i32, i32)) {
        self.discard_session();
        self.mode = OverlayMode::DraggingSelection;
        self.drag_start = Some(origin);
        self.reselecting = true;
    }

    pub fn handle_escape_action(&mut self) -> EscapeAction {
        if self.cancel_active_gesture() {
            EscapeAction::CancelledGesture
        } else if self.thickness_input.take().is_some() {
            set_overlay_text_editing(false);
            EscapeAction::CancelledThicknessEdit
        } else if self.text_edit.take().is_some() {
            EscapeAction::CancelledTextEdit
        } else if let Some(id) = self.selected_id.take() {
            EscapeAction::DeselectedObject(id)
        } else if matches!(
            self.mode,
            OverlayMode::SelectionActive | OverlayMode::DraggingSelection
        ) {
            self.discard_session();
            unsafe {
                let _ = ReleaseCapture();
            }
            EscapeAction::CancelledSelection
        } else {
            EscapeAction::CloseOverlay
        }
    }

    pub fn create_test_state(capture: Rc<CaptureBuffer>) -> Self {
        Self {
            capture,
            mode: OverlayMode::Hovering,
            drag_start: None,
            committed_selection: None,
            hover_snap_rect: None,
            visible_windows: Vec::new(),
            dpi: 96,
            settings: Settings::default(),
            active_tool: ToolKind::Rectangle,
            active_color: [49, 49, 224, 255],
            active_thickness: Settings::default().default_thickness,
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
            text_dragging: false,
            fade_started: None,
            pending_reselect: None,
            reselecting: false,
            discarded_session: None,
            last_down_mode: None,
            toolbar: None,
            hud_rect: None,
            tools_expanded: false,
            mem_dc: HDC::default(),
            dib: HBITMAP::default(),
            old_bmp: HGDIOBJ::default(),
            bits_ptr: std::ptr::null_mut(),
            committed_result: false,
            setup_pending: false,
            scene_dirty: false,
            base_cache: Vec::new(),
            cache_requested: false,
            pointer: (0, 0),
            preferences_dirty: false,
        }
    }

    pub fn mode(&self) -> OverlayMode {
        self.mode
    }

    pub fn text_edit(&self) -> Option<&TextEditState> {
        self.text_edit.as_ref()
    }

    pub fn set_text_edit(&mut self, edit: Option<TextEditState>) {
        self.text_edit = edit;
    }

    pub fn selected_id(&self) -> Option<usize> {
        self.selected_id
    }

    pub fn set_selected_id(&mut self, id: Option<usize>) {
        self.selected_id = id;
    }

    pub fn committed_selection(&self) -> Option<Rect> {
        self.committed_selection
    }

    pub fn set_selection_active(&mut self, rect: Rect) {
        self.mode = OverlayMode::SelectionActive;
        self.committed_selection = Some(rect);
    }

    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub fn add_test_rectangle(&mut self, rect: Rect) {
        let object = AnnotationObject::new(
            self.next_id,
            AnnotationKind::Rectangle {
                rect,
                color: self.active_color,
                thickness: self.active_thickness,
            },
        );
        self.next_id += 1;
        self.history.record(EditCommand::Add(object.clone()));
        self.objects.push(object);
    }

    /// Smoke-test hooks: start a gesture without synthesising mouse input.
    pub fn begin_test_rectangle_gesture(&mut self, start: (i32, i32), current: (i32, i32)) {
        self.drawing_shape = Some(InProgressDrawing::Rectangle { start, current });
    }

    pub fn begin_test_selection_move(&mut self) {
        if let Some(selection) = self.committed_selection {
            self.dragging_selection = Some(DragSelectionState::new(
                selection,
                DragSelectionAction::Move,
                (0, 0),
            ));
        }
    }

    pub fn has_active_gesture(&self) -> bool {
        self.drawing_shape.is_some()
            || self.dragging_object.is_some()
            || self.dragging_selection.is_some()
            || self.thickness_drag.is_some()
    }

    fn commit_text(&mut self, hwnd: HWND) {
        if let Some(edit) = self.text_edit.take() {
            stop_text_input(hwnd);
            let text = edit.text.clone();
            if text.trim().is_empty() {
                if let Some(index) = edit
                    .editing_id
                    .and_then(|id| self.objects.iter().position(|o| o.id == id))
                {
                    let object = self.objects.remove(index);
                    self.history.record(EditCommand::Delete { object, index });
                }
                self.selected_id = None;
            } else {
                let (obj_id, is_modify) = if let Some(existing_id) = edit.editing_id {
                    (existing_id, true)
                } else {
                    let id = self.next_id;
                    self.next_id += 1;
                    (id, false)
                };

                let new_kind = AnnotationKind::Text {
                    pos: edit.pos,
                    text,
                    color: edit.color,
                    font_size: edit.font_size,
                };
                let obj = AnnotationObject::new(obj_id, new_kind.clone());

                if is_modify {
                    if let Some(existing) = self.objects.iter_mut().find(|o| o.id == obj_id) {
                        let old_kind = existing.kind.clone();
                        existing.kind = new_kind.clone();
                        if old_kind != new_kind {
                            self.history.record(EditCommand::Modify {
                                id: obj_id,
                                old_kind,
                                new_kind,
                            });
                        }
                    } else {
                        let old_kind = AnnotationKind::Text {
                            pos: edit.pos,
                            text: edit.original_text.clone(),
                            color: edit.color,
                            font_size: edit.font_size,
                        };
                        self.objects.push(obj);
                        self.history.record(EditCommand::Modify {
                            id: obj_id,
                            old_kind,
                            new_kind,
                        });
                    }
                } else {
                    self.history.record(EditCommand::Add(obj.clone()));
                    self.objects.push(obj);
                }
                self.selected_id = if is_modify && self.active_tool == ToolKind::Select {
                    Some(obj_id)
                } else {
                    None
                };
            }
            self.redraw(hwnd);
        }
    }

    /// Runs a toolbar command. Buttons act when released over themselves; the rows of the
    /// line-width wheel act on press because dragging them scrolls the wheel.
    fn activate_toolbar_item(&mut self, hwnd: HWND, item: ToolbarItem, pt: (i32, i32)) {
        if !matches!(
            item,
            ToolbarItem::Thickness | ToolbarItem::ThicknessOption(_)
        ) && self.thickness_input.take().is_some()
        {
            set_overlay_text_editing(false);
        }
        // Picking a color recolors the text being typed instead of ending it.
        if item != ToolbarItem::Color {
            self.commit_text(hwnd);
        }

        match item {
            ToolbarItem::Tool(k) => {
                self.active_tool = k;
                self.persist_editor_preferences();
                self.selected_id = None;
                self.redraw(hwnd);
            }
            ToolbarItem::MoreTools => {
                self.tools_expanded = !self.tools_expanded;
                self.redraw(hwnd);
            }
            ToolbarItem::Action(ToolbarAction::Undo) => {
                self.history.undo(&mut self.objects);
                if let Some(selection) = self.history.take_selection_update() {
                    self.committed_selection = Some(selection);
                }
                self.selected_id = None;
                self.redraw(hwnd);
            }
            ToolbarItem::Action(ToolbarAction::Redo) => {
                self.history.redo(&mut self.objects);
                if let Some(selection) = self.history.take_selection_update() {
                    self.committed_selection = Some(selection);
                }
                self.selected_id = None;
                self.redraw(hwnd);
            }
            ToolbarItem::Action(ToolbarAction::Save) => match self.save_selection_to_file() {
                Ok(path) => {
                    if self.settings.notify_after_save {
                        crate::tray::show_notification(
                            crate::i18n::t("Screenshot saved"),
                            &path.display().to_string(),
                        );
                    }
                    if self.settings.close_after_action {
                        let _ = unsafe { DestroyWindow(hwnd) };
                    } else {
                        self.redraw(hwnd);
                    }
                }
                Err(error) => Self::show_action_error(hwnd, crate::i18n::t("Save failed"), &error),
            },
            ToolbarItem::Action(ToolbarAction::Copy) => {
                match self.copy_selection_to_clipboard(hwnd) {
                    Ok(()) => {
                        if self.settings.close_after_action {
                            let _ = unsafe { DestroyWindow(hwnd) };
                        } else {
                            self.redraw(hwnd);
                        }
                    }
                    Err(error) => {
                        Self::show_action_error(hwnd, crate::i18n::t("Copy failed"), &error)
                    }
                }
            }
            ToolbarItem::Action(ToolbarAction::Upload) => {
                if let Err(error) = self.upload_selection(hwnd) {
                    Self::show_action_error(hwnd, crate::i18n::t("Upload failed"), &error);
                }
            }
            ToolbarItem::Action(ToolbarAction::Settings) => unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    hwnd,
                    WM_EDITOR_SETTINGS,
                    WPARAM(0),
                    LPARAM(0),
                );
            },
            ToolbarItem::Action(ToolbarAction::Cancel) => {
                self.discard_session();
                self.redraw(hwnd);
            }
            ToolbarItem::Color => unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    hwnd,
                    WM_EDITOR_PICK_COLOR,
                    WPARAM(0),
                    LPARAM(0),
                );
            },
            ToolbarItem::Thickness => {
                let open = self.thickness_input.take().is_none();
                if open {
                    self.thickness_input = Some(String::new());
                }
                set_overlay_text_editing(open);
                self.redraw(hwnd);
            }
            ToolbarItem::ThicknessOption(value) => {
                self.thickness_drag = Some(ThicknessDrag {
                    start_y: pt.1,
                    start_value: self.active_thickness,
                    picked: value,
                    moved: false,
                });
                self.thickness_drag_original = self.selected_id.and_then(|id| {
                    self.objects
                        .iter()
                        .find(|object| object.id == id)
                        .map(|object| (id, object.kind.clone()))
                });
            }
        }
    }

    /// Opens the Text object under `pt` for editing. `place_caret` puts the caret where
    /// it was clicked instead of after the last character.
    fn begin_text_reedit(&mut self, hwnd: HWND, pt: (i32, i32), place_caret: bool) -> bool {
        let Some(object) = self.objects.iter().rfind(|object| {
            matches!(object.kind, AnnotationKind::Text { .. }) && object.hit_test(pt)
        }) else {
            return false;
        };
        let id = object.id;
        let AnnotationKind::Text {
            pos,
            text,
            color,
            font_size,
        } = object.kind.clone()
        else {
            return false;
        };
        self.dragging_object = None;
        let mut edit = TextEditState::new(pos, text, color, font_size, Some(id));
        if place_caret {
            let at = edit.caret_at(pt.0);
            edit.move_to(at, false);
        }
        self.text_edit = Some(edit);
        start_caret_timer(hwnd);
        set_overlay_text_editing(true);
        self.selected_id = None;
        self.redraw(hwnd);
        true
    }

    /// One step of the opening fade; the timer stops once the overlay is opaque.
    fn advance_fade(&mut self, hwnd: HWND) {
        let elapsed = self.fade_started.map_or(FADE_IN_MS, |started| {
            started.elapsed().as_millis().min(FADE_IN_MS as u128) as u32
        });
        unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), fade_alpha(elapsed), LWA_ALPHA);
            if elapsed >= FADE_IN_MS {
                let _ = KillTimer(hwnd, FADE_TIMER);
            }
        }
        if elapsed >= FADE_IN_MS {
            self.fade_started = None;
        }
    }

    fn handle_key_down(&mut self, hwnd: HWND, vk: usize, mods: isize) -> LRESULT {
        if let Some(digits) = &mut self.thickness_input {
            match vk {
                value if value == VK_BACK.0 as usize => {
                    digits.pop();
                }
                value if value == VK_RETURN.0 as usize => {
                    if digits.is_empty() {
                        self.thickness_input = None;
                        set_overlay_text_editing(false);
                    } else if let Ok(value @ 1..=64) = digits.parse::<i32>() {
                        self.thickness_input = None;
                        set_overlay_text_editing(false);
                        self.change_thickness(value, true);
                    }
                }
                value if value == VK_UP.0 as usize || value == VK_DOWN.0 as usize => {
                    digits.clear();
                    let step = if value == VK_UP.0 as usize { -1 } else { 1 };
                    let next = (self.active_thickness + step).clamp(1, 64);
                    if next != self.active_thickness {
                        self.change_thickness(next, true);
                    }
                }
                value if value == VK_ESCAPE.0 as usize => {
                    // A wheel drag is cancelled first; the wheel closes on the next Esc.
                    self.handle_escape_action();
                }
                _ => {}
            }
            self.redraw(hwnd);
            return LRESULT(0);
        }
        // F10 / Apps: expose every toolbar command as a native popup menu for
        // keyboard and screen-reader access. The chosen command is replayed
        // through the ordinary click path so behavior stays identical.
        if matches!(vk, 0x79 | 0x5d)
            && self.mode == OverlayMode::SelectionActive
            && self.text_edit.is_none()
        {
            let buttons = self.toolbar.as_ref().map(|toolbar| toolbar.buttons.clone());
            let Some(mut buttons) = buttons else {
                return LRESULT(0);
            };
            for tool in [
                ToolKind::Highlight,
                ToolKind::Text,
                ToolKind::Step,
                ToolKind::Blur,
                ToolKind::Redact,
            ] {
                let item = ToolbarItem::Tool(tool);
                if !buttons.iter().any(|button| button.item == item) {
                    let at = buttons
                        .iter()
                        .position(|button| button.item == ToolbarItem::Action(ToolbarAction::Undo))
                        .unwrap_or(buttons.len());
                    buttons.insert(
                        at,
                        crate::toolbar::ToolbarButton {
                            item,
                            rect: Rect::default(),
                            is_enabled: true,
                            is_checked: tool == self.active_tool,
                        },
                    );
                }
            }
            let anchor = self.toolbar.as_ref().map(|toolbar| {
                let mut origin = windows::Win32::Foundation::POINT {
                    x: toolbar.action_bounds.left,
                    y: toolbar.action_bounds.top,
                };
                unsafe {
                    let _ = ClientToScreen(hwnd, &mut origin);
                }
                (origin.x, origin.y)
            });
            match crate::toolbar::show_command_menu(hwnd, &buttons, anchor) {
                Ok(Some(ToolbarItem::Tool(tool))) => {
                    self.active_tool = tool;
                    self.selected_id = None;
                    self.persist_editor_preferences();
                    self.redraw(hwnd);
                }
                Ok(Some(item)) => {
                    if let Some(rect) = buttons
                        .iter()
                        .find(|button| button.item == item)
                        .map(|button| button.rect)
                    {
                        let packed = (((rect.left + rect.right) / 2) as u16 as u32)
                            | ((((rect.top + rect.bottom) / 2) as u16 as u32) << 16);
                        unsafe {
                            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                                hwnd,
                                WM_LBUTTONDOWN,
                                WPARAM(1),
                                LPARAM(packed as isize),
                            );
                            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                                hwnd,
                                WM_LBUTTONUP,
                                WPARAM(0),
                                LPARAM(packed as isize),
                            );
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => Self::show_action_error(
                    hwnd,
                    crate::i18n::t("Command menu failed"),
                    &error.to_string(),
                ),
            }
            return LRESULT(0);
        }
        if !is_editor_shortcut(vk as u32, mods as u32, self.text_edit.is_some()) {
            return LRESULT(0);
        }
        let ctrl_down = (mods & MOD_CONTROL.0 as isize) != 0
            || ((unsafe { GetKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0);

        // 1. Hierarchical Escape flow (production method used directly)
        if vk == VK_ESCAPE.0 as usize {
            match self.handle_escape_action() {
                EscapeAction::CancelledTextEdit => {
                    stop_text_input(hwnd);
                    self.redraw(hwnd);
                }
                EscapeAction::CancelledGesture
                | EscapeAction::CancelledThicknessEdit
                | EscapeAction::DeselectedObject(_)
                | EscapeAction::CancelledSelection => {
                    self.redraw(hwnd);
                }
                EscapeAction::CloseOverlay => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
            }
            return LRESULT(0);
        }

        // Settings is also available while text is being edited. Dispatch after
        // releasing this borrow, and keep the current text edit intact.
        if ctrl_down && vk == VK_OEM_COMMA.0 as usize {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    hwnd,
                    WM_EDITOR_SETTINGS,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            return LRESULT(0);
        }

        // 2. Text editing owns keys, except copy: commit first so the flattened
        // screenshot contains the final text and never includes editor affordances.
        if self.text_edit.is_some() {
            if ctrl_down && matches!(vk as u8, b'C' | b'S' | b'U') {
                self.commit_text(hwnd);
            } else if vk == VK_RETURN.0 as usize {
                self.commit_text(hwnd);
                return LRESULT(0);
            } else {
                if ctrl_down && vk == 'V' as usize {
                    match crate::clipboard::read_text(hwnd) {
                        Ok(text) => {
                            if let Some(edit) = &mut self.text_edit {
                                edit.insert_text(&text);
                            }
                        }
                        Err(error) => Self::show_action_error(
                            hwnd,
                            crate::i18n::t("Paste failed"),
                            &error.to_string(),
                        ),
                    }
                } else if let Some(edit) = &mut self.text_edit {
                    let shift =
                        mods as u32 & windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT.0 != 0;
                    match vk {
                        8 if ctrl_down => {
                            edit.delete_word(false);
                        }
                        8 => {
                            edit.backspace();
                        }
                        46 if ctrl_down => {
                            edit.delete_word(true);
                        }
                        46 => {
                            edit.delete();
                        }
                        37 if ctrl_down => edit.move_word(false, shift),
                        37 => edit.step(false, shift),
                        39 if ctrl_down => edit.move_word(true, shift),
                        39 => edit.step(true, shift),
                        36 => edit.move_to(0, shift),
                        35 => edit.move_to(edit.text.chars().count(), shift),
                        0x41 if ctrl_down => edit.select_all(),
                        0x58 if ctrl_down => {
                            if let Some(selected) = edit.selected_text() {
                                match crate::clipboard::copy_text_to_clipboard(
                                    Some(hwnd),
                                    &selected,
                                ) {
                                    Ok(()) => {
                                        edit.delete_selected();
                                    }
                                    Err(error) => Self::show_action_error(
                                        hwnd,
                                        crate::i18n::t("Cut failed"),
                                        &error.to_string(),
                                    ),
                                }
                            }
                        }
                        0x5a if ctrl_down => edit.undo(),
                        0x59 if ctrl_down => edit.redo(),
                        _ => {}
                    }
                    edit.caret_visible = true;
                }
                start_caret_timer(hwnd);
                self.redraw(hwnd);
                return LRESULT(0);
            }
        }

        // 3. Save shortcut: Ctrl+S
        if self.mode == OverlayMode::SelectionActive && ctrl_down && vk == 'S' as usize {
            if mods as u32 & windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT.0 != 0 {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        hwnd,
                        WM_EDITOR_SAVE_AS,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
                return LRESULT(0);
            }
            match self.save_selection_to_file() {
                Ok(path) => {
                    if self.settings.notify_after_save {
                        crate::tray::show_notification(
                            crate::i18n::t("Screenshot saved"),
                            &path.display().to_string(),
                        );
                    }
                    if self.settings.close_after_action {
                        let _ = unsafe { DestroyWindow(hwnd) };
                    } else {
                        self.redraw(hwnd);
                    }
                }
                Err(error) => Self::show_action_error(hwnd, crate::i18n::t("Save failed"), &error),
            }
            return LRESULT(0);
        }

        if self.mode == OverlayMode::SelectionActive && ctrl_down && vk == 'U' as usize {
            if let Err(error) = self.upload_selection(hwnd) {
                Self::show_action_error(hwnd, crate::i18n::t("Upload failed"), &error);
            }
            return LRESULT(0);
        }

        // 5. Copy shortcut: Ctrl+C or Enter
        if self.mode == OverlayMode::SelectionActive
            && ((ctrl_down && vk == 'C' as usize) || vk == VK_RETURN.0 as usize)
        {
            match self.copy_selection_to_clipboard(hwnd) {
                Ok(()) => {
                    if self.settings.close_after_action {
                        let _ = unsafe { DestroyWindow(hwnd) };
                    } else {
                        self.redraw(hwnd);
                    }
                }
                Err(error) => Self::show_action_error(hwnd, crate::i18n::t("Copy failed"), &error),
            }
            return LRESULT(0);
        }

        // 6. Undo: Ctrl+Z
        if ctrl_down && vk == 'Z' as usize {
            if self.mode == OverlayMode::Hovering && self.restore_discarded_session() {
                self.redraw(hwnd);
                return LRESULT(0);
            }
            self.history.undo(&mut self.objects);
            if let Some(selection) = self.history.take_selection_update() {
                self.committed_selection = Some(selection);
            }
            self.selected_id = None;
            self.redraw(hwnd);
            return LRESULT(0);
        }

        // 7. Redo: Ctrl+Y
        if ctrl_down && vk == 'Y' as usize {
            self.history.redo(&mut self.objects);
            if let Some(selection) = self.history.take_selection_update() {
                self.committed_selection = Some(selection);
            }
            self.selected_id = None;
            self.redraw(hwnd);
            return LRESULT(0);
        }

        // 8. Delete / Backspace selected object
        if (vk == VK_DELETE.0 as usize || vk == VK_BACK.0 as usize)
            && let Some(pos) = self
                .selected_id
                .take()
                .and_then(|id| self.objects.iter().position(|o| o.id == id))
        {
            let removed = self.objects.remove(pos);
            self.history.record(EditCommand::Delete {
                object: removed,
                index: pos,
            });
            self.redraw(hwnd);
            return LRESULT(0);
        }

        if !ctrl_down && let Some(tool) = tool_for_key(vk as u32) {
            self.active_tool = tool;
            self.selected_id = None;
            self.persist_editor_preferences();
            self.redraw(hwnd);
        }
        if (37..=40).contains(&vk) && self.mode == OverlayMode::SelectionActive {
            let amount =
                if mods as u32 & windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT.0 != 0 {
                    10
                } else {
                    1
                };
            let (dx, dy) = match vk {
                37 => (-amount, 0),
                38 => (0, -amount),
                39 => (amount, 0),
                _ => (0, amount),
            };
            if let Some(object) = self
                .selected_id
                .and_then(|id| self.objects.iter_mut().find(|o| o.id == id))
            {
                let bounds = object.geometry_bounds();
                let limit = self.committed_selection.unwrap_or(Rect::new(
                    0,
                    0,
                    self.capture.width,
                    self.capture.height,
                ));
                let dx = if bounds.width() <= limit.width() {
                    dx.clamp(limit.left - bounds.left, limit.right - bounds.right)
                } else {
                    0
                };
                let dy = if bounds.height() <= limit.height() {
                    dy.clamp(limit.top - bounds.top, limit.bottom - bounds.bottom)
                } else {
                    0
                };
                if dx == 0 && dy == 0 {
                    return LRESULT(0);
                }
                let old_kind = object.kind.clone();
                object.translate(dx, dy);
                self.history.record_merge(
                    EditCommand::Modify {
                        id: object.id,
                        old_kind,
                        new_kind: object.kind.clone(),
                    },
                    MergeKey::Nudge,
                );
            } else if let Some(before) = self.committed_selection {
                let mut after = before;
                let shift = if ctrl_down {
                    after.right = (after.right + dx)
                        .clamp((after.left + 8).min(self.capture.width), self.capture.width);
                    after.bottom = (after.bottom + dy).clamp(
                        (after.top + 8).min(self.capture.height),
                        self.capture.height,
                    );
                    (0, 0)
                } else {
                    let dx = dx.clamp(-before.left, self.capture.width - before.right);
                    let dy = dy.clamp(-before.top, self.capture.height - before.bottom);
                    after.left += dx;
                    after.right += dx;
                    after.top += dy;
                    after.bottom += dy;
                    for object in &mut self.objects {
                        object.translate(dx, dy);
                    }
                    (dx, dy)
                };
                if after != before {
                    self.history.record_merge(
                        EditCommand::Selection {
                            before,
                            after,
                            shift,
                        },
                        MergeKey::Nudge,
                    );
                    self.committed_selection = Some(after);
                }
            }
            self.redraw(hwnd);
        }
        LRESULT(0)
    }

    fn handle_char(&mut self, hwnd: HWND, ch_code: u32) -> LRESULT {
        if let Some(digits) = &mut self.thickness_input {
            if let Some(digit @ '0'..='9') = char::from_u32(ch_code)
                && digits.len() < 2
            {
                let next = digits.parse::<i32>().unwrap_or(0) * 10 + (digit as i32 - '0' as i32);
                if (1..=64).contains(&next) {
                    digits.push(digit);
                    self.redraw_chrome(hwnd);
                }
            }
            return LRESULT(0);
        }
        if let Some(edit) = &mut self.text_edit {
            edit.insert_utf16(ch_code);
            edit.caret_visible = true;
            start_caret_timer(hwnd);
            self.redraw(hwnd);
        }
        LRESULT(0)
    }

    fn redraw(&mut self, hwnd: HWND) {
        self.base_cache.clear();
        self.redraw_chrome(hwnd);
        self.cache_requested = false;
    }

    fn redraw_chrome(&mut self, hwnd: HWND) {
        self.cache_requested = true;
        self.scene_dirty = true;
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }

    fn redraw_region(&mut self, hwnd: HWND, _region: Rect) {
        // Selection movement also relocates annotations and both floating panels. A full
        // frame invalidation is deterministic even when third-party overlays hook GDI.
        self.redraw(hwnd);
    }

    fn commit_selection(&mut self, hwnd: HWND, rect: Rect) {
        let rect = rect.clamp(self.capture.width, self.capture.height);
        if rect.width() < 8 || rect.height() < 8 {
            self.mode = OverlayMode::Hovering;
            self.committed_selection = None;
            self.redraw(hwnd);
            return;
        }
        self.mode = OverlayMode::SelectionActive;
        self.committed_selection = Some(rect);
        self.hover_snap_rect = None;
        self.selected_id = None;
        self.redraw(hwnd);
    }

    fn show_action_error(hwnd: HWND, action: &str, error: &str) {
        ACTION_ERROR
            .with(|pending| *pending.borrow_mut() = Some((action.to_string(), error.to_string())));
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                hwnd,
                WM_EDITOR_ERROR,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    fn copy_selection_to_clipboard(&mut self, hwnd: HWND) -> std::result::Result<(), String> {
        let sel = self
            .committed_selection
            .ok_or_else(|| crate::i18n::t("Select a screen region first.").to_string())?;

        self.composite_scene_with(CompositionPolicy::EXPORT);

        let width = self.capture.width;
        let height = self.capture.height;
        let len = (width as usize) * (height as usize) * 4;
        let buffer = unsafe { std::slice::from_raw_parts(self.bits_ptr, len) };
        let result = flatten_selection_to_dib(buffer, width, height, &sel)
            .map_err(|error| {
                crate::i18n::tf("Could not prepare the clipboard image: {}", &[&error])
            })
            .and_then(|dib| {
                copy_dib_to_clipboard(Some(hwnd), &dib).map_err(|error| {
                    crate::i18n::tf("Windows rejected the clipboard image: {}", &[&error])
                })
            });

        if result.is_err() || !self.settings.close_after_action {
            self.composite_scene();
        }
        if result.is_ok() {
            self.committed_result = true;
        }
        result
    }

    fn save_selection_to_file(&mut self) -> std::result::Result<std::path::PathBuf, String> {
        let sel = self
            .committed_selection
            .ok_or_else(|| crate::i18n::t("Select a screen region first.").to_string())?;

        self.composite_scene_with(CompositionPolicy::EXPORT);

        let width = self.capture.width;
        let height = self.capture.height;
        let len = (width as usize) * (height as usize) * 4;
        let buffer = unsafe { std::slice::from_raw_parts(self.bits_ptr, len) };
        let result = save_screenshot(
            buffer,
            width,
            height,
            &sel,
            Some(&self.settings.save_directory),
            self.settings.save_format,
            self.settings.jpeg_quality,
        );

        if result.is_err() || !self.settings.close_after_action {
            self.composite_scene();
        }
        if result.is_ok() {
            self.committed_result = true;
        }
        result
    }

    /// Lightshot-style upload: encode the selection, hand it to the upload toast
    /// (which sends it in the background and shows the link), and close the
    /// editor right away. The first upload opens Cloudflare setup instead.
    fn upload_selection(&mut self, hwnd: HWND) -> std::result::Result<(), String> {
        let selection = self
            .committed_selection
            .ok_or_else(|| crate::i18n::t("Select a screen region first.").to_string())?;
        if self.settings.cloud_url.is_none() {
            if self.setup_pending {
                return Ok(());
            }
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    hwnd,
                    WM_EDITOR_CLOUD_SETUP,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .map_err(|error| crate::i18n::tf("Could not open Cloudflare setup: {}", &[&error]))?;
            self.setup_pending = true;
            return Ok(());
        }
        self.composite_scene_with(CompositionPolicy::EXPORT);
        let width = self.capture.width;
        let height = self.capture.height;
        let pixels = unsafe {
            std::slice::from_raw_parts(self.bits_ptr, width as usize * height as usize * 4)
        };
        let prepared = crate::upload::prepare(pixels, width, height, &selection, &self.settings);
        self.composite_scene();
        let prepared = prepared?;
        let anchor = windows::Win32::Foundation::POINT {
            x: self.capture.x + (selection.left + selection.right) / 2,
            y: self.capture.y + (selection.top + selection.bottom) / 2,
        };
        crate::toast::show_upload(prepared, anchor);
        self.committed_result = true;
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                hwnd,
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(|error| crate::i18n::tf("Could not close the editor: {}", &[&error]))?;
        Ok(())
    }

    fn editor_cursor(&self, pt: (i32, i32)) -> PCWSTR {
        if self.mode != OverlayMode::SelectionActive {
            return IDC_CROSS;
        }
        if self.text_edit.is_some() {
            return IDC_IBEAM;
        }
        let Some(selection) = self.committed_selection else {
            return IDC_CROSS;
        };
        let zone = hit_selection(&selection, pt, self.dpi);
        match zone {
            SelectionHitZone::TopLeftCorner | SelectionHitZone::BottomRightCorner => {
                return IDC_SIZENWSE;
            }
            SelectionHitZone::TopRightCorner | SelectionHitZone::BottomLeftCorner => {
                return IDC_SIZENESW;
            }
            SelectionHitZone::TopEdge | SelectionHitZone::BottomEdge => return IDC_SIZENS,
            SelectionHitZone::LeftEdge | SelectionHitZone::RightEdge => return IDC_SIZEWE,
            _ => {}
        }
        if self.hud_rect.is_some_and(|hud| hud.contains(pt.0, pt.1)) {
            return IDC_SIZEALL;
        }
        if self.active_tool != ToolKind::Select {
            // Drawing tools only act inside the selection.
            return if !selection.contains(pt.0, pt.1) {
                IDC_ARROW
            } else if self.active_tool == ToolKind::Text {
                IDC_IBEAM
            } else {
                IDC_CROSS
            };
        }

        if let Some(object) = self
            .selected_id
            .and_then(|id| self.objects.iter().find(|object| object.id == id))
        {
            if let Some(handle) =
                object.hit_resize_handle(pt, object_grip_radius(self.dpi), self.dpi)
            {
                return match handle {
                    AnnotationResizeHandle::TopLeft | AnnotationResizeHandle::BottomRight => {
                        IDC_SIZENWSE
                    }
                    AnnotationResizeHandle::TopRight | AnnotationResizeHandle::BottomLeft => {
                        IDC_SIZENESW
                    }
                    AnnotationResizeHandle::ArrowStart | AnnotationResizeHandle::ArrowEnd => {
                        IDC_SIZEALL
                    }
                };
            }
            if object.geometry_bounds().inflate(4, 4).contains(pt.0, pt.1) {
                return IDC_SIZEALL;
            }
        }

        match zone {
            SelectionHitZone::TopLeftCorner | SelectionHitZone::BottomRightCorner => IDC_SIZENWSE,
            SelectionHitZone::TopRightCorner | SelectionHitZone::BottomLeftCorner => IDC_SIZENESW,
            SelectionHitZone::TopEdge | SelectionHitZone::BottomEdge => IDC_SIZENS,
            SelectionHitZone::LeftEdge | SelectionHitZone::RightEdge => IDC_SIZEWE,
            SelectionHitZone::BorderEdge => IDC_SIZEALL,
            SelectionHitZone::Interior => {
                if self.topmost_object_at(pt).is_some() {
                    IDC_SIZEALL
                } else {
                    IDC_ARROW
                }
            }
            SelectionHitZone::None => IDC_CROSS,
        }
    }

    /// The topmost object whose visible stroke is under `pt`; a hollow frame's interior
    /// only counts when no stroke is hit, so objects inside a frame stay selectable.
    fn topmost_object_at(&self, pt: (i32, i32)) -> Option<&AnnotationObject> {
        self.objects
            .iter()
            .rev()
            .find(|object| object.hit_test_stroke(pt))
            .or_else(|| self.objects.iter().rev().find(|object| object.hit_test(pt)))
    }

    fn change_color(&mut self, color: [u8; 4]) {
        self.active_color = color;
        if let Some(object) = self
            .selected_id
            .and_then(|id| self.objects.iter_mut().find(|object| object.id == id))
        {
            let before = object.kind.clone();
            object.set_color(color);
            if before != object.kind {
                self.history.record(EditCommand::Modify {
                    id: object.id,
                    old_kind: before,
                    new_kind: object.kind.clone(),
                });
            }
        }
        self.persist_editor_preferences();
    }

    fn change_thickness(&mut self, value: i32, record: bool) {
        self.active_thickness = value.clamp(1, 64);
        if let Some(object) = self
            .selected_id
            .and_then(|id| self.objects.iter_mut().find(|object| object.id == id))
        {
            let before = object.kind.clone();
            object.set_thickness(self.active_thickness);
            if record && before != object.kind {
                self.history.record_merge(
                    EditCommand::Modify {
                        id: object.id,
                        old_kind: before,
                        new_kind: object.kind.clone(),
                    },
                    MergeKey::Thickness,
                );
            }
        }
        if record {
            self.persist_editor_preferences();
        }
    }
    fn persist_editor_preferences(&mut self) {
        self.settings.default_color = self.active_color;
        self.settings.default_thickness = self.active_thickness;
        self.settings.last_tool = self.active_tool;
        self.preferences_dirty = true;
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

unsafe extern "system" fn overlay_wnd_proc(
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
                                    DragObjectAction::Resize(handle) => {
                                        object.resize_from(original_kind, handle, pt, selection);
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
                                InProgressDrawing::Rectangle { start, current } => {
                                    *current = clamp_point(
                                        if shift_down {
                                            snap_square(*start, pt)
                                        } else {
                                            pt
                                        },
                                        region,
                                    );
                                }
                                InProgressDrawing::Arrow { start, current } => {
                                    *current = clamp_point(
                                        if shift_down {
                                            snap_angle_45(*start, pt)
                                        } else {
                                            pt
                                        },
                                        pixel_bounds(region),
                                    );
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
                                let at = edit.caret_at(client_x);
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
                                let at = edit.caret_at(client_x);
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
                                    action: DragObjectAction::Resize(handle),
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
                                ToolKind::Arrow => {
                                    state.drawing_shape = Some(InProgressDrawing::Arrow {
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
                                    state.text_edit = Some(TextEditState::new(
                                        pt,
                                        String::new(),
                                        state.active_color,
                                        DEFAULT_FONT_SIZE * state.dpi as i32 / 96,
                                        None,
                                    ));
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
                            let new_obj = match shape {
                                InProgressDrawing::Rectangle { start, current } => {
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
                                            AnnotationKind::Rectangle {
                                                rect: r,
                                                color: state.active_color,
                                                thickness: state.active_thickness,
                                            },
                                        ))
                                    } else {
                                        None
                                    }
                                }
                                InProgressDrawing::Arrow { start, current } => {
                                    let final_cur = clamp_point(
                                        if shift_down {
                                            snap_angle_45(start, current)
                                        } else {
                                            current
                                        },
                                        pixel_bounds(region),
                                    );
                                    let dx = (final_cur.0 - start.0).abs();
                                    let dy = (final_cur.1 - start.1).abs();
                                    if dx >= 3 || dy >= 3 {
                                        Some(AnnotationObject::new(
                                            state.next_id,
                                            AnnotationKind::Arrow {
                                                start,
                                                end: final_cur,
                                                color: state.active_color,
                                                thickness: state.active_thickness,
                                            },
                                        ))
                                    } else {
                                        None
                                    }
                                }
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

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: Rect = Rect::new(0, 0, 1920, 1040);

    fn test_state() -> OverlayState {
        OverlayState::create_test_state(Rc::new(CaptureBuffer::dummy(800, 600)))
    }

    fn state_with_rectangle() -> OverlayState {
        let mut state = test_state();
        state.set_selection_active(Rect::new(100, 100, 500, 400));
        state.add_test_rectangle(Rect::new(150, 150, 250, 250));
        state
    }

    #[test]
    fn hud_goes_above_the_selection_when_nothing_blocks_it() {
        let selection = Rect::new(400, 300, 1000, 700);
        assert_eq!(
            place_capture_hud(selection, VIEWPORT, 96, &[]),
            Rect::new(400, 262, 536, 290)
        );
    }

    #[test]
    fn hud_drops_below_a_selection_touching_the_top() {
        let selection = Rect::new(200, 0, 900, 400);
        assert_eq!(
            place_capture_hud(selection, VIEWPORT, 96, &[]),
            Rect::new(200, 410, 336, 438)
        );
    }

    #[test]
    fn hud_moves_below_when_the_action_bar_sits_above() {
        let selection = Rect::new(400, 300, 1000, 700);
        let action_bar = Rect::new(348, 250, 632, 292);
        assert_eq!(
            place_capture_hud(selection, VIEWPORT, 96, &[action_bar]),
            Rect::new(400, 710, 536, 738)
        );
    }

    #[test]
    fn hud_sits_inside_past_the_corner_grip_for_a_full_screen_selection() {
        let hud = place_capture_hud(VIEWPORT, VIEWPORT, 96, &[]);
        assert_eq!(hud, Rect::new(16, 16, 152, 44));
        let grips = selection_grip_boxes(&VIEWPORT.selection_frame(96), 4);
        assert!(grips.iter().all(|grip| !rects_intersect(&hud, grip)));
    }

    #[test]
    fn hud_falls_back_inside_when_every_outside_spot_is_blocked() {
        let selection = Rect::new(400, 300, 1000, 700);
        let above = Rect::new(0, 200, 1920, 300);
        let below = Rect::new(0, 690, 1920, 800);
        assert_eq!(
            place_capture_hud(selection, VIEWPORT, 96, &[above, below]),
            Rect::new(416, 316, 552, 344)
        );
    }

    #[test]
    fn hud_scales_with_dpi() {
        let selection = Rect::new(400, 300, 1000, 700);
        let hud = place_capture_hud(selection, VIEWPORT, 192, &[]);
        assert_eq!((hud.width(), hud.height()), (272, 56));
        assert!(hud.bottom <= selection.top);
    }

    #[test]
    fn rect_intersection_ignores_touching_and_empty_rects() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(!rects_intersect(&a, &Rect::new(10, 0, 20, 10)));
        assert!(rects_intersect(&a, &Rect::new(5, 5, 15, 15)));
        assert!(!rects_intersect(&a, &Rect::default()));
        assert_eq!(
            intersect_rects(a, Rect::new(5, 5, 15, 15)),
            Rect::new(5, 5, 10, 10)
        );
        assert!(intersect_rects(a, Rect::new(20, 20, 30, 30)).is_empty());
    }

    #[test]
    fn drag_starts_only_beyond_the_threshold() {
        let threshold = (4, 4);
        assert!(!exceeds_drag_threshold((50, 50), (54, 46), threshold));
        assert!(exceeds_drag_threshold((50, 50), (55, 50), threshold));
        assert!(exceeds_drag_threshold((50, 50), (50, 44), threshold));
    }

    #[test]
    fn resize_keeps_the_offset_the_edge_was_grabbed_at() {
        let selection = Rect::new(100, 100, 300, 200);
        assert_eq!(
            resize_grab_offset(SelectionHitZone::TopLeftCorner, selection, (103, 98)),
            (3, -2)
        );
        assert_eq!(
            resize_grab_offset(SelectionHitZone::RightEdge, selection, (297, 150)),
            (-3, 0)
        );
        assert_eq!(
            resize_grab_offset(SelectionHitZone::BottomEdge, selection, (200, 204)),
            (0, 4)
        );
    }

    #[test]
    fn drawing_points_stay_inside_the_selection() {
        let selection = Rect::new(100, 100, 300, 200);
        assert_eq!(clamp_point((50, 400), selection), (100, 200));
        assert_eq!(clamp_point((50, 400), pixel_bounds(selection)), (100, 199));
        assert_eq!(clamp_point((150, 150), selection), (150, 150));
    }

    #[test]
    fn escape_cancels_a_shape_without_touching_the_session() {
        let mut state = state_with_rectangle();
        state.begin_test_rectangle_gesture((300, 300), (350, 350));
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert!(!state.has_active_gesture());
        assert_eq!(state.object_count(), 1);
        assert_eq!(state.mode(), OverlayMode::SelectionActive);
        assert!(state.history.can_undo());
    }

    #[test]
    fn escape_restores_a_dragged_object() {
        let mut state = state_with_rectangle();
        let id = state.objects[0].id;
        let original_kind = state.objects[0].kind.clone();
        state.objects[0].translate(30, 20);
        state.dragging_object = Some(DragObjectState {
            id,
            action: DragObjectAction::Move,
            last_pos: (0, 0),
            original_kind: original_kind.clone(),
        });
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert_eq!(state.objects[0].kind, original_kind);
        assert!(state.dragging_object.is_none());
    }

    #[test]
    fn escape_restores_a_moved_selection_and_its_objects() {
        let mut state = state_with_rectangle();
        let original = state.committed_selection().unwrap();
        let original_kind = state.objects[0].kind.clone();
        state.dragging_selection = Some(DragSelectionState::new(
            original,
            DragSelectionAction::Move,
            (200, 200),
        ));
        state.committed_selection = Some(Rect::new(140, 125, 540, 425));
        state.objects[0].translate(40, 25);
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert_eq!(state.committed_selection(), Some(original));
        assert_eq!(state.objects[0].kind, original_kind);
    }

    #[test]
    fn escape_restores_a_resized_selection_without_moving_objects() {
        let mut state = state_with_rectangle();
        let original = state.committed_selection().unwrap();
        let original_kind = state.objects[0].kind.clone();
        state.dragging_selection = Some(DragSelectionState::new(
            original,
            DragSelectionAction::Resize(SelectionHitZone::RightEdge),
            (500, 250),
        ));
        state.committed_selection = Some(Rect::new(100, 100, 460, 400));
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert_eq!(state.committed_selection(), Some(original));
        assert_eq!(state.objects[0].kind, original_kind);
    }

    #[test]
    fn escape_restores_a_thickness_drag() {
        let mut state = state_with_rectangle();
        let id = state.objects[0].id;
        let original_kind = state.objects[0].kind.clone();
        let start_value = state.active_thickness;
        state.selected_id = Some(id);
        state.thickness_drag = Some(ThicknessDrag {
            start_y: 0,
            start_value,
            picked: start_value,
            moved: true,
        });
        state.thickness_drag_original = Some((id, original_kind.clone()));
        state.change_thickness(start_value + 5, false);
        assert_ne!(state.objects[0].kind, original_kind);
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert_eq!(state.objects[0].kind, original_kind);
        assert_eq!(state.active_thickness, start_value);
    }

    #[test]
    fn a_pending_click_outside_is_cancelled_without_discarding_anything() {
        let mut state = state_with_rectangle();
        state.pending_reselect = Some((10, 10));
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert!(state.pending_reselect.is_none());
        assert_eq!(state.object_count(), 1);
        assert_eq!(state.mode(), OverlayMode::SelectionActive);
    }

    #[test]
    fn a_cancelled_session_comes_back_with_its_history() {
        let mut state = state_with_rectangle();
        assert_eq!(
            state.handle_escape_action(),
            EscapeAction::CancelledSelection
        );
        assert_eq!(state.object_count(), 0);
        assert!(!state.history.can_undo());
        assert!(state.restore_discarded_session());
        assert_eq!(state.mode(), OverlayMode::SelectionActive);
        assert_eq!(state.object_count(), 1);
        assert!(state.history.can_undo());
        assert!(!state.restore_discarded_session());
    }

    #[test]
    fn escape_during_a_new_region_puts_the_old_session_back() {
        let mut state = state_with_rectangle();
        let original = state.committed_selection().unwrap();
        state.begin_reselect((10, 10));
        assert_eq!(state.mode(), OverlayMode::DraggingSelection);
        assert_eq!(state.object_count(), 0);
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert_eq!(state.mode(), OverlayMode::SelectionActive);
        assert_eq!(state.committed_selection(), Some(original));
        assert_eq!(state.object_count(), 1);
    }

    #[test]
    fn escape_cancels_a_pressed_toolbar_button_only() {
        let mut state = state_with_rectangle();
        let selection = state.committed_selection().unwrap();
        let mut toolbar = Toolbar::layout(
            &selection,
            Rect::new(0, 0, 800, 600),
            ToolKind::Rectangle,
            state.active_color,
            state.active_thickness,
            false,
            true,
            true,
            false,
            true,
            false,
            96,
        );
        toolbar.pressed_item = Some(ToolbarItem::Action(ToolbarAction::Cancel));
        state.toolbar = Some(toolbar);
        assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
        assert_eq!(state.toolbar.as_ref().unwrap().pressed_item, None);
        assert_eq!(state.mode(), OverlayMode::SelectionActive);
        assert_eq!(state.object_count(), 1);
    }

    #[test]
    fn grip_zones_follow_the_frame_drawn_outside_the_selection() {
        let selection = Rect::new(100, 100, 300, 200);
        // The frame is two pixels out at 96 DPI, so the top-left grip is centred on (98, 98).
        assert_eq!(
            hit_selection(&selection, (95, 95), 96),
            SelectionHitZone::TopLeftCorner
        );
        assert_ne!(
            hit_selection(&selection, (103, 103), 96),
            SelectionHitZone::TopLeftCorner
        );
        assert_eq!(
            hit_selection(&selection, (200, 95), 96),
            SelectionHitZone::TopEdge
        );
    }

    #[test]
    fn editing_text_forwards_word_shortcuts_to_the_overlay() {
        let ctrl = MOD_CONTROL.0;
        for vk in [8, 46, 0x58, 0x41, 0x56, 37, 39] {
            assert!(is_editor_shortcut(vk, ctrl, true), "vk {vk:#x}");
        }
        // Outside a text box those keys belong to the focused application.
        for vk in [8, 46, 0x58] {
            assert!(!is_editor_shortcut(vk, ctrl, false), "vk {vk:#x}");
        }
    }

    #[test]
    fn caret_blinks_at_the_system_rate_unless_blinking_is_off() {
        assert_eq!(caret_blink_interval(530), Some(530));
        assert_eq!(caret_blink_interval(u32::MAX), None);
        assert_eq!(caret_blink_interval(0), Some(500));
    }

    #[test]
    fn fade_runs_from_nearly_clear_to_opaque_and_stays_there() {
        assert_eq!(fade_alpha(0), 1);
        assert_eq!(fade_alpha(FADE_IN_MS), 255);
        assert_eq!(fade_alpha(FADE_IN_MS * 10), 255);
        let steps: Vec<u8> = (0..=FADE_IN_MS).step_by(8).map(fade_alpha).collect();
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]));
        // Ease-out: more than half of the opacity is reached by the midpoint.
        assert!(fade_alpha(FADE_IN_MS / 2) > 128);
    }
}
