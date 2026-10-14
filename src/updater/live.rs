//! The updater's contact with the real world: GitHub, the user's update folder, the shell and
//! the dialogs. Every decision lives in `updater.rs` and `updater/job.rs`, which the unit tests
//! exercise against loopback servers and temporary folders; this file only hands them their
//! production inputs. It needs the internet and a live desktop, so it is excluded from the
//! coverage gate (scripts/coverage-exclude.txt) and exercised by `isolmass --smoke-test`.

use super::job::{
    Event, Step, download_failure_status, is_skipped, next_step, remove_staged_update, stage_ready,
    start, startup_event, take_ready,
};
use super::*;
use crate::settings_window::UpdateStatus;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn check_for_update() -> Result<Option<UpdateInfo>, String> {
    fetch_release(LIVE.route, env!("CARGO_PKG_VERSION"))
}

fn download_update(update: &UpdateInfo) -> Result<PathBuf, DownloadError> {
    download_from(&LIVE, update, &update_directory)
}

/// Runs on the update worker, so it never races a download. A finished installer cannot delete
/// itself; the next launch removes it here.
fn remove_stale_update_files() {
    if let (Some(current), Ok(root)) =
        (parse_version(env!("CARGO_PKG_VERSION")), update_directory())
    {
        remove_stale_files_in(&root, current);
    }
}

fn launch_installer(path: &Path) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    let _locked_file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
        .map_err(|error| error.to_string())?;
    verify_staged_installer(PINNED_PUBLIC_KEY, &update_directory()?, path)?;
    let parameters = wide(&format!("/S /UPDATE /WAITPID={}", std::process::id()));
    use std::os::windows::ffi::OsStrExt;
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(crate::i18n::tf(
            "Windows could not launch the installer (code {}).",
            &[&(result.0 as isize)],
        ))
    } else {
        Ok(())
    }
}

/// Opens a release page in the browser; only the GitHub hosts are accepted.
fn open_release_page(url: &str) -> Result<(), String> {
    parse_https_url(url)?;
    let url_wide = wide(url);
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(url_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(crate::i18n::tf("Windows could not open '{}'.", &[&url]))
    } else {
        Ok(())
    }
}

pub fn run_manual_update_check() -> bool {
    start(
        false,
        true,
        Box::new(|| Event::Checked(check_for_update(), true)),
    )
}

fn run_automatic_update_check() {
    let _ = start(
        true,
        true,
        Box::new(|| Event::Checked(check_for_update(), false)),
    );
}

/// Once per launch: sweeps leftovers of earlier updates, then runs the automatic check when
/// enabled. Without a check the worker is not flagged as checking, so the menu stays usable.
pub fn run_startup_work(check_for_updates: bool) {
    let _ = start(
        true,
        check_for_updates,
        Box::new(move || {
            startup_event(
                check_for_updates,
                remove_stale_update_files,
                check_for_update,
            )
        }),
    );
}

/// The portable build never runs the installer: the offer leads to the release page instead.
fn offer_release_page(owner: HWND, update: &UpdateInfo) {
    let open_page = crate::ui::confirm(
        owner,
        crate::i18n::t("isolmaSS update"),
        &crate::i18n::tf(
            "isolmaSS {} is available. This portable copy does not update itself. Open the download page to get the new portable ZIP?",
            &[&update.version],
        ),
    );
    if !open_page {
        crate::settings_window::report_update_status(
            owner,
            crate::i18n::t("Not installed. You can check again at any time."),
            UpdateStatus::Idle,
        );
    } else if let Err(error) = open_release_page(&update.release_url) {
        crate::ui::error(
            owner,
            crate::i18n::t("Could not open the download page"),
            &error,
        );
    }
}

/// Called by UI loops without a borrowed window state. Installation waits until editing ends.
pub fn poll(owner: HWND, allow_install: bool) {
    let event = match next_step() {
        Step::Busy => return,
        Step::Recheck => {
            run_automatic_update_check();
            return;
        }
        Step::Event(event) => event,
    };
    if event.is_some() {
        crate::tray::set_update_activity(None);
    }
    match event {
        Some(Event::Checked(Ok(Some(update)), manual)) => {
            if !manual {
                match crate::settings::Settings::load() {
                    Ok(settings) if is_skipped(&settings, &update.version) => {
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
            if !runs_from_installation() {
                offer_release_page(owner, &update);
                return;
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
                    let _ = start(
                        !manual,
                        false,
                        Box::new(move || Event::Downloaded(download_update(&update), !manual)),
                    );
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
            stage_ready(path, automatic);
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
                download_failure_status(error.kind),
                UpdateStatus::Idle,
            );
            if automatic {
                crate::diagnostics::record("update", &error.message);
                crate::tray::show_notification(
                    crate::i18n::t("Could not download the update"),
                    &error.message,
                );
            } else {
                crate::ui::error(
                    owner,
                    crate::i18n::t("Could not install the update"),
                    &error.message,
                );
            }
        }
        _ => {}
    }
    // An upload in flight is killed by the installer's exit request; wait like for an open editor.
    if let Some((path, automatic)) = take_ready(allow_install && !crate::toast::upload_in_flight())
    {
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
