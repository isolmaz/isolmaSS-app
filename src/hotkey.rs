use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    RegisterHotKey, UnregisterHotKey, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    VK_SNAPSHOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HHOOK, KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS, MSG, PM_NOREMOVE, PeekMessageW,
    PostMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER,
};

pub const HOTKEY_ID: i32 = 1001;

/// Configuration for a global hotkey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub modifiers: u32,
    pub vk: u32,
    pub description: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            modifiers: MOD_NOREPEAT.0,
            vk: VK_SNAPSHOT.0 as u32,
            description: "PrintScreen".to_string(),
        }
    }
}

impl HotkeyConfig {
    pub fn is_valid(&self) -> bool {
        Self::from_str(&self.description).is_some_and(|parsed| {
            parsed.vk == self.vk && parsed.modifiers == (self.modifiers | MOD_NOREPEAT.0)
        })
    }

    /// Recommended fallback hotkey when PrintScreen is claimed by Windows Snipping Tool.
    pub fn fallback() -> Self {
        Self {
            modifiers: (MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT).0,
            vk: 0x53, // 'S' key
            description: "Ctrl+Shift+S".to_string(),
        }
    }

    /// Parses a human-readable hotkey string like "Ctrl+Shift+S" or "PrintScreen".
    pub fn from_str(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("printscreen") || trimmed.eq_ignore_ascii_case("prtsc") {
            return Some(Self::default());
        }

        let parts: Vec<&str> = trimmed.split('+').map(|p| p.trim()).collect();
        if parts.is_empty() {
            return None;
        }

        let mut modifiers = MOD_NOREPEAT.0;
        let mut vk = 0u32;

        for part in &parts {
            if vk != 0
                && !matches!(
                    part.to_ascii_lowercase().as_str(),
                    "ctrl" | "control" | "shift" | "alt" | "win" | "windows"
                )
            {
                return None;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" if modifiers & MOD_CONTROL.0 == 0 => modifiers |= MOD_CONTROL.0,
                "shift" if modifiers & MOD_SHIFT.0 == 0 => modifiers |= MOD_SHIFT.0,
                "alt" if modifiers & MOD_ALT.0 == 0 => modifiers |= MOD_ALT.0,
                "win" | "windows" if modifiers & MOD_WIN.0 == 0 => modifiers |= MOD_WIN.0,
                "prtsc" | "printscreen" => vk = VK_SNAPSHOT.0 as u32,
                key if key.len() == 1 => {
                    let ch = key.chars().next().unwrap().to_ascii_uppercase();
                    if ch.is_ascii_alphanumeric() {
                        vk = ch as u32;
                    } else {
                        return None;
                    }
                }
                f_key if f_key.starts_with('f') => {
                    if let Ok(num) = f_key[1..].parse::<u32>() {
                        if (1..=24).contains(&num) {
                            vk = 0x6F + num; // VK_F1 is 0x70
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }

        if vk == 0 || (vk != VK_SNAPSHOT.0 as u32 && modifiers == MOD_NOREPEAT.0) {
            return None;
        }

        Some(Self {
            modifiers,
            vk,
            description: s.to_string(),
        })
    }

    /// Loads the saved hotkey configuration from settings.json.
    pub fn load() -> std::io::Result<Self> {
        crate::settings::Settings::load().map(|settings| settings.hotkey)
    }
}

/// Helper to get the canonical settings.json path in AppData.
pub fn settings_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        let mut p = PathBuf::from(appdata);
        p.push("isolmaSS");
        p.push("settings.json");
        p
    })
}

// -----------------------------------------------------------------------------
// Windows Registry Fix for PrintScreen & Snipping Tool
// -----------------------------------------------------------------------------

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

pub fn is_windows_snipping_tool_disabled() -> bool {
    use windows::Win32::System::Registry::*;
    let mut value = 1u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Control Panel\\Keyboard"),
            windows::core::w!("PrintScreenKeyForSnippingEnabled"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
        .is_ok()
            && value == 0
    }
}

// -----------------------------------------------------------------------------
// Low-Level Keyboard Hook (WH_KEYBOARD_LL) State & Logic
// -----------------------------------------------------------------------------

pub const WM_OVERLAY_KEYDOWN: u32 = WM_USER + 201;
pub const WM_OVERLAY_CHAR: u32 = WM_USER + 202;

struct HookState {
    event_tx: SyncSender<Instant>,
    _config: HotkeyConfig,
}

static HOOK_STATE: Mutex<Option<HookState>> = Mutex::new(None);
static TARGET_VK: AtomicU32 = AtomicU32::new(0);
static TARGET_MODS: AtomicU32 = AtomicU32::new(0);
static SNAPSHOT_HANDLED: AtomicBool = AtomicBool::new(false);

static OVERLAY_SUSPENSIONS: AtomicU32 = AtomicU32::new(0);

pub struct OverlayInputSuspension;
impl OverlayInputSuspension {
    pub fn new() -> Self {
        OVERLAY_SUSPENSIONS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}
impl Drop for OverlayInputSuspension {
    fn drop(&mut self) {
        OVERLAY_SUSPENSIONS.fetch_sub(1, Ordering::SeqCst);
    }
}
pub fn overlay_input_suspended() -> bool {
    OVERLAY_SUSPENSIONS.load(Ordering::SeqCst) != 0
}

static OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);
static OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);
static OVERLAY_TEXT_EDITING: AtomicBool = AtomicBool::new(false);
static OVERLAY_LOCAL_HOOK: AtomicIsize = AtomicIsize::new(0);

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
    OVERLAY_HWND.store(0, Ordering::SeqCst);

    let prev_hook = OVERLAY_LOCAL_HOOK.swap(0, Ordering::SeqCst);
    if prev_hook != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(prev_hook as *mut _));
        }
    }
}

/// Updates the overlay text editing state.
pub fn set_overlay_text_editing(editing: bool) {
    OVERLAY_TEXT_EDITING.store(editing, Ordering::SeqCst);
}

/// Returns whether the overlay is currently active.
pub fn is_overlay_active() -> bool {
    OVERLAY_ACTIVE.load(Ordering::SeqCst)
}

/// Returns whether the overlay is currently in text editing state.
pub fn is_overlay_text_editing() -> bool {
    OVERLAY_TEXT_EDITING.load(Ordering::SeqCst)
}

/// Reads the current physical modifier keys (Ctrl, Shift, Alt, Win) via GetAsyncKeyState.
pub fn get_current_modifiers() -> u32 {
    let mut mods = 0u32;
    unsafe {
        if (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 {
            mods |= MOD_CONTROL.0;
        }
        if (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 {
            mods |= MOD_SHIFT.0;
        }
        if (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0 {
            mods |= MOD_ALT.0;
        }
        if ((GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0)
            || ((GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0)
        {
            mods |= MOD_WIN.0;
        }
    }
    mods
}

/// Core processing logic for low-level keyboard hook callback.
/// When the keystroke matches target VK and target modifiers:
/// - Consumes / swallows the keystroke with LRESULT(1) so Windows Snipping Tool never receives it.
/// - Dispatches the capture trigger event on initial keydown.
/// - Also consumes keyup with LRESULT(1).
///
/// When overlay is open (is_overlay_active):
/// - Routes Esc, text editing input, and shortcuts to the overlay window, consuming them with LRESULT(1).
pub unsafe fn process_keyboard_hook(
    ncode: i32,
    wparam: WPARAM,
    lparam: LPARAM,
    current_mods: u32,
) -> LRESULT {
    if ncode < 0 || lparam.0 == 0 {
        return unsafe { CallNextHookEx(None, ncode, wparam, lparam) };
    }

    let kb = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if !is_overlay_active() && crate::tray::capture_pending() && kb.vkCode == VK_ESCAPE.0 as u32 {
        if wparam.0 as u32 == WM_KEYDOWN {
            unsafe {
                let _ = PostMessageW(
                    crate::tray::window_handle(),
                    crate::tray::WM_CANCEL_CAPTURE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        return LRESULT(1);
    }
    let msg = wparam.0 as u32;
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

    // -------------------------------------------------------------------------
    // Overlay Active Routing (Slice C1)
    // -------------------------------------------------------------------------
    if is_overlay_active() && !overlay_input_suspended() {
        let raw_hwnd = OVERLAY_HWND.load(Ordering::SeqCst);
        let overlay_hwnd = HWND(raw_hwnd as *mut _);
        let text_editing = is_overlay_text_editing();

        // 1. VK_ESCAPE:
        // Esc from idle closes overlay on FIRST press.
        // Esc during text editing cancels text edit only.
        // Esc with shape selected deselects it.
        // Esc with selection committed cancels selection.
        // All Esc presses consume with LRESULT(1).
        if kb.vkCode == VK_ESCAPE.0 as u32 {
            if is_down && !overlay_hwnd.is_invalid() {
                let _ = unsafe {
                    PostMessageW(
                        overlay_hwnd,
                        WM_OVERLAY_KEYDOWN,
                        WPARAM(VK_ESCAPE.0 as usize),
                        LPARAM(0),
                    )
                };
            }
            return LRESULT(1);
        }

        // Let the focused UI thread translate text, including IME and surrogate pairs.
        // Only commands are swallowed by the hook; they share the editor mapping.
        let is_shortcut = crate::overlay::is_editor_shortcut(kb.vkCode, current_mods, text_editing);
        if text_editing && !is_shortcut {
            return unsafe { CallNextHookEx(None, ncode, wparam, lparam) };
        }

        if is_shortcut {
            if is_down && !overlay_hwnd.is_invalid() {
                let _ = unsafe {
                    PostMessageW(
                        overlay_hwnd,
                        WM_OVERLAY_KEYDOWN,
                        WPARAM(kb.vkCode as usize),
                        LPARAM(current_mods as isize),
                    )
                };
            }
            return LRESULT(1);
        }
    }

    // -------------------------------------------------------------------------
    // Global Hotkey Check (PrtScn / Custom Hotkey)
    // -------------------------------------------------------------------------
    let target_vk = TARGET_VK.load(Ordering::Relaxed);
    let target_mods = TARGET_MODS.load(Ordering::Relaxed);

    // If no target VK set, or key does not match target, pass through immediately with zero overhead.
    if target_vk == 0 || kb.vkCode != target_vk {
        return unsafe { CallNextHookEx(None, ncode, wparam, lparam) };
    }

    let mods_match = current_mods == (target_mods & !MOD_NOREPEAT.0);

    if is_down {
        if mods_match {
            let was_down = SNAPSHOT_HANDLED.swap(true, Ordering::SeqCst);
            if !was_down {
                // First keydown: dispatch capture event to listener channel
                let event_tx = HOOK_STATE
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .as_ref()
                    .map(|state| state.event_tx.clone());
                if let Some(tx) = event_tx
                    && !is_overlay_active()
                    && !overlay_input_suspended()
                {
                    let now = Instant::now();
                    let age = if kb.time != 0 {
                        unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
                            .wrapping_sub(kb.time)
                            .min(5_000)
                    } else {
                        0
                    };
                    let _ = tx.try_send(
                        now.checked_sub(std::time::Duration::from_millis(age as u64))
                            .unwrap_or(now),
                    );
                }
            }
            // Return 1 to swallow keystroke: Windows Shell & Snipping Tool will NEVER receive it!
            return LRESULT(1);
        }
    } else if is_up {
        let was_handled = SNAPSHOT_HANDLED.swap(false, Ordering::SeqCst);
        if was_handled || mods_match {
            // Consume keyup as well
            return LRESULT(1);
        }
    }

    unsafe { CallNextHookEx(None, ncode, wparam, lparam) }
}

/// Win32 WH_KEYBOARD_LL hook callback procedure.
pub unsafe extern "system" fn low_level_keyboard_proc(
    ncode: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let mods = get_current_modifiers();
    unsafe { process_keyboard_hook(ncode, wparam, lparam, mods) }
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
                    let _ = ready_tx.send(Err(
                        "Keyboard hook state could not be initialized: state lock poisoned."
                            .to_string(),
                    ));
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
                let _ = ready_tx.send(Err(format!(
                    "Keyboard hook could not be installed: {error}"
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
                let _ = ready_tx.send(Err(format!(
                    "The shortcut '{}' is already registered by Windows or another application.",
                    requested_config.description
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
                    let now = Instant::now();
                    let age = unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
                        .wrapping_sub(msg.time)
                        .min(5_000);
                    let _ = event_tx.try_send(
                        now.checked_sub(std::time::Duration::from_millis(age as u64))
                            .unwrap_or(now),
                    );
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
                "Hotkey thread exited unexpectedly",
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

/// Helper to create a synthetic KBDLLHOOKSTRUCT for testing hook callback logic.
pub fn create_test_kbdllhookstruct(vk: u32) -> KBDLLHOOKSTRUCT {
    KBDLLHOOKSTRUCT {
        vkCode: vk,
        scanCode: 0,
        flags: KBDLLHOOKSTRUCT_FLAGS(0),
        time: 0,
        dwExtraInfo: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overlay_keyboard_routing_in_hook() {
        let kb_esc = create_test_kbdllhookstruct(VK_ESCAPE.0 as u32);
        let lparam_esc = LPARAM(&kb_esc as *const _ as isize);
        let wparam_down = WPARAM(WM_KEYDOWN as usize);

        // 1. Overlay NOT active: Esc passes through
        OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
        let res1 = unsafe { process_keyboard_hook(0, wparam_down, lparam_esc, 0) };
        assert_ne!(
            res1,
            LRESULT(1),
            "Esc should pass through when overlay inactive"
        );

        // 2. Overlay active: Esc is consumed with LRESULT(1)
        OVERLAY_ACTIVE.store(true, Ordering::SeqCst);
        let res2 = unsafe { process_keyboard_hook(0, wparam_down, lparam_esc, 0) };
        assert_eq!(
            res2,
            LRESULT(1),
            "Esc should be consumed with LRESULT(1) when overlay active"
        );

        // 3. Overlay active, text editing mode: keystroke is consumed
        set_overlay_text_editing(true);
        let kb_a = create_test_kbdllhookstruct(0x41); // 'A' key
        let lparam_a = LPARAM(&kb_a as *const _ as isize);
        let res3 = unsafe { process_keyboard_hook(0, wparam_down, lparam_a, 0) };
        assert_ne!(
            res3,
            LRESULT(1),
            "Characters must reach the UI thread for native Unicode and IME translation"
        );

        // 4. Overlay active, not text editing: shortcuts are consumed
        set_overlay_text_editing(false);
        let kb_r = create_test_kbdllhookstruct(0x52); // 'R' key (Rectangle tool)
        let lparam_r = LPARAM(&kb_r as *const _ as isize);
        let res4 = unsafe { process_keyboard_hook(0, wparam_down, lparam_r, 0) };
        assert_eq!(
            res4,
            LRESULT(1),
            "Shortcut 'R' should be consumed when overlay active"
        );

        let kb_c = create_test_kbdllhookstruct(0x43); // 'C' key
        let lparam_c = LPARAM(&kb_c as *const _ as isize);
        let res5 = unsafe { process_keyboard_hook(0, wparam_down, lparam_c, MOD_CONTROL.0) };
        assert_eq!(
            res5,
            LRESULT(1),
            "Shortcut Ctrl+C should be consumed when overlay active"
        );

        // Cleanup
        OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
        set_overlay_text_editing(false);
    }
    #[test]
    fn recorder_shortcuts_require_supported_key_and_modifier() {
        for description in ["PrintScreen", "Ctrl+F24", "Alt+7", "Win+Shift+PrintScreen"] {
            assert!(
                HotkeyConfig::from_str(description).is_some(),
                "{description}"
            );
        }
        for description in [
            "A",
            "F2",
            "Ctrl+F25",
            "Ctrl+Ctrl+S",
            "Ctrl+Mouse4",
            "Ctrl+VolumeUp",
            "Ctrl+A+B",
        ] {
            assert!(
                HotkeyConfig::from_str(description).is_none(),
                "{description}"
            );
        }
    }
}
