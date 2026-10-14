//! Compact native settings dialog in the Windows 11 style: a selector bar of
//! tabs on top, label/control rows with toggle switches and native combo boxes
//! per tab, and a footer band with the commit buttons. No scrolling.
use crate::hotkey::HotkeyConfig;
use crate::i18n::LanguagePreference;
use crate::settings::{PRESET_COLORS, SaveFormat, Settings};
use crate::theme::ThemePreference;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DC_BRUSH, DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetDC, GetMonitorInfoW, GetStockObject, HBRUSH, HDC, HFONT,
    HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MapWindowPoints,
    MonitorFromPoint, MonitorFromWindow, PAINTSTRUCT, PS_SOLID, ReleaseDC, SelectObject,
    SetBkColor, SetBkMode, SetDCBrushColor, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, SetActiveWindow, SetFocus, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

const SETTINGS_CLASS_NAME: PCWSTR = w!("isolmaSS_SettingsClass");

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateStatus {
    Idle,
    Busy,
    Ready,
}

pub struct SettingsWindowState {
    settings: Settings,
    saved: bool,
    update_status: UpdateStatus,
    update_frame: u8,
    tab: usize,
    recording_hotkey: bool,
    original_hotkey: HotkeyConfig,
    updating_thickness: bool,
    dpi: u32,
    owner: Option<HWND>,
    font: HFONT,
    content_brush: HBRUSH,
    footer_brush: HBRUSH,
    input_brush: HBRUSH,
}

impl SettingsWindowState {
    fn refresh_brushes(&mut self) {
        let tokens = crate::theme::tokens();
        unsafe {
            for brush in [self.content_brush, self.footer_brush, self.input_brush] {
                if !brush.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                }
            }
            // Windows 11 dialogs: a lighter content layer over a darker command bar.
            self.content_brush = CreateSolidBrush(tokens.card);
            self.footer_brush = CreateSolidBrush(tokens.page);
            self.input_brush = CreateSolidBrush(tokens.control_fill);
        }
    }

    fn refresh_fonts(&mut self, hwnd: HWND) {
        let previous = self.font;
        self.font = crate::theme::create_ui_font(self.dpi, crate::theme::FONT_BODY_PX, 400);
        for id in ALL_CONTROLS {
            if let Some(child) = control(hwnd, *id) {
                unsafe {
                    SendMessageW(child, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
                }
            }
        }
        if !previous.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(previous.0));
            }
        }
    }
}

impl Drop for SettingsWindowState {
    fn drop(&mut self) {
        if !self.font.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }
        for brush in [self.content_brush, self.footer_brush, self.input_brush] {
            if !brush.is_invalid() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                }
            }
        }
    }
}

const ID_SAVE: i32 = 100;
const ID_CANCEL: i32 = 101;
const ID_BROWSE: i32 = 102;
const ID_CHECK_UPDATE: i32 = 103;
const ID_FOLDER_LABEL: i32 = 104;
const ID_UPDATE_STATUS: i32 = 108;
const ID_UPDATE_PROGRESS: i32 = 109;
const ID_OPEN_CLOUD: i32 = 110;
const ID_CLOUD_STATUS: i32 = 111;
const ID_CLOUD_NOTE: i32 = 112;
const ID_VERSION: i32 = 113;
/// Notes under a control: the shortcut recorder's rejection, the language
/// hint and the line width hint.
const ID_HOTKEY_NOTE: i32 = 114;
const ID_LANGUAGE_NOTE: i32 = 115;
const ID_THICKNESS_NOTE: i32 = 116;
const ID_HOTKEY_RECORD: i32 = 200;
const ID_COLOR_FIRST: i32 = 300;
const ID_COLOR_LAST: i32 = ID_COLOR_FIRST + PALETTE.len() as i32 - 1;
const ID_COLOR_CUSTOM: i32 = 330;
const ID_THICKNESS_SLIDER: i32 = 340;
const ID_THICKNESS_EDIT: i32 = 343;
const ID_TAB_FIRST: i32 = 800;
const TAB_COUNT: usize = 5;
const TAB_EDITOR: usize = 2;
const ID_TAB_LAST: i32 = ID_TAB_FIRST + TAB_COUNT as i32 - 1;

fn tab_names() -> [&'static str; TAB_COUNT] {
    [
        crate::i18n::t("General"),
        crate::i18n::t("Saving"),
        crate::i18n::t("Editor"),
        crate::i18n::t("Sharing"),
        crate::i18n::t("Updates"),
    ]
}

/// Every colour is one click away (BGRA). The first six and the last two of
/// the top row match the toolbar presets.
const PALETTE: [[u8; 4]; 20] = [
    PRESET_COLORS[0],
    PRESET_COLORS[1],
    PRESET_COLORS[2],
    PRESET_COLORS[3],
    PRESET_COLORS[4],
    PRESET_COLORS[5],
    [128, 73, 230, 255], // Pink #E64980
    [153, 133, 12, 255], // Teal #0C8599
    PRESET_COLORS[6],
    PRESET_COLORS[7],
    [135, 135, 255, 255], // Light red #FF8787
    [77, 169, 255, 255],  // Light orange #FFA94D
    [102, 224, 255, 255], // Light yellow #FFE066
    [154, 233, 140, 255], // Light green #8CE99A
    [252, 192, 116, 255], // Light blue #74C0FC
    [242, 119, 218, 255], // Light purple #DA77F2
    [219, 91, 59, 255],   // Indigo #3B5BDB
    [13, 148, 92, 255],   // Olive #5C940D
    [150, 142, 134, 255], // Grey #868E96
    [87, 80, 73, 255],    // Dark grey #495057
];
const ID_WINDOW_SNAP: i32 = 400;
const ID_CLOSE_AFTER_ACTION: i32 = 401;
const ID_START_WITH_WINDOWS: i32 = 402;
const ID_NOTIFY_AFTER_SAVE: i32 = 403;
const ID_CHECK_UPDATES: i32 = 404;
const ID_THEME: i32 = 406;
const ID_LANGUAGE: i32 = 407;
const ID_DELAY: i32 = 500;
const ID_FORMAT: i32 = 600;
const ID_QUALITY: i32 = 610;

const ID_LABEL_HOTKEY: i32 = 701;
const ID_LABEL_COLOR: i32 = 703;
const ID_LABEL_THICKNESS: i32 = 704;
const ID_LABEL_DELAY: i32 = 705;
const ID_LABEL_FORMAT: i32 = 706;
const ID_LABEL_QUALITY: i32 = 707;
const ID_LABEL_FOLDER: i32 = 709;
const ID_LABEL_THEME: i32 = 716;
const ID_LABEL_LANGUAGE: i32 = 717;
const ID_LABEL_PX: i32 = 720;

const ALL_CONTROLS: &[i32] = &[
    ID_SAVE,
    ID_CANCEL,
    ID_BROWSE,
    ID_CHECK_UPDATE,
    ID_FOLDER_LABEL,
    ID_UPDATE_STATUS,
    ID_OPEN_CLOUD,
    ID_HOTKEY_RECORD,
    ID_COLOR_CUSTOM,
    ID_THICKNESS_EDIT,
    ID_WINDOW_SNAP,
    ID_CLOSE_AFTER_ACTION,
    ID_START_WITH_WINDOWS,
    ID_NOTIFY_AFTER_SAVE,
    ID_CHECK_UPDATES,
    ID_THEME,
    ID_LANGUAGE,
    ID_DELAY,
    ID_FORMAT,
    ID_QUALITY,
    ID_LABEL_HOTKEY,
    ID_LABEL_COLOR,
    ID_LABEL_THICKNESS,
    ID_LABEL_DELAY,
    ID_LABEL_FORMAT,
    ID_LABEL_QUALITY,
    ID_LABEL_FOLDER,
    ID_LABEL_THEME,
    ID_LABEL_LANGUAGE,
    ID_LABEL_PX,
    ID_CLOUD_STATUS,
    ID_CLOUD_NOTE,
    ID_VERSION,
    ID_HOTKEY_NOTE,
    ID_LANGUAGE_NOTE,
    ID_THICKNESS_NOTE,
    ID_TAB_FIRST,
    ID_TAB_FIRST + 1,
    ID_TAB_FIRST + 2,
    ID_TAB_FIRST + 3,
    ID_TAB_FIRST + 4,
];

/// The tab a control belongs to; `None` for the tab bar and the footer.
fn tab_of(id: i32) -> Option<usize> {
    Some(match id {
        ID_LABEL_HOTKEY
        | ID_HOTKEY_RECORD
        | ID_HOTKEY_NOTE
        | ID_LANGUAGE_NOTE
        | ID_LABEL_DELAY
        | ID_DELAY
        | ID_LABEL_THEME
        | ID_THEME
        | ID_LABEL_LANGUAGE
        | ID_LANGUAGE
        | ID_START_WITH_WINDOWS
        | ID_NOTIFY_AFTER_SAVE => 0,
        ID_LABEL_FOLDER | ID_FOLDER_LABEL | ID_BROWSE | ID_LABEL_FORMAT | ID_FORMAT
        | ID_LABEL_QUALITY | ID_QUALITY => 1,
        ID_LABEL_COLOR
        | ID_COLOR_FIRST..=ID_COLOR_LAST
        | ID_COLOR_CUSTOM
        | ID_LABEL_THICKNESS
        | ID_THICKNESS_SLIDER
        | ID_THICKNESS_EDIT
        | ID_THICKNESS_NOTE
        | ID_LABEL_PX
        | ID_WINDOW_SNAP
        | ID_CLOSE_AFTER_ACTION => 2,
        ID_CLOUD_NOTE | ID_CLOUD_STATUS | ID_OPEN_CLOUD => 3,
        ID_CHECK_UPDATES | ID_CHECK_UPDATE | ID_UPDATE_STATUS | ID_UPDATE_PROGRESS | ID_VERSION => {
            4
        }
        _ => return None,
    })
}

fn show_tab(hwnd: HWND, state: &SettingsWindowState) {
    let ids = ALL_CONTROLS
        .iter()
        .copied()
        .chain(ID_COLOR_FIRST..=ID_COLOR_LAST)
        .chain([ID_THICKNESS_SLIDER, ID_UPDATE_PROGRESS]);
    for id in ids {
        let Some(tab) = tab_of(id) else { continue };
        let visible = tab == state.tab
            && (id != ID_UPDATE_PROGRESS || state.update_status == UpdateStatus::Busy);
        if let Some(child) = control(hwnd, id) {
            unsafe {
                let _ = ShowWindow(child, if visible { SW_SHOW } else { SW_HIDE });
            }
        }
    }
    for index in 0..TAB_COUNT {
        set_check(hwnd, ID_TAB_FIRST + index as i32, index == state.tab);
    }
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
}

fn delay_choices() -> [(u32, &'static str); 4] {
    [
        (0, crate::i18n::t("None")),
        (1000, crate::i18n::t("1 second")),
        (3000, crate::i18n::t("3 seconds")),
        (5000, crate::i18n::t("5 seconds")),
    ]
}
const QUALITIES: [u8; 3] = [80, 90, 100];
/// `TBM_GETPOS` (WM_USER); the windows crate does not export this trackbar message.
const TBM_GETPOS: u32 = WM_USER;
const EM_SETLIMITTEXT: u32 = 0x00C5;

// Design grid (96-DPI pixels).
const WIDTH: i32 = 480;
const TAB_BAR: i32 = 48;
const MARGIN: i32 = 20;
const CONTROL_X: i32 = 170;
const ROW: i32 = 30;
const PITCH: i32 = 36;
const FOOTER: i32 = 60;
/// A one-line note under a control, and the room it adds to the row above.
const NOTE: i32 = 20;
const NOTE_GAP: i32 = 18;
/// Fluent text box around the line width edit: the edit sits this far inside
/// its painted frame (horizontal, vertical), in design pixels.
const EDIT_FRAME_INSET: (i32, i32) = (3, 4);
/// Inner text margin of that edit, in design pixels.
const EDIT_MARGIN: i32 = 2;

fn tokens() -> crate::theme::Tokens {
    crate::theme::tokens()
}

fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn create_control(
    parent: HWND,
    class_name: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    id: i32,
) -> Result<HWND> {
    let text = wide_string(text);
    unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            parent,
            HMENU(id as *mut std::ffi::c_void),
            HINSTANCE::default(),
            None,
        )
    }
}

fn create_button(parent: HWND, id: i32, text: &str, style: i32) -> Result<HWND> {
    create_control(
        parent,
        w!("BUTTON"),
        text,
        WINDOW_STYLE(style as u32 | WS_TABSTOP.0),
        id,
    )
}

fn create_combo(parent: HWND, id: i32) -> Result<HWND> {
    create_control(
        parent,
        w!("COMBOBOX"),
        "",
        WINDOW_STYLE(WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32),
        id,
    )
}

/// An empty label under a control; `right` aligns its text to the right
/// edge. A `single_line` note ends a long message in an ellipsis
/// (SS_ENDELLIPSIS, 0x4000, which forces one line); any other note wraps and
/// its row is sized with [`measure_note`].
fn create_note(parent: HWND, id: i32, right: bool, single_line: bool) -> Result<HWND> {
    let alignment = if right { 0x0000_0002 } else { 0 };
    let ellipsis = if single_line { 0x0000_4000 } else { 0 };
    create_control(
        parent,
        w!("STATIC"),
        "",
        WINDOW_STYLE(ellipsis | alignment),
        id,
    )
}

/// Design pixels the text of note `id` needs when word-wrapped to `width`
/// design pixels in the note's font: at least one line, 0 while it is empty.
fn measure_note(hwnd: HWND, dpi: u32, id: i32, width: i32) -> i32 {
    let Some(note) = control(hwnd, id) else {
        return 0;
    };
    let length = unsafe { GetWindowTextLengthW(note) }.max(0) as usize;
    if length == 0 {
        return 0;
    }
    let mut text = vec![0u16; length + 1];
    let copied = unsafe { GetWindowTextW(note, &mut text) }.max(0) as usize;
    text.truncate(copied);
    let font = HGDIOBJ(unsafe { SendMessageW(note, WM_GETFONT, WPARAM(0), LPARAM(0)) }.0 as *mut _);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(width, dpi),
        bottom: 0,
    };
    unsafe {
        let dc = GetDC(hwnd);
        let previous = SelectObject(dc, font);
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(dc, previous);
        ReleaseDC(hwnd, dc);
    }
    // Rounded up so the measured text always fits.
    ((rect.bottom * 96 + dpi as i32 - 1) / dpi as i32).max(NOTE)
}

fn control(hwnd: HWND, id: i32) -> Option<HWND> {
    unsafe { GetDlgItem(hwnd, id) }.ok()
}

fn set_text(hwnd: HWND, id: i32, text: &str) {
    if let Some(child) = control(hwnd, id) {
        let text = wide_string(text);
        unsafe {
            let _ = SetWindowTextW(child, PCWSTR(text.as_ptr()));
        }
    }
}

fn place(hwnd: HWND, id: i32, (x, y, width, height): (i32, i32, i32, i32), dpi: u32) {
    if let Some(child) = control(hwnd, id) {
        unsafe {
            let _ = MoveWindow(
                child,
                scale(x, dpi),
                scale(y, dpi),
                scale(width, dpi),
                scale(height, dpi),
                false,
            );
        }
    }
}

/// Room a shortcut note of `height` design pixels adds below the shortcut row
/// (0 while the note is empty): the rows below move down to just under it.
fn warning_room(height: i32) -> i32 {
    if height == 0 {
        0
    } else {
        ROW + height + 4 - PITCH
    }
}

/// Lays out every control and returns the client height in design pixels.
fn layout(hwnd: HWND, dpi: u32) -> i32 {
    let right = WIDTH - MARGIN;
    let full = WIDTH - 2 * MARGIN;
    let column = right - CONTROL_X;
    let label = |id: i32, y: i32| place(hwnd, id, (MARGIN, y + 6, CONTROL_X - MARGIN - 8, 20), dpi);

    // Selector bar: content-width tabs.
    let mut x = MARGIN - 10;
    for (index, name) in tab_names().iter().enumerate() {
        let width = crate::drawing::measure_ui_text(name).0 + 2 * crate::drawing::TAB_PADDING;
        place(hwnd, ID_TAB_FIRST + index as i32, (x, 6, width, 40), dpi);
        x += width;
    }
    let top = TAB_BAR + 16;
    let mut bottom = top;

    // General
    let mut y = top;
    label(ID_LABEL_HOTKEY, y);
    place(hwnd, ID_HOTKEY_RECORD, (CONTROL_X, y, column, ROW), dpi);
    // The rejection note takes room below the row only while it is shown.
    let warning = measure_note(hwnd, dpi, ID_HOTKEY_NOTE, column);
    place(
        hwnd,
        ID_HOTKEY_NOTE,
        (CONTROL_X, y + ROW, column, warning),
        dpi,
    );
    y += PITCH + warning_room(warning);
    label(ID_LABEL_DELAY, y);
    place(hwnd, ID_DELAY, (CONTROL_X, y, 150, 200), dpi);
    y += PITCH;
    label(ID_LABEL_THEME, y);
    place(hwnd, ID_THEME, (CONTROL_X, y, 150, 200), dpi);
    y += PITCH;
    label(ID_LABEL_LANGUAGE, y);
    place(hwnd, ID_LANGUAGE, (CONTROL_X, y, 150, 200), dpi);
    let language_note = measure_note(hwnd, dpi, ID_LANGUAGE_NOTE, column).max(NOTE);
    place(
        hwnd,
        ID_LANGUAGE_NOTE,
        (CONTROL_X, y + ROW, column, language_note),
        dpi,
    );
    y += ROW + language_note + 8;
    place(hwnd, ID_START_WITH_WINDOWS, (MARGIN, y, full, ROW), dpi);
    y += PITCH;
    place(hwnd, ID_NOTIFY_AFTER_SAVE, (MARGIN, y, full, ROW), dpi);
    // The window always has the room one line of the shortcut note needs, so
    // showing it does not resize the dialog.
    bottom = bottom.max(y + PITCH + warning_room(NOTE) - warning_room(warning));

    // Saving
    let mut y = top;
    label(ID_LABEL_FOLDER, y);
    place(
        hwnd,
        ID_FOLDER_LABEL,
        (CONTROL_X, y + 6, column - 96, 20),
        dpi,
    );
    place(hwnd, ID_BROWSE, (right - 88, y, 88, ROW), dpi);
    y += PITCH;
    label(ID_LABEL_FORMAT, y);
    place(hwnd, ID_FORMAT, (CONTROL_X, y, 150, 200), dpi);
    y += PITCH;
    label(ID_LABEL_QUALITY, y);
    place(hwnd, ID_QUALITY, (CONTROL_X, y, 150, 200), dpi);
    bottom = bottom.max(y + PITCH);

    // Editor
    let mut y = top;
    place(hwnd, ID_LABEL_COLOR, (MARGIN, y + 6, full - 100, 20), dpi);
    place(hwnd, ID_COLOR_CUSTOM, (right - 92, y, 92, ROW), dpi);
    y += PITCH;
    let per_row = 10;
    let cell = full / per_row;
    for index in 0..PALETTE.len() as i32 {
        place(
            hwnd,
            ID_COLOR_FIRST + index,
            (
                MARGIN + index % per_row * cell,
                y + index / per_row * 34,
                cell,
                32,
            ),
            dpi,
        );
    }
    y += 2 * 34 + 10;
    label(ID_LABEL_THICKNESS, y);
    place(
        hwnd,
        ID_THICKNESS_SLIDER,
        (CONTROL_X - 6, y, column - 64, ROW),
        dpi,
    );
    // The edit sits inside the 40 × 28 text box painted around it.
    let (inset_x, inset_y) = EDIT_FRAME_INSET;
    place(
        hwnd,
        ID_THICKNESS_EDIT,
        (
            right - 62 + inset_x,
            y + 1 + inset_y,
            40 - 2 * inset_x,
            28 - 2 * inset_y,
        ),
        dpi,
    );
    place(hwnd, ID_LABEL_PX, (right - 18, y + 6, 18, 20), dpi);
    place(
        hwnd,
        ID_THICKNESS_NOTE,
        (CONTROL_X, y + ROW, right - CONTROL_X, NOTE),
        dpi,
    );
    y += PITCH + 4 + NOTE_GAP;
    place(hwnd, ID_WINDOW_SNAP, (MARGIN, y, full, ROW), dpi);
    y += PITCH;
    place(hwnd, ID_CLOSE_AFTER_ACTION, (MARGIN, y, full, ROW), dpi);
    bottom = bottom.max(y + PITCH);

    // Sharing
    let mut y = top;
    place(hwnd, ID_CLOUD_NOTE, (MARGIN, y, full, 60), dpi);
    y += 70;
    place(hwnd, ID_CLOUD_STATUS, (MARGIN, y + 6, full - 190, 20), dpi);
    place(hwnd, ID_OPEN_CLOUD, (right - 180, y, 180, ROW), dpi);
    bottom = bottom.max(y + PITCH);

    // Updates
    let mut y = top;
    place(hwnd, ID_CHECK_UPDATES, (MARGIN, y, full, ROW), dpi);
    y += PITCH + 4;
    place(hwnd, ID_VERSION, (MARGIN, y + 6, full - 140, 20), dpi);
    place(hwnd, ID_CHECK_UPDATE, (right - 128, y, 128, ROW), dpi);
    y += PITCH;
    place(hwnd, ID_UPDATE_STATUS, (MARGIN, y, full, 40), dpi);
    place(hwnd, ID_UPDATE_PROGRESS, (MARGIN, y + 44, full, 8), dpi);
    bottom = bottom.max(y + 56);

    let y = bottom + 8;
    let footer_y = y + (FOOTER - ROW - 2) / 2;
    // Primary action first, as in Windows 11 dialogs.
    place(hwnd, ID_SAVE, (right - 196, footer_y, 96, ROW + 2), dpi);
    place(hwnd, ID_CANCEL, (right - 92, footer_y, 92, ROW + 2), dpi);
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
    y + FOOTER
}

fn footer_top(hwnd: HWND, dpi: u32) -> i32 {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    client.bottom - scale(FOOTER, dpi)
}

fn paint_surface(hwnd: HWND, state: &SettingsWindowState, hdc: HDC) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
        let _ = FillRect(hdc, &client, state.content_brush);
        let footer = RECT {
            top: footer_top(hwnd, state.dpi),
            ..client
        };
        let _ = FillRect(hdc, &footer, state.footer_brush);
    }
    let top = footer_top(hwnd, state.dpi);
    let bar = scale(TAB_BAR, state.dpi);
    crate::drawing::with_pen(hdc, PS_SOLID, 1, tokens().stroke, || unsafe {
        let _ = windows::Win32::Graphics::Gdi::Polyline(
            hdc,
            &[
                POINT { x: 0, y: top },
                POINT {
                    x: client.right,
                    y: top,
                },
            ],
        );
        let _ = windows::Win32::Graphics::Gdi::Polyline(
            hdc,
            &[
                POINT { x: 0, y: bar },
                POINT {
                    x: client.right,
                    y: bar,
                },
            ],
        );
    });
    if let Some((frame, focused)) = thickness_frame(hwnd, state.dpi) {
        draw_text_box(hdc, frame, focused, state.dpi);
    }
}

/// The Fluent text box around the line width edit, in client coordinates and
/// whether the edit has focus; `None` while its tab is hidden.
fn thickness_frame(hwnd: HWND, dpi: u32) -> Option<(RECT, bool)> {
    let edit = control(hwnd, ID_THICKNESS_EDIT)?;
    if !unsafe { IsWindowVisible(edit) }.as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(edit, &mut rect) }.is_err() {
        return None;
    }
    let mut corners = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];
    unsafe {
        MapWindowPoints(HWND::default(), hwnd, &mut corners);
    }
    let (inset_x, inset_y) = EDIT_FRAME_INSET;
    let (inset_x, inset_y) = (scale(inset_x, dpi), scale(inset_y, dpi));
    let frame = RECT {
        left: corners[0].x - inset_x,
        top: corners[0].y - inset_y,
        right: corners[1].x + inset_x,
        bottom: corners[1].y + inset_y,
    };
    Some((frame, unsafe { GetFocus() } == edit))
}

/// Windows 11 text box: a stroked field with a 2 px accent line along the
/// bottom edge while it has focus.
fn draw_text_box(hdc: HDC, frame: RECT, focused: bool, dpi: u32) {
    let tokens = tokens();
    let diameter = crate::drawing::corner_diameter(crate::theme::RADIUS_CONTROL, dpi);
    crate::drawing::rounded(
        hdc,
        crate::capture::Rect::new(frame.left, frame.top, frame.right, frame.bottom),
        diameter,
        tokens.control_fill,
        tokens.stroke,
    );
    if focused {
        let line = RECT {
            left: frame.left + diameter / 2,
            top: frame.bottom - scale(2, dpi).max(2),
            right: frame.right - diameter / 2,
            bottom: frame.bottom,
        };
        // The stock DC brush takes its color from the DC: nothing to create or delete.
        unsafe {
            let brush = HBRUSH(GetStockObject(DC_BRUSH).0);
            SetDCBrushColor(hdc, tokens.accent_ink);
            let _ = FillRect(hdc, &line, brush);
        }
    }
}

fn in_footer(id: i32) -> bool {
    matches!(id, ID_SAVE | ID_CANCEL)
}

fn draw_slider(draw: &windows::Win32::UI::Controls::NMCUSTOMDRAW, state: &SettingsWindowState) {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(draw.hdr.hwndFrom, &mut rect);
        let _ = FillRect(draw.hdc, &rect, state.content_brush);
    }
    let tokens = tokens();
    let left = scale(10, state.dpi);
    let right = rect.right - left;
    let center = (rect.top + rect.bottom) / 2;
    let value =
        unsafe { SendMessageW(draw.hdr.hwndFrom, TBM_GETPOS, WPARAM(0), LPARAM(0)).0 as i32 }
            .clamp(1, 64);
    let knob = left + (right - left).max(0) * (value - 1) / 63;
    let rail = scale(2, state.dpi).max(2);
    crate::drawing::rounded(
        draw.hdc,
        crate::capture::Rect::new(left, center - rail, right + 1, center + rail),
        rail * 2,
        tokens.stroke,
        tokens.stroke,
    );
    crate::drawing::rounded(
        draw.hdc,
        crate::capture::Rect::new(left, center - rail, knob + 1, center + rail),
        rail * 2,
        tokens.accent_ink,
        tokens.accent_ink,
    );
    // Fluent thumb: neutral outer disc with an accent core.
    let outer = scale(10, state.dpi);
    let inner = scale(5, state.dpi);
    crate::drawing::rounded(
        draw.hdc,
        crate::capture::Rect::new(knob - outer, center - outer, knob + outer, center + outer),
        outer * 2,
        tokens.control_fill,
        tokens.stroke,
    );
    crate::drawing::rounded(
        draw.hdc,
        crate::capture::Rect::new(knob - inner, center - inner, knob + inner, center + inner),
        inner * 2,
        tokens.accent_ink,
        tokens.accent_ink,
    );
}

fn draw_progress(draw: &windows::Win32::UI::Controls::NMCUSTOMDRAW, state: &SettingsWindowState) {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(draw.hdr.hwndFrom, &mut rect);
        let _ = FillRect(draw.hdc, &rect, state.content_brush);
    }
    let width = rect.right - rect.left;
    if width <= 2 {
        return;
    }
    let tokens = tokens();
    let top = scale(2, state.dpi);
    let bottom = top + scale(3, state.dpi).max(2);
    let corner = crate::drawing::corner_diameter(crate::theme::RADIUS_CONTROL, state.dpi);
    crate::drawing::rounded(
        draw.hdc,
        crate::capture::Rect::new(1, top, width - 1, bottom),
        corner,
        tokens.stroke,
        tokens.stroke,
    );
    let segment = (width / 5).max(scale(32, state.dpi)).min(width - 2);
    let travel = width - 2 - segment;
    let phase = i32::from(state.update_frame);
    let offset = if phase < 18 {
        travel * phase / 18
    } else {
        travel * (36 - phase) / 18
    };
    crate::drawing::rounded(
        draw.hdc,
        crate::capture::Rect::new(1 + offset, top, 1 + offset + segment, bottom),
        corner,
        tokens.accent_ink,
        tokens.accent_ink,
    );
}

/// Windows 11 buttons, tabs, toggle switches and colour swatches.
fn draw_button(draw: &windows::Win32::UI::Controls::NMCUSTOMDRAW, state: &SettingsWindowState) {
    use crate::drawing::{Align, Look, fluent_button, fluent_tab, fluent_toggle, rounded, ui_text};
    let hdc = draw.hdc;
    let id = draw.hdr.idFrom as i32;
    let dpi = state.dpi;
    let tokens = tokens();
    let look = Look {
        primary: id == ID_SAVE,
        ..Look::from_custom_draw(draw)
    };
    let background = if in_footer(id) {
        state.footer_brush
    } else {
        state.content_brush
    };
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(draw.hdr.hwndFrom, &mut rect);
    }
    let mut label = [0u16; 128];
    let length = unsafe { GetWindowTextW(draw.hdr.hwndFrom, &mut label) }.max(0) as usize;
    let text = &mut label[..length];

    if (ID_TAB_FIRST..=ID_TAB_LAST).contains(&id) {
        fluent_tab(hdc, rect, text, dpi, look, background);
    } else if (ID_WINDOW_SNAP..=ID_CHECK_UPDATES).contains(&id) {
        fluent_toggle(hdc, rect, text, dpi, look, background);
    } else if (ID_COLOR_FIRST..=ID_COLOR_LAST).contains(&id) {
        unsafe {
            let _ = FillRect(hdc, &rect, background);
        }
        let color = crate::annotation::bgra_to_colorref(PALETTE[(id - ID_COLOR_FIRST) as usize]);
        let diameter = scale(20, dpi);
        let x = (rect.right - diameter) / 2;
        let y = (rect.bottom - diameter) / 2;
        if look.selected || look.hot {
            // A ring around the chip: accent (2 px) when chosen, subtle on hover.
            let ring = scale(4, dpi);
            let ring_color = if look.selected {
                tokens.accent_ink
            } else {
                tokens.stroke
            };
            for inset in 0..if look.selected { 2 } else { 1 } {
                rounded(
                    hdc,
                    crate::capture::Rect::new(
                        x - ring + inset,
                        y - ring + inset,
                        x + diameter + ring - inset,
                        y + diameter + ring - inset,
                    ),
                    diameter + ring * 2,
                    tokens.card,
                    ring_color,
                );
            }
        }
        rounded(
            hdc,
            crate::capture::Rect::new(x, y, x + diameter, y + diameter),
            diameter,
            color,
            tokens.stroke,
        );
        if look.focus && !look.selected {
            let chip = RECT {
                left: x,
                top: y,
                right: x + diameter,
                bottom: y + diameter,
            };
            crate::drawing::focus_ring(hdc, chip, diameter / 2, dpi);
        }
    } else if id == ID_COLOR_CUSTOM {
        // The button shows the current colour; its dot is ringed in accent
        // when that colour is not one of the palette chips.
        fluent_button(hdc, rect, &mut [], dpi, look, background);
        let custom = state.settings.default_color;
        let dot = scale(12, dpi);
        let x = scale(10, dpi);
        let y = (rect.bottom - dot) / 2;
        rounded(
            hdc,
            crate::capture::Rect::new(x, y, x + dot, y + dot),
            dot,
            crate::annotation::bgra_to_colorref(custom),
            if PALETTE.contains(&custom) {
                tokens.stroke
            } else {
                tokens.accent_ink
            },
        );
        let mut text_rect = RECT {
            left: x + dot + scale(6, dpi),
            right: rect.right - scale(6, dpi),
            ..rect
        };
        ui_text(hdc, text, &mut text_rect, dpi, tokens.text, Align::Left);
    } else if id == ID_HOTKEY_RECORD && state.recording_hotkey {
        // Listening: a 2 px accent outline on the accent tint.
        unsafe {
            let _ = FillRect(hdc, &rect, background);
        }
        let corner = crate::drawing::corner_diameter(crate::theme::RADIUS_CONTROL, dpi);
        let line = scale(2, dpi).max(2);
        for (inset, fill, diameter) in [
            (1, tokens.accent_ink, corner),
            (1 + line, tokens.accent_tint, (corner - 2 * line).max(2)),
        ] {
            rounded(
                hdc,
                crate::capture::Rect::new(
                    rect.left + inset,
                    rect.top + inset,
                    rect.right - inset,
                    rect.bottom - inset,
                ),
                diameter,
                fill,
                fill,
            );
        }
        let mut text_rect = RECT {
            left: rect.left + scale(8, dpi),
            right: rect.right - scale(8, dpi),
            ..rect
        };
        ui_text(hdc, text, &mut text_rect, dpi, tokens.text, Align::Center);
    } else {
        fluent_button(hdc, rect, text, dpi, look, background);
    }
}

fn create_settings_controls(hwnd: HWND) -> Result<()> {
    let label = |id, text| create_control(hwnd, w!("STATIC"), text, WINDOW_STYLE(0), id);
    for (index, name) in tab_names().iter().enumerate() {
        create_button(
            hwnd,
            ID_TAB_FIRST + index as i32,
            name,
            BS_AUTORADIOBUTTON | BS_PUSHLIKE | if index == 0 { WS_GROUP.0 as i32 } else { 0 },
        )?;
    }
    label(ID_LABEL_HOTKEY, crate::i18n::t("Shortcut"))?;
    // BS_NOTIFY reports BN_KILLFOCUS, which ends a recording in progress.
    create_button(
        hwnd,
        ID_HOTKEY_RECORD,
        crate::i18n::t("Record shortcut"),
        BS_PUSHBUTTON | BS_NOTIFY | WS_GROUP.0 as i32,
    )?;
    create_note(hwnd, ID_HOTKEY_NOTE, false, false)?;
    label(ID_LABEL_DELAY, crate::i18n::t("Delay"))?;
    create_combo(hwnd, ID_DELAY)?;

    label(ID_LABEL_FOLDER, crate::i18n::t("Folder"))?;
    // SS_PATHELLIPSIS keeps the drive and file name of long paths visible.
    create_control(
        hwnd,
        w!("STATIC"),
        "",
        WINDOW_STYLE(0x0000_8000),
        ID_FOLDER_LABEL,
    )?;
    create_button(hwnd, ID_BROWSE, crate::i18n::t("Browse…"), BS_PUSHBUTTON)?;
    label(ID_LABEL_FORMAT, crate::i18n::t("Format"))?;
    create_combo(hwnd, ID_FORMAT)?;
    label(ID_LABEL_QUALITY, crate::i18n::t("JPEG quality"))?;
    create_combo(hwnd, ID_QUALITY)?;

    label(ID_LABEL_COLOR, crate::i18n::t("Default color"))?;
    // The swatches render as circles; the window text stays for screen readers.
    for (index, name) in [
        crate::i18n::t("Red"),
        crate::i18n::t("Orange"),
        crate::i18n::t("Yellow"),
        crate::i18n::t("Green"),
        crate::i18n::t("Blue"),
        crate::i18n::t("Purple"),
        crate::i18n::t("Pink"),
        crate::i18n::t("Teal"),
        crate::i18n::t("White"),
        crate::i18n::t("Black"),
        crate::i18n::t("Light red"),
        crate::i18n::t("Light orange"),
        crate::i18n::t("Light yellow"),
        crate::i18n::t("Light green"),
        crate::i18n::t("Light blue"),
        crate::i18n::t("Light purple"),
        crate::i18n::t("Indigo"),
        crate::i18n::t("Olive"),
        crate::i18n::t("Grey"),
        crate::i18n::t("Dark grey"),
    ]
    .into_iter()
    .enumerate()
    {
        create_button(
            hwnd,
            ID_COLOR_FIRST + index as i32,
            name,
            BS_AUTORADIOBUTTON | if index == 0 { WS_GROUP.0 as i32 } else { 0 },
        )?;
    }
    create_button(
        hwnd,
        ID_COLOR_CUSTOM,
        crate::i18n::t("Custom…"),
        BS_PUSHBUTTON | WS_GROUP.0 as i32,
    )?;
    label(ID_LABEL_THICKNESS, crate::i18n::t("Line width"))?;
    create_control(
        hwnd,
        w!("msctls_trackbar32"),
        "",
        WINDOW_STYLE(WS_TABSTOP.0),
        ID_THICKNESS_SLIDER,
    )?;
    create_control(
        hwnd,
        w!("EDIT"),
        "4",
        WINDOW_STYLE(WS_TABSTOP.0 | ES_NUMBER as u32 | ES_CENTER as u32),
        ID_THICKNESS_EDIT,
    )?;
    label(ID_LABEL_PX, "px")?;
    create_note(hwnd, ID_THICKNESS_NOTE, true, true)?;
    create_button(
        hwnd,
        ID_WINDOW_SNAP,
        crate::i18n::t("Snap to windows"),
        BS_AUTOCHECKBOX,
    )?;
    create_button(
        hwnd,
        ID_CLOSE_AFTER_ACTION,
        crate::i18n::t("Close after an action"),
        BS_AUTOCHECKBOX,
    )?;

    label(ID_LABEL_THEME, crate::i18n::t("Theme"))?;
    create_combo(hwnd, ID_THEME)?;
    label(ID_LABEL_LANGUAGE, crate::i18n::t("Language"))?;
    create_combo(hwnd, ID_LANGUAGE)?;
    create_note(hwnd, ID_LANGUAGE_NOTE, false, false)?;
    set_text(
        hwnd,
        ID_LANGUAGE_NOTE,
        crate::i18n::t("Applies after you save and reopen Settings."),
    );
    create_button(
        hwnd,
        ID_START_WITH_WINDOWS,
        crate::i18n::t("Start with Windows"),
        BS_AUTOCHECKBOX,
    )?;
    create_button(
        hwnd,
        ID_NOTIFY_AFTER_SAVE,
        crate::i18n::t("Notify after saving"),
        BS_AUTOCHECKBOX,
    )?;
    create_button(
        hwnd,
        ID_CHECK_UPDATES,
        crate::i18n::t("Check for updates automatically"),
        BS_AUTOCHECKBOX,
    )?;
    label(ID_VERSION, concat!("isolmaSS ", env!("CARGO_PKG_VERSION")))?;
    create_button(
        hwnd,
        ID_CHECK_UPDATE,
        crate::i18n::t("Check now"),
        BS_PUSHBUTTON,
    )?;
    label(
        ID_UPDATE_STATUS,
        crate::i18n::t(
            "Only releases signed by the publisher are installed. Installing restarts the app.",
        ),
    )?;
    label(
        ID_CLOUD_NOTE,
        crate::i18n::t(
            "Upload sends the screenshot to the Worker in your own Cloudflare account and copies the link. Images go to no other server.",
        ),
    )?;
    // SS_ENDELLIPSIS keeps a long Worker address on one line.
    create_control(
        hwnd,
        w!("STATIC"),
        "",
        WINDOW_STYLE(0x0000_4000),
        ID_CLOUD_STATUS,
    )?;
    let progress = create_control(
        hwnd,
        w!("BUTTON"),
        "",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        ID_UPDATE_PROGRESS,
    )?;
    unsafe {
        let _ = ShowWindow(progress, SW_HIDE);
    }

    create_button(hwnd, ID_OPEN_CLOUD, "Cloudflare…", BS_PUSHBUTTON)?;
    // Created in visual order so Tab follows it.
    create_button(
        hwnd,
        ID_SAVE,
        crate::i18n::t("Save"),
        BS_DEFPUSHBUTTON | WS_GROUP.0 as i32,
    )?;
    create_button(hwnd, ID_CANCEL, crate::i18n::t("Cancel"), BS_PUSHBUTTON)?;

    if let Some(slider) = control(hwnd, ID_THICKNESS_SLIDER) {
        unsafe {
            SendMessageW(
                slider,
                windows::Win32::UI::Controls::TBM_SETRANGE,
                WPARAM(1),
                LPARAM(((64 << 16) | 1) as isize),
            );
        }
    }
    if let Some(edit) = control(hwnd, ID_THICKNESS_EDIT) {
        unsafe {
            SendMessageW(edit, EM_SETLIMITTEXT, WPARAM(2), LPARAM(0));
        }
    }
    Ok(())
}

/// Inner text margins of the line width edit, inside its painted frame.
fn apply_edit_margins(hwnd: HWND, dpi: u32) {
    if let Some(edit) = control(hwnd, ID_THICKNESS_EDIT) {
        let margin = scale(EDIT_MARGIN, dpi).max(1) as isize;
        unsafe {
            SendMessageW(
                edit,
                windows::Win32::UI::Controls::EM_SETMARGINS,
                WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize),
                LPARAM(margin | (margin << 16)),
            );
        }
    }
}

/// Fills a combo box with `(value, label)` items and selects `current`; a value
/// outside the presets (hand-edited settings) is kept as an extra item.
fn fill_combo(hwnd: HWND, id: i32, items: &[(i64, String)], current: i64) {
    let Some(combo) = control(hwnd, id) else {
        return;
    };
    unsafe {
        SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    }
    let mut selected = None;
    for (index, (value, text)) in items.iter().enumerate() {
        let text = wide_string(text);
        unsafe {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(text.as_ptr() as isize),
            );
            SendMessageW(
                combo,
                CB_SETITEMDATA,
                WPARAM(index),
                LPARAM(*value as isize),
            );
        }
        if *value == current {
            selected = Some(index);
        }
    }
    unsafe {
        SendMessageW(
            combo,
            CB_SETCURSEL,
            WPARAM(selected.unwrap_or(usize::MAX)),
            LPARAM(0),
        );
    }
}

fn combo_value(hwnd: HWND, id: i32) -> Option<i64> {
    let combo = control(hwnd, id)?;
    let index = unsafe { SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 };
    if index < 0 {
        return None;
    }
    Some(unsafe { SendMessageW(combo, CB_GETITEMDATA, WPARAM(index as usize), LPARAM(0)).0 } as i64)
}

fn set_check(hwnd: HWND, id: i32, checked: bool) {
    if let Some(control) = control(hwnd, id) {
        unsafe {
            SendMessageW(
                control,
                BM_SETCHECK,
                WPARAM(if checked {
                    windows::Win32::UI::Controls::BST_CHECKED.0 as usize
                } else {
                    windows::Win32::UI::Controls::BST_UNCHECKED.0 as usize
                }),
                LPARAM(0),
            );
        }
    }
}

fn set_quality_enabled(hwnd: HWND, enabled: bool) {
    if let Some(combo) = control(hwnd, ID_QUALITY) {
        unsafe {
            let _ = EnableWindow(combo, enabled);
        }
    }
    // The label stays enabled (a disabled STATIC is drawn embossed);
    // WM_CTLCOLORSTATIC greys it instead.
    if let Some(label) = control(hwnd, ID_LABEL_QUALITY) {
        unsafe {
            let _ = InvalidateRect(label, None, true);
        }
    }
}

fn refresh_hotkey(hwnd: HWND, settings: &Settings) {
    set_text(
        hwnd,
        ID_HOTKEY_RECORD,
        &crate::i18n::tf("{} · Change", &[&settings.hotkey.description]),
    );
}

/// The swatch that matches the default color is checked; the custom button
/// shows the color itself.
fn refresh_colors(hwnd: HWND, settings: &Settings) {
    for (index, color) in PALETTE.iter().enumerate() {
        set_check(
            hwnd,
            ID_COLOR_FIRST + index as i32,
            *color == settings.default_color,
        );
    }
    if let Some(custom) = control(hwnd, ID_COLOR_CUSTOM) {
        unsafe {
            let _ = InvalidateRect(custom, None, false);
        }
    }
}

fn refresh_cloud(hwnd: HWND, settings: &Settings) {
    match settings.cloud_url.as_deref() {
        Some(url) => {
            set_text(
                hwnd,
                ID_CLOUD_STATUS,
                &crate::i18n::tf("Connected · {}", &[&url.trim_start_matches("https://")]),
            );
            set_text(hwnd, ID_OPEN_CLOUD, crate::i18n::t("Manage connection…"));
        }
        None => {
            set_text(hwnd, ID_CLOUD_STATUS, crate::i18n::t("Not connected"));
            set_text(
                hwnd,
                ID_OPEN_CLOUD,
                crate::i18n::t("Connect to Cloudflare…"),
            );
        }
    }
}

fn initialize_control_values(hwnd: HWND, settings: &Settings) {
    refresh_hotkey(hwnd, settings);
    let mut delays: Vec<(i64, String)> = delay_choices()
        .iter()
        .map(|(value, text)| (*value as i64, (*text).to_owned()))
        .collect();
    if !delay_choices()
        .iter()
        .any(|(value, _)| *value == settings.capture_delay_ms)
    {
        delays.push((
            settings.capture_delay_ms as i64,
            format!("{} ms", settings.capture_delay_ms),
        ));
    }
    fill_combo(hwnd, ID_DELAY, &delays, settings.capture_delay_ms as i64);
    fill_combo(
        hwnd,
        ID_FORMAT,
        &[(0, "PNG".into()), (1, "JPEG".into())],
        (settings.save_format == SaveFormat::Jpeg) as i64,
    );
    let mut qualities: Vec<(i64, String)> = QUALITIES
        .iter()
        .map(|value| (*value as i64, crate::i18n::tf("{}%", &[value])))
        .collect();
    if !QUALITIES.contains(&settings.jpeg_quality) {
        qualities.push((
            settings.jpeg_quality as i64,
            crate::i18n::tf("{}%", &[&settings.jpeg_quality]),
        ));
    }
    fill_combo(hwnd, ID_QUALITY, &qualities, settings.jpeg_quality as i64);
    set_quality_enabled(hwnd, settings.save_format == SaveFormat::Jpeg);
    fill_combo(
        hwnd,
        ID_THEME,
        &[
            (0, crate::i18n::t("System").into()),
            (1, crate::i18n::t("Light").into()),
            (2, crate::i18n::t("Dark").into()),
        ],
        match settings.theme_preference {
            ThemePreference::System => 0,
            ThemePreference::Light => 1,
            ThemePreference::Dark => 2,
        },
    );
    fill_combo(
        hwnd,
        ID_LANGUAGE,
        &[
            (0, crate::i18n::t("Windows language").into()),
            (1, "English".into()),
            (2, "Türkçe".into()),
        ],
        match settings.language {
            LanguagePreference::System => 0,
            LanguagePreference::English => 1,
            LanguagePreference::Turkish => 2,
        },
    );
    refresh_colors(hwnd, settings);
    if let Some(slider) = control(hwnd, ID_THICKNESS_SLIDER) {
        unsafe {
            SendMessageW(
                slider,
                windows::Win32::UI::Controls::TBM_SETPOS,
                WPARAM(1),
                LPARAM(settings.default_thickness as isize),
            );
        }
    }
    set_text(
        hwnd,
        ID_THICKNESS_EDIT,
        &settings.default_thickness.to_string(),
    );
    set_check(hwnd, ID_WINDOW_SNAP, settings.enable_window_snap);
    set_check(hwnd, ID_CLOSE_AFTER_ACTION, settings.close_after_action);
    set_check(hwnd, ID_START_WITH_WINDOWS, settings.start_with_windows);
    set_check(hwnd, ID_NOTIFY_AFTER_SAVE, settings.notify_after_save);
    set_check(hwnd, ID_CHECK_UPDATES, settings.check_updates_automatically);
    update_folder_label(hwnd, &settings.save_directory);
    refresh_cloud(hwnd, settings);
}

fn update_folder_label(hwnd: HWND, path: &Path) {
    set_text(hwnd, ID_FOLDER_LABEL, &path.display().to_string());
}

pub(crate) fn report_update_status(owner: HWND, message: &str, status: UpdateStatus) -> bool {
    if control(owner, ID_UPDATE_STATUS).is_none() {
        return false;
    }
    let state_ptr = unsafe { GetWindowLongPtrW(owner, GWLP_USERDATA) } as *mut SettingsWindowState;
    let Some(state) = (unsafe { state_ptr.as_mut() }) else {
        return false;
    };
    set_update_status(owner, state, message, status);
    true
}

fn set_update_status(
    owner: HWND,
    state: &mut SettingsWindowState,
    message: &str,
    status: UpdateStatus,
) {
    let was_busy = state.update_status == UpdateStatus::Busy;
    state.update_status = status;
    if status == UpdateStatus::Busy && !was_busy {
        state.update_frame = 0;
        if unsafe { SetTimer(owner, 2, 75, None) } == 0 {
            crate::diagnostics::record(
                "settings update progress",
                "Could not start progress animation.",
            );
        }
    } else if status != UpdateStatus::Busy && was_busy {
        unsafe {
            let _ = KillTimer(owner, 2);
        }
    }
    set_text(owner, ID_UPDATE_STATUS, message);
    unsafe {
        if let Some(button) = control(owner, ID_CHECK_UPDATE) {
            let _ = EnableWindow(button, status == UpdateStatus::Idle);
        }
        if let Some(progress) = control(owner, ID_UPDATE_PROGRESS) {
            let _ = ShowWindow(
                progress,
                if status == UpdateStatus::Busy && state.tab == 4 {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
    }
}

fn apply_theme(hwnd: HWND, state: &mut SettingsWindowState) {
    crate::theme::set_preference(state.settings.theme_preference);
    unsafe {
        crate::theme::apply_window_theme(
            hwnd,
            crate::theme::theme() == crate::theme::Theme::Dark,
            false,
        );
    }
    state.refresh_brushes();
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
            hwnd,
            None,
            None,
            windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                | windows::Win32::Graphics::Gdi::RDW_ERASE
                | windows::Win32::Graphics::Gdi::RDW_FRAME
                | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN,
        );
    }
}

/// A combo box selection changed.
fn apply_selection(hwnd: HWND, state: &mut SettingsWindowState, id: i32) {
    let Some(value) = combo_value(hwnd, id) else {
        return;
    };
    match id {
        ID_DELAY => state.settings.capture_delay_ms = value.clamp(0, u32::MAX as i64) as u32,
        ID_FORMAT => {
            state.settings.save_format = if value == 1 {
                SaveFormat::Jpeg
            } else {
                SaveFormat::Png
            };
            set_quality_enabled(hwnd, value == 1);
        }
        ID_QUALITY => state.settings.jpeg_quality = value.clamp(1, 100) as u8,
        ID_THEME => {
            state.settings.theme_preference = match value {
                1 => ThemePreference::Light,
                2 => ThemePreference::Dark,
                _ => ThemePreference::System,
            };
            apply_theme(hwnd, state);
        }
        // Takes effect for windows opened after Save.
        ID_LANGUAGE => {
            state.settings.language = match value {
                1 => LanguagePreference::English,
                2 => LanguagePreference::Turkish,
                _ => LanguagePreference::System,
            };
        }
        _ => {}
    }
}

/// Starts listening for a new shortcut on the record button.
fn begin_recording(hwnd: HWND, state: &mut SettingsWindowState) {
    state.recording_hotkey = true;
    crate::hotkey::set_recording_window(Some(hwnd));
    set_hotkey_note(hwnd, state, "");
    set_text(
        hwnd,
        ID_HOTKEY_RECORD,
        crate::i18n::t("Press keys · Esc cancels"),
    );
    if let Some(button) = control(hwnd, ID_HOTKEY_RECORD) {
        unsafe {
            let _ = SetFocus(button);
        }
    }
}

/// Stops listening and restores the button label; no-op when not recording.
fn end_recording(hwnd: HWND, state: &mut SettingsWindowState) {
    if !state.recording_hotkey {
        return;
    }
    state.recording_hotkey = false;
    crate::hotkey::set_recording_window(None);
    refresh_hotkey(hwnd, &state.settings);
}

/// Shows `text` (empty: hides it) in the note under the shortcut button and
/// moves the rows below it; the window keeps its size unless the note needs
/// more than one line.
fn set_hotkey_note(hwnd: HWND, state: &SettingsWindowState, text: &str) {
    let shown =
        control(hwnd, ID_HOTKEY_NOTE).is_some_and(|note| unsafe { GetWindowTextLengthW(note) } > 0);
    if text.is_empty() && !shown {
        return;
    }
    set_text(hwnd, ID_HOTKEY_NOTE, text);
    let height = layout(hwnd, state.dpi);
    let (width, height) = window_size(hwnd, state.dpi, height);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        // Moved children are clipped out of the parent's repaint.
        let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
            hwnd,
            None,
            None,
            windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                | windows::Win32::Graphics::Gdi::RDW_ERASE
                | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN,
        );
    }
}

/// Empties a note without repainting a control that is already empty.
fn clear_note(hwnd: HWND, id: i32) {
    if control(hwnd, id).is_some_and(|note| unsafe { GetWindowTextLengthW(note) } > 0) {
        set_text(hwnd, id, "");
    }
}

fn apply_button_action(hwnd: HWND, state: &mut SettingsWindowState, id: i32) {
    match id {
        ID_OPEN_CLOUD => {
            match crate::cloud_settings_window::show(&state.settings, hwnd) {
                // The Cloudflare dialog persists its own change; keep it so a later
                // Save here cannot overwrite the new address.
                Ok(Some(settings)) => state.settings.cloud_url = settings.cloud_url,
                Ok(None) => {}
                Err(error) => crate::ui::error(
                    hwnd,
                    crate::i18n::t("Cloudflare settings"),
                    &error.to_string(),
                ),
            }
            refresh_cloud(hwnd, &state.settings);
        }
        ID_HOTKEY_RECORD => begin_recording(hwnd, state),
        ID_COLOR_FIRST..=ID_COLOR_LAST => {
            state.settings.default_color = PALETTE[(id - ID_COLOR_FIRST) as usize];
            refresh_colors(hwnd, &state.settings);
        }
        ID_TAB_FIRST..=ID_TAB_LAST => {
            state.tab = (id - ID_TAB_FIRST) as usize;
            show_tab(hwnd, state);
        }
        ID_COLOR_CUSTOM => {
            match crate::ui::choose_color(hwnd, state.settings.default_color) {
                Ok(Some(color)) => {
                    state.settings.default_color = color;
                    state.settings.last_custom_color = color;
                }
                Ok(None) => {}
                Err(error) => crate::ui::error(
                    hwnd,
                    crate::i18n::t("Could not open the color picker"),
                    &error.to_string(),
                ),
            }
            refresh_colors(hwnd, &state.settings);
        }
        ID_WINDOW_SNAP => state.settings.enable_window_snap = !state.settings.enable_window_snap,
        ID_CLOSE_AFTER_ACTION => {
            state.settings.close_after_action = !state.settings.close_after_action
        }
        ID_START_WITH_WINDOWS => {
            state.settings.start_with_windows = !state.settings.start_with_windows
        }
        ID_NOTIFY_AFTER_SAVE => {
            state.settings.notify_after_save = !state.settings.notify_after_save
        }
        ID_CHECK_UPDATES => {
            state.settings.check_updates_automatically = !state.settings.check_updates_automatically
        }
        ID_CHECK_UPDATE => {
            let message = if crate::updater::run_manual_update_check() {
                crate::i18n::t("Checking for the latest signed release…")
            } else {
                crate::i18n::t("An update is already in progress…")
            };
            set_update_status(hwnd, state, message, UpdateStatus::Busy);
        }
        ID_CANCEL => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ => {}
    }
}

fn commit(hwnd: HWND, state: &mut SettingsWindowState) {
    let mut digits = [0u16; 8];
    if let Some(edit) = control(hwnd, ID_THICKNESS_EDIT) {
        let count = unsafe { GetWindowTextW(edit, &mut digits) };
        if !String::from_utf16_lossy(&digits[..count.max(0) as usize])
            .parse::<i32>()
            .is_ok_and(|value| (1..=64).contains(&value))
        {
            // The field lives on the Editor tab: show it, say what is wrong
            // right under it and leave it ready for retyping.
            state.tab = TAB_EDITOR;
            show_tab(hwnd, state);
            set_text(
                hwnd,
                ID_THICKNESS_NOTE,
                crate::i18n::t("Enter a value from 1 to 64 pixels."),
            );
            unsafe {
                let _ = SetFocus(edit);
                SendMessageW(
                    edit,
                    windows::Win32::UI::Controls::EM_SETSEL,
                    WPARAM(0),
                    LPARAM(-1),
                );
            }
            return;
        }
    }
    let mut settings = state.settings.clone();
    match Settings::load() {
        Ok(latest) => settings.skipped_update_version = latest.skipped_update_version,
        Err(error) => crate::diagnostics::record("settings merge", &error.to_string()),
    }
    match save_settings(&settings) {
        Ok(()) => {
            state.settings = settings;
            state.saved = true;
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        Err(error) => crate::ui::error(hwnd, crate::i18n::t("Could not save settings"), &error),
    }
}

unsafe extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsWindowState;
    if state_ptr.is_null() {
        if msg == WM_NCDESTROY {
            unsafe {
                let _ = KillTimer(hwnd, 1);
                let _ = KillTimer(hwnd, 2);
            }
        }
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *state_ptr };
    match msg {
        WM_NCDESTROY => {
            if state.recording_hotkey {
                crate::hotkey::set_recording_window(None);
            }
            unsafe {
                let _ = KillTimer(hwnd, 1);
                let _ = KillTimer(hwnd, 2);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_TIMER if wparam.0 == 1 => {
            crate::updater::poll(hwnd, false);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == 2 => {
            if state.update_status == UpdateStatus::Busy {
                state.update_frame = (state.update_frame + 1) % 36;
                if let Some(progress) = control(hwnd, ID_UPDATE_PROGRESS) {
                    unsafe {
                        let _ = InvalidateRect(progress, None, false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY if lparam.0 != 0 => {
            use windows::Win32::UI::Controls::*;
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.code == NM_CUSTOMDRAW {
                let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                if draw.dwDrawStage == CDDS_PREPAINT {
                    match draw.hdr.idFrom as i32 {
                        ID_THICKNESS_SLIDER => draw_slider(draw, state),
                        ID_UPDATE_PROGRESS => draw_progress(draw, state),
                        _ => draw_button(draw, state),
                    }
                    return LRESULT(CDRF_SKIPDEFAULT as isize);
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
            paint_surface(hwnd, state, hdc);
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SETTINGCHANGE => {
            crate::theme::invalidate_theme_cache();
            if crate::theme::is_color_setting_change(lparam) {
                crate::theme::refresh_native_menus();
            }
            apply_theme(hwnd, state);
            LRESULT(0)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let hdc = HDC(wparam.0 as *mut _);
            let tokens = tokens();
            unsafe {
                let _ = SetTextColor(hdc, tokens.text);
                let _ = SetBkColor(hdc, tokens.control_fill);
            }
            LRESULT(state.input_brush.0 as isize)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let child = HWND(lparam.0 as *mut _);
            let id = unsafe { GetDlgCtrlID(child) };
            let hdc = HDC(wparam.0 as *mut _);
            let tokens = tokens();
            let quality_off = id == ID_LABEL_QUALITY
                && control(hwnd, ID_QUALITY)
                    .is_some_and(|combo| unsafe { !IsWindowEnabled(combo).as_bool() });
            unsafe {
                let _ = SetBkMode(hdc, TRANSPARENT);
                let _ = SetTextColor(
                    hdc,
                    if quality_off || !IsWindowEnabled(child).as_bool() {
                        tokens.text_disabled
                    } else if matches!(id, ID_HOTKEY_NOTE | ID_THICKNESS_NOTE) {
                        tokens.critical
                    } else if matches!(
                        id,
                        ID_UPDATE_STATUS | ID_FOLDER_LABEL | ID_LABEL_PX | ID_LANGUAGE_NOTE
                    ) {
                        tokens.text_secondary
                    } else {
                        tokens.text
                    },
                );
            }
            LRESULT(state.content_brush.0 as isize)
        }
        WM_HSCROLL => {
            let slider = HWND(lparam.0 as *mut _);
            if unsafe { GetDlgCtrlID(slider) } == ID_THICKNESS_SLIDER {
                let value =
                    unsafe { SendMessageW(slider, TBM_GETPOS, WPARAM(0), LPARAM(0)).0 as i32 }
                        .clamp(1, 64);
                state.settings.default_thickness = value;
                state.updating_thickness = true;
                set_text(hwnd, ID_THICKNESS_EDIT, &value.to_string());
                state.updating_thickness = false;
                clear_note(hwnd, ID_THICKNESS_NOTE);
                unsafe {
                    let _ = InvalidateRect(slider, None, false);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            let code = (wparam.0 >> 16) as u32;
            if id == ID_THICKNESS_EDIT {
                if code == EN_SETFOCUS || code == EN_KILLFOCUS {
                    // The accent line of the text box follows the focus.
                    if let Some((frame, _)) = thickness_frame(hwnd, state.dpi) {
                        unsafe {
                            let _ = InvalidateRect(hwnd, Some(std::ptr::from_ref(&frame)), false);
                        }
                    }
                }
                if code == EN_CHANGE && !state.updating_thickness {
                    let mut digits = [0u16; 8];
                    let length = unsafe { GetWindowTextW(HWND(lparam.0 as *mut _), &mut digits) };
                    if let Ok(value) =
                        String::from_utf16_lossy(&digits[..length.max(0) as usize]).parse::<i32>()
                        && (1..=64).contains(&value)
                    {
                        state.settings.default_thickness = value;
                        clear_note(hwnd, ID_THICKNESS_NOTE);
                        if let Some(slider) = control(hwnd, ID_THICKNESS_SLIDER) {
                            unsafe {
                                SendMessageW(
                                    slider,
                                    windows::Win32::UI::Controls::TBM_SETPOS,
                                    WPARAM(1),
                                    LPARAM(value as isize),
                                );
                                let _ = InvalidateRect(slider, None, false);
                            }
                        }
                    }
                }
                return LRESULT(0);
            }
            if code == CBN_SELCHANGE
                && matches!(
                    id,
                    ID_DELAY | ID_FORMAT | ID_QUALITY | ID_THEME | ID_LANGUAGE
                )
            {
                apply_selection(hwnd, state, id);
                return LRESULT(0);
            }
            if id == ID_HOTKEY_RECORD && code == BN_KILLFOCUS {
                end_recording(hwnd, state);
                return LRESULT(0);
            }
            if code != 0 {
                return LRESULT(0);
            }
            match id {
                // IDCANCEL, as translated from Escape by IsDialogMessage.
                2 => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                ID_BROWSE => match choose_folder(hwnd) {
                    Ok(Some(path)) => {
                        state.settings.save_directory = path;
                        update_folder_label(hwnd, &state.settings.save_directory);
                    }
                    Ok(None) => {}
                    Err(error) => crate::ui::error(
                        hwnd,
                        crate::i18n::t("Could not open the folder"),
                        &error.to_string(),
                    ),
                },
                ID_SAVE | 1 => commit(hwnd, state),
                _ => apply_button_action(hwnd, state, id),
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            state.dpi = ((wparam.0 & 0xffff) as u32).max(96);
            state.refresh_fonts(hwnd);
            apply_edit_margins(hwnd, state.dpi);
            let height = layout(hwnd, state.dpi);
            let (width, height) = window_size(hwnd, state.dpi, height);
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                let suggested = unsafe { &*(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
            } else {
                // Still being set up: keep it centred at its new size.
                center_dialog(hwnd, state.owner, width, height);
            }
            // Moved children are clipped out of the parent's repaint.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
                    hwnd,
                    None,
                    None,
                    windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                        | windows::Win32::Graphics::Gdi::RDW_ERASE
                        | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN,
                );
            }
            LRESULT(0)
        }
        crate::ui::WM_SWITCH_TAB => {
            end_recording(hwnd, state);
            state.tab = crate::ui::next_tab(state.tab, TAB_COUNT, wparam);
            show_tab(hwnd, state);
            if let Some(tab) = control(hwnd, ID_TAB_FIRST + state.tab as i32) {
                unsafe {
                    let _ = SetFocus(tab);
                }
            }
            LRESULT(0)
        }
        crate::hotkey::WM_RECORD_HOTKEY => {
            if state.recording_hotkey {
                record_shortcut(hwnd, state, wparam.0 as u32, lparam.0 as u32);
            }
            LRESULT(0)
        }
        // A click on the bare dialog surface does not move the focus away
        // from the record button, so it ends the recording itself.
        WM_LBUTTONDOWN => {
            end_recording(hwnd, state);
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_settings_class() -> Result<()> {
    static REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REGISTERED.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(());
    }
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(settings_wnd_proc),
        hInstance: HINSTANCE::default(),
        hIcon: crate::tray::app_icon(),
        hCursor: unsafe { LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default() },
        lpszClassName: SETTINGS_CLASS_NAME,
        hIconSm: crate::tray::app_icon_small(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0
        && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
    {
        return Err(windows::core::Error::from_win32());
    }
    REGISTERED.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

struct ModalOwner {
    hwnd: Option<HWND>,
    restore_enabled: bool,
}

impl ModalOwner {
    fn disable(hwnd: Option<HWND>) -> Self {
        let restore_enabled = hwnd.is_some_and(|owner| unsafe { IsWindowEnabled(owner).as_bool() });
        if let Some(owner) = hwnd.filter(|_| restore_enabled) {
            unsafe {
                let _ = EnableWindow(owner, false);
            }
        }
        Self {
            hwnd,
            restore_enabled,
        }
    }
}

impl Drop for ModalOwner {
    fn drop(&mut self) {
        if let Some(owner) = self
            .hwnd
            .filter(|owner| unsafe { IsWindow(*owner).as_bool() })
        {
            if self.restore_enabled {
                unsafe {
                    let _ = EnableWindow(owner, true);
                }
            }
            unsafe {
                let _ = SetActiveWindow(owner);
                let _ = SetForegroundWindow(owner);
            }
        }
    }
}

const WINDOW_STYLE_FLAGS: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);

/// Outer window size for a client area of `WIDTH` × `client_height` design px.
fn window_size(hwnd: HWND, dpi: u32, client_height: i32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(WIDTH, dpi),
        bottom: scale(client_height, dpi),
    };
    let _ = hwnd;
    unsafe {
        let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
            &mut rect,
            WINDOW_STYLE_FLAGS,
            false,
            Default::default(),
            dpi,
        );
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// Where the dialog opens: the work area of the monitor that holds its owner
/// (or the cursor) and the rectangle it is centred on.
struct Placement {
    work: RECT,
    anchor: RECT,
}

fn dialog_placement(owner: Option<HWND>) -> Option<Placement> {
    let visible_owner = owner.filter(|candidate| unsafe { IsWindowVisible(*candidate).as_bool() });
    let monitor = match visible_owner {
        Some(owner) => unsafe { MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST) },
        None => {
            let mut cursor = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cursor);
                MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
            }
        }
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } {
        return None;
    }
    let work = info.rcWork;
    let mut anchor = work;
    if let Some(owner) = visible_owner
        && unsafe { GetWindowRect(owner, &mut anchor) }.is_err()
    {
        anchor = work;
    }
    Some(Placement { work, anchor })
}

/// Centres the dialog on its owner (or on the monitor under the cursor).
fn center_dialog(hwnd: HWND, owner: Option<HWND>, width: i32, height: i32) {
    let Some(Placement { work, anchor }) = dialog_placement(owner) else {
        return;
    };
    let height = height.min(work.bottom - work.top);
    let x = ((anchor.left + anchor.right - width) / 2)
        .clamp(work.left, (work.right - width).max(work.left));
    let y = ((anchor.top + anchor.bottom - height) / 2)
        .clamp(work.top, (work.bottom - height).max(work.top));
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

pub fn show_settings_dialog(current: &Settings, owner: Option<HWND>) -> Result<Option<Settings>> {
    unsafe {
        use windows::Win32::UI::Controls::*;
        let init = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&init);
    }
    crate::diagnostics::record("settings", "Opening settings window");
    register_settings_class()?;
    crate::theme::set_preference(current.theme_preference);
    crate::i18n::set_preference(current.language);
    let mut state = Box::new(SettingsWindowState {
        settings: current.clone(),
        saved: false,
        update_status: UpdateStatus::Idle,
        update_frame: 0,
        tab: 0,
        recording_hotkey: false,
        original_hotkey: current.hotkey.clone(),
        updating_thickness: false,
        dpi: 96,
        owner,
        font: HFONT::default(),
        content_brush: HBRUSH::default(),
        footer_brush: HBRUSH::default(),
        input_brush: HBRUSH::default(),
    });
    state.refresh_brushes();
    // The window is created hidden on the monitor it will open on, so its DPI
    // is that monitor's and the layout below is already at the right scale.
    let origin = dialog_placement(owner).map_or((CW_USEDEFAULT, CW_USEDEFAULT), |place| {
        (place.work.left, place.work.top)
    });
    let hwnd = unsafe {
        CreateWindowExW(
            // A taskbar button even when owned by the tray or the editor.
            WS_EX_APPWINDOW,
            SETTINGS_CLASS_NAME,
            &windows::core::HSTRING::from(crate::i18n::t("isolmaSS Settings")),
            WINDOW_STYLE_FLAGS,
            origin.0,
            origin.1,
            WIDTH,
            600,
            owner.unwrap_or_default(),
            None,
            HINSTANCE::default(),
            None,
        )?
    };
    let _window = crate::ui::OwnedWindow(hwnd);
    let _suspend = crate::hotkey::OverlayInputSuspension::new();
    let _modal_owner = ModalOwner::disable(owner);
    unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            state.as_mut() as *mut SettingsWindowState as isize,
        );
    }
    state.dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }.max(96);
    create_settings_controls(hwnd)?;
    state.refresh_fonts(hwnd);
    apply_edit_margins(hwnd, state.dpi);
    initialize_control_values(hwnd, &state.settings);
    unsafe {
        crate::theme::apply_window_theme(
            hwnd,
            crate::theme::theme() == crate::theme::Theme::Dark,
            false,
        );
    }
    let client_height = layout(hwnd, state.dpi);
    show_tab(hwnd, &state);
    let (width, height) = window_size(hwnd, state.dpi, client_height);
    center_dialog(hwnd, owner, width, height);
    if unsafe { SetTimer(hwnd, 1, 350, None) } == 0 {
        crate::diagnostics::record(
            "settings update polling",
            "Could not start update polling timer",
        );
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    crate::ui::bring_to_front(hwnd);
    if let Some(first) = control(hwnd, ID_TAB_FIRST) {
        unsafe {
            let _ = SetFocus(first);
        }
    }
    crate::diagnostics::record("settings", "Window activated");

    crate::ui::window_loop(hwnd, crate::ui::WindowKind::Settings)?;
    if state.saved {
        Ok(Some(state.settings.clone()))
    } else {
        crate::theme::set_preference(current.theme_preference);
        crate::i18n::set_preference(current.language);
        Ok(None)
    }
}

fn choose_folder(owner: HWND) -> Result<Option<PathBuf>> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)? };
    unsafe {
        dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)?;
        dialog.SetTitle(&windows::core::HSTRING::from(crate::i18n::t(
            "Choose the screenshots folder",
        )))?;
    }
    if let Err(error) = unsafe { dialog.Show(owner) } {
        if error.code() == windows::core::HRESULT::from_win32(1223) {
            return Ok(None);
        }
        return Err(error);
    }
    let raw = unsafe { dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)? };
    let path = unsafe { raw.to_string() };
    unsafe {
        CoTaskMemFree(Some(raw.0.cast()));
    }
    Ok(Some(PathBuf::from(path?)))
}

/// The shortcut text (parsed back by `HotkeyConfig`) for a pressed key, or
/// `None` when the key cannot be part of a shortcut: an unsupported key, or a
/// key without a modifier (PrintScreen needs none).
fn shortcut_description(modifiers: u32, vk: u32) -> Option<String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_SNAPSHOT,
    };
    let key = match vk {
        0x30..=0x39 | 0x41..=0x5a => char::from_u32(vk)?.to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6f),
        vk if vk == VK_SNAPSHOT.0 as u32 => "PrintScreen".to_string(),
        _ => return None,
    };
    let mut parts = Vec::with_capacity(5);
    for (flag, name) in [
        (MOD_CONTROL.0, "Ctrl"),
        (MOD_ALT.0, "Alt"),
        (MOD_SHIFT.0, "Shift"),
        (MOD_WIN.0, "Win"),
    ] {
        if modifiers & flag != 0 {
            parts.push(name.to_string());
        }
    }
    if parts.is_empty() && vk != VK_SNAPSHOT.0 as u32 {
        return None;
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// Combinations that must stay with the system and with text editing: Alt+F4
/// closes windows, Ctrl+C/V/X/Z/Y/A/S are the editing commands, and Ctrl+Alt
/// is AltGr on many keyboard layouts, so a global Ctrl+Alt+letter would
/// swallow typed characters (Ctrl+Alt+Q is "@" on the Turkish-Q layout).
fn is_reserved_shortcut(modifiers: u32, vk: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
    const F4: u32 = 0x73;
    let modifiers = modifiers & (MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0 | MOD_WIN.0);
    let typed_key = matches!(vk, 0x30..=0x39 | 0x41..=0x5a);
    let control_alt = MOD_CONTROL.0 | MOD_ALT.0;
    let alt_gr = (modifiers & control_alt) == control_alt && (modifiers & MOD_WIN.0) == 0;
    (modifiers == MOD_ALT.0 && vk == F4)
        || (modifiers == MOD_CONTROL.0
            && matches!(vk, 0x43 | 0x56 | 0x58 | 0x5a | 0x59 | 0x41 | 0x53))
        || (alt_gr && typed_key)
}

/// The `MOD_*` bits of the modifier keys held down right now.
fn pressed_modifiers() -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_CONTROL, VK_LWIN, VK_MENU,
        VK_RWIN, VK_SHIFT,
    };
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    let mut modifiers = 0;
    if down(VK_CONTROL.0) {
        modifiers |= MOD_CONTROL.0;
    }
    if down(VK_MENU.0) {
        modifiers |= MOD_ALT.0;
    }
    if down(VK_SHIFT.0) {
        modifiers |= MOD_SHIFT.0;
    }
    if down(VK_LWIN.0) || down(VK_RWIN.0) {
        modifiers |= MOD_WIN.0;
    }
    modifiers
}

/// Takes a pressed shortcut while recording. Reserved combinations and ones
/// another app holds are refused and the previous shortcut stays; PrintScreen is
/// always accepted because the keyboard hook, not RegisterHotKey, serves it.
/// Pressing the shortcut that was already chosen or is running always works.
fn record_shortcut(hwnd: HWND, state: &mut SettingsWindowState, vk: u32, modifiers: u32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey, VK_SNAPSHOT,
    };
    let Some(description) = shortcut_description(modifiers, vk) else {
        return;
    };
    let Some(candidate) = HotkeyConfig::from_str(&description) else {
        return;
    };
    end_recording(hwnd, state);
    if candidate != state.settings.hotkey && candidate != state.original_hotkey {
        if is_reserved_shortcut(modifiers, vk) {
            set_hotkey_note(
                hwnd,
                state,
                &crate::i18n::tf("{} is reserved. Choose another shortcut.", &[&description]),
            );
            return;
        }
        const PROBE_ID: i32 = 0x5a51;
        if vk != VK_SNAPSHOT.0 as u32 {
            match unsafe {
                RegisterHotKey(
                    HWND::default(),
                    PROBE_ID,
                    HOT_KEY_MODIFIERS(candidate.modifiers),
                    candidate.vk,
                )
            } {
                Ok(()) => {
                    let _ = unsafe { UnregisterHotKey(HWND::default(), PROBE_ID) };
                }
                Err(_) => {
                    crate::ui::error(
                        hwnd,
                        crate::i18n::t("Shortcut conflict"),
                        &crate::i18n::tf(
                            "{} is used by Windows or another app. The previous shortcut was kept; choose another key combination.",
                            &[&description],
                        ),
                    );
                    return;
                }
            }
        }
    }
    state.settings.hotkey = candidate;
    refresh_hotkey(hwnd, &state.settings);
}

/// Keeps shortcut recording to the record button: only keys typed into it are
/// taken, so Tab, Enter, arrows and everything in other controls keep working.
/// Returns true when the message was consumed.
pub fn handle_key_recording(
    hwnd: HWND,
    message: &windows::Win32::UI::WindowsAndMessaging::MSG,
) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SNAPSHOT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP};
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsWindowState;
    if ptr.is_null() || !unsafe { (*ptr).recording_hotkey } {
        return false;
    }
    let state = unsafe { &mut *ptr };
    if control(hwnd, ID_HOTKEY_RECORD) != Some(message.hwnd) {
        return false;
    }
    let is_down = message.message == WM_KEYDOWN || message.message == WM_SYSKEYDOWN;
    let is_up = message.message == WM_KEYUP || message.message == WM_SYSKEYUP;
    if !is_down && !is_up {
        return false;
    }
    let key = message.wParam.0 as u32;
    // A modifier alone is no shortcut; consuming it also keeps Alt from
    // opening the window menu.
    if [VK_CONTROL.0, VK_SHIFT.0, VK_MENU.0, VK_LWIN.0, VK_RWIN.0].contains(&(key as u16)) {
        return true;
    }
    // PrintScreen reports only its release on some systems.
    if is_up && key != VK_SNAPSHOT.0 as u32 {
        return false;
    }
    if is_down && key == VK_ESCAPE.0 as u32 {
        end_recording(hwnd, state);
        return true;
    }
    let modifiers = pressed_modifiers();
    if shortcut_description(modifiers, key).is_none() {
        return false;
    }
    record_shortcut(hwnd, state, key, modifiers);
    true
}

fn save_settings(settings: &Settings) -> std::result::Result<(), String> {
    settings.validate().map_err(|error| error.to_string())?;
    let previous = crate::startup::StartupRegistration::read()?;
    // Compare the exact current registry bytes first: an unchanged toggle skips the
    // registry write (and its rollback) entirely; only a real change writes before JSON.
    let startup_changed = previous.needs_update(settings.start_with_windows)?;
    if startup_changed {
        crate::startup::set_start_with_windows(settings.start_with_windows)?;
    }
    if let Err(error) = settings.save() {
        let rollback = if startup_changed {
            previous
                .restore()
                .err()
                .map(|error| format!(" Startup restoration also failed: {error}"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        return Err(format!("{error}{rollback}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_note_takes_room_only_while_shown() {
        assert_eq!(warning_room(0), 0);
        // One note line pushes the next row 18 px further down than a plain
        // row pitch, the gap the layout used to reserve all the time.
        assert_eq!(warning_room(NOTE), 18);
        assert_eq!(warning_room(2 * NOTE), warning_room(NOTE) + NOTE);
    }
}
