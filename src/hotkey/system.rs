//! The parts of the shortcut that need the live desktop: the user's registry, the
//! low-level keyboard hook installed for an overlay session, and the listener thread
//! with its `RegisterHotKey` message loop. What the hook does with a keystroke, and how
//! shortcuts are parsed and matched, is decided in the parent module.
use super::{
    HOOK_STATE, HOTKEY_ID, HookState, HotkeyConfig, OVERLAY_ACTIVE, OVERLAY_HWND,
    OVERLAY_IME_COMPOSING, OVERLAY_LOCAL_HOOK, OVERLAY_TEXT_EDITING, SNAPSHOT_HANDLED, TARGET_MODS,
    TARGET_VK, is_overlay_active, low_level_keyboard_proc, overlay_input_suspended,
    trigger_instant,
};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey, VK_SNAPSHOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HHOOK, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_HOTKEY, WM_QUIT,
};

/// Disables the Windows Snipping Tool from intercepting the PrintScreen key
/// by setting HKCU\Control Panel\Keyboard -> PrintScreenKeyForSnippingEnabled = 0 (REG_DWORD).
pub fn disable_windows_snipping_tool_hotkey() -> bool {
    use windows::Win32::System::Registry::*;
    let value = 0u32;
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Control Panel\\Keyboard"),
            windows::core::w!("PrintScreenKeyForSnippingEnabled"),
            REG_DWORD.0,
            Some((&value as *const u32).cast()),
            4,
        )
        .is_ok()
    }
}

/// Registers the active overlay window handle and marks the overlay as active.
/// If no global low-level hook is currently running, installs a temporary hook
/// for the duration of the overlay session.
pub fn register_overlay(hwnd: HWND) -> windows::core::Result<()> {
    OVERLAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    OVERLAY_ACTIVE.store(true, Ordering::SeqCst);

    let has_hook = HOOK_STATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_ref()
        .is_some();

    if !has_hook {
        let hinstance = unsafe {
            GetModuleHandleW(None)
                .ok()
                .map(|h| HINSTANCE(h.0))
                .unwrap_or_default()
        };
        let hook_handle: windows::core::Result<HHOOK> = unsafe {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), hinstance, 0)
        };
        let hook = match hook_handle {
            Ok(hook) => hook,
            Err(error) => {
                unregister_overlay();
                return Err(error);
            }
        };
        OVERLAY_LOCAL_HOOK.store(hook.0 as isize, Ordering::SeqCst);
    }
    Ok(())
}

/// Unregisters the overlay window and tears down any temporary overlay hook.
pub fn unregister_overlay() {
    OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
    OVERLAY_TEXT_EDITING.store(false, Ordering::SeqCst);
    OVERLAY_IME_COMPOSING.store(false, Ordering::SeqCst);
    OVERLAY_HWND.store(0, Ordering::SeqCst);

    let prev_hook = OVERLAY_LOCAL_HOOK.swap(0, Ordering::SeqCst);
    if prev_hook != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(prev_hook as *mut _));
        }
    }
}

/// Active hotkey handle with thread cleanup and hook removal on Drop.
struct ListenerResources(HHOOK);
impl Drop for ListenerResources {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWindowsHookEx(self.0);
            let _ = UnregisterHotKey(HWND::default(), HOTKEY_ID);
        }
        TARGET_VK.store(0, Ordering::SeqCst);
        TARGET_MODS.store(0, Ordering::SeqCst);
        SNAPSHOT_HANDLED.store(false, Ordering::SeqCst);
        *HOOK_STATE
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }
}

struct StopSignal(isize);
impl Drop for StopSignal {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(
                self.0 as *mut _,
            ));
        }
    }
}

pub struct HotkeyHandle {
    stop: std::sync::Arc<StopSignal>,
    thread_id: u32,
    join_handle: Option<JoinHandle<()>>,
    pub active_description: String,
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        if let Err(error) = unsafe {
            windows::Win32::System::Threading::SetEvent(windows::Win32::Foundation::HANDLE(
                self.stop.0 as *mut _,
            ))
        } {
            crate::diagnostics::record("hotkey shutdown", &error.to_string());
        }
        if self.thread_id != 0 {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(handle) = self.join_handle.take()
            && handle.join().is_err()
        {
            crate::diagnostics::record("hotkey", "Keyboard listener terminated unexpectedly.");
        }
    }
}

/// Starts the global hotkey listener on a dedicated Win32 message-loop thread.
/// Returns a Receiver for hotkey trigger events and a HotkeyHandle that unregisters on drop.
pub fn start_hotkey_listener(
    requested_config: HotkeyConfig,
) -> windows::core::Result<(Receiver<Instant>, HotkeyHandle)> {
    let (event_tx, event_rx) = sync_channel::<Instant>(1);
    let (ready_tx, ready_rx) = channel::<std::result::Result<(u32, String), String>>();

    let event =
        unsafe { windows::Win32::System::Threading::CreateEventW(None, true, false, None)? };
    let stop = std::sync::Arc::new(StopSignal(event.0 as isize));
    let listener_stop = stop.clone();
    let join_handle = thread::spawn(move || {
        let thread_id = unsafe { GetCurrentThreadId() };

        // Force creation of message queue for this thread
        let mut msg = MSG::default();
        unsafe {
            let _ = PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_NOREMOVE);
        }

        let is_snapshot = requested_config.vk == VK_SNAPSHOT.0 as u32;

        // Populate delivery state before installing the hook so no callback can
        // ever run without it, and fail startup loudly on a poisoned state lock
        // instead of continuing with a shortcut that swallows keys silently.
        {
            let mut guard = match HOOK_STATE.lock() {
                Ok(guard) => guard,
                Err(_) => {
                    let _ = ready_tx.send(Err(crate::i18n::t(
                        "Keyboard hook state could not be initialized: state lock poisoned.",
                    )
                    .to_string()));
                    return;
                }
            };
            *guard = Some(HookState {
                event_tx: event_tx.clone(),
                _config: requested_config.clone(),
            });
        }

        // Set target VK and modifiers for low-level hook
        TARGET_VK.store(requested_config.vk, Ordering::SeqCst);
        TARGET_MODS.store(requested_config.modifiers, Ordering::SeqCst);
        SNAPSHOT_HANDLED.store(false, Ordering::SeqCst);

        let hinstance = unsafe {
            GetModuleHandleW(None)
                .ok()
                .map(|h| HINSTANCE(h.0))
                .unwrap_or_default()
        };

        let hook_handle: windows::core::Result<HHOOK> = unsafe {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), hinstance, 0)
        };

        let hook = match hook_handle {
            Ok(hook) => hook,
            Err(error) => {
                TARGET_VK.store(0, Ordering::SeqCst);
                TARGET_MODS.store(0, Ordering::SeqCst);
                *HOOK_STATE
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = None;
                let _ = ready_tx.send(Err(crate::i18n::tf(
                    "Keyboard hook could not be installed: {}",
                    &[&error],
                )));
                return;
            }
        };
        let _resources = ListenerResources(hook);

        let active_desc = if is_snapshot {
            // Low-level hook is primary for PrintScreen.
            // Attempt RegisterHotKey as well:
            let reg_ok = unsafe {
                RegisterHotKey(
                    HWND::default(),
                    HOTKEY_ID,
                    HOT_KEY_MODIFIERS(requested_config.modifiers),
                    requested_config.vk,
                )
            }
            .is_ok();

            if !reg_ok {
                crate::diagnostics::record(
                    "hotkey",
                    &format!(
                        "RegisterHotKey for '{}' held by OS; WH_KEYBOARD_LL hook active, Snipping Tool suppressed.",
                        requested_config.description
                    ),
                );
            }
            // Do not switch to fallback: WH_KEYBOARD_LL intercepts VK_SNAPSHOT and consumes it!
            requested_config.description.clone()
        } else {
            // Non-snapshot key: RegisterHotKey is primary
            let primary_ok = unsafe {
                RegisterHotKey(
                    HWND::default(),
                    HOTKEY_ID,
                    HOT_KEY_MODIFIERS(requested_config.modifiers),
                    requested_config.vk,
                )
            }
            .is_ok();

            if primary_ok {
                requested_config.description.clone()
            } else {
                let _ = ready_tx.send(Err(crate::i18n::tf(
                    "The shortcut '{}' is already registered by Windows or another application.",
                    &[&requested_config.description],
                )));
                return;
            }
        };

        let _ = ready_tx.send(Ok((thread_id, active_desc)));

        // Wait on shutdown and input together: no polling or detached listener on a lost quit message.
        use windows::Win32::UI::WindowsAndMessaging::{
            MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, QS_ALLINPUT,
        };
        let event = windows::Win32::Foundation::HANDLE(listener_stop.0 as *mut _);
        'messages: loop {
            let result = unsafe {
                MsgWaitForMultipleObjectsEx(
                    Some(&[event]),
                    u32::MAX,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            if result == windows::Win32::Foundation::WAIT_OBJECT_0 {
                break;
            }
            if result == windows::Win32::Foundation::WAIT_FAILED {
                crate::diagnostics::record(
                    "hotkey",
                    &windows::core::Error::from_win32().to_string(),
                );
                break;
            }
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                if msg.message == WM_QUIT {
                    break 'messages;
                }
                if msg.message == WM_HOTKEY && !is_overlay_active() && !overlay_input_suspended() {
                    let _ = event_tx.try_send(trigger_instant(msg.time));
                }
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                    windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
                }
            }
        }
    });

    let (thread_id, active_description) = match ready_rx.recv() {
        Ok(Ok(info)) => info,
        Ok(Err(err)) => {
            let _ = join_handle.join();
            return Err(windows::core::Error::new(windows::core::HRESULT(-1), err));
        }
        Err(_) => {
            let _ = join_handle.join();
            return Err(windows::core::Error::new(
                windows::core::HRESULT(-1),
                crate::i18n::t("Shortcut thread exited unexpectedly"),
            ));
        }
    };

    Ok((
        event_rx,
        HotkeyHandle {
            stop,
            thread_id,
            join_handle: Some(join_handle),
            active_description,
        },
    ))
}
