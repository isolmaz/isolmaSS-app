//! System-owned tray menu: Windows supplies its theme, focus, keyboard navigation and placement.
use super::TrayCommand;
use std::path::PathBuf;
use std::time::Instant;
use windows::Win32::Foundation::{
    GetLastError, HWND, LPARAM, POINT, SetLastError, WIN32_ERROR, WPARAM,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, HMENU, MENU_ITEM_FLAGS, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, PostMessageW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTALIGN,
    TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_NULL,
};
use windows::core::{PCWSTR, Result};

const CAPTURE: usize = 100;
const SETTINGS: usize = 101;
const OPEN_FOLDER: usize = 102;
const CHECK_UPDATES: usize = 103;
const RECENT_FIRST: usize = 104;
const EXIT: usize = 200;

struct PopupMenu(HMENU);

impl Drop for PopupMenu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

fn append(menu: HMENU, id: usize, text: &str, flags: MENU_ITEM_FLAGS) -> Result<()> {
    let label: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe { AppendMenuW(menu, flags, id, PCWSTR(label.as_ptr())) }
}

pub(super) fn show(
    owner: HWND,
    point: POINT,
    recent: Vec<PathBuf>,
    hotkey: &str,
) -> Result<Option<TrayCommand>> {
    let _suspension = crate::hotkey::OverlayInputSuspension::new();
    let menu = PopupMenu(unsafe { CreatePopupMenu()? });
    let capture = if hotkey.is_empty() {
        crate::i18n::t("Take screenshot").to_owned()
    } else {
        format!("{}\t{hotkey}", crate::i18n::t("Take screenshot"))
    };
    append(menu.0, CAPTURE, &capture, MF_STRING)?;
    append(menu.0, SETTINGS, crate::i18n::t("Settings"), MF_STRING)?;
    append(
        menu.0,
        OPEN_FOLDER,
        crate::i18n::t("Screenshots folder"),
        MF_STRING,
    )?;
    append(
        menu.0,
        CHECK_UPDATES,
        if crate::updater::is_checking() {
            crate::i18n::t("Checking for updates…")
        } else {
            crate::i18n::t("Check for updates")
        },
        MF_STRING,
    )?;
    unsafe { AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())? };
    append(
        menu.0,
        0,
        crate::i18n::t("Recent screenshots"),
        MF_STRING | MF_GRAYED,
    )?;
    if recent.is_empty() {
        append(
            menu.0,
            0,
            crate::i18n::t("No screenshots yet"),
            MF_STRING | MF_GRAYED,
        )?;
    } else {
        for (index, path) in recent.iter().take(5).enumerate() {
            let label = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .replace('&', "&&");
            append(menu.0, RECENT_FIRST + index, &label, MF_STRING)?;
        }
    }
    unsafe { AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())? };
    append(menu.0, EXIT, crate::i18n::t("Quit isolmaSS"), MF_STRING)?;

    unsafe {
        let _ = SetForegroundWindow(owner);
        SetLastError(WIN32_ERROR(0));
    }
    let selected = unsafe {
        TrackPopupMenuEx(
            menu.0,
            (TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD).0,
            point.x,
            point.y,
            owner,
            None,
        )
    }
    .0 as usize;
    let popup_error = if selected == 0 && unsafe { GetLastError() }.0 != 0 {
        Some(windows::core::Error::from_win32())
    } else {
        None
    };
    // The Win32 tray-menu contract needs one message after dismissing the popup.
    unsafe { PostMessageW(owner, WM_NULL, WPARAM(0), LPARAM(0))? };
    if let Some(error) = popup_error {
        return Err(error);
    }
    Ok(match selected {
        CAPTURE => Some(TrayCommand::Capture(Instant::now())),
        SETTINGS => Some(TrayCommand::Settings),
        OPEN_FOLDER => Some(TrayCommand::OpenFolder),
        CHECK_UPDATES => Some(TrayCommand::CheckUpdates),
        EXIT => Some(TrayCommand::Exit),
        RECENT_FIRST..=108 => recent
            .get(selected - RECENT_FIRST)
            .cloned()
            .map(TrayCommand::OpenRecent),
        _ => None,
    })
}
