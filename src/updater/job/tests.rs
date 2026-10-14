use super::*;
use crate::i18n::t;
use crate::settings::Settings;
use crate::updater::tests::Scratch;
use std::sync::{Arc, mpsc};

/// The tests share the updater's process-wide state. Whoever holds the guard has it to
/// themselves and starts from a quiet updater: nothing running, nothing pending.
pub(in crate::updater) fn exclusive() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = lock(&LOCK);
    let worker = lock(&WORKER).take();
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
    *lock(&EVENT) = None;
    *lock(&READY) = None;
    *lock(&RECHECK) = None;
    guard
}

/// Waits for the worker started last to finish.
fn wait_until_idle() {
    let worker = lock(&WORKER).take();
    worker
        .expect("a worker was started")
        .join()
        .expect("the worker finished");
}

/// Work that blocks until the returned sender releases it, then yields `event`.
fn gated(event: impl FnOnce() -> Event + Send + 'static) -> (mpsc::Sender<()>, Work) {
    let (release, wait) = mpsc::channel::<()>();
    let work = move || {
        wait.recv().expect("the worker is released");
        event()
    };
    (release, Box::new(work))
}

fn pending() -> Option<Event> {
    lock(&EVENT).take()
}

fn ready() -> Option<(PathBuf, bool)> {
    lock(&READY).clone()
}

fn set_pending(event: Event) {
    *lock(&EVENT) = Some(event);
}

fn set_ready(path: &Path, automatic: bool) {
    *lock(&READY) = Some((path.to_path_buf(), automatic));
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
fn a_poisoned_lock_hands_its_data_back() {
    let mutex = Arc::new(Mutex::new(5));
    let poisoner = mutex.clone();
    let crashed = std::thread::spawn(move || {
        let _guard = poisoner.lock();
        panic!("the thread dies holding the lock");
    })
    .join();
    assert!(crashed.is_err());
    assert!(mutex.is_poisoned());
    assert_eq!(*lock(&mutex), 5);
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
    *lock(&WORKER) = Some(std::thread::spawn(|| {}));
    assert!(start(
        false,
        false,
        Box::new(|| Event::Checked(Ok(None), true))
    ));
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
    assert!(!start(true, true, Box::new(nothing)));
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
    assert!(start(false, true, Box::new(nothing)));
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
    assert!(start(false, true, Box::new(nothing)));
    assert!(matches!(pending(), Some(Event::Checked(Ok(None), true))));
    RUNNING.store(false, Ordering::Release);
}

#[test]
fn a_manual_request_is_refused_while_a_download_runs() {
    let _state = exclusive();
    let (release, work) = gated(nothing);
    assert!(start(true, false, work));
    assert!(!start(false, true, Box::new(nothing)));
    assert!(!MANUAL_REQUESTED.load(Ordering::Acquire));
    release.send(()).expect("release the worker");
    wait_until_idle();
}

#[test]
fn a_manual_result_replaces_a_pending_automatic_one() {
    let _state = exclusive();
    set_pending(nothing());
    let work = || Event::Checked(Err("fresh".to_string()), true);
    assert!(start(false, true, Box::new(work)));
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
    let work = || Event::Checked(Err("automatic".to_string()), false);
    assert!(start(true, true, Box::new(work)));
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

static STARTUP_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn sweep() {
    lock(&STARTUP_LOG).push("sweep");
}

fn offline() -> Result<Option<UpdateInfo>, String> {
    lock(&STARTUP_LOG).push("check");
    Err("offline".to_string())
}

fn startup(check_for_updates: bool) -> (Event, Vec<&'static str>) {
    lock(&STARTUP_LOG).clear();
    let event = startup_event(check_for_updates, sweep, offline);
    (event, std::mem::take(&mut *lock(&STARTUP_LOG)))
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
    assert!(lock(&RECHECK).is_none());
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
    assert!(lock(&RECHECK).is_none());
    configure(&settings(false), &settings(true));
    assert_eq!(*lock(&RECHECK), Some(true));
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
    *lock(&RECHECK) = Some(true);
    set_pending(nothing());
    assert!(matches!(next_step(), Step::Busy));
    assert!(lock(&RECHECK).is_some());
    RUNNING.store(false, Ordering::Release);
    assert!(matches!(next_step(), Step::Recheck));
    assert!(lock(&RECHECK).is_none());
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
    let work = move || {
        std::thread::sleep(Duration::from_millis(50));
        Event::Downloaded(Ok(result), true)
    };
    assert!(start(true, false, Box::new(work)));
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

    assert!(start(
        true,
        false,
        Box::new(|| panic!("the worker crashes"))
    ));
    shutdown();
    assert!(lock(&WORKER).is_none());
}
