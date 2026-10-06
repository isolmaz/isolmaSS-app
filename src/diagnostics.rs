//! Local, bounded operational diagnostics. Never record pixels, typed text or keys.
use std::io::Write;
use std::sync::Mutex;
static WRITER: Mutex<()> = Mutex::new(());

pub fn record(category: &str, message: &str) {
    if cfg!(test) {
        return;
    }
    let Ok(_lock) = WRITER.lock() else {
        return;
    };
    let Some(root) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let root = std::path::PathBuf::from(root).join("isolmaSS").join("logs");
    // Serialize rotation and appends across isolmaSS processes (best-effort;
    // logging proceeds even if the lock cannot be taken).
    let _process_lock = CrossProcessLogLock::acquire();
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&root)?;
        let path = root.join("diagnostic.log");
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= 1_048_576) {
            let previous = root.join("diagnostic.previous.log");
            let _ = std::fs::remove_file(&previous);
            // Rotation is best-effort: on failure (e.g. the file is held by
            // another process) keep appending instead of dropping this record.
            let _ = std::fs::rename(&path, previous);
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_secs());
        let message: String = message
            .chars()
            .take(2048)
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(
            file,
            "{timestamp} [{}] [{category}] {message}",
            env!("CARGO_PKG_VERSION")
        )
    })();
    if let Err(error) = result {
        std::eprintln!("Diagnostic log unavailable: {error}");
    }
}

/// Best-effort cross-process lock so concurrent isolmaSS processes do not
/// interleave rotation and appends. If the lock cannot be taken, logging
/// still proceeds so no record is dropped.
struct CrossProcessLogLock(Option<windows::Win32::Foundation::HANDLE>);

impl CrossProcessLogLock {
    fn acquire() -> Self {
        use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        use windows::core::w;
        let Ok(handle) =
            (unsafe { CreateMutexW(None, false, w!("Local\\isolmaSS.DiagnosticsLog")) })
        else {
            return Self(None);
        };
        if matches!(
            unsafe { WaitForSingleObject(handle, 250) },
            WAIT_OBJECT_0 | WAIT_ABANDONED
        ) {
            Self(Some(handle))
        } else {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            Self(None)
        }
    }
}

impl Drop for CrossProcessLogLock {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            unsafe {
                let _ = windows::Win32::System::Threading::ReleaseMutex(handle);
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
        }
    }
}
