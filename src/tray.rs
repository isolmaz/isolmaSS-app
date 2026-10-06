use crate::settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

pub const WM_TRAYICON: u32 = WM_APP + 101;
pub const WM_SHOW_EXISTING: u32 = WM_APP + 102;
const TRAY_CLASS_NAME: PCWSTR = w!("isolmaSS_TrayClass");
const TRAY_ICON_ID: u32 = 1001;
const RESOURCE_ICON_ID: usize = 101;
// The windows crate does not expose the SDK's `NIN_KEYSELECT` macro.
const NIN_KEYSELECT_EVENT: u32 = NIN_SELECT + 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    Capture(Instant),
    CancelCapture,
    Settings,
    CheckUpdates,
    OpenRecent(PathBuf),
    OpenFolder,
    Exit,
}

static PREFERENCES: std::sync::Mutex<Option<Settings>> = std::sync::Mutex::new(None);
static ACTIVE_HOTKEY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// Records the shortcut the running hook actually registered. An empty
/// description means no shortcut is active and none must be advertised.
pub fn set_active_hotkey(description: &str) {
    *ACTIVE_HOTKEY
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(description.to_owned());
}
pub fn refresh_preferences(settings: &Settings) {
    *PREFERENCES
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(settings.clone());
    crate::save::recent::refresh(&settings.save_directory);
}

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static LAST_FINISHED: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
static CAPTURE_PENDING: AtomicBool = AtomicBool::new(false);
pub const WM_CANCEL_CAPTURE: u32 = WM_APP + 103;
pub fn capture_finished() {
    *LAST_FINISHED
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(Instant::now());
    CAPTURE_PENDING.store(false, Ordering::Release);
}
pub fn capture_pending() -> bool {
    CAPTURE_PENDING.load(Ordering::Acquire)
}
pub fn window_handle() -> HWND {
    HWND(TRAY_HWND.load(Ordering::Acquire) as *mut _)
}

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

static PENDING_COMMANDS: std::sync::Mutex<Vec<TrayCommand>> = std::sync::Mutex::new(Vec::new());
const MAX_PENDING_COMMANDS: usize = 16;

/// Enqueues one command, coalescing it with an identical pending command so the
/// queue is bounded by the distinct command set. Returns false only when the
/// hard capacity bound rejects it.
fn push_pending(command: TrayCommand) -> bool {
    let mut pending = PENDING_COMMANDS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if pending.contains(&command) {
        return true;
    }
    if pending.len() >= MAX_PENDING_COMMANDS {
        crate::diagnostics::record("tray", &format!("Command queue full; dropped {command:?}."));
        return false;
    }
    pending.push(command);
    true
}

/// Queues a control command for the daemon message loop and wakes the loop, so
/// a command produced during a nested modal session is delivered right after
/// that session returns instead of being dropped.
pub fn queue_command(command: TrayCommand) {
    push_pending(command);
    notify_tray_wakeup();
}

/// Moves every pending command out for the daemon message loop. Commands are
/// taken under the lock so running one may safely open a nested modal loop.
pub fn take_commands() -> Vec<TrayCommand> {
    std::mem::take(
        &mut *PENDING_COMMANDS
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
    )
}

fn copy_wide<const N: usize>(destination: &mut [u16; N], value: &str) {
    destination.fill(0);
    for (slot, code_unit) in destination
        .iter_mut()
        .take(N.saturating_sub(1))
        .zip(value.encode_utf16())
    {
        *slot = code_unit;
    }
    if N > 1 && (0xd800..=0xdbff).contains(&destination[N - 2]) {
        destination[N - 2] = 0;
    }
}

pub fn app_icon() -> HICON {
    let instance = unsafe {
        GetModuleHandleW(None)
            .ok()
            .map(|module| HINSTANCE(module.0))
            .unwrap_or_default()
    };
    unsafe {
        LoadIconW(instance, PCWSTR(RESOURCE_ICON_ID as *const u16))
            .or_else(|_| LoadIconW(HINSTANCE::default(), IDI_APPLICATION))
            .unwrap_or_default()
    }
}

fn base_notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: app_icon(),
        ..Default::default()
    };
    copy_wide(&mut data.szTip, "isolmaSS - Screenshot Utility");
    data
}

fn add_tray_icon(hwnd: HWND) -> Result<NOTIFYICONDATAW> {
    let mut data = base_notify_data(hwnd);
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        return Err(windows::core::Error::from_win32());
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        return Err(windows::core::Error::from_win32());
    }
    Ok(data)
}

pub fn capture_is_stale(triggered: Instant) -> bool {
    LAST_FINISHED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_some_and(|finished| triggered < finished)
}

pub fn queue_capture(triggered: Instant) {
    if capture_is_stale(triggered) {
        return;
    }
    if crate::hotkey::is_overlay_active()
        || crate::hotkey::overlay_input_suspended()
        || CAPTURE_PENDING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    if !push_pending(TrayCommand::Capture(triggered)) {
        capture_finished();
    }
    notify_tray_wakeup();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayEventAction {
    Capture,
    ContextMenu,
}

fn decode_tray_event(wparam: usize, lparam: isize) -> (u32, u32, bool) {
    let packed = lparam as u32;
    let version_four_icon_id = packed >> 16;
    if version_four_icon_id != 0 {
        (packed & 0xffff, version_four_icon_id, true)
    } else {
        (packed, wparam as u32, false)
    }
}

fn tray_event_action(event: u32, icon_id: u32) -> Option<TrayEventAction> {
    if icon_id != TRAY_ICON_ID {
        return None;
    }
    match event {
        WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | NIN_KEYSELECT_EVENT => {
            Some(TrayEventAction::Capture)
        }
        WM_RBUTTONUP | WM_CONTEXTMENU => Some(TrayEventAction::ContextMenu),
        _ => None,
    }
}

fn callback_point(wparam: WPARAM) -> Option<POINT> {
    let packed = wparam.0 as u32;
    let x = (packed as u16 as i16) as i32;
    let y = ((packed >> 16) as u16 as i16) as i32;
    (x != -1 || y != -1).then_some(POINT { x, y })
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == TASKBAR_CREATED.load(Ordering::SeqCst) && msg != 0 {
        if add_tray_icon(hwnd).is_err() {
            let _ = unsafe {
                MessageBoxW(
                    hwnd,
                    w!("The isolmaSS tray icon could not be restored after Explorer restarted."),
                    w!("isolmaSS tray error"),
                    MB_OK | MB_ICONERROR,
                )
            };
        }
        return LRESULT(0);
    }

    match msg {
        WM_SHOW_EXISTING => {
            queue_command(TrayCommand::Settings);
            LRESULT(0)
        }
        WM_TRAYICON => {
            let (event, icon_id, is_version_four) = decode_tray_event(wparam.0, lparam.0);
            match tray_event_action(event, icon_id) {
                Some(TrayEventAction::Capture) => queue_capture(Instant::now()),
                Some(TrayEventAction::ContextMenu) => {
                    let anchor = (is_version_four && event == WM_CONTEXTMENU)
                        .then(|| callback_point(wparam))
                        .flatten();
                    show_context_menu(hwnd, anchor);
                }
                None => {}
            }
            LRESULT(0)
        }
        WM_CANCEL_CAPTURE => {
            queue_command(TrayCommand::CancelCapture);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            crate::theme::invalidate_theme_cache();
            LRESULT(0)
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            crate::theme::invalidate_accent();
            LRESULT(0)
        }
        WM_CLOSE => {
            queue_command(TrayCommand::Exit);
            if crate::hotkey::is_overlay_active()
                || crate::hotkey::overlay_input_suspended()
                || capture_pending()
            {
                show_notification(
                    crate::i18n::t("Quit requested"),
                    crate::i18n::t(
                        "isolmaSS will quit after the current capture or window closes.",
                    ),
                );
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub(crate) mod menu;

static MENU_OPEN: AtomicBool = AtomicBool::new(false);

struct MenuGuard;
impl Drop for MenuGuard {
    fn drop(&mut self) {
        MENU_OPEN.store(false, Ordering::Release);
    }
}

fn show_context_menu(hwnd: HWND, anchor: Option<POINT>) {
    // Tray callbacks are sent messages: ignore a second right-click while
    // the system popup's modal menu loop is active.
    if MENU_OPEN.swap(true, Ordering::AcqRel) {
        return;
    }
    let _guard = MenuGuard;
    let mut point = anchor.unwrap_or_default();
    if anchor.is_none() {
        unsafe {
            let _ = GetCursorPos(&mut point);
        }
    }
    let preferences = PREFERENCES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .unwrap_or_default();
    crate::save::recent::refresh(&preferences.save_directory);
    let active_hotkey = ACTIVE_HOTKEY
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .unwrap_or(preferences.hotkey.description);
    match menu::show(hwnd, point, crate::save::recent::list(), &active_hotkey) {
        Ok(Some(TrayCommand::Capture(triggered))) => queue_capture(triggered),
        Ok(Some(command)) => queue_command(command),
        Ok(None) => {}
        Err(error) => crate::ui::error(
            hwnd,
            crate::i18n::t("Could not open the tray menu"),
            &error.to_string(),
        ),
    }
}

fn register_tray_class() -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }

    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(tray_wnd_proc),
        hInstance: HINSTANCE::default(),
        lpszClassName: TRAY_CLASS_NAME,
        ..Default::default()
    };
    let atom = unsafe { RegisterClassExW(&class) };
    if atom == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
        return Err(windows::core::Error::from_win32());
    }
    REGISTERED.store(true, Ordering::Release);
    Ok(())
}

pub struct TrayManager {
    hwnd: HWND,
    nid: NOTIFYICONDATAW,
}

impl TrayManager {
    pub fn create() -> Result<Self> {
        register_tray_class()?;
        TASKBAR_CREATED.store(
            unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            Ordering::SeqCst,
        );
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                TRAY_CLASS_NAME,
                w!("isolmaSS_TrayWindow"),
                Default::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                HINSTANCE::default(),
                None,
            )?
        };
        TRAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        let nid = match add_tray_icon(hwnd) {
            Ok(data) => data,
            Err(error) => {
                TRAY_HWND.store(0, Ordering::SeqCst);
                let _ = unsafe { DestroyWindow(hwnd) };
                return Err(error);
            }
        };
        Ok(Self { hwnd, nid })
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
            let _ = DestroyWindow(self.hwnd);
        }
        TRAY_HWND.store(0, Ordering::SeqCst);
    }
}

pub fn set_update_activity(message: Option<&str>) {
    let raw = TRAY_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(raw as *mut _),
        uID: TRAY_ICON_ID,
        uFlags: NIF_TIP,
        ..Default::default()
    };
    copy_wide(
        &mut data.szTip,
        message.unwrap_or("isolmaSS - Screenshot Utility"),
    );
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        crate::diagnostics::record("tray update status", "Could not update the tray tooltip.");
    }
}

pub fn show_notification(title: &str, message: &str) {
    show_notification_with_mode(title, message, false);
}

pub fn show_update_notification(title: &str, message: &str) {
    show_notification_with_mode(title, message, true);
}

fn show_notification_with_mode(title: &str, message: &str, user_requested: bool) {
    let raw = TRAY_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(raw as *mut _),
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO,
        dwInfoFlags: if user_requested {
            NIIF_INFO
        } else {
            NIIF_INFO | NIIF_RESPECT_QUIET_TIME
        },
        ..Default::default()
    };
    copy_wide(&mut data.szInfoTitle, title);
    copy_wide(&mut data.szInfo, message);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

pub fn request_exit() {
    let raw = TRAY_HWND.load(Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(HWND(raw as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn notify_tray_wakeup() {
    let raw = TRAY_HWND.load(Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(HWND(raw as *mut _), 0, WPARAM(0), LPARAM(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_four_and_legacy_events_dispatch_for_our_icon() {
        assert_eq!(
            decode_tray_event(0x0123_0456, (TRAY_ICON_ID << 16 | WM_CONTEXTMENU) as isize),
            (WM_CONTEXTMENU, TRAY_ICON_ID, true)
        );
        assert_eq!(
            decode_tray_event(TRAY_ICON_ID as usize, WM_RBUTTONUP as isize),
            (WM_RBUTTONUP, TRAY_ICON_ID, false)
        );

        for event in [
            WM_LBUTTONUP,
            WM_LBUTTONDBLCLK,
            NIN_SELECT,
            NIN_KEYSELECT_EVENT,
        ] {
            assert_eq!(
                tray_event_action(event, TRAY_ICON_ID),
                Some(TrayEventAction::Capture)
            );
        }
        for event in [WM_RBUTTONUP, WM_CONTEXTMENU] {
            assert_eq!(
                tray_event_action(event, TRAY_ICON_ID),
                Some(TrayEventAction::ContextMenu)
            );
        }
        assert_eq!(tray_event_action(NIN_SELECT, TRAY_ICON_ID + 1), None);
        assert_eq!(tray_event_action(WM_CLOSE, TRAY_ICON_ID), None);
    }

    #[test]
    fn test_tray_manager_lifecycle() {
        let manager = TrayManager::create();
        assert!(manager.is_ok(), "TrayManager::create should succeed");
        notify_tray_wakeup();
        drop(manager);
        assert_eq!(TRAY_HWND.load(Ordering::SeqCst), 0);
    }
}
