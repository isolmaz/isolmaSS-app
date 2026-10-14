//! The two upload steps that need the live system: the HTTPS request to the user's
//! Worker over the real internet and the browser launch through the shell. What is
//! sent, what is accepted and which links may be opened is decided in the parent module.
use super::{Prepared, browser_target, send_with};
use crate::cloudflare_setup::{self, UploadProgress};

/// Sends the prepared screenshot (blocking) and returns its share link.
/// `progress` receives `(bytes sent, total bytes)` while the file is written.
pub fn send(prepared: &Prepared, progress: UploadProgress<'_>) -> Result<String, String> {
    send_with(prepared, progress, cloudflare_setup::api_request)
}

/// Opens a validated share link in the default browser.
pub fn open_link(url: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};
    let wide = browser_target(url)?;
    let result = unsafe {
        ShellExecuteW(
            windows::Win32::Foundation::HWND::default(),
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(crate::i18n::t("The link could not be opened in the browser.").to_string())
    } else {
        Ok(())
    }
}
