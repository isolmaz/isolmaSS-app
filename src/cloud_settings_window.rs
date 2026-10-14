//! Cloudflare sharing dialog in the same compact Windows 11 style as Settings:
//! a guided two-step connection page, or — once paired — tabs for the
//! connection, quotas and recent images, with status in the footer band.
use crate::cloudflare_oauth::{self, Account, Authorization};
use crate::cloudflare_setup;
use crate::settings::Settings;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetDC, GetMonitorInfoW, HBRUSH, HDC, HFONT, HGDIOBJ, HMONITOR,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MapWindowPoints, MonitorFromPoint,
    MonitorFromWindow, PAINTSTRUCT, PATCOPY, PS_SOLID, PatBlt, RDW_ALLCHILDREN, RDW_ERASE,
    RDW_FRAME, RDW_INVALIDATE, RectVisible, RedrawWindow, ReleaseDC, SelectObject, SetBkColor,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CDDS_PREPAINT, CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, EM_SETMARGINS, MEASUREITEMSTRUCT,
    NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, SetActiveWindow, SetFocus,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

const CLASS: PCWSTR = w!("isolmaSS_CloudSettings");
const WM_CLOUD_RESULT: u32 = WM_APP + 215;
const ID_CONNECT: i32 = 1001;
const ID_ACCOUNT: i32 = 1002;
const ID_INSTALL: i32 = 1003;
const ID_FORGET: i32 = 1004;
const ID_REFRESH: i32 = 1005;
const ID_STATUS: i32 = 1006;
const ID_STATS: i32 = 1007;
const ID_LIMIT_FIRST: i32 = 1020;
const ID_MODE: i32 = 1027;
const ID_SAVE_LIMITS: i32 = 1028;
const ID_IMAGES: i32 = 1029;
const ID_DELETE: i32 = 1030;
const ID_CLOSE: i32 = 1031;
const ID_PASSWORD: i32 = 1032;
const ID_SAVE_PASSWORD: i32 = 1033;
const ID_OPEN_IMAGE: i32 = 1034;
const ID_COPY_IMAGE: i32 = 1035;
const ID_UPDATE_WORKER: i32 = 1036;
// Labels.
const ID_INTRO: i32 = 1101;
const ID_STEP_ACCOUNT: i32 = 1102;
const ID_PASSWORD_LABEL: i32 = 1104;
const ID_MODE_LABEL: i32 = 1106;
const ID_IMAGES_LABEL: i32 = 1107;
const ID_COST_NOTE: i32 = 1108;
const ID_IMAGES_EMPTY: i32 = 1109;
const ID_STEP_CONSENT: i32 = 1110;
const ID_CONSENT_NOTE: i32 = 1111;
const ID_ACCOUNT_GUIDE: i32 = 1112;
const ID_LIMIT_LABEL_FIRST: i32 = 1200;
const ID_TAB_FIRST: i32 = 1300;
const TAB_COUNT: usize = 3;

fn tab_names() -> [&'static str; TAB_COUNT] {
    [
        crate::i18n::t("Connection"),
        crate::i18n::t("Limits"),
        crate::i18n::t("Images"),
    ]
}

/// What to do on Cloudflare's consent page, shown before the browser opens: with an
/// existing Worker (`cloud_url`) the account that owns it, otherwise any one account.
fn account_guidance(cloud_url: Option<&str>) -> String {
    match cloud_url {
        Some(url) => crate::i18n::tf(
            "On the Cloudflare page, allow the account that owns {}.",
            &[&url.trim_start_matches("https://").trim_end_matches('/')],
        ),
        None => crate::i18n::t(
            "On the Cloudflare page, allow the account that should host your Worker. If you have several accounts, allow only that one.",
        )
        .to_owned(),
    }
}

/// Worker setting and the factor between the stored value and the shown one.
const LIMITS: [(&str, u64); 7] = [
    ("max_active", 1),
    ("daily_upload_limit", 1),
    ("daily_view_limit", 1),
    ("max_image_bytes", 1024 * 1024),
    ("max_storage_bytes", 1024 * 1024),
    ("retention_days", 1),
    ("warning_percent", 1),
];

fn limit_labels() -> [&'static str; 7] {
    [
        crate::i18n::t("Latest images kept"),
        crate::i18n::t("Daily uploads"),
        crate::i18n::t("View warning (daily)"),
        crate::i18n::t("Image size (MB)"),
        crate::i18n::t("Total storage (MB)"),
        crate::i18n::t("Retention (days)"),
        crate::i18n::t("Warn/stop at (%)"),
    ]
}

// Design grid (96-DPI pixels), shared with the Settings dialog.
const WIDTH: i32 = 480;
const MARGIN: i32 = 20;
const TAB_BAR: i32 = 48;
const ROW: i32 = 30;
/// Room for a footer status of three lines.
const FOOTER: i32 = 64;
const CONTENT_TOP: i32 = TAB_BAR + 12;
/// One line of label text.
const LABEL: i32 = 20;
/// Text-box frame height, equal to a button's.
const FIELD: i32 = 32;
const LABEL_GAP: i32 = 2;
const CELL_HEIGHT: i32 = LABEL + LABEL_GAP + FIELD;
/// Limits grid: row distance and the gutter between its two columns.
const GRID_PITCH: i32 = CELL_HEIGHT + 4;
const GUTTER: i32 = 20;
const SAVE_LIMITS_WIDTH: i32 = 150;
const IMAGE_LIST_HEIGHT: i32 = 190;
/// Height of an image row, as a Windows 11 list item.
const IMAGE_ROW: i32 = 32;
/// Side room of the empty-list text inside the list.
const EMPTY_INSET: i32 = 24;
/// Footer status: left of the Close button.
const STATUS_WIDTH: i32 = WIDTH - 2 * MARGIN - 100 - 12;
/// Horizontal and vertical gap between an edit and its frame. The dialog
/// paints the frame, which a child cannot, so the edit sits inset in it.
const EDIT_PAD: (i32, i32) = (2, 6);
/// The same for the image list, whose frame hugs it.
const LIST_PAD: (i32, i32) = (1, 1);
/// Text margin inside an edit, on top of its inset.
const EDIT_MARGIN: i32 = 4;

// Static control styles (the SystemServices feature is not enabled).
const SS_CENTER: u32 = 0x1;
const SS_NOPREFIX: u32 = 0x80;
const SS_ENDELLIPSIS: u32 = 0x4000;

/// Top of the Limits grid cell `index` (row-major, two columns); `column` is
/// the cell width.
fn limit_cell(index: i32, column: i32) -> (i32, i32) {
    (
        MARGIN + index % 2 * (column + GUTTER),
        CONTENT_TOP + index / 2 * GRID_PITCH,
    )
}

/// Where the cost note and the Save button start: below the grid, whose last
/// cell holds the "At the limit" choice after the limit fields.
fn limits_note_top() -> i32 {
    let rows = (LIMITS.len() as i32 + 2) / 2;
    CONTENT_TOP + (rows - 1) * GRID_PITCH + CELL_HEIGHT + 12
}

/// Bottom of the Limits tab for a cost note `note` design pixels tall.
fn limits_bottom(note: i32) -> i32 {
    limits_note_top() + note.max(ROW + 2)
}

/// Design pixels for `pixels` device pixels at `dpi`, rounded up so measured
/// text always fits.
fn design(pixels: i32, dpi: u32) -> i32 {
    (pixels * 96 + dpi as i32 - 1) / dpi as i32
}

/// The button Enter presses while focus rests on control `id`: the one that
/// applies the edits of the focused section, otherwise the dialog's own.
fn default_for(id: i32) -> i32 {
    match id {
        ID_PASSWORD => ID_SAVE_PASSWORD,
        ID_MODE => ID_SAVE_LIMITS,
        ID_IMAGES => ID_OPEN_IMAGE,
        _ if is_limit_edit(id) => ID_SAVE_LIMITS,
        _ => ID_CLOSE,
    }
}

fn is_limit_edit(id: i32) -> bool {
    (ID_LIMIT_FIRST..ID_LIMIT_FIRST + LIMITS.len() as i32).contains(&id)
}

/// Controls that report focus changes and selections through WM_COMMAND.
fn is_input(id: i32) -> bool {
    matches!(id, ID_PASSWORD | ID_MODE | ID_IMAGES) || is_limit_edit(id)
}

fn frame_padding(id: i32) -> (i32, i32) {
    if id == ID_IMAGES { LIST_PAD } else { EDIT_PAD }
}

struct State {
    settings: Settings,
    saved: bool,
    for_upload: bool,
    pending: bool,
    authorizing: bool,
    /// The next authorization is for updating the existing Worker.
    updating: bool,
    /// The Worker runs older code than the app bundles.
    outdated: bool,
    cancel: Arc<AtomicBool>,
    authorization: Option<Authorization>,
    loaded: bool,
    tab: usize,
    /// (id, share link) of the listed images.
    images: Vec<(String, String)>,
    response: Arc<Mutex<Option<Completion>>>,
    dpi: u32,
    owner: Option<HWND>,
    /// Push button Enter presses; it is drawn with the accent fill.
    default_id: i32,
    font: HFONT,
    heading_font: HFONT,
    /// Dialog content: the lighter layer.
    content_brush: HBRUSH,
    /// Command band with the status and Close button.
    footer_brush: HBRUSH,
    input_brush: HBRUSH,
}
impl State {
    fn guided(&self) -> bool {
        self.settings.cloud_url.is_none()
    }

    fn refresh_brushes(&mut self) {
        let tokens = crate::theme::tokens();
        unsafe {
            for brush in [self.content_brush, self.footer_brush, self.input_brush] {
                if !brush.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                }
            }
            self.content_brush = CreateSolidBrush(tokens.card);
            self.footer_brush = CreateSolidBrush(tokens.page);
            self.input_brush = CreateSolidBrush(tokens.control_fill);
        }
    }

    fn refresh_fonts(&mut self, hwnd: HWND) {
        let previous = [self.font, self.heading_font];
        self.font = crate::theme::create_ui_font(self.dpi, crate::theme::FONT_BODY_PX, 400);
        self.heading_font = crate::theme::create_ui_font(
            self.dpi,
            crate::theme::FONT_BODY_PX,
            crate::theme::FONT_WEIGHT_SECTION,
        );
        for id in 1000..=1310 {
            if let Some(child) = control(hwnd, id) {
                let font = if matches!(id, ID_STEP_CONSENT | ID_STEP_ACCOUNT) {
                    self.heading_font
                } else {
                    self.font
                };
                unsafe {
                    SendMessageW(child, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
                }
            }
        }
        fit_inputs(hwnd, self.dpi);
        for font in previous {
            if !font.is_invalid() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
            }
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        for font in [self.font, self.heading_font] {
            if !font.is_invalid() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
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
enum Completion {
    Authorization(std::result::Result<Authorization, String>),
    Install(std::result::Result<cloudflare_oauth::Installed, String>),
    Refresh(std::result::Result<(Value, Value), String>),
    Limits(std::result::Result<Value, String>),
    Delete(std::result::Result<String, String>),
    WorkerUpdate(std::result::Result<cloudflare_setup::CloudCredentials, String>),
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn control(hwnd: HWND, id: i32) -> Option<HWND> {
    unsafe { GetDlgItem(hwnd, id).ok() }
}
fn set_text(hwnd: HWND, id: i32, text: &str) {
    if let Some(child) = control(hwnd, id) {
        let value = wide(text);
        unsafe {
            let _ = SetWindowTextW(child, PCWSTR(value.as_ptr()));
        }
    }
}
fn get_text(hwnd: HWND, id: i32) -> std::result::Result<String, String> {
    let child = control(hwnd, id).ok_or(crate::i18n::t("Cloudflare field not found."))?;
    let size = unsafe { GetWindowTextLengthW(child) };
    if !(0..=512).contains(&size) {
        return Err(crate::i18n::t("Cloudflare field is too long.").into());
    }
    let mut buffer = vec![0u16; size as usize + 1];
    let count = unsafe { GetWindowTextW(child, &mut buffer) };
    Ok(String::from_utf16_lossy(&buffer[..count.max(0) as usize])
        .trim()
        .to_string())
}
fn create(hwnd: HWND, class: PCWSTR, id: i32, text: &str, style: WINDOW_STYLE) -> Result<HWND> {
    let text = wide(text);
    unsafe {
        CreateWindowExW(
            Default::default(),
            class,
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            hwnd,
            HMENU(id as *mut _),
            HINSTANCE::default(),
            None,
        )
    }
}
fn button(hwnd: HWND, id: i32, text: &str) -> Result<HWND> {
    push_button(hwnd, id, text, BS_PUSHBUTTON)
}
/// The button Enter presses when the dialog opens.
fn default_button(hwnd: HWND, id: i32, text: &str) -> Result<HWND> {
    push_button(hwnd, id, text, BS_DEFPUSHBUTTON)
}
fn push_button(hwnd: HWND, id: i32, text: &str, style: i32) -> Result<HWND> {
    create(
        hwnd,
        w!("BUTTON"),
        id,
        text,
        WINDOW_STYLE(WS_TABSTOP.0 | style as u32),
    )
}
/// Wrapping text; its row is sized with [`measure_text`].
fn label(hwnd: HWND, id: i32, text: &str) -> Result<HWND> {
    create(hwnd, w!("STATIC"), id, text, WINDOW_STYLE(SS_NOPREFIX))
}
/// One line that ends in an ellipsis when it is too long for its row.
/// SS_ENDELLIPSIS forces single-line rendering, so wrapping text must use
/// [`label`].
fn single_line_label(hwnd: HWND, id: i32, text: &str) -> Result<HWND> {
    create(
        hwnd,
        w!("STATIC"),
        id,
        text,
        WINDOW_STYLE(SS_NOPREFIX | SS_ENDELLIPSIS),
    )
}
/// A single-line text box; `kind` is `ES_PASSWORD` or `ES_NUMBER`.
fn edit(hwnd: HWND, id: i32, kind: i32) -> Result<HWND> {
    create(
        hwnd,
        w!("EDIT"),
        id,
        "",
        WINDOW_STYLE(WS_TABSTOP.0 | ES_AUTOHSCROLL as u32 | kind as u32),
    )
}
fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
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
/// Places an input inside the frame the dialog paints around it.
fn place_framed(hwnd: HWND, id: i32, (x, y, width, height): (i32, i32, i32, i32), dpi: u32) {
    let (pad_x, pad_y) = frame_padding(id);
    place(
        hwnd,
        id,
        (x + pad_x, y + pad_y, width - 2 * pad_x, height - 2 * pad_y),
        dpi,
    );
}

/// Text margins inside the framed edits, the selection height of the combo
/// boxes and the row height of the image list, in device pixels. Re-applied
/// whenever the fonts are, as all depend on the DPI.
fn fit_inputs(hwnd: HWND, dpi: u32) {
    let margin = scale(EDIT_MARGIN, dpi) as isize;
    for id in
        std::iter::once(ID_PASSWORD).chain(ID_LIMIT_FIRST..ID_LIMIT_FIRST + LIMITS.len() as i32)
    {
        if let Some(edit) = control(hwnd, id) {
            unsafe {
                SendMessageW(
                    edit,
                    EM_SETMARGINS,
                    WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize),
                    LPARAM(margin | margin << 16),
                );
            }
        }
    }
    if let Some(list) = control(hwnd, ID_IMAGES) {
        unsafe {
            SendMessageW(
                list,
                LB_SETITEMHEIGHT,
                WPARAM(0),
                LPARAM(scale(IMAGE_ROW, dpi) as isize),
            );
        }
    }
    for id in [ID_MODE, ID_ACCOUNT] {
        if let Some(combo) = control(hwnd, id) {
            unsafe {
                // wParam -1 addresses the selection field.
                SendMessageW(
                    combo,
                    CB_SETITEMHEIGHT,
                    WPARAM(usize::MAX),
                    LPARAM(scale(FIELD - 4, dpi) as isize),
                );
            }
        }
    }
}

fn current_text(child: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(child) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let copied = unsafe { GetWindowTextW(child, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

/// Design pixels `text` needs when word-wrapped to `width` design pixels in
/// the font of control `id`, at least one line.
fn measure_text(hwnd: HWND, dpi: u32, id: i32, text: &str, width: i32) -> i32 {
    let Some(child) = control(hwnd, id) else {
        return LABEL;
    };
    let mut text: Vec<u16> = text.encode_utf16().collect();
    if text.is_empty() {
        return LABEL;
    }
    let font =
        HGDIOBJ(unsafe { SendMessageW(child, WM_GETFONT, WPARAM(0), LPARAM(0)) }.0 as *mut _);
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
    design(rect.bottom, dpi).max(LABEL)
}

/// [`measure_text`] for the text the control shows now.
fn measure_current(hwnd: HWND, dpi: u32, id: i32, width: i32) -> i32 {
    match control(hwnd, id) {
        Some(child) => measure_text(hwnd, dpi, id, &current_text(child), width),
        None => LABEL,
    }
}

/// Statistics summary of the Connection tab: uploads and views today and this
/// month, then the active images and their size in MB.
fn stats_text(values: [&dyn std::fmt::Display; 6]) -> String {
    crate::i18n::tf(
        "Today: uploads {} · views {}\r\nThis month: uploads {} · views {}\r\nActive images: {} · {} MB\r\nWatch your Cloudflare account's overall usage in its dashboard.",
        &values,
    )
}

/// Footer status: centred in the band at `band_top` (device pixels), up to
/// the three lines the band holds.
fn fit_status(hwnd: HWND, dpi: u32, band_top: i32) {
    let Some(child) = control(hwnd, ID_STATUS) else {
        return;
    };
    let height = measure_current(hwnd, dpi, ID_STATUS, STATUS_WIDTH).min(FOOTER - 4);
    unsafe {
        let _ = MoveWindow(
            child,
            scale(MARGIN, dpi),
            band_top + scale(FOOTER - height, dpi) / 2,
            scale(STATUS_WIDTH, dpi),
            scale(height, dpi),
            true,
        );
    }
}

/// Which tab of the paired dialog a control belongs to (`None`: always shown).
fn tab_of(id: i32) -> Option<usize> {
    match id {
        ID_INTRO | ID_STATS | ID_PASSWORD_LABEL | ID_PASSWORD | ID_SAVE_PASSWORD | ID_REFRESH
        | ID_UPDATE_WORKER | ID_ACCOUNT_GUIDE | ID_FORGET => Some(0),
        ID_MODE_LABEL | ID_MODE | ID_SAVE_LIMITS | ID_COST_NOTE => Some(1),
        _ if is_limit_edit(id)
            || (ID_LIMIT_LABEL_FIRST..ID_LIMIT_LABEL_FIRST + LIMITS.len() as i32).contains(&id) =>
        {
            Some(1)
        }
        ID_IMAGES_LABEL | ID_IMAGES | ID_IMAGES_EMPTY | ID_DELETE | ID_OPEN_IMAGE
        | ID_COPY_IMAGE => Some(2),
        _ => None,
    }
}

fn show_tab(hwnd: HWND, state: &State) {
    if state.guided() {
        return;
    }
    for id in 1000..1300 {
        if let (Some(tab), Some(child)) = (tab_of(id), control(hwnd, id)) {
            let visible = tab == state.tab
                && match id {
                    ID_UPDATE_WORKER | ID_ACCOUNT_GUIDE => state.outdated,
                    // Only once the list has loaded and turned out empty.
                    ID_IMAGES_EMPTY => state.loaded && state.images.is_empty(),
                    _ => true,
                };
            unsafe {
                let _ = ShowWindow(child, if visible { SW_SHOW } else { SW_HIDE });
            }
        }
    }
    for index in 0..TAB_COUNT as i32 {
        if let Some(tab) = control(hwnd, ID_TAB_FIRST + index) {
            unsafe {
                SendMessageW(
                    tab,
                    BM_SETCHECK,
                    WPARAM(usize::from(index as usize == state.tab)),
                    LPARAM(0),
                );
            }
        }
    }
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
}

/// Positions every control; returns the client height in design pixels.
/// Text rows take the height their text needs in the current font and DPI.
fn layout(hwnd: HWND, state: &State) -> i32 {
    let dpi = state.dpi;
    let right = WIDTH - MARGIN;
    let full = WIDTH - 2 * MARGIN;
    let measure = |id: i32, width: i32| measure_current(hwnd, dpi, id, width);
    let bottom = if state.guided() {
        let intro = measure(ID_INTRO, full);
        place(hwnd, ID_INTRO, (MARGIN, 18, full, intro), dpi);
        let step = 18 + intro + 16;
        place(hwnd, ID_STEP_CONSENT, (MARGIN, step, full, LABEL), dpi);
        place(hwnd, ID_STEP_ACCOUNT, (MARGIN, step, full, LABEL), dpi);
        let note_top = step + LABEL + 4;
        let note = measure(ID_CONSENT_NOTE, full);
        place(hwnd, ID_CONSENT_NOTE, (MARGIN, note_top, full, note), dpi);
        let guide_top = note_top + note + 8;
        let guide = measure(ID_ACCOUNT_GUIDE, full);
        place(
            hwnd,
            ID_ACCOUNT_GUIDE,
            (MARGIN, guide_top, full, guide),
            dpi,
        );
        let combo_top = step + LABEL + 8;
        place(hwnd, ID_ACCOUNT, (MARGIN, combo_top, full, 220), dpi);
        // Both steps put their button on the same row.
        let action = (guide_top + guide + 10).max(combo_top + FIELD + 14);
        place(hwnd, ID_CONNECT, (MARGIN, action, 230, ROW + 2), dpi);
        place(hwnd, ID_INSTALL, (MARGIN, action, 230, ROW + 2), dpi);
        let status = action + ROW + 2 + 14;
        place(hwnd, ID_STATUS, (MARGIN, status, full, 3 * LABEL), dpi);
        status + 3 * LABEL
    } else {
        let mut x = MARGIN - 10;
        for (index, name) in tab_names().iter().enumerate() {
            let width = crate::drawing::measure_ui_text(name).0 + 2 * crate::drawing::TAB_PADDING;
            place(hwnd, ID_TAB_FIRST + index as i32, (x, 6, width, 40), dpi);
            x += width;
        }
        let top = CONTENT_TOP;
        // Connection
        place(hwnd, ID_INTRO, (MARGIN, top, full, LABEL), dpi);
        let stats_top = top + LABEL + 8;
        // Until the numbers arrive, reserve the height of a loaded summary so
        // the dialog does not jump; afterwards the real text decides.
        let mut stats = measure(ID_STATS, full);
        if !state.loaded {
            let digits: &dyn std::fmt::Display = &"88888";
            let megabytes: &dyn std::fmt::Display = &"8888.8";
            let loaded = stats_text([digits, digits, digits, digits, digits, megabytes]);
            stats = stats.max(measure_text(hwnd, dpi, ID_STATS, &loaded, full));
        }
        place(hwnd, ID_STATS, (MARGIN, stats_top, full, stats), dpi);
        let password_label = stats_top + stats + 14;
        let password_label_height = measure(ID_PASSWORD_LABEL, full);
        place(
            hwnd,
            ID_PASSWORD_LABEL,
            (MARGIN, password_label, full, password_label_height),
            dpi,
        );
        let password_top = password_label + password_label_height + 4;
        place_framed(
            hwnd,
            ID_PASSWORD,
            (MARGIN, password_top, full - 162, FIELD),
            dpi,
        );
        place(
            hwnd,
            ID_SAVE_PASSWORD,
            (right - 150, password_top, 150, ROW + 2),
            dpi,
        );
        let actions = password_top + FIELD + 16;
        place(hwnd, ID_REFRESH, (MARGIN, actions, 100, ROW + 2), dpi);
        place(
            hwnd,
            ID_UPDATE_WORKER,
            (MARGIN + 108, actions, 150, ROW + 2),
            dpi,
        );
        place(hwnd, ID_FORGET, (right - 170, actions, 170, ROW + 2), dpi);
        // Under the Update Worker button; it takes room only while that button shows.
        let guide = measure(ID_ACCOUNT_GUIDE, full);
        let guide_top = actions + ROW + 2 + 10;
        place(
            hwnd,
            ID_ACCOUNT_GUIDE,
            (MARGIN, guide_top, full, guide),
            dpi,
        );
        let connection = if state.outdated {
            guide_top + guide
        } else {
            actions + ROW + 2
        };
        // Limits: two columns, each label above its field; the choice of what
        // happens at the limit takes the cell after the last field.
        let column = (full - GUTTER) / 2;
        for index in 0..LIMITS.len() as i32 {
            let (x, y) = limit_cell(index, column);
            place(
                hwnd,
                ID_LIMIT_LABEL_FIRST + index,
                (x, y, column, LABEL),
                dpi,
            );
            place_framed(
                hwnd,
                ID_LIMIT_FIRST + index,
                (x, y + LABEL + LABEL_GAP, column, FIELD),
                dpi,
            );
        }
        let (x, y) = limit_cell(LIMITS.len() as i32, column);
        place(hwnd, ID_MODE_LABEL, (x, y, column, LABEL), dpi);
        place(hwnd, ID_MODE, (x, y + LABEL + LABEL_GAP, column, 200), dpi);
        let note_width = full - SAVE_LIMITS_WIDTH - 16;
        let note = measure(ID_COST_NOTE, note_width);
        place(
            hwnd,
            ID_COST_NOTE,
            (MARGIN, limits_note_top(), note_width, note),
            dpi,
        );
        place(
            hwnd,
            ID_SAVE_LIMITS,
            (
                right - SAVE_LIMITS_WIDTH,
                limits_note_top(),
                SAVE_LIMITS_WIDTH,
                ROW + 2,
            ),
            dpi,
        );
        let limits = limits_bottom(note);
        // Images
        let header = measure(ID_IMAGES_LABEL, full);
        place(hwnd, ID_IMAGES_LABEL, (MARGIN, top, full, header), dpi);
        let list_top = top + header + 6;
        place_framed(
            hwnd,
            ID_IMAGES,
            (MARGIN, list_top, full, IMAGE_LIST_HEIGHT),
            dpi,
        );
        // The empty-state text sits in the middle of the list.
        let empty_width = full - 2 * EMPTY_INSET;
        let empty = measure(ID_IMAGES_EMPTY, empty_width);
        place(
            hwnd,
            ID_IMAGES_EMPTY,
            (
                MARGIN + EMPTY_INSET,
                list_top + (IMAGE_LIST_HEIGHT - empty) / 2,
                empty_width,
                empty,
            ),
            dpi,
        );
        let image_actions = list_top + IMAGE_LIST_HEIGHT + 8;
        place(
            hwnd,
            ID_OPEN_IMAGE,
            (MARGIN, image_actions, 80, ROW + 2),
            dpi,
        );
        place(
            hwnd,
            ID_COPY_IMAGE,
            (MARGIN + 88, image_actions, 150, ROW + 2),
            dpi,
        );
        place(
            hwnd,
            ID_DELETE,
            (right - 150, image_actions, 150, ROW + 2),
            dpi,
        );
        let images = image_actions + ROW + 2;
        connection.max(limits).max(images)
    };
    let footer = bottom + 8;
    let button_y = footer + (FOOTER - ROW - 2) / 2;
    if !state.guided() {
        fit_status(hwnd, dpi, scale(footer, dpi));
    }
    place(hwnd, ID_CLOSE, (right - 100, button_y, 100, ROW + 2), dpi);
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
    footer + FOOTER
}

/// Measures the text rows again and resizes the dialog to the new content
/// height; for text that changed after the first layout.
fn relayout(hwnd: HWND, state: &State) {
    let (width, height) = window_size(state.dpi, layout(hwnd, state));
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
        // Children moved without repainting; the parent clips them.
        let _ = RedrawWindow(
            hwnd,
            None,
            None,
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
    }
}

/// Guided setup: step one (consent) or, after consent with several accounts,
/// step two (account choice).
fn account_controls(hwnd: HWND, visible: bool) {
    for id in [
        ID_STEP_ACCOUNT,
        ID_ACCOUNT,
        ID_INSTALL,
        ID_STEP_CONSENT,
        ID_CONSENT_NOTE,
        ID_ACCOUNT_GUIDE,
        ID_CONNECT,
    ] {
        if let Some(child) = control(hwnd, id) {
            let account_step = matches!(id, ID_STEP_ACCOUNT | ID_ACCOUNT | ID_INSTALL);
            unsafe {
                let _ = ShowWindow(
                    child,
                    if visible == account_step {
                        SW_SHOW
                    } else {
                        SW_HIDE
                    },
                );
            }
        }
    }
}
/// Moves the guided page to the account step or back to consent, with the
/// step's main button as default and its first control focused.
fn show_step(hwnd: HWND, state: &mut State, account_step: bool) {
    account_controls(hwnd, account_step);
    let (button, first) = if account_step {
        (ID_INSTALL, ID_ACCOUNT)
    } else {
        (ID_CONNECT, ID_CONNECT)
    };
    set_default(hwnd, state, button);
    if let Some(child) = control(hwnd, first) {
        unsafe {
            let _ = SetFocus(child);
        }
    }
}
fn set_status(hwnd: HWND, text: &str) {
    set_text(hwnd, ID_STATUS, text);
    // The footer status sits centred in its band; the guided page keeps a
    // fixed three-line area.
    if control(hwnd, ID_TAB_FIRST).is_some() {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        fit_status(hwnd, dpi, footer_top(hwnd, dpi));
    }
}
fn set_busy(hwnd: HWND, state: &mut State, busy: bool) {
    state.pending = busy;
    for id in [
        ID_CONNECT,
        ID_INSTALL,
        ID_REFRESH,
        ID_SAVE_LIMITS,
        ID_SAVE_PASSWORD,
        ID_FORGET,
        ID_UPDATE_WORKER,
    ] {
        if let Some(child) = control(hwnd, id) {
            unsafe {
                let _ = EnableWindow(child, !busy);
            }
        }
    }
    sync_image_actions(hwnd, state);
}
/// Open, Copy link and Delete work on the selected image only.
fn sync_image_actions(hwnd: HWND, state: &State) {
    let enabled = !state.pending && selected_image(hwnd, state).is_some();
    for id in [ID_OPEN_IMAGE, ID_COPY_IMAGE, ID_DELETE] {
        if let Some(button) = control(hwnd, id) {
            unsafe {
                let _ = EnableWindow(button, enabled);
            }
        }
    }
}
/// Makes `id` the push button Enter presses: its style and accent fill
/// follow, as with `DM_SETDEFID` in a standard dialog.
fn set_default(hwnd: HWND, state: &mut State, id: i32) {
    if state.default_id == id {
        return;
    }
    let previous = std::mem::replace(&mut state.default_id, id);
    for (button, style) in [(previous, BS_PUSHBUTTON), (id, BS_DEFPUSHBUTTON)] {
        if let Some(child) = control(hwnd, button) {
            unsafe {
                SendMessageW(child, BM_SETSTYLE, WPARAM(style as usize), LPARAM(1));
            }
        }
    }
}
fn start(hwnd: HWND, state: &mut State, task: impl FnOnce() -> Completion + Send + 'static) {
    if state.pending {
        return;
    }
    set_busy(hwnd, state, true);
    let output = state.response.clone();
    let window = hwnd.0 as usize;
    if let Err(error) = std::thread::Builder::new()
        .name("isolmass-cloud-settings".into())
        .spawn(move || {
            let completion = task();
            match output.lock() {
                Ok(mut slot) => *slot = Some(completion),
                Err(_) => {
                    crate::diagnostics::record("cloud settings", "Cloud response lock failed.");
                    return;
                }
            }
            if let Err(error) = unsafe {
                PostMessageW(
                    HWND(window as *mut _),
                    WM_CLOUD_RESULT,
                    WPARAM(0),
                    LPARAM(0),
                )
            } {
                crate::diagnostics::record(
                    "cloud settings",
                    &format!("Cloud response notification failed: {error}"),
                );
            }
        })
    {
        set_busy(hwnd, state, false);
        set_status(
            hwnd,
            &crate::i18n::tf("Could not start the Cloudflare request: {}", &[&error]),
        );
    }
}
fn begin_authorization(hwnd: HWND, state: &mut State) {
    state.cancel.store(false, Ordering::Relaxed);
    state.authorizing = true;
    set_status(
        hwnd,
        crate::i18n::t(
            "Sign in to Cloudflare in the browser and grant permission. Nothing is uploaded meanwhile.",
        ),
    );
    let cancel = state.cancel.clone();
    start(hwnd, state, move || {
        Completion::Authorization(cloudflare_oauth::authorize(&cancel))
    });
}
fn begin_install(hwnd: HWND, state: &mut State, account: Account) {
    let Some(auth) = state.authorization.take() else {
        return;
    };
    state.authorizing = false;
    set_status(
        hwnd,
        &crate::i18n::tf(
            "Installing a new Worker in {}; keep this window open…",
            &[&account.name],
        ),
    );
    let cancel = state.cancel.clone();
    start(hwnd, state, move || {
        Completion::Install(cloudflare_oauth::install(
            &auth.access_token,
            &account,
            &cancel,
        ))
    });
}
fn refresh(hwnd: HWND, state: &mut State) {
    let Some(origin) = state.settings.cloud_url.clone() else {
        return;
    };
    set_status(hwnd, crate::i18n::t("Loading statistics and images…"));
    start(hwnd, state, move || {
        Completion::Refresh(
            cloudflare_setup::admin_json(&origin, "/api/stats", "GET", None).and_then(|stats| {
                cloudflare_setup::admin_json(&origin, "/api/images?limit=50&offset=0", "GET", None)
                    .map(|images| (stats, images))
            }),
        )
    });
}
fn save_limits(hwnd: HWND, state: &mut State) {
    let Some(origin) = state.settings.cloud_url.clone() else {
        return;
    };
    if !state.loaded {
        crate::ui::error(
            hwnd,
            "Cloudflare",
            crate::i18n::t("Load the current limits with Refresh first."),
        );
        return;
    }
    let mut values = serde_json::Map::new();
    for (index, (field, multiplier)) in LIMITS.iter().enumerate() {
        let parsed = get_text(hwnd, ID_LIMIT_FIRST + index as i32)
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
            .and_then(|number| number.checked_mul(*multiplier));
        let Some(number) = parsed else {
            crate::ui::error(
                hwnd,
                crate::i18n::t("Invalid limit"),
                crate::i18n::t("Enter a positive whole number in every field."),
            );
            return;
        };
        values.insert((*field).into(), json!(number));
    }
    let choice = control(hwnd, ID_MODE)
        .map(|combo| unsafe { SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 })
        .unwrap_or(-1);
    let action = match choice {
        0 => "warn",
        1 => "block_upload",
        _ => {
            crate::ui::error(
                hwnd,
                "Cloudflare",
                crate::i18n::t("Choose what happens at the limit."),
            );
            return;
        }
    };
    values.insert("limit_action".into(), json!(action));
    set_status(hwnd, crate::i18n::t("Saving limits…"));
    start(hwnd, state, move || {
        Completion::Limits(cloudflare_setup::admin_json(
            &origin,
            "/api/settings",
            "PUT",
            Some(&Value::Object(values)),
        ))
    });
}
fn show_stats(hwnd: HWND, state: &mut State, stats: &Value, images: &Value) {
    // Workers installed before versioning report nothing: treat them as 1.
    state.outdated =
        stats["worker_version"].as_u64().unwrap_or(1) < cloudflare_oauth::WORKER_VERSION;
    let count = |value: &Value, key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
    let daily = &stats["daily"];
    let monthly = &stats["monthly"];
    let stored = &stats["images"];
    let usage = stored["stored_bytes"].as_u64().unwrap_or(0);
    set_text(
        hwnd,
        ID_STATS,
        &stats_text([
            &count(daily, "uploads"),
            &count(daily, "views"),
            &count(monthly, "uploads"),
            &count(monthly, "views"),
            &count(stored, "active"),
            &format!("{:.1}", usage as f64 / 1_048_576.0),
        ]),
    );
    for (index, (field, multiplier)) in LIMITS.iter().enumerate() {
        if let Some(number) = stats["settings"][field].as_u64() {
            set_text(
                hwnd,
                ID_LIMIT_FIRST + index as i32,
                &(number / multiplier).to_string(),
            );
        }
    }
    let mode = match stats["settings"]["limit_action"].as_str() {
        Some("warn") => 0,
        Some("block_upload") => 1,
        Some("block_all") => 1,
        _ => -1,
    };
    if let Some(combo) = control(hwnd, ID_MODE) {
        unsafe {
            SendMessageW(combo, CB_SETCURSEL, WPARAM(mode as usize), LPARAM(0));
        }
    }
    state.loaded = true;
    state.images.clear();
    if let Some(list) = control(hwnd, ID_IMAGES) {
        unsafe {
            SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        }
        let origin = state.settings.cloud_url.clone().unwrap_or_default();
        if let Some(records) = images["images"].as_array() {
            for record in records {
                let (Some(id), Some(url)) = (record["id"].as_str(), record["url"].as_str()) else {
                    continue;
                };
                // Only links of this installation are ever opened or copied.
                if !crate::upload::is_share_link(&origin, url) {
                    continue;
                }
                state.images.push((id.to_string(), url.to_string()));
                {
                    let created = record["created_at"]
                        .as_i64()
                        .map(format_time)
                        .unwrap_or_default();
                    let size = count(record, "size_bytes") as f64 / 1024.0;
                    let views = count(record, "views");
                    let size = format!("{size:.0}");
                    let password = if record["password_protected"].as_i64() == Some(1)
                        || record["password_protected"] == true
                    {
                        crate::i18n::t(" · password")
                    } else {
                        ""
                    };
                    let args: [&dyn std::fmt::Display; 4] = [&created, &size, &views, &password];
                    let label = wide(&if views == 1 {
                        crate::i18n::tf("{} · {} KB · {} view{}", &args)
                    } else {
                        crate::i18n::tf("{} · {} KB · {} views{}", &args)
                    });
                    unsafe {
                        SendMessageW(
                            list,
                            LB_ADDSTRING,
                            WPARAM(0),
                            LPARAM(label.as_ptr() as isize),
                        );
                    }
                }
            }
        }
        if !state.images.is_empty() {
            unsafe {
                SendMessageW(list, LB_SETCURSEL, WPARAM(0), LPARAM(0));
            }
        }
    }
    // The summary may wrap differently now that the numbers are known.
    relayout(hwnd, state);
    // The Update Worker button and the empty-list text depend on what loaded.
    show_tab(hwnd, state);
    sync_image_actions(hwnd, state);
    set_status(
        hwnd,
        if state.outdated {
            crate::i18n::t("A Worker update is available. Press Update Worker.")
        } else if state.images.is_empty() {
            crate::i18n::t("Connected · No images uploaded yet.")
        } else {
            crate::i18n::t("Connected · Statistics are up to date.")
        },
    );
}

/// Local `DD.MM.YYYY HH:MM` (Turkish) or `YYYY-MM-DD HH:MM` for a Unix timestamp,
/// using the Windows time zone.
fn format_time(seconds: i64) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // FILETIME counts 100 ns intervals since 1601-01-01.
    let ticks = (seconds.max(0) as u64 + 11_644_473_600) * 10_000_000;
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut universal = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    let converted = unsafe {
        FileTimeToSystemTime(&file_time, &mut universal).is_ok()
            && SystemTimeToTzSpecificLocalTime(None, &universal, &mut local).is_ok()
    };
    if !converted {
        return String::new();
    }
    match crate::i18n::current() {
        crate::i18n::Language::Turkish => format!(
            "{:02}.{:02}.{:04} {:02}:{:02}",
            local.wDay, local.wMonth, local.wYear, local.wHour, local.wMinute
        ),
        crate::i18n::Language::English => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
        ),
    }
}

fn selected_image(hwnd: HWND, state: &State) -> Option<(String, String)> {
    let list = control(hwnd, ID_IMAGES)?;
    let index = unsafe { SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 };
    usize::try_from(index)
        .ok()
        .and_then(|i| state.images.get(i))
        .cloned()
}

fn open_image(hwnd: HWND, state: &State) {
    match selected_image(hwnd, state) {
        Some((_, url)) => {
            if let Err(error) = crate::upload::open_link(&url) {
                crate::ui::error(hwnd, crate::i18n::t("Open image"), &error);
            }
        }
        None => crate::ui::error(
            hwnd,
            crate::i18n::t("Open image"),
            crate::i18n::t("Select an image in the list first."),
        ),
    }
}

fn copy_image(hwnd: HWND, state: &State) {
    match selected_image(hwnd, state) {
        Some((_, url)) => match crate::clipboard::copy_text_to_clipboard(Some(hwnd), &url) {
            Ok(()) => set_status(hwnd, crate::i18n::t("Link copied to the clipboard.")),
            Err(error) => crate::ui::error(hwnd, crate::i18n::t("Copy link"), &error.to_string()),
        },
        None => crate::ui::error(
            hwnd,
            crate::i18n::t("Copy link"),
            crate::i18n::t("Select an image in the list first."),
        ),
    }
}
fn delete_image(hwnd: HWND, state: &mut State) {
    let Some(origin) = state.settings.cloud_url.clone() else {
        return;
    };
    let Some((id, _)) = selected_image(hwnd, state) else {
        crate::ui::error(
            hwnd,
            crate::i18n::t("Delete image"),
            crate::i18n::t("Select an image in the list first."),
        );
        return;
    };
    if !crate::ui::confirm_action(
        hwnd,
        crate::i18n::t("Delete image"),
        crate::i18n::t("The link will stop working permanently. Continue?"),
        crate::i18n::t("Delete"),
        true,
    ) {
        return;
    }
    set_status(hwnd, crate::i18n::t("Deleting image…"));
    start(hwnd, state, move || {
        Completion::Delete(
            cloudflare_setup::admin_json(&origin, &format!("/api/images/{id}"), "DELETE", None)
                .map(|_| id),
        )
    });
}
fn save_password(hwnd: HWND, state: &mut State) {
    let Some(origin) = state.settings.cloud_url.as_deref() else {
        return;
    };
    let password = match get_text(hwnd, ID_PASSWORD) {
        Ok(value) if value.is_empty() => None,
        Ok(value)
            if value.chars().count() >= 12
                && value.len() <= 128
                && !value.chars().any(char::is_control) =>
        {
            Some(value)
        }
        Ok(_) => {
            crate::ui::error(
                hwnd,
                crate::i18n::t("Image password"),
                crate::i18n::t(
                    "The password needs at least 12 characters and at most 128 UTF-8 bytes; leave it empty for none.",
                ),
            );
            return;
        }
        Err(error) => {
            crate::ui::error(hwnd, crate::i18n::t("Image password"), &error);
            return;
        }
    };
    match cloudflare_setup::load_credentials(origin).and_then(|mut credentials| {
        credentials.share_password = password;
        cloudflare_setup::save_credentials(&credentials)
    }) {
        Ok(()) => set_status(
            hwnd,
            crate::i18n::t("The password for new uploads is stored protected on this computer."),
        ),
        Err(error) => crate::ui::error(hwnd, crate::i18n::t("Image password"), &error),
    }
}
fn disconnect(hwnd: HWND, state: &mut State) {
    if !crate::ui::confirm_action(
        hwnd,
        crate::i18n::t("Disconnect"),
        crate::i18n::t(
            "Only this computer's Cloudflare connection is removed; the Worker and images in the cloud stay. Continue?",
        ),
        crate::i18n::t("Disconnect"),
        true,
    ) {
        return;
    }
    // Only the address is persisted: the settings held here may carry edits
    // the owner has not saved yet.
    if let Err(error) = Settings::save_cloud_url(None) {
        crate::ui::error(
            hwnd,
            crate::i18n::t("Could not save settings"),
            &error.to_string(),
        );
        return;
    }
    if let Err(error) = cloudflare_setup::forget_credentials() {
        crate::ui::error(
            hwnd,
            crate::i18n::t("Could not delete the credentials"),
            &error,
        );
        return;
    }
    state.settings.cloud_url = None;
    state.saved = true;
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}
fn command(hwnd: HWND, state: &mut State, id: i32) {
    if state.pending && id != ID_CLOSE {
        return;
    }
    match id {
        ID_CONNECT => begin_authorization(hwnd, state),
        ID_INSTALL => {
            let selection = control(hwnd, ID_ACCOUNT)
                .map(|combo| unsafe { SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 });
            let selected = selection
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| state.authorization.as_ref()?.accounts.get(i))
                .cloned();
            if let Some(account) = selected {
                begin_install(hwnd, state, account);
            } else {
                crate::ui::error(
                    hwnd,
                    "Cloudflare",
                    crate::i18n::t("Choose the account to install into."),
                );
            }
        }
        ID_REFRESH => refresh(hwnd, state),
        ID_SAVE_LIMITS => save_limits(hwnd, state),
        ID_SAVE_PASSWORD => save_password(hwnd, state),
        ID_DELETE => delete_image(hwnd, state),
        ID_OPEN_IMAGE => open_image(hwnd, state),
        ID_COPY_IMAGE => copy_image(hwnd, state),
        ID_UPDATE_WORKER => {
            state.updating = true;
            begin_authorization(hwnd, state);
            set_status(
                hwnd,
                crate::i18n::t(
                    "In the browser, authorize the account that holds the Worker; the address, links and images stay.",
                ),
            );
        }
        ID_FORGET => disconnect(hwnd, state),
        ID_CLOSE => close(hwnd, state),
        _ => {}
    }
}
fn close(hwnd: HWND, state: &mut State) {
    if state.pending {
        if state.authorizing {
            state.cancel.store(true, Ordering::Relaxed);
            set_status(hwnd, crate::i18n::t("Cancelling the Cloudflare sign-in…"));
        } else {
            set_status(
                hwnd,
                crate::i18n::t(
                    "The window cannot close until the Cloudflare operation ends; wait a few seconds.",
                ),
            );
        }
        return;
    }
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

fn footer_top(hwnd: HWND, dpi: u32) -> i32 {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    client.bottom - scale(FOOTER, dpi)
}

fn in_footer(id: i32, state: &State) -> bool {
    id == ID_CLOSE || (id == ID_STATUS && !state.guided())
}

/// The text-box frame around a visible input, in client coordinates. The
/// input sits inset in it (see [`place_framed`]), so the frame is its window
/// rectangle grown by the padding.
fn input_frame(hwnd: HWND, dpi: u32, id: i32) -> Option<RECT> {
    let child = control(hwnd, id)?;
    if !unsafe { IsWindowVisible(child) }.as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    unsafe { GetWindowRect(child, &mut rect) }.ok()?;
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
    // A null source window maps from screen coordinates.
    unsafe { MapWindowPoints(HWND::default(), hwnd, &mut corners) };
    let (pad_x, pad_y) = frame_padding(id);
    Some(RECT {
        left: corners[0].x - scale(pad_x, dpi),
        top: corners[0].y - scale(pad_y, dpi),
        right: corners[1].x + scale(pad_x, dpi),
        bottom: corners[1].y + scale(pad_y, dpi),
    })
}

fn framed_ids() -> impl Iterator<Item = i32> {
    [ID_PASSWORD, ID_IMAGES]
        .into_iter()
        .chain(ID_LIMIT_FIRST..ID_LIMIT_FIRST + LIMITS.len() as i32)
}

/// Windows 11 text boxes: a rounded `stroke` frame on the input fill, and
/// while an edit has the focus a 2 px accent line along its bottom edge.
fn paint_input_frames(hwnd: HWND, state: &State, hdc: HDC) {
    let tokens = crate::theme::tokens();
    let focus = unsafe { GetFocus() };
    let diameter = crate::drawing::corner_diameter(crate::theme::RADIUS_CONTROL, state.dpi);
    for id in framed_ids() {
        let Some(frame) = input_frame(hwnd, state.dpi, id) else {
            continue;
        };
        // A partial repaint (one focus change) redraws only the frames it touches.
        if !unsafe { RectVisible(hdc, &frame) }.as_bool() {
            continue;
        }
        crate::drawing::rounded(
            hdc,
            crate::capture::Rect::new(frame.left, frame.top, frame.right, frame.bottom),
            diameter,
            tokens.control_fill,
            tokens.stroke,
        );
        if id != ID_IMAGES && control(hwnd, id) == Some(focus) {
            // Between the corner arcs, over the bottom stroke.
            let line = RECT {
                left: frame.left + diameter / 2,
                top: frame.bottom - scale(2, state.dpi).max(1),
                right: frame.right - diameter / 2,
                bottom: frame.bottom,
            };
            // The shared brush cache keeps this paint free of GDI allocations.
            crate::drawing::with_brush(hdc, tokens.accent_ink, || unsafe {
                let _ = PatBlt(
                    hdc,
                    line.left,
                    line.top,
                    line.right - line.left,
                    line.bottom - line.top,
                    PATCOPY,
                );
            });
        }
    }
}

/// Repaints the frame of input `id` after it gained or lost the focus.
fn invalidate_input_frame(hwnd: HWND, state: &State, id: i32) {
    if let Some(frame) = input_frame(hwnd, state.dpi, id) {
        unsafe {
            let _ = InvalidateRect(hwnd, Some(&frame), false);
        }
    }
}

fn paint_surface(hwnd: HWND, state: &State, hdc: HDC) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
        let _ = FillRect(hdc, &client, state.content_brush);
    }
    let top = footer_top(hwnd, state.dpi);
    unsafe {
        let footer = RECT { top, ..client };
        let _ = FillRect(hdc, &footer, state.footer_brush);
    }
    let guided = state.guided();
    let bar = scale(TAB_BAR, state.dpi);
    crate::drawing::with_pen(hdc, PS_SOLID, 1, crate::theme::tokens().stroke, || unsafe {
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
        if !guided {
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
        }
    });
    paint_input_frames(hwnd, state, hdc);
}

/// Parts of an image row, in device pixels, for the row rectangle `row`:
/// the selection background, the accent pill at its left edge and the text
/// area.
struct RowParts {
    background: RECT,
    pill: RECT,
    text: RECT,
}

fn image_row_parts(row: RECT, dpi: u32) -> RowParts {
    // The vertical inset leaves room for the focus ring around the background.
    let background = RECT {
        left: row.left + scale(4, dpi),
        top: row.top + scale(3, dpi),
        right: row.right - scale(4, dpi),
        bottom: row.bottom - scale(3, dpi),
    };
    let pill_height = scale(16, dpi);
    let pill_top = (background.top + background.bottom - pill_height) / 2;
    RowParts {
        background,
        pill: RECT {
            left: background.left,
            top: pill_top,
            right: background.left + scale(3, dpi),
            bottom: pill_top + pill_height,
        },
        text: RECT {
            left: row.left + scale(16, dpi),
            top: row.top,
            right: row.right - scale(12, dpi),
            bottom: row.bottom,
        },
    }
}

/// Windows 11 list item for `WM_DRAWITEM` of the image list: text on the
/// list's fill, a selected row on a rounded `subtle_hover` background with
/// an accent pill, and the focus visual only while the list has the keyboard
/// focus.
fn draw_image_row(draw: &DRAWITEMSTRUCT, state: &State) {
    let tokens = crate::theme::tokens();
    let dpi = state.dpi;
    let hdc = draw.hDC;
    unsafe {
        let _ = FillRect(hdc, &draw.rcItem, state.input_brush);
    }
    // An empty list asks for its focus rectangle with no item.
    if draw.itemID == u32::MAX {
        return;
    }
    let parts = image_row_parts(draw.rcItem, dpi);
    let selected = draw.itemState.0 & ODS_SELECTED.0 != 0;
    if selected {
        crate::drawing::rounded(
            hdc,
            crate::capture::Rect::new(
                parts.background.left,
                parts.background.top,
                parts.background.right,
                parts.background.bottom,
            ),
            crate::drawing::corner_diameter(crate::theme::RADIUS_CONTROL, dpi),
            tokens.subtle_hover,
            tokens.subtle_hover,
        );
        crate::drawing::rounded(
            hdc,
            crate::capture::Rect::new(
                parts.pill.left,
                parts.pill.top,
                parts.pill.right,
                parts.pill.bottom,
            ),
            scale(3, dpi),
            tokens.accent_ink,
            tokens.accent_ink,
        );
    }
    let item = WPARAM(draw.itemID as usize);
    let length = unsafe { SendMessageW(draw.hwndItem, LB_GETTEXTLEN, item, LPARAM(0)) }.0;
    if let Ok(length) = usize::try_from(length) {
        let mut text = vec![0u16; length + 1];
        let copied = unsafe {
            SendMessageW(
                draw.hwndItem,
                LB_GETTEXT,
                item,
                LPARAM(text.as_mut_ptr() as isize),
            )
        }
        .0;
        text.truncate(usize::try_from(copied).unwrap_or(0));
        let mut area = parts.text;
        crate::drawing::ui_text(
            hdc,
            &mut text,
            &mut area,
            dpi,
            tokens.text,
            crate::drawing::Align::Left,
        );
    }
    let cues = draw.itemState.0 & ODS_FOCUS.0 != 0 && draw.itemState.0 & ODS_NOFOCUSRECT.0 == 0;
    if cues && unsafe { GetFocus() } == draw.hwndItem {
        crate::drawing::focus_ring(
            hdc,
            parts.background,
            scale(crate::theme::RADIUS_CONTROL, dpi),
            dpi,
        );
    }
}

fn draw_cloud_button(draw: &NMCUSTOMDRAW, state: &State) {
    use crate::drawing::{Look, fluent_button, fluent_tab};
    let id = draw.hdr.idFrom as i32;
    let look = Look {
        primary: id == state.default_id,
        ..Look::from_custom_draw(draw)
    };
    let background = if in_footer(id, state) {
        state.footer_brush
    } else {
        state.content_brush
    };
    let mut rect = RECT::default();
    let mut label = [0u16; 128];
    let length = unsafe {
        let _ = GetClientRect(draw.hdr.hwndFrom, &mut rect);
        GetWindowTextW(draw.hdr.hwndFrom, &mut label)
    }
    .max(0) as usize;
    if (ID_TAB_FIRST..ID_TAB_FIRST + TAB_COUNT as i32).contains(&id) {
        fluent_tab(
            draw.hdc,
            rect,
            &mut label[..length],
            state.dpi,
            look,
            background,
        );
    } else {
        fluent_button(
            draw.hdc,
            rect,
            &mut label[..length],
            state.dpi,
            look,
            background,
        );
    }
}

/// Focus changes and selections of the inputs. Enter presses the button of
/// the section that has the focus; leaving the section restores Close.
fn input_event(hwnd: HWND, state: &mut State, id: i32, code: u32) {
    let (gained, lost) = match id {
        ID_MODE => (CBN_SETFOCUS, CBN_KILLFOCUS),
        ID_IMAGES => (LBN_SETFOCUS, LBN_KILLFOCUS),
        _ => (EN_SETFOCUS, EN_KILLFOCUS),
    };
    if code == gained || code == lost {
        let target = if code == gained {
            default_for(id)
        } else {
            ID_CLOSE
        };
        set_default(hwnd, state, target);
        if id != ID_MODE {
            invalidate_input_frame(hwnd, state, id);
        }
    } else if id == ID_IMAGES && code == LBN_SELCHANGE {
        sync_image_actions(hwnd, state);
    } else if id == ID_IMAGES && code == LBN_DBLCLK {
        open_image(hwnd, state);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    match msg {
        WM_ERASEBKGND if !pointer.is_null() => LRESULT(1),
        WM_PAINT if !pointer.is_null() => {
            let mut paint = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
            paint_surface(hwnd, unsafe { &*pointer }, hdc);
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_NOTIFY if !pointer.is_null() && lparam.0 != 0 => {
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.code == NM_CUSTOMDRAW {
                let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                if draw.dwDrawStage == CDDS_PREPAINT {
                    draw_cloud_button(draw, unsafe { &*pointer });
                    return LRESULT(CDRF_SKIPDEFAULT as isize);
                }
            }
            LRESULT(0)
        }
        WM_MEASUREITEM if !pointer.is_null() && wparam.0 as i32 == ID_IMAGES => {
            let state = unsafe { &*pointer };
            let item = unsafe { &mut *(lparam.0 as *mut MEASUREITEMSTRUCT) };
            item.itemHeight = scale(IMAGE_ROW, state.dpi) as u32;
            LRESULT(1)
        }
        WM_DRAWITEM if !pointer.is_null() && wparam.0 as i32 == ID_IMAGES => {
            let draw = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            draw_image_row(draw, unsafe { &*pointer });
            LRESULT(1)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX if !pointer.is_null() => {
            let state = unsafe { &*pointer };
            let tokens = crate::theme::tokens();
            let hdc = HDC(wparam.0 as *mut _);
            unsafe {
                let _ = SetTextColor(hdc, tokens.text);
                let _ = SetBkColor(hdc, tokens.control_fill);
            }
            LRESULT(state.input_brush.0 as isize)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN if !pointer.is_null() => {
            let state = unsafe { &*pointer };
            let child = HWND(lparam.0 as *mut _);
            let id = unsafe { GetDlgCtrlID(child) };
            let hdc = HDC(wparam.0 as *mut _);
            let tokens = crate::theme::tokens();
            let secondary = matches!(
                id,
                ID_CONSENT_NOTE | ID_COST_NOTE | ID_STATUS | ID_IMAGES_EMPTY
            ) || (id == ID_INTRO && !state.guided());
            unsafe {
                let _ = SetBkMode(hdc, TRANSPARENT);
                let _ = SetTextColor(
                    hdc,
                    if !IsWindowEnabled(child).as_bool() {
                        tokens.text_disabled
                    } else if secondary {
                        tokens.text_secondary
                    } else {
                        tokens.text
                    },
                );
            }
            LRESULT(if id == ID_IMAGES_EMPTY {
                // Lies on the list, so it takes the list's fill.
                state.input_brush.0 as isize
            } else if in_footer(id, state) {
                state.footer_brush.0 as isize
            } else {
                state.content_brush.0 as isize
            })
        }
        WM_SETTINGCHANGE if !pointer.is_null() => {
            crate::theme::invalidate_theme_cache();
            let state = unsafe { &mut *pointer };
            state.refresh_brushes();
            unsafe {
                crate::theme::apply_window_theme(
                    hwnd,
                    crate::theme::theme() == crate::theme::Theme::Dark,
                    false,
                );
                let _ = RedrawWindow(
                    hwnd,
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
                );
            }
            LRESULT(0)
        }
        WM_DPICHANGED if !pointer.is_null() => {
            let state = unsafe { &mut *pointer };
            state.dpi = ((wparam.0 & 0xffff) as u32).max(96);
            state.refresh_fonts(hwnd);
            let height = layout(hwnd, state);
            let (width, height) = window_size(state.dpi, height);
            if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                // Still being set up: centre at the new size on the target monitor.
                position(
                    hwnd,
                    state.owner,
                    target_monitor(state.owner),
                    width,
                    height,
                );
            } else if lparam.0 != 0 {
                let rect = unsafe { &*(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        rect.left,
                        rect.top,
                        width,
                        height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            unsafe {
                // Children moved without repainting; the parent clips them.
                let _ = RedrawWindow(
                    hwnd,
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                );
            }
            LRESULT(0)
        }
        crate::ui::WM_SWITCH_TAB if !pointer.is_null() => {
            let state = unsafe { &mut *pointer };
            if !state.guided() {
                state.tab = crate::ui::next_tab(state.tab, TAB_COUNT, wparam);
                show_tab(hwnd, state);
                if let Some(tab) = control(hwnd, ID_TAB_FIRST + state.tab as i32) {
                    unsafe {
                        let _ = SetFocus(tab);
                    }
                }
            }
            LRESULT(0)
        }
        DM_GETDEFID if !pointer.is_null() => {
            let state = unsafe { &*pointer };
            LRESULT(((DC_HASDEFID << 16) | state.default_id as u32) as isize)
        }
        DM_SETDEFID if !pointer.is_null() => {
            set_default(hwnd, unsafe { &mut *pointer }, wparam.0 as i32);
            LRESULT(1)
        }
        WM_CLOUD_RESULT if !pointer.is_null() => {
            let state = unsafe { &mut *pointer };
            set_busy(hwnd, state, false);
            state.authorizing = false;
            let response = state.response.lock().ok().and_then(|mut slot| slot.take());
            if matches!(response, Some(Completion::Authorization(_))) {
                // The browser holds the foreground after the consent page.
                crate::ui::bring_to_front(hwnd);
            }
            match response {
                Some(Completion::Authorization(Ok(auth)))
                    if state.cancel.load(Ordering::Relaxed) =>
                {
                    drop(auth);
                    set_status(
                        hwnd,
                        crate::i18n::t(
                            "The Cloudflare connection was cancelled; the screenshot stayed local.",
                        ),
                    );
                }
                Some(Completion::Authorization(Ok(auth))) if state.updating => {
                    state.updating = false;
                    match state
                        .settings
                        .cloud_url
                        .as_deref()
                        .ok_or_else(|| crate::i18n::t("No connection found.").to_string())
                        .and_then(cloudflare_setup::load_credentials)
                    {
                        Ok(credentials) => {
                            set_status(
                                hwnd,
                                crate::i18n::t("Updating the Worker; keep this window open…"),
                            );
                            let cancel = state.cancel.clone();
                            start(hwnd, state, move || {
                                Completion::WorkerUpdate(cloudflare_oauth::update_worker(
                                    &auth.access_token,
                                    &auth.accounts,
                                    &credentials,
                                    &cancel,
                                ))
                            });
                        }
                        Err(error) => {
                            crate::ui::error(hwnd, crate::i18n::t("Worker update"), &error)
                        }
                    }
                }
                Some(Completion::WorkerUpdate(Ok(credentials))) => {
                    match cloudflare_setup::save_credentials(&credentials) {
                        Ok(()) => {
                            state.outdated = false;
                            relayout(hwnd, state);
                            show_tab(hwnd, state);
                            set_status(hwnd, crate::i18n::t("The Worker was updated."));
                            refresh(hwnd, state);
                        }
                        Err(error) => {
                            crate::ui::error(hwnd, crate::i18n::t("Worker update"), &error)
                        }
                    }
                }
                Some(Completion::WorkerUpdate(Err(error))) => {
                    set_status(
                        hwnd,
                        crate::i18n::t("Could not update the Worker; see the message for details."),
                    );
                    crate::ui::error(hwnd, crate::i18n::t("Worker update"), &error);
                }
                Some(Completion::Authorization(Ok(auth))) => {
                    let only = auth.accounts.len() == 1;
                    if let Some(combo) = control(hwnd, ID_ACCOUNT) {
                        unsafe {
                            SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
                        }
                        for account in &auth.accounts {
                            let text = wide(&format!("{} · {}", account.name, &account.id[24..]));
                            unsafe {
                                SendMessageW(
                                    combo,
                                    CB_ADDSTRING,
                                    WPARAM(0),
                                    LPARAM(text.as_ptr() as isize),
                                );
                            }
                        }
                        unsafe {
                            SendMessageW(combo, CB_SETCURSEL, WPARAM(0), LPARAM(0));
                        }
                    }
                    state.authorization = Some(auth);
                    if only {
                        if let Some(account) = state
                            .authorization
                            .as_ref()
                            .and_then(|auth| auth.accounts.first())
                            .cloned()
                        {
                            begin_install(hwnd, state, account);
                        }
                    } else {
                        show_step(hwnd, state, true);
                        set_status(
                            hwnd,
                            crate::i18n::t(
                                "Choose an account; a new Worker is created only in the account you choose.",
                            ),
                        );
                    }
                }
                Some(Completion::Install(Ok(installed))) => {
                    let credentials = installed.credentials;
                    // Only the address is persisted: the settings held here may
                    // carry edits the owner has not saved yet.
                    if let Err(error) =
                        cloudflare_setup::save_credentials(&credentials).and_then(|()| {
                            Settings::save_cloud_url(Some(credentials.origin.clone()))
                                .map_err(|error| error.to_string())
                        })
                    {
                        set_status(
                            hwnd,
                            crate::i18n::t(
                                "The Worker was installed but the local connection could not be saved.",
                            ),
                        );
                        crate::ui::error(hwnd, crate::i18n::t("Could not save the setup"), &error);
                    } else {
                        state.settings.cloud_url = Some(credentials.origin);
                        state.saved = true;
                        let message = match (installed.ready, state.for_upload) {
                            (true, true) => {
                                crate::i18n::t("Connected. The screenshot is uploading now.")
                            }
                            (true, false) => crate::i18n::t("Connected. You can now use Upload."),
                            (false, true) => crate::i18n::t(
                                "The Worker is installed in your account and paired with this computer. Its workers.dev address can take a few minutes to come online; the upload is tried now, and if it fails, try again shortly.",
                            ),
                            (false, false) => crate::i18n::t(
                                "The Worker is installed in your account and paired with this computer. Its workers.dev address can take a few minutes to come online.",
                            ),
                        };
                        crate::ui::info(hwnd, crate::i18n::t("Cloudflare is ready"), message);
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                }
                Some(Completion::Refresh(Ok((stats, images)))) => {
                    show_stats(hwnd, state, &stats, &images)
                }
                Some(Completion::Limits(Ok(_))) => {
                    set_status(hwnd, crate::i18n::t("Limits saved."));
                    refresh(hwnd, state);
                }
                Some(Completion::Delete(Ok(_))) => {
                    set_status(hwnd, crate::i18n::t("Image deleted."));
                    refresh(hwnd, state);
                }
                Some(Completion::Install(Err(error))) => {
                    show_step(hwnd, state, false);
                    set_status(
                        hwnd,
                        crate::i18n::t(
                            "Setup did not finish. Choose Continue with Cloudflare again.",
                        ),
                    );
                    crate::ui::error(hwnd, "Cloudflare", &error);
                }
                Some(Completion::Authorization(Err(error))) => {
                    state.updating = false;
                    set_status(
                        hwnd,
                        crate::i18n::t(
                            "The Cloudflare operation failed; see the message for details.",
                        ),
                    );
                    crate::ui::error(hwnd, "Cloudflare", &error);
                }
                Some(Completion::Refresh(Err(error)))
                | Some(Completion::Limits(Err(error)))
                | Some(Completion::Delete(Err(error))) => {
                    set_status(
                        hwnd,
                        crate::i18n::t(
                            "The Cloudflare operation failed; see the message for details.",
                        ),
                    );
                    crate::ui::error(hwnd, "Cloudflare", &error);
                }
                None => crate::diagnostics::record(
                    "cloud settings",
                    "Completion message without response.",
                ),
            }
            LRESULT(0)
        }
        WM_COMMAND if !pointer.is_null() && is_input((wparam.0 & 0xffff) as i32) => {
            input_event(
                hwnd,
                unsafe { &mut *pointer },
                (wparam.0 & 0xffff) as i32,
                (wparam.0 >> 16) as u32,
            );
            LRESULT(0)
        }
        WM_COMMAND if !pointer.is_null() && wparam.0 >> 16 == 0 => {
            let state = unsafe { &mut *pointer };
            let id = (wparam.0 & 0xffff) as i32;
            if (ID_TAB_FIRST..ID_TAB_FIRST + TAB_COUNT as i32).contains(&id) {
                state.tab = (id - ID_TAB_FIRST) as usize;
                show_tab(hwnd, state);
            } else {
                command(hwnd, state, id);
            }
            LRESULT(0)
        }
        WM_CLOSE if !pointer.is_null() => {
            close(hwnd, unsafe { &mut *pointer });
            LRESULT(0)
        }
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
fn register() -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE::default(),
        hIcon: crate::tray::app_icon(),
        hIconSm: crate::tray::app_icon_small(),
        hCursor: unsafe { LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default() },
        hbrBackground: HBRUSH::default(),
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
fn build_controls(hwnd: HWND, state: &State) -> Result<()> {
    if state.guided() {
        label(
            hwnd,
            ID_INTRO,
            crate::i18n::t(
                "Your screenshots are uploaded only to the Worker in your own Cloudflare account. Nothing is sent until you connect.",
            ),
        )?;
        label(
            hwnd,
            ID_STEP_CONSENT,
            crate::i18n::t("1 · Grant permission in Cloudflare"),
        )?;
        label(
            hwnd,
            ID_CONSENT_NOTE,
            crate::i18n::t(
                "The consent page opens in the browser. After granting permission, come back to this window; setup finishes here.",
            ),
        )?;
        label(
            hwnd,
            ID_ACCOUNT_GUIDE,
            &account_guidance(state.settings.cloud_url.as_deref()),
        )?;
        default_button(hwnd, ID_CONNECT, crate::i18n::t("Continue with Cloudflare"))?;
        label(
            hwnd,
            ID_STEP_ACCOUNT,
            crate::i18n::t("2 · Choose the account"),
        )?;
        create(
            hwnd,
            w!("COMBOBOX"),
            ID_ACCOUNT,
            "",
            WINDOW_STYLE(WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0),
        )?;
        button(hwnd, ID_INSTALL, crate::i18n::t("Install in this account"))?;
        label(
            hwnd,
            ID_STATUS,
            crate::i18n::t("Not connected · The screenshot stays on this computer."),
        )?;
        button(hwnd, ID_CLOSE, crate::i18n::t("Cancel"))?;
        account_controls(hwnd, false);
        return Ok(());
    }
    for (index, name) in tab_names().iter().enumerate() {
        create(
            hwnd,
            w!("BUTTON"),
            ID_TAB_FIRST + index as i32,
            name,
            WINDOW_STYLE(
                WS_TABSTOP.0
                    | BS_AUTORADIOBUTTON as u32
                    | BS_PUSHLIKE as u32
                    | if index == 0 { WS_GROUP.0 } else { 0 },
            ),
        )?;
    }
    single_line_label(
        hwnd,
        ID_INTRO,
        state
            .settings
            .cloud_url
            .as_deref()
            .unwrap_or("")
            .trim_start_matches("https://"),
    )?;
    label(hwnd, ID_STATS, crate::i18n::t("Loading statistics…"))?;
    label(
        hwnd,
        ID_PASSWORD_LABEL,
        crate::i18n::t("Image password for new uploads (empty: none)"),
    )?;
    edit(hwnd, ID_PASSWORD, ES_PASSWORD)?;
    button(hwnd, ID_SAVE_PASSWORD, crate::i18n::t("Save password"))?;
    button(hwnd, ID_REFRESH, crate::i18n::t("Refresh"))?;
    button(hwnd, ID_UPDATE_WORKER, crate::i18n::t("Update Worker"))?;
    label(
        hwnd,
        ID_ACCOUNT_GUIDE,
        &account_guidance(state.settings.cloud_url.as_deref()),
    )?;
    button(hwnd, ID_FORGET, crate::i18n::t("Disconnect"))?;
    for (index, name) in limit_labels().iter().enumerate() {
        single_line_label(hwnd, ID_LIMIT_LABEL_FIRST + index as i32, name)?;
        edit(hwnd, ID_LIMIT_FIRST + index as i32, ES_NUMBER)?;
    }
    single_line_label(hwnd, ID_MODE_LABEL, crate::i18n::t("At the limit"))?;
    let combo = create(
        hwnd,
        w!("COMBOBOX"),
        ID_MODE,
        "",
        WINDOW_STYLE(WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0),
    )?;
    for text in [
        crate::i18n::t("Warn only"),
        crate::i18n::t("Stop new uploads"),
    ] {
        let value = wide(text);
        unsafe {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(value.as_ptr() as isize),
            );
        }
    }
    label(
        hwnd,
        ID_COST_NOTE,
        crate::i18n::t(
            "Viewing is never blocked; limits count this Worker only and do not guarantee the bill.",
        ),
    )?;
    button(hwnd, ID_SAVE_LIMITS, crate::i18n::t("Save limits"))?;
    label(
        hwnd,
        ID_IMAGES_LABEL,
        crate::i18n::t("Recent images · Double-click to open"),
    )?;
    create(
        hwnd,
        w!("LISTBOX"),
        ID_IMAGES,
        "",
        WINDOW_STYLE(
            WS_TABSTOP.0
                | WS_VSCROLL.0
                | LBS_NOTIFY as u32
                | LBS_OWNERDRAWFIXED as u32
                | LBS_HASSTRINGS as u32,
        ),
    )?;
    // Created after the list so it lies on top of it.
    create(
        hwnd,
        w!("STATIC"),
        ID_IMAGES_EMPTY,
        crate::i18n::t("No uploads yet. Use Upload in the editor to share a screenshot."),
        WINDOW_STYLE(SS_CENTER | SS_NOPREFIX),
    )?;
    button(hwnd, ID_OPEN_IMAGE, crate::i18n::t("Open"))?;
    button(hwnd, ID_COPY_IMAGE, crate::i18n::t("Copy link"))?;
    button(hwnd, ID_DELETE, crate::i18n::t("Delete"))?;
    label(hwnd, ID_STATUS, crate::i18n::t("Connected"))?;
    default_button(hwnd, ID_CLOSE, crate::i18n::t("Close"))?;
    if let Some(origin) = state.settings.cloud_url.as_deref()
        && let Ok(credentials) = cloudflare_setup::load_credentials(origin)
    {
        set_text(
            hwnd,
            ID_PASSWORD,
            credentials.share_password.as_deref().unwrap_or(""),
        );
    }
    Ok(())
}
struct OwnerGuard(HWND);
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = EnableWindow(self.0, true);
            let _ = SetActiveWindow(self.0);
        }
    }
}
pub fn show(current: &Settings, owner: HWND) -> Result<Option<Settings>> {
    show_impl(current, Some(owner), false)
}
/// Opened from the capture editor, which hides itself meanwhile: the dialog is
/// a normal top-level window with its own taskbar button so the user can move
/// between it and the browser that shows the Cloudflare consent page.
pub fn show_for_upload(current: &Settings) -> Result<Option<Settings>> {
    show_impl(current, None, true)
}

const STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);

fn window_size(dpi: u32, client_height: i32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(WIDTH, dpi),
        bottom: scale(client_height, dpi),
    };
    unsafe {
        let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
            &mut rect,
            STYLE,
            false,
            Default::default(),
            dpi,
        );
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// The monitor the dialog opens on: the owner's, or the one under the cursor.
fn target_monitor(owner: Option<HWND>) -> HMONITOR {
    match owner {
        Some(owner) => unsafe { MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST) },
        None => {
            let mut cursor = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cursor);
                MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
            }
        }
    }
}

fn monitor_dpi(monitor: HMONITOR) -> Option<u32> {
    let (mut x, mut y) = (0, 0);
    unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) }
        .ok()
        .map(|()| x)
}

fn work_area(monitor: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then_some(info.rcWork)
}

/// Centres the dialog on its owner, or on the monitor under the cursor.
fn position(hwnd: HWND, owner: Option<HWND>, monitor: HMONITOR, width: i32, height: i32) {
    let Some(work) = work_area(monitor) else {
        crate::diagnostics::record("cloudflare view", "Could not measure the work area.");
        return;
    };
    let mut anchor = work;
    if let Some(owner) = owner
        && unsafe { GetWindowRect(owner, &mut anchor) }.is_err()
    {
        anchor = work;
    }
    let height = height.min(work.bottom - work.top);
    let x = ((anchor.left + anchor.right - width) / 2)
        .clamp(work.left, (work.right - width).max(work.left));
    let y = ((anchor.top + anchor.bottom - height) / 2)
        .clamp(work.top, (work.bottom - height).max(work.top));
    if let Err(error) = unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    } {
        crate::diagnostics::record("cloudflare view", &error.to_string());
    }
}

fn show_impl(
    current: &Settings,
    owner: Option<HWND>,
    for_upload: bool,
) -> Result<Option<Settings>> {
    register()?;
    crate::theme::set_preference(current.theme_preference);
    crate::i18n::set_preference(current.language);
    let mut state = Box::new(State {
        settings: current.clone(),
        saved: false,
        for_upload,
        pending: false,
        authorizing: false,
        updating: false,
        outdated: false,
        cancel: Arc::new(AtomicBool::new(false)),
        authorization: None,
        loaded: false,
        tab: 0,
        images: Vec::new(),
        response: Arc::new(Mutex::new(None)),
        dpi: 96,
        owner,
        default_id: if current.cloud_url.is_none() {
            ID_CONNECT
        } else {
            ID_CLOSE
        },
        font: HFONT::default(),
        heading_font: HFONT::default(),
        content_brush: HBRUSH::default(),
        footer_brush: HBRUSH::default(),
        input_brush: HBRUSH::default(),
    });
    state.refresh_brushes();
    // Created on the monitor it will be shown on, so the window starts with
    // that monitor's DPI and is laid out once at the right scale.
    let monitor = target_monitor(owner);
    let origin = work_area(monitor).map(|work| (work.left, work.top));
    let hwnd = unsafe {
        CreateWindowExW(
            // A taskbar button even when owned by the Settings window.
            WS_EX_APPWINDOW,
            CLASS,
            w!("isolmaSS Cloudflare"),
            STYLE,
            origin.map_or(CW_USEDEFAULT, |(left, _)| left),
            origin.map_or(CW_USEDEFAULT, |(_, top)| top),
            WIDTH,
            400,
            owner.unwrap_or_default(),
            None,
            HINSTANCE::default(),
            None,
        )
    }?;
    let _window = crate::ui::OwnedWindow(hwnd);
    let _suspend = crate::hotkey::OverlayInputSuspension::new();
    let _owner = owner.map(|owner| {
        unsafe {
            let _ = EnableWindow(owner, false);
        }
        OwnerGuard(owner)
    });
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state.as_mut() as *mut State as isize);
    }
    state.dpi = monitor_dpi(monitor)
        .unwrap_or_else(|| unsafe { GetDpiForWindow(hwnd) })
        .max(96);
    build_controls(hwnd, &state)?;
    state.refresh_fonts(hwnd);
    unsafe {
        crate::theme::apply_window_theme(
            hwnd,
            crate::theme::theme() == crate::theme::Theme::Dark,
            false,
        );
    }
    let height = layout(hwnd, &state);
    show_tab(hwnd, &state);
    sync_image_actions(hwnd, &state);
    let (width, height) = window_size(state.dpi, height);
    position(hwnd, owner, monitor, width, height);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    crate::ui::bring_to_front(hwnd);
    // The consent button of the guided page, or the selected tab.
    let first = if state.guided() {
        ID_CONNECT
    } else {
        ID_TAB_FIRST + state.tab as i32
    };
    if let Some(child) = control(hwnd, first) {
        unsafe {
            let _ = SetFocus(child);
        }
    }
    if !state.guided() {
        // Load current statistics, limits and images right away.
        refresh(hwnd, &mut state);
    }
    crate::ui::window_loop(hwnd, crate::ui::WindowKind::CloudSettings)?;
    if state.saved {
        Ok(Some(state.settings.clone()))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920×1080 display at 200 %: 984 px of work area (96 px taskbar)
    /// minus about 74 px of window frame, in design pixels.
    const CLIENT_BUDGET: i32 = (984 - 74) / 2;

    #[test]
    fn consent_guidance_names_the_worker_only_when_there_is_one() {
        use crate::i18n::{LanguagePreference, set_preference};
        let _lock = crate::settings::test_env::lock(&crate::i18n::TEST_LANGUAGE_LOCK);
        let worker = Some("https://isolmass-ab12.demo.workers.dev/");

        set_preference(LanguagePreference::English);
        assert_eq!(
            account_guidance(None),
            "On the Cloudflare page, allow the account that should host your Worker. \
             If you have several accounts, allow only that one."
        );
        assert_eq!(
            account_guidance(worker),
            "On the Cloudflare page, allow the account that owns isolmass-ab12.demo.workers.dev."
        );

        set_preference(LanguagePreference::Turkish);
        assert_eq!(
            account_guidance(None),
            "Cloudflare sayfasında, Worker'ınızı barındıracak hesaba izin verin. \
             Birden fazla hesabınız varsa yalnızca o hesaba izin verin."
        );
        assert_eq!(
            account_guidance(worker),
            "Cloudflare sayfasında, isolmass-ab12.demo.workers.dev adresine sahip olan hesaba izin verin."
        );
        set_preference(LanguagePreference::System);
    }

    #[test]
    fn the_guidance_belongs_to_the_connection_tab() {
        assert_eq!(tab_of(ID_ACCOUNT_GUIDE), tab_of(ID_UPDATE_WORKER));
        assert_eq!(tab_of(ID_ACCOUNT_GUIDE), Some(0));
    }

    #[test]
    fn limits_tab_fits_a_1080p_work_area_at_200_percent() {
        // The cost note wraps fully; a translation may need a fourth line.
        for note in [LABEL, 2 * LABEL, 3 * LABEL, 4 * LABEL] {
            let client = limits_bottom(note) + 8 + FOOTER;
            assert!(client <= CLIENT_BUDGET, "{client} > {CLIENT_BUDGET}");
        }
    }

    #[test]
    fn image_row_has_an_inset_background_and_a_left_edge_pill() {
        for dpi in [96, 192] {
            let row = RECT {
                left: 0,
                top: 100,
                right: 400,
                bottom: 100 + scale(IMAGE_ROW, dpi),
            };
            let parts = image_row_parts(row, dpi);
            assert_eq!(parts.background.left, scale(4, dpi));
            assert_eq!(row.right - parts.background.right, scale(4, dpi));
            assert_eq!(parts.pill.left, parts.background.left);
            assert_eq!(parts.pill.right - parts.pill.left, scale(3, dpi));
            assert_eq!(parts.pill.bottom - parts.pill.top, scale(16, dpi));
            // Centred in the background, which leaves room for the focus ring.
            assert_eq!(
                parts.pill.top - parts.background.top,
                parts.background.bottom - parts.pill.bottom
            );
            assert!(parts.background.top >= row.top + scale(3, dpi));
            assert!(parts.background.bottom <= row.bottom - scale(3, dpi));
            assert!(parts.text.left > parts.pill.right);
        }
    }

    #[test]
    fn limit_cells_form_two_columns_that_neither_overlap_nor_leave_the_dialog() {
        let column = (WIDTH - 2 * MARGIN - GUTTER) / 2;
        let cells: Vec<(i32, i32)> = (0..=LIMITS.len() as i32)
            .map(|index| limit_cell(index, column))
            .collect();
        let mut columns: Vec<i32> = cells.iter().map(|cell| cell.0).collect();
        columns.sort_unstable();
        columns.dedup();
        assert_eq!(columns.len(), 2);
        assert!(columns[0] + column + GUTTER <= columns[1]);
        assert!(columns[1] + column <= WIDTH - MARGIN);
        for (index, (x, y)) in cells.iter().enumerate() {
            for (other_x, other_y) in &cells[index + 1..] {
                assert!(
                    x != other_x || other_y >= &(y + CELL_HEIGHT),
                    "cells in one column overlap"
                );
            }
            assert!(y + CELL_HEIGHT <= limits_note_top());
        }
    }

    #[test]
    fn framed_edit_keeps_room_for_a_line_of_text() {
        // The 14 px body font needs about 19 px of height at 96 DPI.
        assert!(FIELD - 2 * EDIT_PAD.1 >= 19);
        assert!(EDIT_PAD.0 + EDIT_MARGIN > 2);
    }

    #[test]
    fn measured_heights_round_up_to_design_pixels() {
        assert_eq!(design(19, 96), 19);
        // 18.5 design pixels at 200 %.
        assert_eq!(design(37, 192), 19);
        assert_eq!(design(39, 192), 20);
        assert_eq!(design(24, 120), 20);
        assert_eq!(design(25, 120), 20);
    }

    #[test]
    fn enter_applies_the_section_that_has_the_focus() {
        assert_eq!(default_for(ID_PASSWORD), ID_SAVE_PASSWORD);
        assert_eq!(default_for(ID_LIMIT_FIRST), ID_SAVE_LIMITS);
        assert_eq!(
            default_for(ID_LIMIT_FIRST + LIMITS.len() as i32 - 1),
            ID_SAVE_LIMITS
        );
        assert_eq!(default_for(ID_MODE), ID_SAVE_LIMITS);
        assert_eq!(default_for(ID_IMAGES), ID_OPEN_IMAGE);
        assert_eq!(default_for(ID_REFRESH), ID_CLOSE);
        assert_eq!(default_for(ID_TAB_FIRST), ID_CLOSE);
    }

    #[test]
    fn every_input_reports_through_wm_command_and_lives_on_a_tab() {
        for id in framed_ids().chain([ID_MODE]) {
            assert!(is_input(id), "{id}");
            assert!(tab_of(id).is_some(), "{id}");
        }
        assert_eq!(tab_of(ID_IMAGES_EMPTY), tab_of(ID_IMAGES));
        assert!(!is_input(ID_SAVE_LIMITS));
    }
}
