use super::*;
use crate::settings_window::UpdateStatus;
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

enum Event {
    Checked(Result<Option<UpdateInfo>, String>, bool),
    Downloaded(Result<PathBuf, String>, bool),
}

impl Event {
    fn manual(&self) -> bool {
        match self {
            Event::Checked(_, manual) => *manual,
            Event::Downloaded(_, automatic) => !*automatic,
        }
    }
}

fn remove_staged_update(path: &Path) {
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

fn start(automatic: bool, checking: bool, work: impl FnOnce() -> Event + Send + 'static) -> bool {
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

pub fn run_manual_update_check() -> bool {
    start(false, true, || Event::Checked(check_for_update(), true))
}
pub fn run_automatic_update_check() {
    let _ = start(true, true, || Event::Checked(check_for_update(), false));
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

/// Called by UI loops without a borrowed window state. Installation waits until editing ends.
pub fn poll(owner: HWND, allow_install: bool) {
    if RUNNING.load(Ordering::Acquire) {
        return;
    }
    let recheck = RECHECK
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if recheck.is_some() {
        run_automatic_update_check();
        return;
    }
    let event = EVENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if event.is_some() {
        crate::tray::set_update_activity(None);
    }
    match event {
        Some(Event::Checked(Ok(Some(update)), manual)) => {
            if !manual {
                match crate::settings::Settings::load() {
                    Ok(settings)
                        if settings.skipped_update_version.as_deref() == Some(&update.version) =>
                    {
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        crate::diagnostics::record("update preferences", &error.to_string());
                        return;
                    }
                }
            }
            if manual {
                crate::settings_window::report_update_status(
                    owner,
                    &crate::i18n::tf("Version {} is ready", &[&update.version]),
                    UpdateStatus::Idle,
                );
            }
            match crate::ui::ask_for_update(owner, &update.version) {
                Ok(crate::ui::UpdateChoice::Install) => {
                    let inline = crate::settings_window::report_update_status(
                        owner,
                        crate::i18n::t("Downloading and verifying the installer…"),
                        UpdateStatus::Busy,
                    );
                    crate::tray::set_update_activity(Some(crate::i18n::t(
                        "isolmaSS · Downloading update…",
                    )));
                    if !inline {
                        crate::tray::show_update_notification(
                            crate::i18n::t("Downloading update"),
                            crate::i18n::t(
                                "Verifying the signed installer. isolmaSS restarts after installation.",
                            ),
                        );
                    }
                    let _ = start(!manual, false, move || {
                        Event::Downloaded(download_update(&update), !manual)
                    });
                }
                Ok(crate::ui::UpdateChoice::SkipVersion) => {
                    if let Err(error) =
                        crate::settings::Settings::skip_update_version(&update.version)
                    {
                        crate::ui::error(
                            owner,
                            crate::i18n::t("Could not skip this version"),
                            &error.to_string(),
                        );
                    } else {
                        crate::settings_window::report_update_status(
                            owner,
                            crate::i18n::t("This version was skipped."),
                            UpdateStatus::Idle,
                        );
                    }
                }
                Ok(crate::ui::UpdateChoice::Later) => {
                    crate::settings_window::report_update_status(
                        owner,
                        crate::i18n::t("Not installed. You can check again at any time."),
                        UpdateStatus::Idle,
                    );
                }
                Err(error) => {
                    crate::settings_window::report_update_status(
                        owner,
                        crate::i18n::t("Could not open the update options. Try again."),
                        UpdateStatus::Idle,
                    );
                    crate::ui::error(
                        owner,
                        crate::i18n::t("Could not open the update window"),
                        &error.to_string(),
                    );
                }
            }
        }
        Some(Event::Checked(Ok(None), true)) => {
            let message = &crate::i18n::tf(
                "isolmaSS {} is the latest version.",
                &[&env!("CARGO_PKG_VERSION")],
            );
            if !crate::settings_window::report_update_status(owner, message, UpdateStatus::Idle) {
                crate::tray::show_update_notification(
                    crate::i18n::t("isolmaSS is up to date"),
                    message,
                );
                crate::ui::info(owner, crate::i18n::t("isolmaSS is up to date"), message);
            }
        }
        Some(Event::Checked(Err(error), manual)) => {
            crate::diagnostics::record("update", &error);
            if manual {
                crate::settings_window::report_update_status(
                    owner,
                    crate::i18n::t("Could not check for updates. Try again."),
                    UpdateStatus::Idle,
                );
                crate::ui::error(owner, crate::i18n::t("Update check failed"), &error);
            }
        }
        Some(Event::Downloaded(Ok(path), automatic)) => {
            let mut ready = READY.lock().unwrap_or_else(|error| error.into_inner());
            let same_path = ready.as_ref().is_some_and(|(old, _)| old == &path);
            if let Some((old, _)) = ready.replace((path, automatic))
                && !same_path
            {
                remove_staged_update(&old);
            }
            drop(ready);
            if !allow_install {
                let message = crate::i18n::t("Verified. Save or close Settings to install.");
                let inline = crate::settings_window::report_update_status(
                    owner,
                    message,
                    UpdateStatus::Ready,
                );
                if inline && !automatic {
                    crate::ui::info(
                        owner,
                        crate::i18n::t("Update ready"),
                        crate::i18n::t(
                            "The signed installer is ready. Save or close Settings; isolmaSS will install and reopen.",
                        ),
                    );
                } else if automatic {
                    crate::tray::show_notification(
                        crate::i18n::t("Update ready"),
                        crate::i18n::t("Finish the open task to install and restart."),
                    );
                } else {
                    crate::tray::show_update_notification(
                        crate::i18n::t("Update ready"),
                        crate::i18n::t("Finish the open capture to install."),
                    );
                }
            }
        }
        Some(Event::Downloaded(Err(error), automatic)) => {
            crate::settings_window::report_update_status(
                owner,
                crate::i18n::t("Verification failed. Try the update again."),
                UpdateStatus::Idle,
            );
            if automatic {
                crate::diagnostics::record("update", &error);
                crate::tray::show_notification(
                    crate::i18n::t("Could not download the update"),
                    &error,
                );
            } else {
                crate::ui::error(
                    owner,
                    crate::i18n::t("Could not install the update"),
                    &error,
                );
            }
        }
        _ => {}
    }
    let ready = if allow_install {
        READY
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    } else {
        None
    };
    if let Some((path, automatic)) = ready {
        if !automatic {
            crate::tray::show_update_notification(
                crate::i18n::t("Installing update"),
                crate::i18n::t("isolmaSS closes, installs the verified version and reopens."),
            );
        }
        match launch_installer(&path) {
            Ok(()) => crate::tray::request_exit(),
            Err(error) => {
                crate::ui::error(
                    owner,
                    crate::i18n::t("Could not start the installer"),
                    &error,
                );
                remove_staged_update(&path);
            }
        }
    }
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
