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
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, HBRUSH, HDC, HFONT, HGDIOBJ,
    InvalidateRect, PAINTSTRUCT, PS_SOLID, RedrawWindow, SetBkColor, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CDDS_PREPAINT, CDRF_SKIPDEFAULT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled, SetActiveWindow};
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
const ID_STEP_CONSENT: i32 = 1110;
const ID_CONSENT_NOTE: i32 = 1111;
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
const PITCH: i32 = 36;
const FOOTER: i32 = 60;

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
    font: HFONT,
    heading_font: HFONT,
    page_brush: HBRUSH,
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
            for brush in [self.page_brush, self.footer_brush, self.input_brush] {
                if !brush.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                }
            }
            self.page_brush = CreateSolidBrush(tokens.page);
            self.footer_brush = CreateSolidBrush(tokens.card);
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
        for brush in [self.page_brush, self.footer_brush, self.input_brush] {
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
    create(
        hwnd,
        w!("BUTTON"),
        id,
        text,
        WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
    )
}
fn label(hwnd: HWND, id: i32, text: &str) -> Result<HWND> {
    create(hwnd, w!("STATIC"), id, text, WINDOW_STYLE(0))
}
fn edit(hwnd: HWND, id: i32, password: bool) -> Result<HWND> {
    create(
        hwnd,
        w!("EDIT"),
        id,
        "",
        WINDOW_STYLE(
            WS_TABSTOP.0 | ES_AUTOHSCROLL as u32 | if password { ES_PASSWORD as u32 } else { 0 },
        ),
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

/// Which tab of the paired dialog a control belongs to (`None`: always shown).
fn tab_of(id: i32) -> Option<usize> {
    match id {
        ID_INTRO | ID_STATS | ID_PASSWORD_LABEL | ID_PASSWORD | ID_SAVE_PASSWORD | ID_REFRESH
        | ID_UPDATE_WORKER | ID_FORGET => Some(0),
        ID_MODE_LABEL | ID_MODE | ID_SAVE_LIMITS | ID_COST_NOTE => Some(1),
        _ if (ID_LIMIT_FIRST..ID_LIMIT_FIRST + LIMITS.len() as i32).contains(&id)
            || (ID_LIMIT_LABEL_FIRST..ID_LIMIT_LABEL_FIRST + LIMITS.len() as i32).contains(&id) =>
        {
            Some(1)
        }
        ID_IMAGES_LABEL | ID_IMAGES | ID_DELETE | ID_OPEN_IMAGE | ID_COPY_IMAGE => Some(2),
        _ => None,
    }
}

fn show_tab(hwnd: HWND, state: &State) {
    if state.guided() {
        return;
    }
    for id in 1000..1300 {
        if let (Some(tab), Some(child)) = (tab_of(id), control(hwnd, id)) {
            let visible = tab == state.tab && (id != ID_UPDATE_WORKER || state.outdated);
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
fn layout(hwnd: HWND, state: &State) -> i32 {
    let dpi = state.dpi;
    let right = WIDTH - MARGIN;
    let full = WIDTH - 2 * MARGIN;
    let bottom = if state.guided() {
        place(hwnd, ID_INTRO, (MARGIN, 18, full, 40), dpi);
        place(hwnd, ID_STEP_CONSENT, (MARGIN, 74, full, 20), dpi);
        place(hwnd, ID_CONSENT_NOTE, (MARGIN, 98, full, 40), dpi);
        place(hwnd, ID_STEP_ACCOUNT, (MARGIN, 74, full, 20), dpi);
        place(hwnd, ID_ACCOUNT, (MARGIN, 102, full, 220), dpi);
        place(hwnd, ID_CONNECT, (MARGIN, 146, 230, ROW + 2), dpi);
        place(hwnd, ID_INSTALL, (MARGIN, 146, 230, ROW + 2), dpi);
        place(hwnd, ID_STATUS, (MARGIN, 192, full, 40), dpi);
        240
    } else {
        let mut x = MARGIN - 10;
        for (index, name) in tab_names().iter().enumerate() {
            let width = crate::drawing::measure_text(name, crate::theme::FONT_BODY_PX).0 + 30;
            place(hwnd, ID_TAB_FIRST + index as i32, (x, 6, width, 40), dpi);
            x += width;
        }
        let top = TAB_BAR + 16;
        // Connection
        place(hwnd, ID_INTRO, (MARGIN, top, full, 20), dpi);
        place(hwnd, ID_STATS, (MARGIN, top + 30, full, 80), dpi);
        place(hwnd, ID_PASSWORD_LABEL, (MARGIN, top + 124, full, 20), dpi);
        place(hwnd, ID_PASSWORD, (MARGIN, top + 150, full - 162, 26), dpi);
        place(
            hwnd,
            ID_SAVE_PASSWORD,
            (right - 150, top + 147, 150, ROW + 2),
            dpi,
        );
        place(hwnd, ID_REFRESH, (MARGIN, top + 200, 100, ROW + 2), dpi);
        place(
            hwnd,
            ID_UPDATE_WORKER,
            (MARGIN + 108, top + 200, 150, ROW + 2),
            dpi,
        );
        place(hwnd, ID_FORGET, (right - 170, top + 200, 170, ROW + 2), dpi);
        let connection = top + 236;
        // Limits
        let mut y = top;
        for index in 0..LIMITS.len() as i32 {
            place(
                hwnd,
                ID_LIMIT_LABEL_FIRST + index,
                (MARGIN, y + 6, full - 140, 20),
                dpi,
            );
            place(
                hwnd,
                ID_LIMIT_FIRST + index,
                (right - 120, y + 2, 120, 26),
                dpi,
            );
            y += PITCH;
        }
        place(hwnd, ID_MODE_LABEL, (MARGIN, y + 6, 140, 20), dpi);
        place(hwnd, ID_MODE, (right - 240, y, 240, 200), dpi);
        y += PITCH + 8;
        place(hwnd, ID_COST_NOTE, (MARGIN, y, full - 176, 60), dpi);
        place(hwnd, ID_SAVE_LIMITS, (right - 160, y, 160, ROW + 2), dpi);
        let limits = y + 64;
        // Images
        place(hwnd, ID_IMAGES_LABEL, (MARGIN, top, full, 20), dpi);
        place(hwnd, ID_IMAGES, (MARGIN, top + 28, full, 230), dpi);
        place(hwnd, ID_OPEN_IMAGE, (MARGIN, top + 268, 80, ROW + 2), dpi);
        place(
            hwnd,
            ID_COPY_IMAGE,
            (MARGIN + 88, top + 268, 150, ROW + 2),
            dpi,
        );
        place(hwnd, ID_DELETE, (right - 150, top + 268, 150, ROW + 2), dpi);
        let images = top + 304;
        connection.max(limits).max(images)
    };
    let footer = bottom + 8;
    let button_y = footer + (FOOTER - ROW - 2) / 2;
    if !state.guided() {
        place(hwnd, ID_STATUS, (MARGIN, footer + 10, full - 120, 40), dpi);
    }
    place(hwnd, ID_CLOSE, (right - 100, button_y, 100, ROW + 2), dpi);
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
    footer + FOOTER
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
fn set_status(hwnd: HWND, text: &str) {
    set_text(hwnd, ID_STATUS, text);
}
fn set_busy(hwnd: HWND, state: &mut State, busy: bool) {
    state.pending = busy;
    for id in [
        ID_CONNECT,
        ID_INSTALL,
        ID_REFRESH,
        ID_SAVE_LIMITS,
        ID_SAVE_PASSWORD,
        ID_DELETE,
        ID_FORGET,
        ID_UPDATE_WORKER,
    ] {
        if let Some(child) = control(hwnd, id) {
            unsafe {
                let _ = EnableWindow(child, !busy);
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
    let outdated = stats["worker_version"].as_u64().unwrap_or(1) < cloudflare_oauth::WORKER_VERSION;
    if let Some(button) = control(hwnd, ID_UPDATE_WORKER) {
        unsafe {
            let _ = ShowWindow(
                button,
                if outdated && state.tab == 0 {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
    }
    state.outdated = outdated;
    let count = |value: &Value, key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
    let daily = &stats["daily"];
    let monthly = &stats["monthly"];
    let stored = &stats["images"];
    let usage = stored["stored_bytes"].as_u64().unwrap_or(0);
    set_text(
        hwnd,
        ID_STATS,
        &crate::i18n::tf(
            "Today: uploads {} · views {}\r\nThis month: uploads {} · views {}\r\nActive images: {} · {} MB\r\nWatch your Cloudflare account's overall usage in its dashboard.",
            &[
                &count(daily, "uploads"),
                &count(daily, "views"),
                &count(monthly, "uploads"),
                &count(monthly, "views"),
                &count(stored, "active"),
                &format!("{:.1}", usage as f64 / 1_048_576.0),
            ],
        ),
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
    if !crate::ui::confirm(
        hwnd,
        crate::i18n::t("Delete image"),
        crate::i18n::t("The link will stop working permanently. Continue?"),
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
    if !crate::ui::confirm(
        hwnd,
        crate::i18n::t("Disconnect"),
        crate::i18n::t(
            "Only this computer's Cloudflare connection is removed; the Worker and images in the cloud stay. Continue?",
        ),
    ) {
        return;
    }
    let mut changed = state.settings.clone();
    changed.cloud_url = None;
    if let Err(error) = changed.save() {
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
    state.settings = changed;
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

fn paint_surface(hwnd: HWND, state: &State, hdc: HDC) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
        let _ = FillRect(hdc, &client, state.page_brush);
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
}

fn draw_cloud_button(draw: &NMCUSTOMDRAW, state: &State) {
    use crate::drawing::{Look, fluent_button, fluent_tab};
    let id = draw.hdr.idFrom as i32;
    let look = Look {
        primary: matches!(id, ID_CONNECT | ID_INSTALL | ID_SAVE_LIMITS),
        ..Look::from_custom_draw(draw)
    };
    let background = if in_footer(id, state) {
        state.footer_brush
    } else {
        state.page_brush
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
            let secondary = matches!(id, ID_CONSENT_NOTE | ID_COST_NOTE | ID_STATUS)
                || (id == ID_INTRO && !state.guided());
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
            LRESULT(if in_footer(id, state) {
                state.footer_brush.0 as isize
            } else {
                state.page_brush.0 as isize
            })
        }
        WM_SETTINGCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED if !pointer.is_null() => {
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
                    windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                        | windows::Win32::Graphics::Gdi::RDW_ERASE
                        | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN,
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
            if lparam.0 != 0 {
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
            LRESULT(0)
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
                        account_controls(hwnd, true);
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
                    let mut updated = state.settings.clone();
                    updated.cloud_url = Some(credentials.origin.clone());
                    if let Err(error) = cloudflare_setup::save_credentials(&credentials)
                        .and_then(|()| updated.save().map_err(|error| error.to_string()))
                    {
                        set_status(
                            hwnd,
                            crate::i18n::t(
                                "The Worker was installed but the local connection could not be saved.",
                            ),
                        );
                        crate::ui::error(hwnd, crate::i18n::t("Could not save the setup"), &error);
                    } else {
                        state.settings = updated;
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
                    account_controls(hwnd, false);
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
        WM_COMMAND if !pointer.is_null() && (wparam.0 & 0xffff) as i32 == ID_IMAGES => {
            const LBN_DBLCLK: usize = 2;
            if wparam.0 >> 16 == LBN_DBLCLK {
                open_image(hwnd, unsafe { &*pointer });
            }
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
        hIconSm: crate::tray::app_icon(),
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
        button(hwnd, ID_CONNECT, crate::i18n::t("Continue with Cloudflare"))?;
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
    label(
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
    edit(hwnd, ID_PASSWORD, true)?;
    button(hwnd, ID_SAVE_PASSWORD, crate::i18n::t("Save password"))?;
    button(hwnd, ID_REFRESH, crate::i18n::t("Refresh"))?;
    button(hwnd, ID_UPDATE_WORKER, crate::i18n::t("Update Worker"))?;
    button(hwnd, ID_FORGET, crate::i18n::t("Disconnect"))?;
    for (index, name) in limit_labels().iter().enumerate() {
        label(hwnd, ID_LIMIT_LABEL_FIRST + index as i32, name)?;
        edit(hwnd, ID_LIMIT_FIRST + index as i32, false)?;
    }
    label(hwnd, ID_MODE_LABEL, crate::i18n::t("At the limit"))?;
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
        crate::i18n::t("Recent images · Double-click to open; deleted links stop at once"),
    )?;
    create(
        hwnd,
        w!("LISTBOX"),
        ID_IMAGES,
        "",
        WINDOW_STYLE(WS_TABSTOP.0 | WS_VSCROLL.0 | LBS_NOTIFY as u32),
    )?;
    button(hwnd, ID_OPEN_IMAGE, crate::i18n::t("Open"))?;
    button(hwnd, ID_COPY_IMAGE, crate::i18n::t("Copy link"))?;
    button(hwnd, ID_DELETE, crate::i18n::t("Delete"))?;
    label(hwnd, ID_STATUS, crate::i18n::t("Connected"))?;
    button(hwnd, ID_CLOSE, crate::i18n::t("Close"))?;
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

/// Centres the dialog on its owner, or on the monitor under the cursor.
fn position(hwnd: HWND, owner: Option<HWND>, width: i32, height: i32) {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
    };
    let monitor = match owner {
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
        crate::diagnostics::record("cloudflare view", "Could not measure the work area.");
        return;
    }
    let work = info.rcWork;
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
        font: HFONT::default(),
        heading_font: HFONT::default(),
        page_brush: HBRUSH::default(),
        footer_brush: HBRUSH::default(),
        input_brush: HBRUSH::default(),
    });
    state.refresh_brushes();
    let hwnd = unsafe {
        CreateWindowExW(
            // A taskbar button even when owned by the Settings window.
            WS_EX_APPWINDOW,
            CLASS,
            w!("isolmaSS Cloudflare"),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
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
    state.dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }.max(96);
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
    let (width, height) = window_size(state.dpi, height);
    position(hwnd, owner, width, height);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    crate::ui::bring_to_front(hwnd);
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
