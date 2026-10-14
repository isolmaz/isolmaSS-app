//! Keyboard handling of the editor: `handle_key_down` and `handle_char`.
use super::test_support::*;
use super::*;
use windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT;

const CTRL: isize = MOD_CONTROL.0 as isize;
const SHIFT: isize = MOD_SHIFT.0 as isize;
const BACK: usize = 8;
const RETURN: usize = 13;
const ESCAPE: usize = 27;
const END: usize = 35;
const HOME: usize = 36;
const LEFT: usize = 37;
const UP: usize = 38;
const RIGHT: usize = 39;
const DOWN: usize = 40;
const DELETE: usize = 46;
const COMMA: usize = 0xbc;
const F10: usize = 0x79;
const APPS: usize = 0x5d;

fn key(letter: char) -> usize {
    letter.to_ascii_uppercase() as usize
}

fn press(state: &mut OverlayState, window: &TestWindow, vk: usize, mods: isize) {
    assert_eq!(state.handle_key_down(window.0, vk, mods).0, 0);
}

fn editing_state(text: &str) -> OverlayState {
    let mut state = state_with_rectangle();
    state.set_text_edit(Some(TextEditState::new(
        (200, 200),
        text.to_string(),
        [0, 0, 0, 255],
        22,
        None,
    )));
    state
}

fn edit(state: &OverlayState) -> &TextEditState {
    state.text_edit().expect("a text box is open")
}

fn rectangle_of(object: &AnnotationObject) -> Rect {
    match &object.kind {
        AnnotationKind::Rectangle { rect, .. } => *rect,
        other => panic!("expected a rectangle, found {other:?}"),
    }
}

fn wheel_state(digits: &str) -> OverlayState {
    let mut state = state_with_rectangle();
    state.thickness_input = Some(digits.to_string());
    state
}

#[test]
fn wheel_backspace_removes_the_last_digit() {
    let window = TestWindow::new();
    let mut state = wheel_state("12");
    press(&mut state, &window, BACK, 0);
    assert_eq!(state.thickness_input.as_deref(), Some("1"));
    assert!(state.scene_dirty);
}

#[test]
fn wheel_return_applies_the_typed_width() {
    let window = TestWindow::new();
    let mut state = wheel_state("12");
    set_overlay_text_editing(true);
    press(&mut state, &window, RETURN, 0);
    assert_eq!(state.thickness_input, None);
    assert_eq!(state.active_thickness, 12);
    assert_eq!(state.settings.default_thickness, 12);
    assert!(!crate::hotkey::is_overlay_text_editing());
}

#[test]
fn wheel_return_without_digits_just_closes_it() {
    let window = TestWindow::new();
    let mut state = wheel_state("");
    let before = state.active_thickness;
    press(&mut state, &window, RETURN, 0);
    assert_eq!(state.thickness_input, None);
    assert_eq!(state.active_thickness, before);
}

#[test]
fn wheel_return_keeps_the_wheel_open_for_an_impossible_width() {
    let window = TestWindow::new();
    let mut state = wheel_state("0");
    let before = state.active_thickness;
    press(&mut state, &window, RETURN, 0);
    assert_eq!(state.thickness_input.as_deref(), Some("0"));
    assert_eq!(state.active_thickness, before);
}

#[test]
fn wheel_arrows_step_the_width_and_clear_the_digits() {
    let window = TestWindow::new();
    let mut state = wheel_state("7");
    state.active_thickness = 10;
    press(&mut state, &window, UP, 0);
    assert_eq!(state.active_thickness, 9);
    assert_eq!(state.thickness_input.as_deref(), Some(""));
    press(&mut state, &window, DOWN, 0);
    press(&mut state, &window, DOWN, 0);
    assert_eq!(state.active_thickness, 11);
}

#[test]
fn wheel_arrows_stop_at_the_ends_of_the_range() {
    let window = TestWindow::new();
    let mut state = wheel_state("");
    state.active_thickness = 1;
    press(&mut state, &window, UP, 0);
    assert_eq!(state.active_thickness, 1);
    state.active_thickness = 64;
    press(&mut state, &window, DOWN, 0);
    assert_eq!(state.active_thickness, 64);
}

#[test]
fn wheel_escape_cancels_a_drag_first_and_then_closes() {
    let window = TestWindow::new();
    let mut state = wheel_state("");
    state.thickness_drag = Some(ThicknessDrag {
        start_y: 0,
        start_value: state.active_thickness,
        picked: state.active_thickness,
        moved: true,
    });
    press(&mut state, &window, ESCAPE, 0);
    assert!(state.thickness_drag.is_none());
    assert!(state.thickness_input.is_some());
    press(&mut state, &window, ESCAPE, 0);
    assert!(state.thickness_input.is_none());
    assert!(window.exists());
}

#[test]
fn wheel_ignores_unrelated_keys() {
    let window = TestWindow::new();
    let mut state = wheel_state("3");
    press(&mut state, &window, key('q'), 0);
    assert_eq!(state.thickness_input.as_deref(), Some("3"));
}

#[test]
fn keys_that_are_not_editor_shortcuts_do_nothing() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    press(&mut state, &window, key('q'), 0);
    assert!(!state.scene_dirty);
    assert_eq!(state.active_tool, ToolKind::Rectangle);
}

#[test]
fn f10_and_the_menu_key_need_a_laid_out_toolbar() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    assert!(state.toolbar.is_none());
    press(&mut state, &window, F10, 0);
    press(&mut state, &window, APPS, 0);
    assert!(!window.has(WM_LBUTTONDOWN));
    assert!(!state.scene_dirty);
}

#[test]
fn escape_closes_a_text_box_before_anything_else() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    set_overlay_text_editing(true);
    set_caret_timer(window.0, Some(15));
    press(&mut state, &window, ESCAPE, 0);
    assert!(state.text_edit().is_none());
    assert!(!crate::hotkey::is_overlay_text_editing());
    assert!(state.scene_dirty);
    assert!(
        !window.wait_for_timer(CARET_TIMER, std::time::Duration::from_millis(120)),
        "the caret timer must stop with the text box"
    );
    assert_eq!(state.mode(), OverlayMode::SelectionActive);
}

#[test]
fn escape_deselects_then_cancels_the_selection_then_closes_the_editor() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    press(&mut state, &window, ESCAPE, 0);
    assert_eq!(state.selected_id(), None);
    assert_eq!(state.mode(), OverlayMode::SelectionActive);
    press(&mut state, &window, ESCAPE, 0);
    assert_eq!(state.mode(), OverlayMode::Hovering);
    assert!(window.exists());
    press(&mut state, &window, ESCAPE, 0);
    assert!(!window.exists(), "Esc on an empty editor closes it");
}

#[test]
fn escape_cancels_a_gesture_in_progress() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.begin_test_rectangle_gesture((300, 300), (350, 350));
    press(&mut state, &window, ESCAPE, 0);
    assert!(!state.has_active_gesture());
    assert!(state.scene_dirty);
}

#[test]
fn ctrl_comma_asks_for_settings_even_while_typing() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    press(&mut state, &window, COMMA, CTRL);
    assert!(window.take(WM_EDITOR_SETTINGS).is_some());
    assert_eq!(edit(&state).text, "abc");
}

#[test]
fn backspace_and_delete_edit_the_text() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    press(&mut state, &window, BACK, 0);
    assert_eq!(edit(&state).text, "ab");
    state.text_edit.as_mut().unwrap().caret = 0;
    press(&mut state, &window, DELETE, 0);
    assert_eq!(edit(&state).text, "b");
    assert!(edit(&state).caret_visible);
    assert!(state.scene_dirty);
}

#[test]
fn ctrl_backspace_and_ctrl_delete_remove_whole_words() {
    let window = TestWindow::new();
    let mut state = editing_state("one two three");
    press(&mut state, &window, BACK, CTRL);
    assert_eq!(edit(&state).text, "one two ");
    state.text_edit.as_mut().unwrap().caret = 4;
    press(&mut state, &window, DELETE, CTRL);
    assert_eq!(edit(&state).text, "one ");
}

#[test]
fn arrows_move_the_caret_and_shift_extends_the_selection() {
    let window = TestWindow::new();
    let mut state = editing_state("one two");
    press(&mut state, &window, LEFT, 0);
    assert_eq!(edit(&state).caret, 6);
    press(&mut state, &window, LEFT, SHIFT);
    assert_eq!((edit(&state).caret, edit(&state).anchor), (5, Some(6)));
    press(&mut state, &window, RIGHT, 0);
    assert_eq!((edit(&state).caret, edit(&state).anchor), (6, None));
    press(&mut state, &window, LEFT, CTRL);
    assert_eq!(edit(&state).caret, 4);
    press(&mut state, &window, RIGHT, CTRL);
    assert_eq!(edit(&state).caret, 7);
    press(&mut state, &window, UP, 0);
    assert_eq!(edit(&state).caret, 7);
}

#[test]
fn home_and_end_jump_to_the_ends_of_the_text() {
    let window = TestWindow::new();
    let mut state = editing_state("hello");
    press(&mut state, &window, HOME, 0);
    assert_eq!(edit(&state).caret, 0);
    press(&mut state, &window, END, SHIFT);
    assert_eq!((edit(&state).caret, edit(&state).anchor), (5, Some(0)));
}

#[test]
fn ctrl_a_selects_everything_and_ctrl_z_ctrl_y_walk_the_undo_history() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    press(&mut state, &window, key('a'), CTRL);
    assert_eq!(edit(&state).selection(), Some(0..3));
    press(&mut state, &window, BACK, 0);
    assert_eq!(edit(&state).text, "");
    press(&mut state, &window, key('z'), CTRL);
    assert_eq!(edit(&state).text, "abc");
    press(&mut state, &window, key('y'), CTRL);
    assert_eq!(edit(&state).text, "");
}

#[test]
fn text_keys_restart_the_caret_blink() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    state.text_edit.as_mut().unwrap().caret_visible = false;
    press(&mut state, &window, LEFT, 0);
    assert!(edit(&state).caret_visible);
    // The blink timer was (re)started at the system rate, 500 ms by default.
    assert!(window.wait_for_timer(CARET_TIMER, std::time::Duration::from_millis(1500)));
}

#[test]
fn ctrl_v_pastes_the_clipboard_text_at_the_caret() {
    let window = TestWindow::new();
    crate::clipboard::copy_text_to_clipboard(None, "pasted").expect("a text clipboard");
    let mut state = editing_state("abc");
    press(&mut state, &window, key('v'), CTRL);
    assert_eq!(edit(&state).text, "abcpasted");
    assert!(take_action_error().is_none());
}

#[test]
fn ctrl_v_reports_a_clipboard_that_another_program_holds() {
    let window = TestWindow::new();
    crate::clipboard::copy_text_to_clipboard(None, "pasted").expect("a text clipboard");
    let mut state = editing_state("abc");
    {
        let _held = ClipboardHold::new();
        press(&mut state, &window, key('v'), CTRL);
    }
    assert_eq!(edit(&state).text, "abc");
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Paste failed"));
    assert!(window.take(WM_EDITOR_ERROR).is_some());
}

#[test]
fn ctrl_x_moves_the_selected_text_to_the_clipboard() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    state.text_edit.as_mut().unwrap().anchor = Some(1);
    press(&mut state, &window, key('x'), CTRL);
    assert_eq!(edit(&state).text, "a");
    assert_eq!(clipboard_text().as_deref(), Some("bc"));
}

#[test]
fn ctrl_x_without_a_selection_leaves_text_and_clipboard_alone() {
    let window = TestWindow::new();
    crate::clipboard::copy_text_to_clipboard(None, "kept").expect("a text clipboard");
    let mut state = editing_state("abc");
    press(&mut state, &window, key('x'), CTRL);
    assert_eq!(edit(&state).text, "abc");
    assert_eq!(clipboard_text().as_deref(), Some("kept"));
}

#[test]
fn ctrl_x_keeps_the_text_when_the_clipboard_is_unavailable() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    state.text_edit.as_mut().unwrap().anchor = Some(1);
    {
        let _held = ClipboardHold::new();
        press(&mut state, &window, key('x'), CTRL);
    }
    assert_eq!(edit(&state).text, "abc");
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Cut failed"));
}

#[test]
fn return_commits_the_text_box_without_copying() {
    let window = TestWindow::new();
    let mut state = editing_state("Hello");
    set_overlay_text_editing(true);
    press(&mut state, &window, RETURN, 0);
    assert!(state.text_edit().is_none());
    assert_eq!(state.object_count(), 2);
    assert!(!state.committed_result);
    assert!(!crate::hotkey::is_overlay_text_editing());
}

#[test]
fn ctrl_u_while_typing_commits_the_text_then_starts_the_upload() {
    let window = TestWindow::new();
    let mut state = editing_state("Hello");
    press(&mut state, &window, key('u'), CTRL);
    assert!(state.text_edit().is_none());
    assert_eq!(state.object_count(), 2);
    assert!(window.take(WM_EDITOR_CLOUD_SETUP).is_some());
    assert!(state.setup_pending);
}

#[test]
fn ctrl_s_while_typing_commits_the_text_and_saves_it() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-save-typing");
    let mut state = export_state(&folder);
    state.set_text_edit(Some(TextEditState::new(
        (200, 200),
        "Hello".to_string(),
        [0, 0, 0, 255],
        22,
        None,
    )));
    press(&mut state, &window, key('s'), CTRL);
    assert_eq!(state.object_count(), 2);
    assert_eq!(folder.entries().len(), 1);
}

#[test]
fn ctrl_c_while_typing_commits_the_text_and_copies() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-copy-typing");
    let mut state = export_state(&folder);
    state.set_text_edit(Some(TextEditState::new(
        (200, 200),
        "Hello".to_string(),
        [0, 0, 0, 255],
        22,
        None,
    )));
    press(&mut state, &window, key('c'), CTRL);
    assert_eq!(state.object_count(), 2);
    assert!(state.committed_result);
}

#[test]
fn ctrl_shift_s_asks_for_a_save_location() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    press(&mut state, &window, key('s'), CTRL | SHIFT);
    assert!(window.take(WM_EDITOR_SAVE_AS).is_some());
    assert!(!state.committed_result);
}

#[test]
fn ctrl_s_saves_the_selection_and_closes_the_editor() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-save-close");
    let mut state = export_state(&folder);
    state.settings.close_after_action = true;
    state.settings.notify_after_save = true;
    press(&mut state, &window, key('s'), CTRL);
    assert_eq!(folder.entries().len(), 1);
    assert!(state.committed_result);
    assert!(!window.exists(), "the editor closes after saving");
}

#[test]
fn ctrl_s_can_keep_the_editor_open_after_saving() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-save-stay");
    let mut state = export_state(&folder);
    state.settings.notify_after_save = false;
    press(&mut state, &window, key('s'), CTRL);
    assert_eq!(folder.entries().len(), 1);
    assert!(window.exists());
    assert!(state.scene_dirty);
    assert!(state.toolbar.is_some(), "the editor scene is drawn again");
}

#[test]
fn ctrl_s_reports_a_folder_that_cannot_be_written() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-save-fails");
    let blocker = folder.0.join("a-file");
    std::fs::write(&blocker, b"not a folder").unwrap();
    let mut state = export_state(&folder);
    state.settings.save_directory = blocker;
    press(&mut state, &window, key('s'), CTRL);
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Save failed"));
    assert!(!state.committed_result);
    assert!(window.exists());
}

#[test]
fn ctrl_u_opens_cloudflare_setup_once_when_no_cloud_is_connected() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    press(&mut state, &window, key('u'), CTRL);
    assert!(window.take(WM_EDITOR_CLOUD_SETUP).is_some());
    assert!(state.setup_pending);
    press(&mut state, &window, key('u'), CTRL);
    assert!(
        !window.has(WM_EDITOR_CLOUD_SETUP),
        "a second request while setup is open is ignored"
    );
}

#[test]
fn ctrl_u_reports_missing_cloud_keys() {
    let _appdata = LocalAppData::new("keys-upload");
    let window = TestWindow::new();
    let folder = TempDir::new("keys-upload-out");
    let mut state = export_state(&folder);
    state.settings.cloud_url = Some("https://share.example.com".to_string());
    press(&mut state, &window, key('u'), CTRL);
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Upload failed"));
    assert!(window.exists());
    assert!(state.toolbar.is_some(), "the editor scene is drawn again");
}

#[test]
fn ctrl_c_copies_the_selection_to_the_clipboard() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-copy");
    crate::clipboard::copy_text_to_clipboard(None, "stale").expect("a text clipboard");
    let mut state = export_state(&folder);
    press(&mut state, &window, key('c'), CTRL);
    assert!(state.committed_result);
    assert!(
        unsafe {
            windows::Win32::System::DataExchange::IsClipboardFormatAvailable(
                crate::clipboard::CF_DIB_FORMAT,
            )
        }
        .is_ok(),
        "the image replaced the text"
    );
    assert!(window.exists());
    assert!(state.scene_dirty);
}

#[test]
fn enter_copies_the_selection_and_closes_the_editor() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-enter");
    let mut state = export_state(&folder);
    state.settings.close_after_action = true;
    press(&mut state, &window, RETURN, 0);
    assert!(state.committed_result);
    assert!(!window.exists());
}

#[test]
fn copy_reports_a_clipboard_that_another_program_holds() {
    let window = TestWindow::new();
    let folder = TempDir::new("keys-copy-fails");
    let mut state = export_state(&folder);
    {
        let _held = ClipboardHold::new();
        press(&mut state, &window, key('c'), CTRL);
    }
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Copy failed"));
    assert!(!state.committed_result);
    assert!(window.exists());
    assert!(state.toolbar.is_some(), "the editor scene is drawn again");
}

#[test]
fn ctrl_z_brings_back_a_cancelled_session_and_then_undoes_edits() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.discard_session();
    assert_eq!(state.mode(), OverlayMode::Hovering);
    press(&mut state, &window, key('z'), CTRL);
    assert_eq!(state.mode(), OverlayMode::SelectionActive);
    assert_eq!(state.object_count(), 1);
    press(&mut state, &window, key('z'), CTRL);
    assert_eq!(state.object_count(), 0);
    press(&mut state, &window, key('y'), CTRL);
    assert_eq!(state.object_count(), 1);
    assert_eq!(state.selected_id(), None);
}

#[test]
fn undo_restores_a_moved_selection() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let before = state.committed_selection().unwrap();
    press(&mut state, &window, RIGHT, 0);
    assert_ne!(state.committed_selection(), Some(before));
    press(&mut state, &window, key('z'), CTRL);
    assert_eq!(state.committed_selection(), Some(before));
    press(&mut state, &window, key('y'), CTRL);
    assert_eq!(
        state.committed_selection().map(|r| r.left),
        Some(before.left + 1)
    );
}

#[test]
fn delete_and_backspace_remove_the_selected_object() {
    let window = TestWindow::new();
    for vk in [DELETE, BACK] {
        let mut state = state_with_rectangle();
        let id = state.objects[0].id;
        state.set_selected_id(Some(id));
        press(&mut state, &window, vk, 0);
        assert_eq!(state.object_count(), 0);
        assert_eq!(state.selected_id(), None);
        press(&mut state, &window, key('z'), CTRL);
        assert_eq!(state.object_count(), 1, "the removal is one undo step");
    }
}

#[test]
fn delete_without_a_selected_object_changes_nothing() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    press(&mut state, &window, DELETE, 0);
    assert_eq!(state.object_count(), 1);
}

#[test]
fn letter_keys_pick_tools_and_remember_the_choice() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    press(&mut state, &window, key('p'), 0);
    assert_eq!(state.active_tool, ToolKind::Pen);
    assert_eq!(state.selected_id(), None);
    assert_eq!(state.settings.last_tool, ToolKind::Pen);
    assert!(state.preferences_dirty);
    press(&mut state, &window, key('b'), 0);
    assert_eq!(state.active_tool, ToolKind::Blur);
}

#[test]
fn tool_letters_are_ordinary_characters_inside_a_text_box() {
    let window = TestWindow::new();
    let mut state = editing_state("abc");
    press(&mut state, &window, key('p'), 0);
    assert_eq!(state.active_tool, ToolKind::Rectangle);
    assert_eq!(edit(&state).text, "abc");
}

#[test]
fn arrows_nudge_the_selected_object_by_one_pixel_or_ten_with_shift() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    press(&mut state, &window, LEFT, 0);
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(149, 150, 249, 250)
    );
    press(&mut state, &window, DOWN, SHIFT);
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(149, 160, 249, 260)
    );
    press(&mut state, &window, UP, 0);
    press(&mut state, &window, RIGHT, SHIFT);
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(159, 159, 259, 259)
    );
    assert!(state.scene_dirty);
    // Repeated nudges are one undo step.
    press(&mut state, &window, key('z'), CTRL);
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(150, 150, 250, 250)
    );
}

#[test]
fn nudges_stop_at_the_selection_edge() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    // 50 px from the left edge: five shifted nudges reach it, the others are clamped.
    for _ in 0..8 {
        press(&mut state, &window, LEFT, SHIFT);
    }
    assert_eq!(rectangle_of(&state.objects[0]).left, 100);
    state.scene_dirty = false;
    press(&mut state, &window, LEFT, 0);
    assert_eq!(rectangle_of(&state.objects[0]).left, 100);
    assert!(
        !state.scene_dirty,
        "a nudge that moves nothing does not redraw"
    );
}

#[test]
fn an_object_larger_than_the_selection_does_not_nudge_that_way() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.objects[0].kind = AnnotationKind::Rectangle {
        rect: Rect::new(80, 120, 520, 140),
        color: [1, 2, 3, 255],
        thickness: 2,
    };
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    press(&mut state, &window, RIGHT, 0);
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(80, 120, 520, 140)
    );
    press(&mut state, &window, DOWN, 0);
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(80, 121, 520, 141)
    );
}

#[test]
fn arrows_move_the_selection_and_its_objects_together() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    press(&mut state, &window, RIGHT, 0);
    press(&mut state, &window, DOWN, SHIFT);
    assert_eq!(
        state.committed_selection(),
        Some(Rect::new(101, 110, 501, 410))
    );
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(151, 160, 251, 260)
    );
}

#[test]
fn the_selection_stays_inside_the_capture_when_moved_with_arrows() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.committed_selection = Some(Rect::new(0, 0, 400, 300));
    state.scene_dirty = false;
    press(&mut state, &window, LEFT, 0);
    press(&mut state, &window, UP, 0);
    assert_eq!(state.committed_selection(), Some(Rect::new(0, 0, 400, 300)));
    assert!(state.scene_dirty);
    // Nothing moved, so nothing was recorded: undo reverts the rectangle, not a nudge.
    press(&mut state, &window, key('z'), CTRL);
    assert_eq!(state.object_count(), 0);
    assert_eq!(state.committed_selection(), Some(Rect::new(0, 0, 400, 300)));
}

#[test]
fn ctrl_arrows_resize_the_selection_within_the_capture() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    press(&mut state, &window, RIGHT, CTRL | SHIFT);
    press(&mut state, &window, DOWN, CTRL);
    assert_eq!(
        state.committed_selection(),
        Some(Rect::new(100, 100, 510, 401))
    );
    // Never smaller than 8 px, never past the capture.
    for _ in 0..60 {
        press(&mut state, &window, LEFT, CTRL | SHIFT);
        press(&mut state, &window, UP, CTRL | SHIFT);
    }
    assert_eq!(
        state.committed_selection(),
        Some(Rect::new(100, 100, 108, 108))
    );
    for _ in 0..90 {
        press(&mut state, &window, RIGHT, CTRL | SHIFT);
        press(&mut state, &window, DOWN, CTRL | SHIFT);
    }
    assert_eq!(
        state.committed_selection(),
        Some(Rect::new(100, 100, 800, 600))
    );
    // Resizing does not move the objects.
    assert_eq!(
        rectangle_of(&state.objects[0]),
        Rect::new(150, 150, 250, 250)
    );
}

#[test]
fn arrows_do_nothing_before_a_region_is_chosen() {
    let window = TestWindow::new();
    let mut state = plain_state();
    press(&mut state, &window, LEFT, 0);
    assert!(!state.scene_dirty);
}

#[test]
fn wheel_digits_are_typed_into_the_width_box() {
    let window = TestWindow::new();
    let mut state = wheel_state("");
    assert_eq!(state.handle_char(window.0, '5' as u32).0, 0);
    assert_eq!(state.thickness_input.as_deref(), Some("5"));
    assert!(state.scene_dirty);
    state.handle_char(window.0, '0' as u32);
    assert_eq!(state.thickness_input.as_deref(), Some("50"));
    // A third digit does not fit.
    state.handle_char(window.0, '1' as u32);
    assert_eq!(state.thickness_input.as_deref(), Some("50"));
}

#[test]
fn wheel_rejects_widths_outside_one_to_sixty_four() {
    let window = TestWindow::new();
    let mut state = wheel_state("");
    state.handle_char(window.0, '0' as u32);
    assert_eq!(state.thickness_input.as_deref(), Some(""));
    state.handle_char(window.0, '7' as u32);
    state.handle_char(window.0, '9' as u32);
    assert_eq!(state.thickness_input.as_deref(), Some("7"));
    state.handle_char(window.0, 'x' as u32);
    assert_eq!(state.thickness_input.as_deref(), Some("7"));
}

#[test]
fn typed_characters_go_into_the_text_box() {
    let window = TestWindow::new();
    let mut state = editing_state("ab");
    state.text_edit.as_mut().unwrap().caret_visible = false;
    assert_eq!(state.handle_char(window.0, 'c' as u32).0, 0);
    assert_eq!(edit(&state).text, "abc");
    assert!(edit(&state).caret_visible);
    assert!(state.scene_dirty);
    // An emoji arrives as a surrogate pair.
    state.handle_char(window.0, 0xd83d);
    state.handle_char(window.0, 0xde42);
    assert_eq!(edit(&state).text, "abc🙂");
}

#[test]
fn characters_without_a_text_box_are_ignored() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    assert_eq!(state.handle_char(window.0, 'a' as u32).0, 0);
    assert!(!state.scene_dirty);
}
