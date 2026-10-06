use crate::tray::WM_SHOW_EXISTING;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WPARAM,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
use windows::core::w;

pub enum InstanceState {
    Primary(SingleInstance),
    ExistingNotified,
}

pub struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn acquire_or_notify() -> windows::core::Result<InstanceState> {
    let handle = unsafe { CreateMutexW(None, false, w!("Local\\isolmaSS.Singleton"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Ok(window) = unsafe { FindWindowW(w!("isolmaSS_TrayClass"), None) }
                && !window.is_invalid()
            {
                unsafe {
                    PostMessageW(window, WM_SHOW_EXISTING, WPARAM(0), LPARAM(0))?;
                }
                return Ok(InstanceState::ExistingNotified);
            }
            if std::time::Instant::now() >= deadline {
                return Err(windows::core::Error::new(
                    windows::core::HRESULT::from_win32(1460),
                    "The running isolmaSS instance is still starting or is unresponsive. Try opening settings again.",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    Ok(InstanceState::Primary(SingleInstance(handle)))
}

/// Serializes read/merge/write configuration transactions across application processes.
pub struct SettingsLock(HANDLE);
impl SettingsLock {
    pub fn acquire() -> std::io::Result<Self> {
        use windows::Win32::System::Threading::WaitForSingleObject;
        let handle = unsafe { CreateMutexW(None, false, w!("Local\\isolmaSS.SettingsWrite")) }
            .map_err(std::io::Error::other)?;
        let result = unsafe { WaitForSingleObject(handle, 3000) };
        if result == windows::Win32::Foundation::WAIT_OBJECT_0
            || result == windows::Win32::Foundation::WAIT_ABANDONED
        {
            return Ok(Self(handle));
        }
        if result == windows::Win32::Foundation::WAIT_FAILED {
            // Capture the Win32 error before the handle is closed.
            let failure = windows::core::Error::from_win32();
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(std::io::Error::other(format!(
                "could not lock isolmaSS settings for writing: {failure}"
            )));
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Another process is updating isolmaSS settings; retry saving.",
        ))
    }
}
impl Drop for SettingsLock {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
