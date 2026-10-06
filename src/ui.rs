//! Shared Win32 lifetime and modal-loop rules. Child windows never quit the UI thread.
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

/// Native Windows color picker; cancellation is distinct from a dialog failure.
pub fn choose_color(owner: HWND, initial: [u8; 4]) -> Result<Option<[u8; 4]>> {
    use windows::Win32::UI::Controls::Dialogs::{
        CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW, CommDlgExtendedError,
    };
    let _suspend = crate::hotkey::OverlayInputSuspension::new();
    let mut custom = [COLORREF(0); 16];
    let mut dialog = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: crate::annotation::bgra_to_colorref(initial),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    if !unsafe { ChooseColorW(&mut dialog).as_bool() } {
        let error = unsafe { CommDlgExtendedError() };
        return if error.0 == 0 {
            Ok(None)
        } else {
            Err(windows::core::Error::from_hresult(
                windows::core::HRESULT::from_win32(error.0),
            ))
        };
    }
    let value = dialog.rgbResult.0;
    Ok(Some([
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        255,
    ]))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UpdateChoice {
    Install,
    Later,
    SkipVersion,
}

/// Three explicit outcomes; task-dialog failure never authorizes an install.
pub fn ask_for_update(owner: HWND, version: &str) -> Result<UpdateChoice> {
    use windows::Win32::UI::Controls::{
        TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDF_ALLOW_DIALOG_CANCELLATION, TaskDialogIndirect,
    };
    let _suspend = crate::hotkey::OverlayInputSuspension::new();
    let title: Vec<u16> = crate::i18n::t("isolmaSS update")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let heading: Vec<u16> = crate::i18n::tf("isolmaSS {} is ready", &[&version])
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let content: Vec<u16> =
        crate::i18n::t("Save your work first. After verification isolmaSS closes, installs the update and starts again. Continue?")
            .encode_utf16()
            .chain(Some(0))
            .collect();
    let labels: Vec<Vec<u16>> = [
        crate::i18n::t("Install"),
        crate::i18n::t("Later"),
        crate::i18n::t("Skip this version"),
    ]
    .iter()
    .map(|label| label.encode_utf16().chain(Some(0)).collect())
    .collect();
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: 100,
            pszButtonText: PCWSTR(labels[0].as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: 101,
            pszButtonText: PCWSTR(labels[1].as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: 102,
            pszButtonText: PCWSTR(labels[2].as_ptr()),
        },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: 101,
        ..Default::default()
    };
    let mut selected = 0;
    unsafe { TaskDialogIndirect(&config, Some(&mut selected), None, None)? };
    Ok(match selected {
        100 => UpdateChoice::Install,
        102 => UpdateChoice::SkipVersion,
        _ => UpdateChoice::Later,
    })
}

/// Activates `hwnd` even when another process (e.g. the browser used for a
/// Cloudflare login) owns the foreground. Windows only lets the foreground
/// thread hand activation over, so the input queues are joined for the call;
/// when activation is still refused the taskbar button flashes instead.
pub fn bring_to_front(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let foreground = GetForegroundWindow();
        let current = GetCurrentThreadId();
        let other = if foreground.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let attached =
            other != 0 && other != current && AttachThreadInput(current, other, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let activated = SetForegroundWindow(hwnd).as_bool();
        if attached {
            let _ = AttachThreadInput(current, other, false);
        }
        if !activated && GetForegroundWindow() != hwnd {
            let flash = FLASHWINFO {
                cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 3,
                dwTimeout: 0,
            };
            let _ = FlashWindowEx(&flash);
        }
    }
}

/// Declare after the state stored in GWLP_USERDATA so the window is destroyed first.
pub struct OwnedWindow(pub HWND);

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        if unsafe { IsWindow(self.0).as_bool() } {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
}

#[derive(PartialEq, Eq)]
pub enum WindowKind {
    Overlay,
    Settings,
    CloudSettings,
}

fn combo_dropped(target: HWND) -> bool {
    if target.is_invalid() {
        return false;
    }
    let mut class = [0u16; 16];
    let length = unsafe { GetClassNameW(target, &mut class) }.max(0) as usize;
    String::from_utf16_lossy(&class[..length]).eq_ignore_ascii_case("ComboBox")
        && unsafe { SendMessageW(target, CB_GETDROPPEDSTATE, WPARAM(0), LPARAM(0)) }.0 != 0
}

pub fn window_loop(hwnd: HWND, kind: WindowKind) -> Result<()> {
    let dialog = kind != WindowKind::Overlay;
    let mut msg = MSG::default();
    while unsafe { IsWindow(hwnd).as_bool() } {
        let status = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if status == -1 {
            return Err(windows::core::Error::from_win32());
        }
        if status == 0 {
            // Preserve a real application quit for the enclosing message loop.
            unsafe {
                PostQuitMessage(msg.wParam.0 as i32);
            }
            break;
        }
        crate::updater::poll(hwnd, false);
        // Central cache invalidation for every window this loop pumps; the
        // message is still dispatched so window procs keep their own arms.
        match msg.message {
            WM_SETTINGCHANGE => crate::theme::invalidate_theme_cache(),
            WM_DWMCOLORIZATIONCOLORCHANGED => crate::theme::invalidate_accent(),
            _ => {}
        }
        if kind == WindowKind::Settings && crate::settings_window::handle_key_recording(hwnd, &msg)
        {
            continue;
        }
        // Escape closes an open combo-box list first, like a native dialog.
        if dialog
            && msg.message == WM_KEYDOWN
            && msg.wParam == WPARAM(27)
            && !combo_dropped(msg.hwnd)
        {
            unsafe {
                PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0))?;
            }
            continue;
        }
        if !dialog || !unsafe { IsDialogMessageW(hwnd, &msg).as_bool() } {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if unsafe { IsWindow(hwnd).as_bool() } {
            crate::updater::poll(hwnd, false);
        }
    }
    Ok(())
}

/// A task dialog whose buttons follow the interface language; the common OK/Yes/No
/// buttons of `TaskDialog` would follow the Windows display language instead.
fn task_dialog(
    owner: HWND,
    title: &[u16],
    message: &[u16],
    icon: PCWSTR,
    buttons: &[(i32, &str)],
    default: i32,
) -> Result<i32> {
    use windows::Win32::UI::Controls::{
        TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TDF_ALLOW_DIALOG_CANCELLATION,
        TaskDialogIndirect,
    };
    let labels: Vec<Vec<u16>> = buttons
        .iter()
        .map(|(_, label)| label.encode_utf16().chain(Some(0)).collect())
        .collect();
    let specs: Vec<TASKDIALOG_BUTTON> = buttons
        .iter()
        .zip(&labels)
        .map(|((id, _), label)| TASKDIALOG_BUTTON {
            nButtonID: *id,
            pszButtonText: PCWSTR(label.as_ptr()),
        })
        .collect();
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        pszWindowTitle: w!("isolmaSS"),
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: icon },
        pszMainInstruction: PCWSTR(title.as_ptr()),
        pszContent: PCWSTR(message.as_ptr()),
        cButtons: specs.len() as u32,
        pButtons: specs.as_ptr(),
        nDefaultButton: default,
        ..Default::default()
    };
    let mut selected = 0;
    unsafe { TaskDialogIndirect(&config, Some(&mut selected), None, None)? };
    Ok(selected)
}

pub fn error(owner: HWND, title: &str, message: &str) {
    crate::diagnostics::record(title, message);
    let _suspend = crate::hotkey::OverlayInputSuspension::new();
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    use windows::Win32::UI::Controls::TD_ERROR_ICON;
    let ok = [(IDOK.0, crate::i18n::t("OK"))];
    if task_dialog(owner, &title, &message, TD_ERROR_ICON, &ok, IDOK.0).is_err() {
        unsafe {
            let _ = MessageBoxW(
                owner,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

pub fn info(owner: HWND, title: &str, message: &str) {
    use windows::Win32::UI::Controls::TD_INFORMATION_ICON;
    let _suspend = crate::hotkey::OverlayInputSuspension::new();
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let ok = [(IDOK.0, crate::i18n::t("OK"))];
    if task_dialog(owner, &title, &message, TD_INFORMATION_ICON, &ok, IDOK.0).is_err() {
        unsafe {
            let _ = MessageBoxW(
                owner,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }
}

pub fn confirm(owner: HWND, title: &str, message: &str) -> bool {
    use windows::Win32::UI::Controls::TD_INFORMATION_ICON;
    let _suspend = crate::hotkey::OverlayInputSuspension::new();
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    // No is the default, as in the message-box fallback below.
    let buttons = [
        (IDYES.0, crate::i18n::t("Yes")),
        (IDNO.0, crate::i18n::t("No")),
    ];
    match task_dialog(
        owner,
        &title,
        &message,
        TD_INFORMATION_ICON,
        &buttons,
        IDNO.0,
    ) {
        Ok(selected) => selected == IDYES.0,
        Err(error) => {
            crate::diagnostics::record("confirmation dialog", &error.to_string());
            let selected = unsafe {
                MessageBoxW(
                    owner,
                    PCWSTR(message.as_ptr()),
                    PCWSTR(title.as_ptr()),
                    MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                )
            };
            if selected.0 == 0 {
                crate::diagnostics::record(
                    "confirmation fallback",
                    &windows::core::Error::from_win32().to_string(),
                );
            }
            selected == IDYES
        }
    }
}
