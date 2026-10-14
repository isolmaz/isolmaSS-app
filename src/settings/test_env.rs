//! Test support: a temporary folder and a sandbox that points `%APPDATA%` and
//! `%LOCALAPPDATA%` into it, so nothing a test saves or loads can reach the real
//! user profile (settings, Cloudflare keys, logs).
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Locks `mutex`, taking over a lock that a failed test left poisoned so one failure
/// does not fail every test after it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Poisons `mutex` the way a panic in a thread that holds it would. The thread unwinds
/// without the panic hook, so the test output stays clean.
pub(crate) fn poison<T: Send>(mutex: &Mutex<T>) {
    std::thread::scope(|scope| {
        let _ = scope
            .spawn(|| {
                let _held = mutex.lock();
                std::panic::resume_unwind(Box::new("poisoning the lock"));
            })
            .join();
    });
}

/// A uniquely named folder below the system temp folder, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("isolmass_{label}_{}_{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create the temporary test folder");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A test that failed halfway may leave files that are still open; the
        // operating system clears the temp folder eventually, so never panic here.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static ENVIRONMENT: Mutex<()> = Mutex::new(());

const REDIRECTED: [&str; 2] = ["APPDATA", "LOCALAPPDATA"];

fn set_variable(name: &str, value: Option<&OsStr>) {
    // SAFETY: the process environment is only changed through a `Sandbox`, which
    // serializes such tests, and Windows reads and writes it through thread-safe
    // system calls.
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

/// Runs one test against private `%APPDATA%` and `%LOCALAPPDATA%` folders and the
/// English interface language. Tests using it never overlap; the previous
/// environment and language are restored on drop.
pub(crate) struct Sandbox {
    folder: TempDir,
    previous: [Option<OsString>; 2],
    _language: MutexGuard<'static, ()>,
    _environment: MutexGuard<'static, ()>,
}

impl Sandbox {
    pub(crate) fn new(label: &str) -> Self {
        let environment = lock(&ENVIRONMENT);
        let language = lock(&crate::i18n::TEST_LANGUAGE_LOCK);
        crate::i18n::set_preference(crate::i18n::LanguagePreference::English);
        let folder = TempDir::new(label);
        let previous = REDIRECTED.map(std::env::var_os);
        set_variable("APPDATA", Some(folder.path().join("Roaming").as_os_str()));
        set_variable(
            "LOCALAPPDATA",
            Some(folder.path().join("Local").as_os_str()),
        );
        Self {
            folder,
            previous,
            _language: language,
            _environment: environment,
        }
    }

    /// The sandbox folder; use it for files a test creates itself.
    pub(crate) fn path(&self) -> &Path {
        self.folder.path()
    }

    /// Where the application keeps `settings.json` inside the sandbox.
    pub(crate) fn settings_file(&self) -> PathBuf {
        self.path()
            .join("Roaming")
            .join("isolmaSS")
            .join("settings.json")
    }

    /// Makes `%APPDATA%` unavailable, as on a profile without a roaming folder.
    pub(crate) fn remove_appdata(&self) {
        set_variable("APPDATA", None);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for (name, value) in REDIRECTED.iter().zip(&self.previous) {
            set_variable(name, value.as_deref());
        }
        crate::i18n::set_preference(crate::i18n::LanguagePreference::System);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_poisoned_by_a_panic_is_taken_over() {
        let numbers = Mutex::new(vec![1u64]);
        poison(&numbers);
        assert!(numbers.lock().is_err(), "the panic poisoned the lock");
        lock(&numbers).push(2);
        assert_eq!(
            *lock(&numbers),
            [1, 2],
            "the data is still there and usable"
        );

        let unit = Mutex::new(());
        assert!(unit.lock().is_ok());
        drop(lock(&unit));
        poison(&unit);
        assert!(unit.lock().is_err());
        drop(lock(&unit));
    }
}
