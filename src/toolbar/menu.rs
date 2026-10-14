//! The modal popup-menu session of the toolbar command menu. `TrackPopupMenuEx` runs its
//! own message loop and returns only once the user picks a command or dismisses the menu,
//! so it needs a live desktop and a person; what the menu contains and how a command id
//! maps back to a toolbar item is built and tested in `toolbar.rs`.
use super::{ToolbarButton, ToolbarItem, build_command_menu};
use windows::Win32::Foundation::{HWND, POINT};

/// Presents the toolbar's commands as a native popup menu for keyboard and screen-reader
/// access (F10 / Apps key). Returns the chosen command; the overlay is never mutated.
/// The menu opens at `anchor` (screen pixels) or, without one, at the cursor.
pub fn show_command_menu(
    owner: HWND,
    buttons: &[ToolbarButton],
    anchor: Option<(i32, i32)>,
) -> windows::core::Result<Option<ToolbarItem>> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    };
    let Some(menu) = build_command_menu(buttons)? else {
        return Ok(None);
    };
    // The menu runs a modal loop; suspend overlay hotkeys for its lifetime.
    let _suspension = crate::hotkey::OverlayInputSuspension::new();
    let point = match anchor {
        Some((x, y)) => POINT { x, y },
        None => {
            let mut cursor = POINT::default();
            if unsafe { GetCursorPos(&mut cursor) }.is_err() {
                return Ok(None);
            }
            cursor
        }
    };
    let command = unsafe {
        TrackPopupMenuEx(
            menu.menu.0,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            point.x,
            point.y,
            owner,
            None,
        )
    };
    Ok(menu.item(command.0))
}
