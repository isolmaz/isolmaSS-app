use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VIRTUAL_KEY,
    VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SNAPSHOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS, PostMessageW, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER,
};

mod system;
pub use system::{
    HotkeyHandle, disable_windows_snipping_tool_hotkey, register_overlay, start_hotkey_listener,
    unregister_overlay,
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

        let mut modifiers = MOD_NOREPEAT.0;
        let mut vk = 0u32;

        for part in trimmed.split('+').map(|p| p.trim()) {
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
/// Posted to the Settings window while it records a shortcut when the running
/// shortcut is pressed: `wparam` is the virtual key, `lparam` the `MOD_*` bits.
pub const WM_RECORD_HOTKEY: u32 = WM_USER + 203;

struct HookState {
    event_tx: SyncSender<Instant>,
    _config: HotkeyConfig,
}

static HOOK_STATE: Mutex<Option<HookState>> = Mutex::new(None);
static TARGET_VK: AtomicU32 = AtomicU32::new(0);
static TARGET_MODS: AtomicU32 = AtomicU32::new(0);
static SNAPSHOT_HANDLED: AtomicBool = AtomicBool::new(false);
static RECORDING_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Names the window that is recording a new shortcut, or `None` when it stops.
/// The running shortcut is registered with the system (and swallowed by the
/// hook), so it never arrives as a key message; while a window records, the
/// hook posts it [`WM_RECORD_HOTKEY`] instead of triggering a capture. The key
/// stays swallowed, so the Snipping Tool does not open mid-recording.
pub fn set_recording_window(window: Option<HWND>) {
    RECORDING_WINDOW.store(window.map_or(0, |hwnd| hwnd.0 as isize), Ordering::SeqCst);
}

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
static OVERLAY_IME_COMPOSING: AtomicBool = AtomicBool::new(false);
static OVERLAY_LOCAL_HOOK: AtomicIsize = AtomicIsize::new(0);

/// Updates the overlay text editing state.
pub fn set_overlay_text_editing(editing: bool) {
    OVERLAY_TEXT_EDITING.store(editing, Ordering::SeqCst);
}

/// Marks an IME composition in the overlay. While it runs the hook leaves every
/// key alone, so Enter, Backspace, arrows and Esc reach the IME instead of the editor.
pub fn set_overlay_ime_composing(composing: bool) {
    OVERLAY_IME_COMPOSING.store(composing, Ordering::SeqCst);
}

/// Returns whether the overlay is currently active.
pub fn is_overlay_active() -> bool {
    OVERLAY_ACTIVE.load(Ordering::SeqCst)
}

/// Returns whether the overlay is currently in text editing state.
pub fn is_overlay_text_editing() -> bool {
    OVERLAY_TEXT_EDITING.load(Ordering::SeqCst)
}

/// The `MOD_*` bits of the modifier keys that `is_down` reports as held.
fn modifiers_from(is_down: impl Fn(VIRTUAL_KEY) -> bool) -> u32 {
    let mut mods = 0u32;
    if is_down(VK_CONTROL) {
        mods |= MOD_CONTROL.0;
    }
    if is_down(VK_SHIFT) {
        mods |= MOD_SHIFT.0;
    }
    if is_down(VK_MENU) {
        mods |= MOD_ALT.0;
    }
    if is_down(VK_LWIN) || is_down(VK_RWIN) {
        mods |= MOD_WIN.0;
    }
    mods
}

/// Reads the current physical modifier keys (Ctrl, Shift, Alt, Win) via GetAsyncKeyState.
pub fn get_current_modifiers() -> u32 {
    modifiers_from(|key| (unsafe { GetAsyncKeyState(key.0 as i32) } as u16 & 0x8000) != 0)
}

/// When a trigger key was pressed, from the `GetTickCount` time of its message.
/// Delivering it to the capture thread takes a moment, so captures are timed from the
/// key press itself; a tick further back than five seconds (a stale or wrapped one)
/// counts as five seconds ago.
fn trigger_instant(tick: u32) -> Instant {
    let now = Instant::now();
    let age = unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
        .wrapping_sub(tick)
        .min(5_000);
    now.checked_sub(std::time::Duration::from_millis(u64::from(age)))
        .unwrap_or(now)
}

/// Core processing logic for low-level keyboard hook callback.
/// When the keystroke matches target VK and target modifiers:
/// - Consumes / swallows the keystroke with LRESULT(1) so Windows Snipping Tool never receives it.
/// - Dispatches the capture trigger event on initial keydown.
/// - Also consumes keyup with LRESULT(1).
///
/// When overlay is open (is_overlay_active):
/// - Routes Esc, text editing input, and shortcuts to the overlay window, consuming them with LRESULT(1).
/// - During an IME composition (`set_overlay_ime_composing`) none of that applies; the keys go to the IME.
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
    // Overlay Active Routing
    // -------------------------------------------------------------------------
    if is_overlay_active()
        && !overlay_input_suspended()
        && !OVERLAY_IME_COMPOSING.load(Ordering::SeqCst)
    {
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
            let recording = RECORDING_WINDOW.load(Ordering::SeqCst);
            if !was_down && recording != 0 {
                let _ = unsafe {
                    PostMessageW(
                        HWND(recording as *mut _),
                        WM_RECORD_HOTKEY,
                        WPARAM(kb.vkCode as usize),
                        LPARAM(current_mods as isize),
                    )
                };
            } else if !was_down {
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
                    let pressed = if kb.time != 0 {
                        trigger_instant(kb.time)
                    } else {
                        Instant::now()
                    };
                    let _ = tx.try_send(pressed);
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
    use crate::settings::test_env::Sandbox;
    use std::sync::MutexGuard;
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, WINDOW_STYLE,
    };

    /// The hook reads process-wide state; tests that drive it run one at a time.
    static HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_overlay_keyboard_routing_in_hook() {
        let _serial = HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
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

        // 5. IME composition: editor keys must reach the IME, not be swallowed
        set_overlay_text_editing(true);
        set_overlay_ime_composing(true);
        // Esc, Enter, Backspace, Left, Delete, End
        for vk in [VK_ESCAPE.0 as u32, 0x0D, 0x08, 0x25, 0x2E, 0x23] {
            let kb = create_test_kbdllhookstruct(vk);
            let lparam = LPARAM(&kb as *const _ as isize);
            let res = unsafe { process_keyboard_hook(0, wparam_down, lparam, 0) };
            assert_ne!(res, LRESULT(1), "vk {vk:#x} must pass to the IME");
        }
        let kb_ctrl_z = create_test_kbdllhookstruct(0x5A);
        let lparam_ctrl_z = LPARAM(&kb_ctrl_z as *const _ as isize);
        let res_ctrl_z =
            unsafe { process_keyboard_hook(0, wparam_down, lparam_ctrl_z, MOD_CONTROL.0) };
        assert_ne!(res_ctrl_z, LRESULT(1), "Ctrl+Z must pass to the IME");

        // 6. Composition over: the same keys are swallowed for the editor again
        set_overlay_ime_composing(false);
        let kb_enter = create_test_kbdllhookstruct(0x0D);
        let lparam_enter = LPARAM(&kb_enter as *const _ as isize);
        let res_enter = unsafe { process_keyboard_hook(0, wparam_down, lparam_enter, 0) };
        assert_eq!(
            res_enter,
            LRESULT(1),
            "Enter is an editor key outside composition"
        );

        // Leaving the overlay resets a composition that never reported its end
        set_overlay_ime_composing(true);
        unregister_overlay();
        assert!(!OVERLAY_IME_COMPOSING.load(Ordering::SeqCst));

        // Cleanup
        OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
        set_overlay_text_editing(false);
    }
    #[test]
    fn running_shortcut_is_handed_to_the_recording_window() {
        let _serial = HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let window = unsafe {
            CreateWindowExW(
                Default::default(),
                windows::core::w!("STATIC"),
                windows::core::w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                HINSTANCE::default(),
                None,
            )
        }
        .expect("message-only window");
        // Alt in the combination keeps the editor-shortcut routing out of the way.
        const F9: u32 = 0x78;
        let mods = MOD_CONTROL.0 | MOD_ALT.0;
        TARGET_VK.store(F9, Ordering::SeqCst);
        TARGET_MODS.store(mods | MOD_NOREPEAT.0, Ordering::SeqCst);
        SNAPSHOT_HANDLED.store(false, Ordering::SeqCst);
        let kb = create_test_kbdllhookstruct(F9);
        let lparam = LPARAM(&kb as *const _ as isize);
        let take_message = || {
            let mut msg = MSG::default();
            let found = unsafe {
                PeekMessageW(
                    &mut msg,
                    window,
                    WM_RECORD_HOTKEY,
                    WM_RECORD_HOTKEY,
                    PM_REMOVE,
                )
            }
            .as_bool();
            found.then_some(msg)
        };

        set_recording_window(Some(window));
        let pressed =
            unsafe { process_keyboard_hook(0, WPARAM(WM_KEYDOWN as usize), lparam, mods) };
        let handed_over = take_message();
        let repeated =
            unsafe { process_keyboard_hook(0, WPARAM(WM_KEYDOWN as usize), lparam, mods) };
        let repeat_handed_over = take_message();
        let released = unsafe { process_keyboard_hook(0, WPARAM(WM_KEYUP as usize), lparam, mods) };
        set_recording_window(None);

        TARGET_VK.store(0, Ordering::SeqCst);
        TARGET_MODS.store(0, Ordering::SeqCst);
        SNAPSHOT_HANDLED.store(false, Ordering::SeqCst);
        unsafe {
            let _ = DestroyWindow(window);
        }

        for (name, result) in [
            ("key down", pressed),
            ("key repeat", repeated),
            ("key up", released),
        ] {
            assert_eq!(result, LRESULT(1), "{name} stays swallowed");
        }
        let message = handed_over.expect("the running shortcut is posted to the window");
        assert_eq!(message.wParam.0, F9 as usize);
        assert_eq!(message.lParam.0 as u32, mods);
        assert!(repeat_handed_over.is_none(), "a held key is posted once");
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

    /// Restores the hook's idle state around a test and keeps tests that drive it apart.
    struct HookTest {
        _serial: MutexGuard<'static, ()>,
    }

    impl HookTest {
        fn start() -> Self {
            let serial = HOOK_TEST_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let test = Self { _serial: serial };
            test.reset();
            test
        }

        fn reset(&self) {
            OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
            OVERLAY_TEXT_EDITING.store(false, Ordering::SeqCst);
            OVERLAY_IME_COMPOSING.store(false, Ordering::SeqCst);
            OVERLAY_HWND.store(0, Ordering::SeqCst);
            TARGET_VK.store(0, Ordering::SeqCst);
            TARGET_MODS.store(0, Ordering::SeqCst);
            SNAPSHOT_HANDLED.store(false, Ordering::SeqCst);
            RECORDING_WINDOW.store(0, Ordering::SeqCst);
            *HOOK_STATE
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = None;
            crate::tray::capture_finished();
        }

        /// The hook state of a running listener: its capture events go to the receiver.
        fn listen(&self) -> std::sync::mpsc::Receiver<Instant> {
            let (events, received) = sync_channel(1);
            *HOOK_STATE
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(HookState {
                event_tx: events,
                _config: HotkeyConfig::fallback(),
            });
            received
        }

        /// What the hook answers for one key message with the given modifiers held.
        fn send(&self, vk: u32, message: u32, mods: u32) -> LRESULT {
            let kb = create_test_kbdllhookstruct(vk);
            unsafe {
                process_keyboard_hook(
                    0,
                    WPARAM(message as usize),
                    LPARAM(&kb as *const _ as isize),
                    mods,
                )
            }
        }
    }

    impl Drop for HookTest {
        fn drop(&mut self) {
            self.reset();
        }
    }

    const ESCAPE: u32 = 0x1B;
    const F9: u32 = 0x78;

    /// A message-only window that receives what the hook posts.
    struct TestWindow(HWND);

    impl TestWindow {
        fn new() -> Self {
            Self(
                unsafe {
                    CreateWindowExW(
                        Default::default(),
                        windows::core::w!("STATIC"),
                        windows::core::w!(""),
                        WINDOW_STYLE(0),
                        0,
                        0,
                        0,
                        0,
                        HWND_MESSAGE,
                        None,
                        HINSTANCE::default(),
                        None,
                    )
                }
                .expect("message-only window"),
            )
        }

        /// The next queued `message` for this window, taken off the queue.
        fn take(&self, message: u32) -> Option<MSG> {
            take_message(self.0, message)
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    /// The next queued `message` of this thread (for `window` or, with a null window, posted
    /// without one), taken off the queue.
    fn take_message(window: HWND, message: u32) -> Option<MSG> {
        let mut msg = MSG::default();
        unsafe { PeekMessageW(&mut msg, window, message, message, PM_REMOVE) }
            .as_bool()
            .then_some(msg)
    }

    #[test]
    fn calls_that_are_not_key_events_pass_through() {
        let hook = HookTest::start();
        TARGET_VK.store(F9, Ordering::SeqCst);
        TARGET_MODS.store(MOD_NOREPEAT.0, Ordering::SeqCst);
        let kb = create_test_kbdllhookstruct(F9);
        let lparam = LPARAM(&kb as *const _ as isize);
        let down = WPARAM(WM_KEYDOWN as usize);
        // A negative code means "do not process"; a null pointer has no key to read.
        assert_ne!(
            unsafe { process_keyboard_hook(-1, down, lparam, 0) },
            LRESULT(1)
        );
        assert_ne!(
            unsafe { process_keyboard_hook(0, down, LPARAM(0), 0) },
            LRESULT(1)
        );
        assert_ne!(
            unsafe { low_level_keyboard_proc(-1, down, lparam) },
            LRESULT(1)
        );
        assert_ne!(
            unsafe { low_level_keyboard_proc(0, down, LPARAM(0)) },
            LRESULT(1)
        );
        // Other messages (characters, timers) are neither press nor release.
        assert_ne!(hook.send(F9, 0x0102, 0), LRESULT(1));
        assert!(!SNAPSHOT_HANDLED.load(Ordering::SeqCst));
    }

    #[test]
    fn escape_cancels_a_pending_capture_and_stays_swallowed() {
        let hook = HookTest::start();
        while take_message(HWND::default(), crate::tray::WM_CANCEL_CAPTURE).is_some() {}
        crate::tray::queue_capture(Instant::now());
        assert!(
            crate::tray::capture_pending(),
            "a capture waits for its overlay"
        );

        assert_eq!(hook.send(ESCAPE, WM_KEYDOWN, 0), LRESULT(1));
        assert!(
            take_message(HWND::default(), crate::tray::WM_CANCEL_CAPTURE).is_some(),
            "the tray is asked to cancel the capture"
        );
        assert_eq!(
            hook.send(ESCAPE, WM_KEYUP, 0),
            LRESULT(1),
            "the release is swallowed too"
        );
        assert!(
            take_message(HWND::default(), crate::tray::WM_CANCEL_CAPTURE).is_none(),
            "only the press cancels"
        );
        assert_ne!(
            hook.send(0x41, WM_KEYDOWN, 0),
            LRESULT(1),
            "other keys are not affected"
        );

        crate::tray::capture_finished();
        assert_ne!(
            hook.send(ESCAPE, WM_KEYDOWN, 0),
            LRESULT(1),
            "no capture, no claim on Esc"
        );
    }

    #[test]
    fn overlay_commands_are_posted_to_the_overlay_window() {
        let hook = HookTest::start();
        let window = TestWindow::new();
        OVERLAY_HWND.store(window.0.0 as isize, Ordering::SeqCst);
        OVERLAY_ACTIVE.store(true, Ordering::SeqCst);

        assert_eq!(hook.send(ESCAPE, WM_KEYDOWN, 0), LRESULT(1));
        let escape = window
            .take(WM_OVERLAY_KEYDOWN)
            .expect("Esc reaches the editor");
        assert_eq!((escape.wParam.0, escape.lParam.0), (ESCAPE as usize, 0));
        assert_eq!(hook.send(ESCAPE, WM_KEYUP, 0), LRESULT(1));
        assert!(
            window.take(WM_OVERLAY_KEYDOWN).is_none(),
            "a release is not a command"
        );

        // Ctrl+Z (undo) carries the modifiers, the plain key a tool shortcut.
        assert_eq!(hook.send(0x5A, WM_KEYDOWN, MOD_CONTROL.0), LRESULT(1));
        let undo = window
            .take(WM_OVERLAY_KEYDOWN)
            .expect("Ctrl+Z reaches the editor");
        assert_eq!((undo.wParam.0, undo.lParam.0 as u32), (0x5A, MOD_CONTROL.0));
        assert_eq!(hook.send(0x52, WM_KEYDOWN, 0), LRESULT(1));
        assert_eq!(
            window
                .take(WM_OVERLAY_KEYDOWN)
                .expect("R reaches the editor")
                .wParam
                .0,
            0x52
        );
        assert_eq!(hook.send(0x52, WM_KEYUP, 0), LRESULT(1));
        assert!(window.take(WM_OVERLAY_KEYDOWN).is_none());

        // While text is typed only commands are taken; characters go to the focused window.
        set_overlay_text_editing(true);
        assert!(is_overlay_text_editing());
        assert_ne!(hook.send(0x52, WM_KEYDOWN, 0), LRESULT(1));
        assert!(window.take(WM_OVERLAY_KEYDOWN).is_none());
        assert_eq!(
            hook.send(0x08, WM_KEYDOWN, 0),
            LRESULT(1),
            "Backspace edits"
        );
        assert!(window.take(WM_OVERLAY_KEYDOWN).is_some());
    }

    #[test]
    fn suspended_overlay_input_lets_keys_through() {
        let hook = HookTest::start();
        let window = TestWindow::new();
        OVERLAY_HWND.store(window.0.0 as isize, Ordering::SeqCst);
        OVERLAY_ACTIVE.store(true, Ordering::SeqCst);
        assert!(is_overlay_active());

        assert!(!overlay_input_suspended());
        let outer = OverlayInputSuspension::new();
        let inner = OverlayInputSuspension::new();
        assert!(overlay_input_suspended());
        assert_ne!(
            hook.send(ESCAPE, WM_KEYDOWN, 0),
            LRESULT(1),
            "a dialog owns the keyboard"
        );
        assert_ne!(hook.send(0x52, WM_KEYDOWN, 0), LRESULT(1));
        assert!(window.take(WM_OVERLAY_KEYDOWN).is_none());
        drop(inner);
        assert!(
            overlay_input_suspended(),
            "the outer suspension still holds"
        );
        drop(outer);
        assert!(!overlay_input_suspended());
        assert_eq!(hook.send(ESCAPE, WM_KEYDOWN, 0), LRESULT(1));
    }

    #[test]
    fn the_running_shortcut_starts_one_capture_per_key_press() {
        let hook = HookTest::start();
        let received = hook.listen();
        // Alt in the combination keeps the editor-shortcut routing out of the way.
        let mods = MOD_CONTROL.0 | MOD_ALT.0;
        TARGET_VK.store(F9, Ordering::SeqCst);
        TARGET_MODS.store(mods | MOD_NOREPEAT.0, Ordering::SeqCst);

        for (down, up) in [(WM_KEYDOWN, WM_KEYUP), (WM_SYSKEYDOWN, WM_SYSKEYUP)] {
            assert_eq!(
                hook.send(F9, down, mods),
                LRESULT(1),
                "the shortcut never reaches Windows"
            );
            let pressed = received
                .try_recv()
                .expect("the first press starts a capture");
            assert!(pressed.elapsed() < Duration::from_secs(5));
            assert_eq!(
                hook.send(F9, down, mods),
                LRESULT(1),
                "auto-repeat is swallowed"
            );
            assert!(
                received.try_recv().is_err(),
                "a held key starts only one capture"
            );
            assert_eq!(
                hook.send(F9, up, mods),
                LRESULT(1),
                "the release is swallowed"
            );
            assert!(received.try_recv().is_err());
        }
        // Released, so the next press is a new one.
        assert_eq!(hook.send(F9, WM_KEYDOWN, mods), LRESULT(1));
        assert!(received.try_recv().is_ok());
    }

    #[test]
    fn the_running_shortcut_needs_exactly_its_modifiers() {
        let hook = HookTest::start();
        let received = hook.listen();
        let mods = MOD_CONTROL.0 | MOD_ALT.0;
        TARGET_VK.store(F9, Ordering::SeqCst);
        TARGET_MODS.store(mods | MOD_NOREPEAT.0, Ordering::SeqCst);

        for wrong in [0, MOD_CONTROL.0, mods | MOD_SHIFT.0] {
            assert_ne!(
                hook.send(F9, WM_KEYDOWN, wrong),
                LRESULT(1),
                "modifiers {wrong:#x}"
            );
            assert_ne!(
                hook.send(F9, WM_KEYUP, wrong),
                LRESULT(1),
                "modifiers {wrong:#x}"
            );
        }
        assert!(
            received.try_recv().is_err(),
            "no capture without the full shortcut"
        );
        assert!(!SNAPSHOT_HANDLED.load(Ordering::SeqCst));
        assert_ne!(
            hook.send(0x41, WM_KEYDOWN, mods),
            LRESULT(1),
            "another key is not the shortcut"
        );
        // A release whose press was never seen (modifiers matching) is still swallowed.
        assert_eq!(hook.send(F9, WM_KEYUP, mods), LRESULT(1));
    }

    #[test]
    fn a_capture_is_not_started_while_an_overlay_or_dialog_is_open() {
        let hook = HookTest::start();
        let received = hook.listen();
        let mods = MOD_CONTROL.0 | MOD_ALT.0;
        TARGET_VK.store(F9, Ordering::SeqCst);
        TARGET_MODS.store(mods | MOD_NOREPEAT.0, Ordering::SeqCst);

        OVERLAY_ACTIVE.store(true, Ordering::SeqCst);
        assert_eq!(
            hook.send(F9, WM_KEYDOWN, mods),
            LRESULT(1),
            "still swallowed"
        );
        assert!(received.try_recv().is_err(), "the overlay is already open");
        assert_eq!(hook.send(F9, WM_KEYUP, mods), LRESULT(1));
        OVERLAY_ACTIVE.store(false, Ordering::SeqCst);

        let suspension = OverlayInputSuspension::new();
        assert_eq!(hook.send(F9, WM_KEYDOWN, mods), LRESULT(1));
        assert!(received.try_recv().is_err(), "a dialog is open");
        assert_eq!(hook.send(F9, WM_KEYUP, mods), LRESULT(1));
        drop(suspension);

        // Without a listener (no state) the key is still claimed but nothing is sent.
        *HOOK_STATE
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
        assert_eq!(hook.send(F9, WM_KEYDOWN, mods), LRESULT(1));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_capture_is_timed_from_the_key_press() {
        let hook = HookTest::start();
        let received = hook.listen();
        let mods = MOD_CONTROL.0 | MOD_ALT.0;
        TARGET_VK.store(F9, Ordering::SeqCst);
        TARGET_MODS.store(mods | MOD_NOREPEAT.0, Ordering::SeqCst);
        let mut kb = create_test_kbdllhookstruct(F9);
        kb.time = unsafe { GetTickCount() }.wrapping_sub(300);
        let result = unsafe {
            process_keyboard_hook(
                0,
                WPARAM(WM_KEYDOWN as usize),
                LPARAM(&kb as *const _ as isize),
                mods,
            )
        };
        assert_eq!(result, LRESULT(1));
        let pressed = received.try_recv().expect("a capture was started");
        assert!(
            pressed.elapsed() >= Duration::from_millis(300),
            "the press happened {:?} ago",
            pressed.elapsed()
        );
    }

    #[test]
    fn trigger_times_look_back_at_most_five_seconds() {
        let tick = unsafe { GetTickCount() };
        let recent = trigger_instant(tick.wrapping_sub(250));
        assert!(recent.elapsed() >= Duration::from_millis(250));
        assert!(recent.elapsed() < Duration::from_secs(5));
        let stale = trigger_instant(tick.wrapping_sub(60_000));
        assert!(stale.elapsed() >= Duration::from_secs(5));
        assert!(stale.elapsed() < Duration::from_secs(10));
        let now = trigger_instant(unsafe { GetTickCount() });
        assert!(now.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn modifier_bits_follow_the_held_keys() {
        let held = |keys: &'static [VIRTUAL_KEY]| move |key: VIRTUAL_KEY| keys.contains(&key);
        assert_eq!(modifiers_from(held(&[])), 0);
        assert_eq!(modifiers_from(held(&[VK_CONTROL])), MOD_CONTROL.0);
        assert_eq!(modifiers_from(held(&[VK_SHIFT])), MOD_SHIFT.0);
        assert_eq!(modifiers_from(held(&[VK_MENU])), MOD_ALT.0);
        assert_eq!(modifiers_from(held(&[VK_LWIN])), MOD_WIN.0);
        assert_eq!(modifiers_from(held(&[VK_RWIN])), MOD_WIN.0);
        assert_eq!(modifiers_from(held(&[VK_LWIN, VK_RWIN])), MOD_WIN.0);
        assert_eq!(
            modifiers_from(held(&[VK_CONTROL, VK_SHIFT, VK_MENU, VK_RWIN])),
            MOD_CONTROL.0 | MOD_SHIFT.0 | MOD_ALT.0 | MOD_WIN.0
        );
        assert_eq!(
            modifiers_from(held(&[VIRTUAL_KEY(0x41)])),
            0,
            "letters are not modifiers"
        );
        let live = get_current_modifiers();
        assert_eq!(
            live & !(MOD_CONTROL.0 | MOD_SHIFT.0 | MOD_ALT.0 | MOD_WIN.0),
            0
        );
    }

    #[test]
    fn printscreen_and_modifier_combinations_parse_to_their_key_codes() {
        let default = HotkeyConfig::from_str("PrintScreen").unwrap();
        assert_eq!(default, HotkeyConfig::default());
        assert_eq!(HotkeyConfig::from_str("  prtsc ").unwrap(), default);
        assert_eq!(HotkeyConfig::from_str("PRTSC").unwrap(), default);

        let all = HotkeyConfig::from_str("Alt+Shift+Win+Ctrl+A").unwrap();
        assert_eq!(all.vk, 0x41);
        assert_eq!(
            all.modifiers,
            MOD_ALT.0 | MOD_SHIFT.0 | MOD_WIN.0 | MOD_CONTROL.0 | MOD_NOREPEAT.0
        );
        assert_eq!(all.description, "Alt+Shift+Win+Ctrl+A");

        let spaced = HotkeyConfig::from_str(" control + windows + 7 ").unwrap();
        assert_eq!(spaced.vk, 0x37);
        assert_eq!(spaced.modifiers, MOD_CONTROL.0 | MOD_WIN.0 | MOD_NOREPEAT.0);
        assert_eq!(
            spaced.description, " control + windows + 7 ",
            "kept as typed"
        );

        let function = HotkeyConfig::from_str("ctrl+f1").unwrap();
        assert_eq!(
            (function.vk, function.modifiers),
            (0x70, MOD_CONTROL.0 | MOD_NOREPEAT.0)
        );
        assert_eq!(HotkeyConfig::from_str("Ctrl+F24").unwrap().vk, 0x87);
        assert_eq!(
            HotkeyConfig::from_str("Ctrl+F").unwrap().vk,
            0x46,
            "the letter F"
        );

        let snapshot = HotkeyConfig::from_str("Ctrl+PrintScreen").unwrap();
        assert_eq!(snapshot.vk, VK_SNAPSHOT.0 as u32);
        assert_eq!(snapshot.modifiers, MOD_CONTROL.0 | MOD_NOREPEAT.0);
        let after = HotkeyConfig::from_str("PrintScreen+Shift").unwrap();
        assert_eq!(
            after.modifiers,
            MOD_SHIFT.0 | MOD_NOREPEAT.0,
            "modifiers may follow the key"
        );
    }

    #[test]
    fn malformed_shortcuts_are_refused() {
        for text in [
            "",
            "Ctrl",
            "Ctrl+",
            "+A",
            "Ctrl+Shift",
            "Ctrl+Ctrl+A",
            "Shift+Shift+A",
            "Alt+Alt+A",
            "Win+Windows+A",
            "A",
            "Ctrl+AB",
            "Ctrl+-",
            "Ctrl+F0",
            "Ctrl+F25",
            "Ctrl+Fx",
            "Ctrl+Delete",
            "Ctrl+A+B",
            "Ctrl+A+Shift+B",
            "F5",
        ] {
            assert_eq!(HotkeyConfig::from_str(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_stored_shortcut_must_match_its_description() {
        assert!(HotkeyConfig::default().is_valid());
        let fallback = HotkeyConfig::fallback();
        assert_eq!(
            fallback,
            HotkeyConfig::from_str("Ctrl+Shift+S").unwrap(),
            "the fallback is the shortcut it describes"
        );
        assert!(fallback.is_valid());

        let without_repeat_flag = HotkeyConfig {
            modifiers: MOD_CONTROL.0 | MOD_SHIFT.0,
            ..fallback.clone()
        };
        assert!(
            without_repeat_flag.is_valid(),
            "the no-repeat flag is implied"
        );
        for broken in [
            HotkeyConfig {
                vk: 0x41,
                ..fallback.clone()
            },
            HotkeyConfig {
                modifiers: MOD_CONTROL.0 | MOD_NOREPEAT.0,
                ..fallback.clone()
            },
            HotkeyConfig {
                description: "Ctrl+Shift".into(),
                ..fallback.clone()
            },
            HotkeyConfig {
                description: "Ctrl+Shift+T".into(),
                ..fallback.clone()
            },
        ] {
            assert!(!broken.is_valid(), "{broken:?}");
        }
    }

    #[test]
    fn the_shortcut_is_read_from_the_stored_settings() {
        let sandbox = Sandbox::new("hotkey_load");
        assert_eq!(
            HotkeyConfig::load().unwrap(),
            HotkeyConfig::default(),
            "a new installation uses PrintScreen"
        );
        let hotkey = HotkeyConfig::from_str("Ctrl+Alt+F9").unwrap();
        crate::settings::Settings {
            hotkey: hotkey.clone(),
            ..crate::settings::Settings::default()
        }
        .save()
        .expect("save");
        assert_eq!(HotkeyConfig::load().unwrap(), hotkey);

        assert_eq!(settings_path(), Some(sandbox.settings_file()));
        sandbox.remove_appdata();
        assert_eq!(settings_path(), None);
    }

    /// The Snipping Tool value as `reg.exe` reports it, read separately from the code under test.
    fn snipping_value_in_registry() -> Option<String> {
        let output = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Control Panel\Keyboard",
                "/v",
                "PrintScreenKeyForSnippingEnabled",
            ])
            .output()
            .expect("run reg.exe");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let line = text
            .lines()
            .find(|line| line.contains("PrintScreenKeyForSnippingEnabled"))?;
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.get(1) == Some(&"REG_DWORD")).then(|| fields[2].to_string())
    }

    #[test]
    fn the_snipping_tool_state_is_read_from_the_user_registry() {
        assert_eq!(
            is_windows_snipping_tool_disabled(),
            snipping_value_in_registry().as_deref() == Some("0x0")
        );
    }
}
