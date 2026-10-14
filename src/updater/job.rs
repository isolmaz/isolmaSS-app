use super::*;
use std::sync::MutexGuard;
use std::thread::JoinHandle;

pub(super) static CANCELLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static AUTOMATIC: AtomicBool = AtomicBool::new(false);
static CHECKING: AtomicBool = AtomicBool::new(false);
static MANUAL_REQUESTED: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static EVENT: Mutex<Option<Event>> = Mutex::new(None);
static READY: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);
static RECHECK: Mutex<Option<bool>> = Mutex::new(None);

pub(super) enum Event {
    Checked(Result<Option<UpdateInfo>, String>, bool),
    Downloaded(Result<PathBuf, DownloadError>, bool),
}

impl Event {
    fn manual(&self) -> bool {
        match self {
            Event::Checked(_, manual) => *manual,
            Event::Downloaded(_, automatic) => !*automatic,
        }
    }
}

/// Work for the update worker thread.
pub(super) type Work = Box<dyn FnOnce() -> Event + Send>;

/// Locks `mutex`, taking the data back from a thread that panicked while holding it. Every
/// critical section here leaves the state consistent, so a panic elsewhere must not wedge the
/// updater.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub(super) fn remove_staged_update(path: &Path) {
    for file in [path.to_path_buf(), path.with_extension("exe.sig")] {
        if let Err(error) = std::fs::remove_file(&file)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::diagnostics::record("update cleanup", &error.to_string());
        }
    }
}

fn discard_event(event: Event) {
    if let Event::Downloaded(Ok(path), _) = event {
        let ready = lock(&READY);
        if ready
            .as_ref()
            .is_some_and(|(ready_path, _)| ready_path == &path)
        {
            return;
        }
        drop(ready);
        remove_staged_update(&path);
    }
}

pub(super) fn start(automatic: bool, checking: bool, work: Work) -> bool {
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if !automatic {
            let mut pending = lock(&EVENT);
            if let Some(Event::Checked(_, manual)) = pending.as_mut() {
                *manual = true;
                return true;
            }
            if CHECKING.load(Ordering::Acquire) {
                MANUAL_REQUESTED.store(true, Ordering::Release);
                return true;
            }
        }
        return false;
    }
    if let Some(previous) = lock(&WORKER).take() {
        let _ = previous.join();
    }
    CANCELLED.store(false, Ordering::Release);
    AUTOMATIC.store(automatic, Ordering::Release);
    CHECKING.store(checking, Ordering::Release);
    MANUAL_REQUESTED.store(false, Ordering::Release);
    let handle = std::thread::spawn(move || {
        let mut event = work();
        {
            // Publish under the EVENT lock and recheck cancellation there, so a
            // concurrent cancel either discards this result or clears it afterwards.
            let mut pending = lock(&EVENT);
            if checking
                && MANUAL_REQUESTED.swap(false, Ordering::AcqRel)
                && let Event::Checked(_, manual) = &mut event
            {
                *manual = true;
            }
            if CANCELLED.load(Ordering::Acquire) {
                discard_event(event);
            } else if !event.manual() && pending.as_ref().is_some_and(Event::manual) {
                // Never let automatic work displace a pending manual request.
            } else {
                *pending = Some(event);
            }
        }
        CHECKING.store(false, Ordering::Release);
        RUNNING.store(false, Ordering::Release);
        crate::tray::notify_tray_wakeup();
    });
    *lock(&WORKER) = Some(handle);
    true
}

pub fn is_checking() -> bool {
    CHECKING.load(Ordering::Acquire)
}

/// The first work of a launch: sweeps leftovers of earlier updates, then runs the automatic
/// check when enabled. Without a check the result is empty, so nothing is offered.
pub(super) fn startup_event(
    check_for_updates: bool,
    sweep: fn(),
    check: fn() -> Result<Option<UpdateInfo>, String>,
) -> Event {
    sweep();
    Event::Checked(if check_for_updates { check() } else { Ok(None) }, false)
}

pub fn configure(previous: &crate::settings::Settings, current: &crate::settings::Settings) {
    if previous.check_updates_automatically != current.check_updates_automatically {
        {
            // Cancel and clear pending automatic results under the same lock the
            // worker publishes with, so nothing stale survives a settings change
            // while pending manual requests are preserved.
            let mut pending = lock(&EVENT);
            if AUTOMATIC.load(Ordering::Acquire)
                && !MANUAL_REQUESTED.load(Ordering::Acquire)
                && !pending.as_ref().is_some_and(Event::manual)
            {
                CANCELLED.store(true, Ordering::Release);
            }
            let stale = if pending.as_ref().is_some_and(|event| !event.manual()) {
                pending.take()
            } else {
                None
            };
            let mut ready = lock(&READY);
            if let Some((path, automatic)) = ready.take() {
                if automatic {
                    remove_staged_update(&path);
                } else {
                    *ready = Some((path, automatic));
                }
            }
            drop(ready);
            if let Some(event) = stale {
                discard_event(event);
            }
        }
        *lock(&RECHECK) = current.check_updates_automatically.then_some(true);
    }
}

pub(super) fn download_failure_status(kind: DownloadFailure) -> &'static str {
    match kind {
        DownloadFailure::Network => crate::i18n::t(
            "The update could not be downloaded. Check your connection and try again.",
        ),
        DownloadFailure::Integrity => crate::i18n::t("Verification failed. Try the update again."),
        DownloadFailure::Storage => crate::i18n::t(
            "The update could not be saved. Check free disk space and folder access, then try again.",
        ),
    }
}

/// What a UI loop tick should do about the updater.
pub(super) enum Step {
    /// A worker is still running; look again on the next tick.
    Busy,
    /// Automatic checking was switched on: start a fresh check.
    Recheck,
    /// Take the result a finished worker left, if any.
    Event(Option<Event>),
}

pub(super) fn next_step() -> Step {
    if RUNNING.load(Ordering::Acquire) {
        return Step::Busy;
    }
    if lock(&RECHECK).take().is_some() {
        return Step::Recheck;
    }
    Step::Event(lock(&EVENT).take())
}

/// Remembers a verified installer for installation, dropping the one it replaces.
pub(super) fn stage_ready(path: PathBuf, automatic: bool) {
    let mut ready = lock(&READY);
    let same_path = ready.as_ref().is_some_and(|(old, _)| old == &path);
    if let Some((old, _)) = ready.replace((path, automatic))
        && !same_path
    {
        remove_staged_update(&old);
    }
}

/// The verified installer to run now, when installing is allowed.
pub(super) fn take_ready(install_now: bool) -> Option<(PathBuf, bool)> {
    if install_now {
        lock(&READY).take()
    } else {
        None
    }
}

/// Whether the user chose to skip `version`.
pub(super) fn is_skipped(settings: &crate::settings::Settings, version: &str) -> bool {
    settings.skipped_update_version.as_deref() == Some(version)
}

pub fn shutdown() {
    {
        let _guard = lock(&EVENT);
        CANCELLED.store(true, Ordering::Release);
    }
    if let Some(worker) = lock(&WORKER).take()
        && worker.join().is_err()
    {
        crate::diagnostics::record("update", "Update worker terminated unexpectedly.");
    }
    if let Some(event) = lock(&EVENT).take() {
        discard_event(event);
    }
    if let Some((path, _)) = lock(&READY).take() {
        remove_staged_update(&path);
    }
}

#[cfg(test)]
pub(super) mod tests;
