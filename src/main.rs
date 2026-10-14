#![windows_subsystem = "windows"]

mod annotation;
mod capture;
mod clipboard;
mod cloud_settings_window;
mod cloudflare_oauth;
mod cloudflare_setup;
mod diagnostics;
mod drag;
mod drawing;
mod hotkey;
mod i18n;
mod instance;
mod overlay;
mod pin;
mod save;
mod settings;
mod settings_window;
mod smoke;
mod startup;
mod theme;
mod toast;
mod toolbar;
mod tray;
mod ui;
mod updater;
mod upload;
mod window_snap;

use capture::CaptureBuffer;
use hotkey::{HotkeyConfig, HotkeyHandle, start_hotkey_listener};
use instance::InstanceState;
use overlay::show_overlay_session;
use settings::{Settings, show_settings_dialog};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;
use tray::{TrayCommand, TrayManager};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

fn print_usage() {
    println!("isolmaSS - Lightweight Native Windows Screenshot Utility");
    println!("Usage:");
    println!("  isolmass                   Run tray daemon with configured capture shortcut");
    println!(
        "  isolmass --fix-printscreen Apply registry fix to disable Windows Snipping Tool on PrtScn"
    );
    println!("  isolmass --smoke-test      Run Windows behavior and package verification");
    println!("  isolmass --test-capture    Alias for --smoke-test");
    println!("  isolmass --capture-once    Capture immediately and open overlay once");
    println!("  isolmass --settings        Open native settings dialog");
    println!("  isolmass --check-update    Check GitHub Releases for a newer signed version");
    println!(
        "  isolmass --benchmark [N]   Measure N capture-to-visible samples (default 50; JSON lines)"
    );
    println!("  isolmass --verify-update P Verify installer with pinned publisher signature");
    println!("  isolmass --help            Show this help message");
}

fn start_hotkey_runtime(config: HotkeyConfig) -> Result<HotkeyRuntime, Box<dyn std::error::Error>> {
    let requested = config.description.clone();
    let (event_rx, handle) = start_hotkey_listener(config)?;
    tray::set_active_hotkey(&handle.active_description);
    if handle.active_description != requested {
        tray::show_notification(
            crate::i18n::t("Capture shortcut changed"),
            &i18n::tf(
                "{} is in use. This session captures with {}.",
                &[&requested, &handle.active_description],
            ),
        );
    }
    let forward_thread = std::thread::spawn(move || {
        while let Ok(triggered) = event_rx.recv() {
            tray::queue_capture(triggered);
        }
    });
    Ok(HotkeyRuntime {
        handle: Some(handle),
        forwarder: Some(forward_thread),
    })
}

fn rollback_hotkey_setting(settings: &mut Settings, previous: &HotkeyConfig) -> Result<(), String> {
    settings.hotkey = previous.clone();
    settings
        .save()
        .map_err(|error| i18n::tf("Could not save the previous shortcut: {}", &[&error]))
}

fn open_recent_capture(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            windows::Win32::Foundation::HWND::default(),
            w!("open"),
            PCWSTR(wide_path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(i18n::tf("Windows could not open '{}'.", &[&path.display()]))
    } else {
        Ok(())
    }
}

/// The newest saved capture, when it sits directly in `folder`.
fn newest_capture_in<'a>(recent: &'a [PathBuf], folder: &Path) -> Option<&'a Path> {
    recent
        .first()
        .map(PathBuf::as_path)
        .filter(|path| path.parent() == Some(folder))
}

/// Opens Explorer on the file's folder with the file selected; false when Explorer refused.
fn select_in_explorer(file: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};
    let wide_path: Vec<u16> = file.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut item: *mut ITEMIDLIST = std::ptr::null_mut();
    let parsed = unsafe {
        SHParseDisplayName(
            PCWSTR(wide_path.as_ptr()),
            None::<&IBindCtx>,
            &mut item,
            0,
            None,
        )
    };
    if parsed.is_err() || item.is_null() {
        return false;
    }
    // Without child items, the single PIDL names the item to select in its parent folder.
    let opened = unsafe { SHOpenFolderAndSelectItems(item, None, 0) }.is_ok();
    unsafe { CoTaskMemFree(Some(item.cast())) };
    opened
}

/// Opens the folder of the latest save, or the default save folder. A fresh install has no
/// screenshots folder yet, so it is created first; the newest capture is selected when it is there.
fn open_screenshots_folder(default_directory: &Path) -> Result<(), String> {
    let folder = save::recent::last_folder().unwrap_or_else(|| default_directory.to_owned());
    if let Err(error) = std::fs::create_dir_all(&folder) {
        diagnostics::record("screenshots folder", &error.to_string());
    }
    let recent = save::recent::list();
    if newest_capture_in(&recent, &folder).is_some_and(select_in_explorer) {
        return Ok(());
    }
    open_recent_capture(&folder)
}

struct HotkeyRuntime {
    handle: Option<HotkeyHandle>,
    forwarder: Option<std::thread::JoinHandle<()>>,
}
impl Drop for HotkeyRuntime {
    fn drop(&mut self) {
        drop(self.handle.take());
        if let Some(forwarder) = self.forwarder.take()
            && forwarder.join().is_err()
        {
            diagnostics::record(
                "hotkey",
                "The capture queue worker terminated unexpectedly.",
            );
        }
    }
}

struct BackgroundServices;
impl Drop for BackgroundServices {
    fn drop(&mut self) {
        updater::shutdown();
        save::recent::shutdown();
    }
}

fn apply_settings(
    settings: &mut Settings,
    mut latest: Settings,
    runtime: &mut Option<HotkeyRuntime>,
) {
    if latest.hotkey != settings.hotkey {
        drop(runtime.take());
        match start_hotkey_runtime(latest.hotkey.clone()) {
            Ok(next) => *runtime = Some(next),
            Err(error) => {
                let requested = latest.hotkey.description.clone();
                let keep_previous = ui::confirm(
                    tray::window_handle(),
                    crate::i18n::t("Shortcut conflict"),
                    &i18n::tf(
                        "'{}' is unavailable: {}\nKeep the previous shortcut '{}'? If you choose No, you can capture from the tray until you record a new shortcut.",
                        &[&requested, &error, &settings.hotkey.description],
                    ),
                );
                if keep_previous {
                    match start_hotkey_runtime(settings.hotkey.clone()) {
                        Ok(previous) => {
                            *runtime = Some(previous);
                            if let Err(rollback) =
                                rollback_hotkey_setting(&mut latest, &settings.hotkey)
                            {
                                tray::show_notification(
                                    crate::i18n::t("Could not restore the shortcut"),
                                    &rollback,
                                );
                            }
                        }
                        Err(restart_error) => {
                            *runtime = None;
                            tray::set_active_hotkey("");
                            let mut detail = i18n::tf(
                                "The previous shortcut is unavailable too: {}",
                                &[&restart_error],
                            );
                            if let Err(rollback) =
                                rollback_hotkey_setting(&mut latest, &settings.hotkey)
                            {
                                detail.push_str(&format!(" {rollback}"));
                            }
                            tray::show_notification(
                                crate::i18n::t("Shortcut unavailable"),
                                &detail,
                            );
                        }
                    }
                } else {
                    *runtime = None;
                    tray::set_active_hotkey("");
                    if let Err(rollback) = rollback_hotkey_setting(&mut latest, &settings.hotkey) {
                        tray::show_notification(
                            crate::i18n::t("Could not restore the shortcut"),
                            &rollback,
                        );
                    }
                }
            }
        }
    }
    theme::set_preference(latest.theme_preference);
    i18n::set_preference(latest.language);
    updater::configure(settings, &latest);
    *settings = latest;
    tray::refresh_preferences(settings);
}

/// Captures the screen and opens the editor; `last_region` preselects the region of the
/// last export when it is still on a monitor.
fn capture(triggered: Instant, last_region: bool) {
    match CaptureBuffer::capture_virtual_screen().and_then(|capture| {
        let preselected = last_region
            .then(|| capture.last_region_selection())
            .flatten();
        show_overlay_session(Rc::new(capture), triggered, preselected)
    }) {
        Ok(_) => {}
        Err(error) => tray::show_notification(crate::i18n::t("Capture failed"), &error.to_string()),
    }
    tray::capture_finished();
}
/// Exercises the installed executable's normal startup dependencies before setup commits.
fn run_health_check() -> Result<(), Box<dyn std::error::Error>> {
    let instance = match instance::acquire_or_notify()? {
        InstanceState::Primary(instance) => instance,
        InstanceState::ExistingNotified => {
            return Err(crate::i18n::t("Another isolmaSS instance is still running.").into());
        }
    };
    let (settings, warning) = Settings::load_with_warning()?;
    if let Some(warning) = warning {
        diagnostics::record("health check settings", &warning);
    }
    theme::set_preference(settings.theme_preference);
    i18n::set_preference(settings.language);
    tray::set_active_hotkey(&settings.hotkey.description);
    let tray = tray::TrayManager::create()?;
    let runtime = start_hotkey_runtime(settings.hotkey)?;
    drop(runtime);
    drop(tray);
    drop(instance);
    Ok(())
}

fn run_interactive_session(open_settings: bool) -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::UI::WindowsAndMessaging::*;
    let _instance = match instance::acquire_or_notify()? {
        InstanceState::Primary(instance) => instance,
        InstanceState::ExistingNotified => return Ok(()),
    };
    let first_run = Settings::config_path().is_some_and(|path| !path.exists());
    let (mut settings, warning) = Settings::load_with_warning()?;
    theme::set_preference(settings.theme_preference);
    i18n::set_preference(settings.language);
    if let Err(error) = startup::set_start_with_windows(settings.start_with_windows) {
        diagnostics::record("startup", &error);
    }
    let tray_manager = TrayManager::create()?;
    let services = BackgroundServices;
    tray::refresh_preferences(&settings);
    if let Some(warning) = warning {
        tray::show_notification(crate::i18n::t("Settings recovered"), &warning);
    }
    let mut runtime = match start_hotkey_runtime(settings.hotkey.clone()) {
        Ok(active) => Some(active),
        Err(error) => {
            let fallback = HotkeyConfig::fallback();
            if settings.hotkey != fallback
                && ui::confirm(
                    tray::window_handle(),
                    crate::i18n::t("Shortcut unavailable"),
                    &i18n::tf(
                        "{}\nUse {} instead for this session?",
                        &[&error, &fallback.description],
                    ),
                )
            {
                match start_hotkey_runtime(fallback) {
                    Ok(active) => Some(active),
                    Err(fallback_error) => {
                        tray::set_active_hotkey("");
                        tray::show_notification(
                            crate::i18n::t("Shortcut unavailable"),
                            &fallback_error.to_string(),
                        );
                        None
                    }
                }
            } else {
                tray::set_active_hotkey("");
                tray::show_notification(
                    crate::i18n::t("Shortcut unavailable"),
                    &i18n::tf(
                        "{} Capture from the tray or choose another shortcut in Settings.",
                        &[&error],
                    ),
                );
                None
            }
        }
    };
    if first_run {
        // New tray icons start in Windows 11's overflow flyout, so say that the app is running.
        if let Some(handle) = runtime.as_ref().and_then(|active| active.handle.as_ref()) {
            tray::show_notification(
                crate::i18n::t("isolmaSS is ready"),
                &i18n::tf(
                    "Press {} to capture. isolmaSS waits in the notification area.",
                    &[&handle.active_description],
                ),
            );
        }
        // Writing the file is what makes the next launch a regular one.
        if let Err(error) = settings.save() {
            diagnostics::record("first run", &error.to_string());
        }
    }
    updater::run_startup_work(settings.check_updates_automatically);
    drag::sweep_in_background();
    if open_settings {
        tray::queue_command(TrayCommand::Settings);
    }
    const CAPTURE_TIMER: usize = 41;
    let mut scheduled: Option<(Instant, bool)> = None;
    let mut msg = MSG::default();
    let mut running = true;
    // Quit asked while an upload is in flight; the loop ends once the upload card is done.
    let mut exit_requested = false;
    while running {
        let status = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if status == -1 {
            return Err(windows::core::Error::from_win32().into());
        }
        if status == 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let mut reload = false;
        if msg.message == WM_TIMER && msg.wParam.0 == CAPTURE_TIMER {
            unsafe {
                let _ = KillTimer(tray::window_handle(), CAPTURE_TIMER);
            }
            if let Some((triggered, last_region)) = scheduled.take() {
                capture(triggered, last_region);
                reload = true;
            }
        }
        for command in tray::take_commands() {
            let last_region = matches!(command, TrayCommand::CaptureLastRegion(_));
            if exit_requested {
                if matches!(
                    command,
                    TrayCommand::Capture(_) | TrayCommand::CaptureLastRegion(_)
                ) {
                    tray::capture_finished();
                }
                continue;
            }
            match command {
                TrayCommand::Capture(triggered) | TrayCommand::CaptureLastRegion(triggered) => {
                    if tray::capture_is_stale(triggered) {
                        continue;
                    }
                    if settings.capture_delay_ms > 0 {
                        scheduled = Some((triggered, last_region));
                        if unsafe {
                            SetTimer(
                                tray::window_handle(),
                                CAPTURE_TIMER,
                                settings.capture_delay_ms,
                                None,
                            )
                        } == 0
                        {
                            scheduled = None;
                            tray::capture_finished();
                            tray::show_notification(
                                crate::i18n::t("Capture failed"),
                                crate::i18n::t("Could not start the capture timer."),
                            );
                        } else {
                            tray::show_notification(
                                crate::i18n::t("Capture timer"),
                                &i18n::tf(
                                    "Capturing in {} s. Press Esc to cancel.",
                                    &[&(settings.capture_delay_ms as f64 / 1000.0)],
                                ),
                            );
                        }
                    } else {
                        capture(triggered, last_region);
                        reload = true;
                    }
                }
                TrayCommand::CancelCapture => {
                    unsafe {
                        let _ = KillTimer(tray::window_handle(), CAPTURE_TIMER);
                    }
                    scheduled = None;
                    tray::capture_finished();
                }
                TrayCommand::Settings => {
                    unsafe {
                        let _ = KillTimer(tray::window_handle(), CAPTURE_TIMER);
                    }
                    scheduled = None;
                    tray::capture_finished();
                    match show_settings_dialog(&settings, Some(tray::window_handle())) {
                        Ok(Some(latest)) => apply_settings(&mut settings, latest, &mut runtime),
                        Ok(None) => {}
                        Err(error) => ui::error(
                            tray::window_handle(),
                            crate::i18n::t("Could not open Settings"),
                            &error.to_string(),
                        ),
                    }
                    tray::capture_finished();
                }
                TrayCommand::CheckUpdates => {
                    if updater::run_manual_update_check() {
                        tray::set_update_activity(Some(crate::i18n::t(
                            "isolmaSS · Checking for updates…",
                        )));
                        tray::show_update_notification(
                            crate::i18n::t("Checking for updates"),
                            crate::i18n::t(
                                "Looking for the latest signed release. You can keep using isolmaSS.",
                            ),
                        );
                    } else {
                        tray::show_update_notification(
                            crate::i18n::t("Update in progress"),
                            crate::i18n::t("An update is already downloading or installing."),
                        );
                    }
                }
                TrayCommand::OpenFolder => {
                    if let Err(error) = open_screenshots_folder(&settings.save_directory) {
                        tray::show_notification(
                            crate::i18n::t("Could not open the folder"),
                            &error,
                        );
                    }
                }
                TrayCommand::OpenRecent(path) => {
                    if let Err(error) = open_recent_capture(&path) {
                        tray::show_notification(
                            crate::i18n::t("Could not open the screenshot"),
                            &error,
                        );
                    }
                }
                TrayCommand::Exit => {
                    unsafe {
                        let _ = KillTimer(tray::window_handle(), CAPTURE_TIMER);
                    }
                    scheduled = None;
                    exit_requested = true;
                }
            }
        }
        if exit_requested && !toast::upload_in_flight() {
            running = false;
        }
        if reload {
            match Settings::load_with_warning() {
                Ok((latest, warning)) => {
                    if let Some(warning) = warning {
                        tray::show_notification(crate::i18n::t("Settings recovered"), &warning);
                    }
                    apply_settings(&mut settings, latest, &mut runtime);
                }
                Err(error) => {
                    diagnostics::record("settings", &error.to_string());
                    tray::show_notification(
                        crate::i18n::t("Could not reload settings"),
                        &i18n::tf("The current settings are kept. {}", &[&error]),
                    );
                }
            }
        }
        if running && !exit_requested {
            updater::poll(tray::window_handle(), true);
            save::recent::refresh(&settings.save_directory);
        }
    }
    unsafe {
        let _ = KillTimer(tray::window_handle(), CAPTURE_TIMER);
    }
    tray::capture_finished();
    // The open upload card takes its dragged copy of the image with it.
    toast::close_now();
    pin::close_all();
    drag::finish_pending();
    drop(services);
    drop(runtime.take());
    drop(tray_manager);
    Ok(())
}

fn run_capture_once() -> Result<(), Box<dyn std::error::Error>> {
    println!("[isolmaSS] Capturing virtual screen immediately...");
    let triggered = Instant::now();
    let capture = CaptureBuffer::capture_virtual_screen()?;
    println!(
        "[isolmaSS] Capture={}us (setup={}us, BitBlt={}us, dim={}us).",
        capture.timings.total.as_micros(),
        capture.timings.setup.as_micros(),
        capture.timings.bit_blt.as_micros(),
        capture.timings.dim.as_micros(),
    );
    let capture_rc = Rc::new(capture);
    match show_overlay_session(capture_rc, triggered, None)? {
        Some(selection) => {
            println!(
                "[isolmaSS] Capture committed: [({}, {}) to ({}, {})] ({}x{} pixels).",
                selection.left,
                selection.top,
                selection.right,
                selection.bottom,
                selection.width(),
                selection.height()
            );
        }
        None => {
            println!("[isolmaSS] Overlay dismissed.");
        }
    }
    toast::wait_until_closed();
    drag::finish_pending();
    pin::wait_until_closed();
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    }
    .ok()?;
    let _apartment = ComApartment;
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        // Attach to parent terminal so CLI flags print output properly
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }

    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    if args.len() > 1 {
        match args[1].as_str() {
            "--fix-printscreen" => {
                println!(
                    "[isolmaSS] Applying Windows registry fix to suppress Snipping Tool on PrintScreen..."
                );
                if hotkey::disable_windows_snipping_tool_hotkey() {
                    println!(
                        "[isolmaSS] SUCCESS: Set HKCU\\Control Panel\\Keyboard -> PrintScreenKeyForSnippingEnabled = 0."
                    );
                    println!(
                        "[isolmaSS] Windows Snipping Tool is permanently disabled from capturing PrintScreen."
                    );
                    println!("[isolmaSS] PrintScreen is now dedicated exclusively to isolmaSS.");
                } else {
                    eprintln!(
                        "[isolmaSS] ERROR: Failed to update Windows registry value PrintScreenKeyForSnippingEnabled."
                    );
                    std::process::exit(1);
                }
                return Ok(());
            }
            "--smoke-test" | "--test-capture" => {
                return smoke::run_smoke_test();
            }
            "--benchmark" => {
                let count = args
                    .get(2)
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(50);
                return smoke::run_benchmark(count);
            }
            "--capture-once" => {
                return run_capture_once();
            }
            "--health-check" => {
                return run_health_check();
            }
            "--verify-update" => {
                let path = args
                    .get(2)
                    .ok_or("--verify-update requires an installer path")?;
                let version = updater::verify_update_artifact(Path::new(path))?;
                println!("Installer v{version} matches the pinned publisher signature.");
                return Ok(());
            }
            "--settings" => {
                return run_interactive_session(true);
            }
            "--check-update" => {
                match updater::check_for_update() {
                    Ok(Some(update)) => println!(
                        "Update available: {} ({})",
                        update.version, update.release_url
                    ),
                    Ok(None) => println!("isolmaSS {} is up to date.", env!("CARGO_PKG_VERSION")),
                    Err(error) => return Err(error.into()),
                }
                return Ok(());
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => {
                eprintln!("Unknown argument: {}", other);
                print_usage();
                std::process::exit(1);
            }
        }
    }

    run_interactive_session(false)
}

fn main() {
    let result = run();
    updater::shutdown();
    save::recent::shutdown();
    if let Err(error) = result {
        diagnostics::record("startup", &error.to_string());
        std::eprintln!("isolmaSS: {error}");
        if !std::env::args().any(|arg| {
            matches!(
                arg.as_str(),
                "--smoke-test"
                    | "--test-capture"
                    | "--check-update"
                    | "--verify-update"
                    | "--benchmark"
            )
        }) {
            ui::error(
                windows::Win32::Foundation::HWND::default(),
                crate::i18n::t("isolmaSS could not complete the operation"),
                &error.to_string(),
            );
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_capture_is_selected_only_inside_the_opened_folder() {
        let recent = [
            PathBuf::from(r"C:\Shots\newer.png"),
            PathBuf::from(r"C:\Shots\older.png"),
        ];
        assert_eq!(
            newest_capture_in(&recent, Path::new(r"C:\Shots")),
            Some(Path::new(r"C:\Shots\newer.png"))
        );
        assert_eq!(newest_capture_in(&recent, Path::new(r"D:\Elsewhere")), None);
        assert_eq!(newest_capture_in(&[], Path::new(r"C:\Shots")), None);
    }
}
