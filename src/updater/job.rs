use super::*;
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
        let ready = READY.lock().unwrap_or_else(|error| error.into_inner());
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

pub(super) fn start(
    automatic: bool,
    checking: bool,
    work: impl FnOnce() -> Event + Send + 'static,
) -> bool {
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if !automatic {
            let mut pending = EVENT.lock().unwrap_or_else(|error| error.into_inner());
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
    if let Some(previous) = WORKER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
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
            let mut pending = EVENT.lock().unwrap_or_else(|error| error.into_inner());
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
    *WORKER.lock().unwrap_or_else(|error| error.into_inner()) = Some(handle);
    true
}

pub fn is_checking() -> bool {
    CHECKING.load(Ordering::Acquire)
}

/// The first work of a launch: sweeps leftovers of earlier updates, then runs the automatic
/// check when enabled. Without a check the result is empty, so nothing is offered.
pub(super) fn startup_event(
    check_for_updates: bool,
    sweep: impl FnOnce(),
    check: impl FnOnce() -> Result<Option<UpdateInfo>, String>,
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
            let mut pending = EVENT.lock().unwrap_or_else(|error| error.into_inner());
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
            let mut ready = READY.lock().unwrap_or_else(|error| error.into_inner());
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
        *RECHECK.lock().unwrap_or_else(|error| error.into_inner()) =
            current.check_updates_automatically.then_some(true);
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
    let recheck = RECHECK
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if recheck.is_some() {
        return Step::Recheck;
    }
    Step::Event(
        EVENT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take(),
    )
}

/// Remembers a verified installer for installation, dropping the one it replaces.
pub(super) fn stage_ready(path: PathBuf, automatic: bool) {
    let mut ready = READY.lock().unwrap_or_else(|error| error.into_inner());
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
        READY
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    } else {
        None
    }
}

/// Whether the user chose to skip `version`.
pub(super) fn is_skipped(settings: &crate::settings::Settings, version: &str) -> bool {
    settings.skipped_update_version.as_deref() == Some(version)
}

/// The tests share the updater's process-wide state. Whoever holds the guard has it to
/// themselves and starts from a quiet updater: nothing running, nothing pending.
#[cfg(test)]
pub(super) fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let worker = WORKER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    let _ = worker.map(JoinHandle::join);
    for flag in [
        &CANCELLED,
        &RUNNING,
        &AUTOMATIC,
        &CHECKING,
        &MANUAL_REQUESTED,
    ] {
        flag.store(false, Ordering::Release);
    }
    *EVENT.lock().unwrap_or_else(|error| error.into_inner()) = None;
    *READY.lock().unwrap_or_else(|error| error.into_inner()) = None;
    *RECHECK.lock().unwrap_or_else(|error| error.into_inner()) = None;
    guard
}

pub fn shutdown() {
    {
        let _guard = EVENT.lock().unwrap_or_else(|error| error.into_inner());
        CANCELLED.store(true, Ordering::Release);
    }
    if let Some(worker) = WORKER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        && worker.join().is_err()
    {
        crate::diagnostics::record("update", "Update worker terminated unexpectedly.");
    }
    if let Some(event) = EVENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        discard_event(event);
    }
    if let Some((path, _)) = READY
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        remove_staged_update(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::t;
    use crate::settings::Settings;
    use crate::updater::tests::Scratch;
    use std::cell::RefCell;
    use std::sync::mpsc;

    /// Waits for the worker started last to finish.
    fn wait_until_idle() {
        let worker = WORKER
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        worker
            .expect("a worker was started")
            .join()
            .expect("the worker finished");
    }

    /// Work that blocks until the returned sender releases it, then yields `event`.
    fn gated(
        event: impl FnOnce() -> Event + Send + 'static,
    ) -> (mpsc::Sender<()>, impl FnOnce() -> Event + Send + 'static) {
        let (release, wait) = mpsc::channel::<()>();
        (release, move || {
            wait.recv().expect("the worker is released");
            event()
        })
    }

    fn pending() -> Option<Event> {
        EVENT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    fn ready() -> Option<(PathBuf, bool)> {
        READY
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn set_pending(event: Event) {
        *EVENT.lock().unwrap_or_else(|error| error.into_inner()) = Some(event);
    }

    fn set_ready(path: &Path, automatic: bool) {
        *READY.lock().unwrap_or_else(|error| error.into_inner()) =
            Some((path.to_path_buf(), automatic));
    }

    /// A staged installer and its signature in `scratch`.
    fn stage(scratch: &Scratch, version: &str) -> PathBuf {
        let path = scratch.join(&format!("isolmass-setup-{version}.exe"));
        std::fs::write(&path, b"installer").expect("write installer");
        std::fs::write(path.with_extension("exe.sig"), b"signature").expect("write signature");
        path
    }

    fn staged(path: &Path) -> bool {
        path.exists() || path.with_extension("exe.sig").exists()
    }

    fn nothing() -> Event {
        Event::Checked(Ok(None), false)
    }

    fn settings(check_updates_automatically: bool) -> Settings {
        Settings {
            check_updates_automatically,
            ..Settings::default()
        }
    }

    #[test]
    fn an_idle_start_runs_the_work_and_publishes_its_event() {
        let _state = exclusive();
        let (release, work) = gated(nothing);
        assert!(start(true, true, work));
        assert!(RUNNING.load(Ordering::Acquire));
        assert!(AUTOMATIC.load(Ordering::Acquire));
        assert!(is_checking());
        assert!(
            pending().is_none(),
            "nothing is published before the work ends"
        );
        release.send(()).expect("release the worker");
        wait_until_idle();
        assert!(!is_checking());
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), false))));

        // A handle left over from an earlier run is joined by the next start, which is manual
        // this time.
        *WORKER.lock().unwrap_or_else(|error| error.into_inner()) = Some(std::thread::spawn(|| {}));
        assert!(start(false, false, || Event::Checked(Ok(None), true)));
        assert!(!AUTOMATIC.load(Ordering::Acquire));
        assert!(!is_checking(), "a download is not a check");
        wait_until_idle();
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), true))));
    }

    #[test]
    fn automatic_work_is_refused_while_a_worker_runs() {
        let _state = exclusive();
        let (release, work) = gated(nothing);
        assert!(start(true, true, work));
        assert!(!start(true, true, nothing));
        assert!(!MANUAL_REQUESTED.load(Ordering::Acquire));
        release.send(()).expect("release the worker");
        wait_until_idle();
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), false))));
    }

    #[test]
    fn a_manual_request_during_a_check_is_folded_into_its_result() {
        let _state = exclusive();
        let (release, work) = gated(nothing);
        assert!(start(true, true, work));
        assert!(start(false, true, nothing));
        assert!(MANUAL_REQUESTED.load(Ordering::Acquire));
        release.send(()).expect("release the worker");
        wait_until_idle();
        assert!(!MANUAL_REQUESTED.load(Ordering::Acquire));
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), true))));
    }

    #[test]
    fn a_manual_request_marks_a_pending_check_result_manual() {
        let _state = exclusive();
        set_pending(nothing());
        RUNNING.store(true, Ordering::Release);
        assert!(start(false, true, nothing));
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), true))));
        RUNNING.store(false, Ordering::Release);
    }

    #[test]
    fn a_manual_request_is_refused_while_a_download_runs() {
        let _state = exclusive();
        let (release, work) = gated(nothing);
        assert!(start(true, false, work));
        assert!(!start(false, true, nothing));
        assert!(!MANUAL_REQUESTED.load(Ordering::Acquire));
        release.send(()).expect("release the worker");
        wait_until_idle();
    }

    #[test]
    fn a_manual_result_replaces_a_pending_automatic_one() {
        let _state = exclusive();
        set_pending(nothing());
        assert!(start(false, true, || Event::Checked(
            Err("fresh".to_string()),
            true
        )));
        wait_until_idle();
        assert!(matches!(
            pending(),
            Some(Event::Checked(Err(message), true)) if message == "fresh"
        ));
    }

    #[test]
    fn an_automatic_result_never_displaces_a_pending_manual_one() {
        let _state = exclusive();
        set_pending(Event::Checked(Ok(None), true));
        assert!(start(true, true, || Event::Checked(
            Err("automatic".to_string()),
            false
        )));
        wait_until_idle();
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), true))));
    }

    #[test]
    fn cancelled_work_is_discarded_together_with_its_staged_files() {
        let _state = exclusive();
        let scratch = Scratch::new("job-cancelled");
        let installer = stage(&scratch, "9.9.9");
        let path = installer.clone();
        let (release, work) = gated(move || Event::Downloaded(Ok(path), true));
        assert!(start(true, false, work));
        CANCELLED.store(true, Ordering::Release);
        release.send(()).expect("release the worker");
        wait_until_idle();
        assert!(pending().is_none());
        assert!(!staged(&installer));
    }

    #[test]
    fn discarding_a_downloaded_installer_spares_the_one_ready_to_install() {
        let _state = exclusive();
        let scratch = Scratch::new("job-discard");
        let (kept, dropped) = (stage(&scratch, "1.0.0"), stage(&scratch, "2.0.0"));
        set_ready(&kept, true);
        discard_event(Event::Downloaded(Ok(kept.clone()), true));
        assert!(staged(&kept));
        discard_event(Event::Downloaded(Ok(dropped.clone()), false));
        assert!(!staged(&dropped));
        // Events without a staged file leave everything alone.
        discard_event(Event::Downloaded(
            Err(DownloadError::network("offline".to_string())),
            true,
        ));
        discard_event(nothing());
        assert!(staged(&kept));
        assert_eq!(ready(), Some((kept, true)));
    }

    #[test]
    fn removing_a_staged_update_deletes_both_files_and_tolerates_what_is_gone_or_stuck() {
        let scratch = Scratch::new("job-remove");
        let installer = stage(&scratch, "1.0.0");
        remove_staged_update(&installer);
        assert!(!staged(&installer));
        remove_staged_update(&installer);

        // An installer path that is a folder cannot be deleted as a file; its signature goes.
        let stuck = scratch.join("isolmass-setup-2.0.0.exe");
        std::fs::create_dir(&stuck).expect("create folder");
        std::fs::write(stuck.with_extension("exe.sig"), b"signature").expect("write signature");
        remove_staged_update(&stuck);
        assert!(stuck.is_dir());
        assert!(!stuck.with_extension("exe.sig").exists());
    }

    fn startup(check_for_updates: bool) -> (Event, Vec<&'static str>) {
        let log = RefCell::new(Vec::new());
        let event = startup_event(
            check_for_updates,
            || log.borrow_mut().push("sweep"),
            || {
                log.borrow_mut().push("check");
                Err("offline".to_string())
            },
        );
        (event, log.into_inner())
    }

    #[test]
    fn startup_sweeps_leftovers_before_checking_and_only_checks_when_enabled() {
        let (event, log) = startup(true);
        assert_eq!(log, ["sweep", "check"]);
        assert!(matches!(
            event,
            Event::Checked(Err(message), false) if message == "offline"
        ));
        let (event, log) = startup(false);
        assert_eq!(log, ["sweep"]);
        assert!(matches!(event, Event::Checked(Ok(None), false)));
    }

    #[test]
    fn disabling_automatic_checks_cancels_and_clears_everything_automatic() {
        let _state = exclusive();
        let scratch = Scratch::new("job-configure-off");
        let (ready_path, event_path) = (stage(&scratch, "1.0.0"), stage(&scratch, "2.0.0"));
        AUTOMATIC.store(true, Ordering::Release);
        set_ready(&ready_path, true);
        set_pending(Event::Downloaded(Ok(event_path.clone()), true));
        configure(&settings(true), &settings(false));
        assert!(CANCELLED.load(Ordering::Acquire));
        assert!(pending().is_none());
        assert!(ready().is_none());
        assert!(!staged(&ready_path) && !staged(&event_path));
        assert!(RECHECK.lock().expect("recheck").is_none());
    }

    #[test]
    fn disabling_automatic_checks_keeps_what_the_user_asked_for() {
        let _state = exclusive();
        let scratch = Scratch::new("job-configure-manual");
        let ready_path = stage(&scratch, "1.0.0");
        AUTOMATIC.store(true, Ordering::Release);
        set_ready(&ready_path, false);
        set_pending(Event::Checked(Ok(None), true));
        configure(&settings(true), &settings(false));
        assert!(!CANCELLED.load(Ordering::Acquire));
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), true))));
        assert_eq!(ready(), Some((ready_path.clone(), false)));
        assert!(staged(&ready_path));

        // A manual request recorded for the running check protects it as well.
        set_pending(nothing());
        MANUAL_REQUESTED.store(true, Ordering::Release);
        configure(&settings(true), &settings(false));
        assert!(!CANCELLED.load(Ordering::Acquire));
        assert!(pending().is_none(), "only the automatic result is cleared");
    }

    #[test]
    fn an_unchanged_setting_leaves_the_updater_alone_and_enabling_schedules_a_check() {
        let _state = exclusive();
        AUTOMATIC.store(true, Ordering::Release);
        set_pending(nothing());
        configure(&settings(true), &settings(true));
        assert!(!CANCELLED.load(Ordering::Acquire));
        assert!(matches!(pending(), Some(Event::Checked(Ok(None), false))));
        assert!(RECHECK.lock().expect("recheck").is_none());
        configure(&settings(false), &settings(true));
        assert_eq!(*RECHECK.lock().expect("recheck"), Some(true));
    }

    #[test]
    fn failures_are_worded_by_what_went_wrong() {
        assert_eq!(
            download_failure_status(DownloadFailure::Network),
            t("The update could not be downloaded. Check your connection and try again.")
        );
        assert_eq!(
            download_failure_status(DownloadFailure::Integrity),
            t("Verification failed. Try the update again.")
        );
        assert_eq!(
            download_failure_status(DownloadFailure::Storage),
            t(
                "The update could not be saved. Check free disk space and folder access, then try again."
            )
        );
    }

    #[test]
    fn a_tick_waits_for_a_running_worker_then_rechecks_then_takes_the_result() {
        let _state = exclusive();
        RUNNING.store(true, Ordering::Release);
        *RECHECK.lock().expect("recheck") = Some(true);
        set_pending(nothing());
        assert!(matches!(next_step(), Step::Busy));
        assert!(RECHECK.lock().expect("recheck").is_some());
        RUNNING.store(false, Ordering::Release);
        assert!(matches!(next_step(), Step::Recheck));
        assert!(RECHECK.lock().expect("recheck").is_none());
        assert!(matches!(
            next_step(),
            Step::Event(Some(Event::Checked(Ok(None), false)))
        ));
        assert!(matches!(next_step(), Step::Event(None)));
    }

    #[test]
    fn a_verified_installer_replaces_the_previous_one_and_waits_for_permission() {
        let _state = exclusive();
        let scratch = Scratch::new("job-ready");
        let (first, second) = (stage(&scratch, "1.0.0"), stage(&scratch, "2.0.0"));
        stage_ready(first.clone(), true);
        assert_eq!(ready(), Some((first.clone(), true)));
        stage_ready(second.clone(), false);
        assert_eq!(ready(), Some((second.clone(), false)));
        assert!(!staged(&first), "the replaced installer is deleted");
        // The same installer again keeps its files and takes the new flag.
        stage_ready(second.clone(), true);
        assert_eq!(ready(), Some((second.clone(), true)));
        assert!(staged(&second));

        assert_eq!(take_ready(false), None);
        assert_eq!(ready(), Some((second.clone(), true)));
        assert_eq!(take_ready(true), Some((second, true)));
        assert_eq!(take_ready(true), None);
    }

    #[test]
    fn only_the_skipped_version_is_skipped() {
        let skipping = Settings {
            skipped_update_version: Some("1.2.3".to_string()),
            ..Settings::default()
        };
        assert!(is_skipped(&skipping, "1.2.3"));
        assert!(!is_skipped(&skipping, "1.2.4"));
        assert!(!is_skipped(&Settings::default(), "1.2.3"));
    }

    #[test]
    fn shutdown_cancels_the_worker_and_deletes_every_staged_installer() {
        let _state = exclusive();
        let scratch = Scratch::new("job-shutdown");
        let (ready_path, finished) = (stage(&scratch, "1.0.0"), stage(&scratch, "2.0.0"));
        set_ready(&ready_path, false);
        let result = finished.clone();
        // Whether the worker publishes before or after the cancellation, nothing may survive.
        assert!(start(true, false, move || {
            std::thread::sleep(Duration::from_millis(50));
            Event::Downloaded(Ok(result), true)
        }));
        shutdown();
        assert!(CANCELLED.load(Ordering::Acquire));
        assert!(!RUNNING.load(Ordering::Acquire));
        assert!(pending().is_none());
        assert!(ready().is_none());
        assert!(!staged(&ready_path) && !staged(&finished));
    }

    #[test]
    fn shutdown_discards_an_unclaimed_result_and_survives_a_crashed_worker() {
        let _state = exclusive();
        let scratch = Scratch::new("job-shutdown-pending");
        let unclaimed = stage(&scratch, "1.0.0");
        set_pending(Event::Downloaded(Ok(unclaimed.clone()), false));
        shutdown();
        assert!(pending().is_none());
        assert!(!staged(&unclaimed));

        assert!(start(true, false, || panic!("the worker crashes")));
        shutdown();
        assert!(WORKER.lock().expect("worker").is_none());
    }
}
