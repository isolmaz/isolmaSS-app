//! Toolbar commands and the export actions behind them: save, copy and upload.
use super::test_support::*;
use super::*;
use crate::toolbar::ToolbarButton;
use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;

fn activate(state: &mut OverlayState, window: &TestWindow, item: ToolbarItem) {
    state.activate_toolbar_item(window.0, item, (0, 0));
}

fn dib_on_clipboard() -> bool {
    unsafe { IsClipboardFormatAvailable(crate::clipboard::CF_DIB_FORMAT) }.is_ok()
}

fn button(item: ToolbarItem, rect: Rect) -> ToolbarButton {
    ToolbarButton {
        item,
        rect,
        is_enabled: true,
        is_checked: false,
    }
}

fn cloud_credentials() -> crate::cloudflare_setup::CloudCredentials {
    crate::cloudflare_setup::CloudCredentials {
        origin: "https://share.example.com".to_string(),
        upload_token: "A".repeat(43),
        admin_token: "B".repeat(43),
        share_password: None,
        account_id: None,
        worker_name: None,
    }
}

// ---- toolbar commands ---------------------------------------------------------------

#[test]
fn a_tool_button_selects_the_tool_and_remembers_it() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    activate(&mut state, &window, ToolbarItem::Tool(ToolKind::Pen));
    assert_eq!(state.active_tool, ToolKind::Pen);
    assert_eq!(state.settings.last_tool, ToolKind::Pen);
    assert_eq!(state.selected_id(), None);
    assert!(state.scene_dirty);
}

#[test]
fn the_more_tools_button_expands_and_collapses_the_tool_strip() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(&mut state, &window, ToolbarItem::MoreTools);
    assert!(state.tools_expanded);
    activate(&mut state, &window, ToolbarItem::MoreTools);
    assert!(!state.tools_expanded);
}

#[test]
fn using_another_button_closes_the_width_wheel() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.thickness_input = Some("5".to_string());
    set_overlay_text_editing(true);
    activate(&mut state, &window, ToolbarItem::Tool(ToolKind::Arrow));
    assert_eq!(state.thickness_input, None);
    assert!(!crate::hotkey::is_overlay_text_editing());
}

#[test]
fn picking_a_width_row_keeps_the_wheel_open_and_starts_a_drag() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    state.thickness_input = Some("5".to_string());
    state.activate_toolbar_item(window.0, ToolbarItem::ThicknessOption(8), (10, 77));
    assert_eq!(state.thickness_input.as_deref(), Some("5"));
    let drag = state.thickness_drag.expect("a wheel drag starts");
    assert_eq!((drag.start_y, drag.picked, drag.moved), (77, 8, false));
    assert_eq!(drag.start_value, state.active_thickness);
    let (original_id, original_kind) = state.thickness_drag_original.clone().unwrap();
    assert_eq!(original_id, id);
    assert_eq!(original_kind, state.objects[0].kind);
}

#[test]
fn a_width_drag_without_a_selected_object_remembers_no_object() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(&mut state, &window, ToolbarItem::ThicknessOption(2));
    assert!(state.thickness_drag.is_some());
    assert!(state.thickness_drag_original.is_none());
}

#[test]
fn the_width_button_opens_and_closes_the_wheel() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(&mut state, &window, ToolbarItem::Thickness);
    assert_eq!(state.thickness_input.as_deref(), Some(""));
    assert!(crate::hotkey::is_overlay_text_editing());
    activate(&mut state, &window, ToolbarItem::Thickness);
    assert_eq!(state.thickness_input, None);
    assert!(!crate::hotkey::is_overlay_text_editing());
}

#[test]
fn a_button_press_commits_the_text_being_typed() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.set_text_edit(Some(TextEditState::new(
        (200, 200),
        "Note".to_string(),
        [0, 0, 0, 255],
        22,
        None,
    )));
    activate(&mut state, &window, ToolbarItem::Tool(ToolKind::Select));
    assert!(state.text_edit().is_none());
    assert_eq!(state.object_count(), 2);
}

#[test]
fn the_colour_button_opens_the_picker_without_ending_the_text() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.set_text_edit(Some(TextEditState::new(
        (200, 200),
        "Note".to_string(),
        [0, 0, 0, 255],
        22,
        None,
    )));
    activate(&mut state, &window, ToolbarItem::Color);
    assert!(window.take(WM_EDITOR_PICK_COLOR).is_some());
    assert!(state.text_edit().is_some());
}

#[test]
fn the_settings_button_asks_for_the_settings_window() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Settings),
    );
    assert!(window.take(WM_EDITOR_SETTINGS).is_some());
}

#[test]
fn the_cancel_button_discards_the_session_but_keeps_it_restorable() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Cancel),
    );
    assert_eq!(state.mode(), OverlayMode::Hovering);
    assert_eq!(state.object_count(), 0);
    assert!(state.restore_discarded_session());
}

#[test]
fn undo_and_redo_buttons_walk_the_history() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Undo),
    );
    assert_eq!(state.object_count(), 0);
    assert_eq!(state.selected_id(), None);
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Redo),
    );
    assert_eq!(state.object_count(), 1);
}

#[test]
fn undo_and_redo_buttons_move_the_selection_back_and_forth() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let before = state.committed_selection().unwrap();
    let after = Rect::new(110, 100, 510, 400);
    state.committed_selection = Some(after);
    state.history.record(EditCommand::Selection {
        before,
        after,
        shift: (10, 0),
    });
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Undo),
    );
    assert_eq!(state.committed_selection(), Some(before));
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Redo),
    );
    assert_eq!(state.committed_selection(), Some(after));
}

#[test]
fn the_save_button_writes_the_selection_and_closes_when_asked_to() {
    let window = TestWindow::new();
    let folder = TempDir::new("toolbar-save-close");
    let mut state = export_state(&folder);
    state.settings.close_after_action = true;
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Save),
    );
    assert_eq!(folder.entries().len(), 1);
    assert!(!window.exists());
}

#[test]
fn the_save_button_can_keep_the_editor_open() {
    let window = TestWindow::new();
    let folder = TempDir::new("toolbar-save-stay");
    let mut state = export_state(&folder);
    state.settings.notify_after_save = false;
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Save),
    );
    assert_eq!(folder.entries().len(), 1);
    assert!(window.exists());
    assert!(state.scene_dirty);
}

#[test]
fn the_save_button_reports_a_failed_save() {
    let window = TestWindow::new();
    let folder = TempDir::new("toolbar-save-fails");
    let blocker = folder.0.join("a-file");
    std::fs::write(&blocker, b"not a folder").unwrap();
    let mut state = export_state(&folder);
    state.settings.save_directory = blocker;
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Save),
    );
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Save failed"));
    assert!(window.take(WM_EDITOR_ERROR).is_some());
}

#[test]
fn the_copy_button_puts_the_image_on_the_clipboard() {
    let window = TestWindow::new();
    let folder = TempDir::new("toolbar-copy");
    let mut state = export_state(&folder);
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Copy),
    );
    assert!(dib_on_clipboard());
    assert!(state.committed_result);
    assert!(window.exists());
    assert!(state.scene_dirty);
}

#[test]
fn the_copy_button_closes_the_editor_when_asked_to() {
    let window = TestWindow::new();
    let folder = TempDir::new("toolbar-copy-close");
    let mut state = export_state(&folder);
    state.settings.close_after_action = true;
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Copy),
    );
    assert!(!window.exists());
}

#[test]
fn the_copy_button_reports_an_unavailable_clipboard() {
    let window = TestWindow::new();
    let folder = TempDir::new("toolbar-copy-fails");
    let mut state = export_state(&folder);
    {
        let _held = ClipboardHold::new();
        activate(
            &mut state,
            &window,
            ToolbarItem::Action(ToolbarAction::Copy),
        );
    }
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Copy failed"));
    assert!(!state.committed_result);
}

#[test]
fn the_upload_button_opens_cloudflare_setup_when_nothing_is_connected() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Upload),
    );
    assert!(window.take(WM_EDITOR_CLOUD_SETUP).is_some());
    assert!(state.setup_pending);
}

#[test]
fn the_upload_button_reports_why_an_upload_cannot_start() {
    let _appdata = LocalAppData::new("toolbar-upload");
    let window = TestWindow::new();
    let folder = TempDir::new("toolbar-upload-out");
    let mut state = export_state(&folder);
    state.settings.cloud_url = Some("https://share.example.com".to_string());
    activate(
        &mut state,
        &window,
        ToolbarItem::Action(ToolbarAction::Upload),
    );
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Upload failed"));
}

// ---- export actions -----------------------------------------------------------------

#[test]
fn exports_need_a_selection() {
    let window = TestWindow::new();
    let folder = TempDir::new("export-none");
    let mut state = export_state(&folder);
    state.committed_selection = None;
    let expected = crate::i18n::t("Select a screen region first.");
    assert_eq!(
        state.copy_selection_to_clipboard(window.0),
        Err(expected.to_string())
    );
    assert_eq!(state.save_selection_to_file(), Err(expected.to_string()));
    assert_eq!(state.upload_selection(window.0), Err(expected.to_string()));
    assert!(!state.committed_result);
}

#[test]
fn saving_writes_the_exported_selection_as_a_png() {
    let folder = TempDir::new("export-save");
    let mut state = export_state(&folder);
    let path = state.save_selection_to_file().expect("the file is saved");
    assert_eq!(path.parent(), Some(folder.0.as_path()));
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert!(state.committed_result);
    // The editor scene is back in the buffer for the next frame.
    assert!(state.toolbar.is_some());
}

#[test]
fn a_selection_outside_the_capture_cannot_be_copied() {
    let window = TestWindow::new();
    let folder = TempDir::new("export-outside");
    let mut state = export_state(&folder);
    state.committed_selection = Some(Rect::new(1000, 1000, 1100, 1100));
    assert!(state.copy_selection_to_clipboard(window.0).is_err());
    assert!(!state.committed_result);
}

#[test]
fn an_upload_is_prepared_from_the_exported_selection() {
    let appdata = LocalAppData::new("export-upload");
    let folder = TempDir::new("export-upload-out");
    let mut state = export_state(&folder);
    Rc::get_mut(&mut state.capture).expect("one owner").x = 40;
    state.settings.cloud_url = Some("https://share.example.com".to_string());
    crate::cloudflare_setup::save_credentials(&cloud_credentials()).expect("protected keys");
    assert!(appdata.dir.join("isolmaSS").is_dir());
    let selection = state.committed_selection().unwrap();
    let (_prepared, anchor) = state.prepare_upload(selection).expect("an encoded upload");
    // The toast opens in the middle of the selection, in screen coordinates.
    assert_eq!((anchor.x, anchor.y), (40 + 300, 250));
    assert!(state.toolbar.is_some(), "the editor scene is drawn again");
}

#[test]
fn an_upload_without_stored_keys_is_not_prepared() {
    let _appdata = LocalAppData::new("export-upload-nokeys");
    let folder = TempDir::new("export-upload-nokeys-out");
    let mut state = export_state(&folder);
    state.settings.cloud_url = Some("https://share.example.com".to_string());
    let selection = state.committed_selection().unwrap();
    assert!(state.prepare_upload(selection).is_err());
    assert!(state.toolbar.is_some(), "the editor scene is drawn again");
}

#[test]
fn finishing_an_upload_closes_the_editor_window() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    assert_eq!(state.finish_upload(window.0), Ok(()));
    assert!(state.committed_result);
    assert!(window.take(WM_CLOSE).is_some());
}

#[test]
fn finishing_an_upload_reports_a_window_that_is_already_gone() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    unsafe {
        DestroyWindow(window.0).expect("the test window closes");
    }
    assert!(state.finish_upload(window.0).is_err());
    assert!(state.committed_result, "the upload itself had started");
}

#[test]
fn an_upload_that_cannot_reach_its_window_does_not_wait_for_setup() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    unsafe {
        DestroyWindow(window.0).expect("the test window closes");
    }
    let error = state
        .begin_upload(window.0)
        .err()
        .expect("the setup request cannot be posted");
    assert!(!error.is_empty());
    assert!(!state.setup_pending, "a failed request is not pending");
}

// ---- command menu -------------------------------------------------------------------

fn state_with_toolbar() -> OverlayState {
    let mut state = backbuffer_state(640, 480);
    state.set_selection_active(Rect::new(100, 100, 500, 400));
    state.composite_scene();
    assert!(state.toolbar.is_some());
    state
}

#[test]
fn the_command_menu_lists_every_tool_even_when_the_strip_is_collapsed() {
    let mut state = state_with_toolbar();
    state.active_tool = ToolKind::Text;
    let shown = state.toolbar.as_ref().unwrap().buttons.clone();
    assert!(
        !shown
            .iter()
            .any(|b| b.item == ToolbarItem::Tool(ToolKind::Text))
    );
    let menu = state.command_menu_buttons().expect("a toolbar is laid out");
    assert_eq!(menu.len(), shown.len() + MORE_TOOLS.len() + 1, "the hidden tools and Pin");
    let items: Vec<ToolbarItem> = menu.iter().map(|b| b.item).collect();
    let undo = items
        .iter()
        .position(|item| *item == ToolbarItem::Action(ToolbarAction::Undo))
        .unwrap();
    // The hidden tools sit together just before Undo, in toolbar order.
    assert_eq!(
        &items[undo - MORE_TOOLS.len()..undo],
        &[
            ToolbarItem::Tool(ToolKind::Ellipse),
            ToolbarItem::Tool(ToolKind::Line),
            ToolbarItem::Tool(ToolKind::Highlight),
            ToolbarItem::Tool(ToolKind::Text),
            ToolbarItem::Tool(ToolKind::Step),
            ToolbarItem::Tool(ToolKind::Blur),
            ToolbarItem::Tool(ToolKind::Redact),
            ToolbarItem::Tool(ToolKind::ColorPicker),
        ]
    );
    let text = menu
        .iter()
        .find(|b| b.item == ToolbarItem::Tool(ToolKind::Text))
        .unwrap();
    assert!(text.is_checked && text.is_enabled);
    let blur = menu
        .iter()
        .find(|b| b.item == ToolbarItem::Tool(ToolKind::Blur))
        .unwrap();
    assert!(!blur.is_checked);
}

#[test]
fn an_expanded_strip_needs_no_extra_tool_entries() {
    let mut state = state_with_toolbar();
    state.tools_expanded = true;
    state.composite_scene();
    let shown = state.toolbar.as_ref().unwrap().buttons.len();
    assert_eq!(state.command_menu_buttons().unwrap().len(), shown + 1);
}

#[test]
fn the_command_menu_offers_pin_to_screen_right_after_upload() {
    let state = state_with_toolbar();
    let items: Vec<ToolbarItem> = state
        .command_menu_buttons()
        .unwrap()
        .iter()
        .map(|button| button.item)
        .collect();
    let upload = items
        .iter()
        .position(|item| *item == ToolbarItem::Action(ToolbarAction::Upload))
        .unwrap();
    assert_eq!(items[upload + 1], ToolbarItem::Action(ToolbarAction::Pin));
    assert_eq!(
        items
            .iter()
            .filter(|item| **item == items[upload + 1])
            .count(),
        1
    );
}

#[test]
fn choosing_pin_in_the_command_menu_asks_for_a_pin_instead_of_clicking() {
    let window = TestWindow::new();
    let mut state = state_with_toolbar();
    let buttons = state.command_menu_buttons().unwrap();
    state.apply_command_menu_choice(
        window.0,
        &buttons,
        Ok(Some(ToolbarItem::Action(ToolbarAction::Pin))),
    );
    assert!(window.take(WM_EDITOR_PIN).is_some());
    assert!(
        !window.has(WM_LBUTTONDOWN),
        "no click is replayed at the menu entry's empty rectangle"
    );
}

#[test]
fn the_pin_command_asks_the_window_to_pin() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(&mut state, &window, ToolbarItem::Action(ToolbarAction::Pin));
    assert!(window.take(WM_EDITOR_PIN).is_some());
    assert!(!state.committed_result, "the pin itself is made later");
}

#[test]
fn a_pin_is_the_exported_selection_at_its_screen_position() {
    let folder = TempDir::new("export-pin");
    let mut state = export_state(&folder);
    Rc::get_mut(&mut state.capture).expect("one owner").x = -40;
    Rc::get_mut(&mut state.capture).expect("one owner").y = 30;
    state.settings.save_format = crate::settings::SaveFormat::Jpeg;
    state.settings.jpeg_quality = 55;
    let pin = state.begin_pin().expect("a pin image");
    assert_eq!((pin.width, pin.height), (400, 300));
    assert_eq!(pin.origin, (-40 + 100, 30 + 100));
    assert_eq!(pin.pixels.len(), 400 * 300 * 4);
    assert!(
        pin.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    assert_eq!(pin.format, crate::settings::SaveFormat::Jpeg);
    assert_eq!(pin.jpeg_quality, 55);
    // The corner of the selection is bare capture; the rectangle's outline differs from it.
    let corner = &pin.pixels[..3];
    assert!(
        pin.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| &pixel[..3] != corner),
        "the annotation is part of the pinned pixels"
    );
    assert!(state.toolbar.is_some(), "the editor scene is drawn again");
    assert!(!state.committed_result, "closing the editor commits it");
}

#[test]
fn a_pin_needs_a_selection_inside_the_capture() {
    let folder = TempDir::new("export-pin-none");
    let mut state = export_state(&folder);
    state.committed_selection = None;
    assert_eq!(
        state.begin_pin().err(),
        Some(crate::i18n::t("Select a screen region first.").to_string())
    );
    state.committed_selection = Some(Rect::new(1000, 1000, 1100, 1100));
    let error = state.begin_pin().err().expect("nothing to pin");
    let prefix = crate::i18n::tf("Could not prepare the screenshot for pinning: {}", &[&""]);
    assert!(error.starts_with(&prefix), "{error}");
    assert!(state.toolbar.is_some(), "the editor scene is drawn again");
}

#[test]
fn there_is_no_command_menu_before_the_toolbar_exists() {
    let window = TestWindow::new();
    let state = state_with_rectangle();
    assert!(state.command_menu_buttons().is_none());
    assert!(state.command_menu_anchor(window.0).is_none());
}

#[test]
fn the_command_menu_opens_at_the_action_bar_in_screen_coordinates() {
    let window = TestWindow::new();
    let state = state_with_toolbar();
    let bar = state.toolbar.as_ref().unwrap().action_bounds;
    let mut origin = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    unsafe {
        let _ = ClientToScreen(window.0, &mut origin);
    }
    assert_eq!(
        state.command_menu_anchor(window.0),
        Some((origin.x + bar.left, origin.y + bar.top))
    );
}

#[test]
fn choosing_a_tool_in_the_command_menu_selects_it() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    state.apply_command_menu_choice(window.0, &[], Ok(Some(ToolbarItem::Tool(ToolKind::Blur))));
    assert_eq!(state.active_tool, ToolKind::Blur);
    assert_eq!(state.selected_id(), None);
    assert_eq!(state.settings.last_tool, ToolKind::Blur);
    assert!(state.scene_dirty);
}

#[test]
fn choosing_a_command_replays_a_click_on_its_button() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let buttons = [
        button(
            ToolbarItem::Action(ToolbarAction::Undo),
            Rect::new(10, 20, 30, 40),
        ),
        button(
            ToolbarItem::Action(ToolbarAction::Save),
            Rect::new(100, 200, 140, 240),
        ),
    ];
    state.apply_command_menu_choice(
        window.0,
        &buttons,
        Ok(Some(ToolbarItem::Action(ToolbarAction::Save))),
    );
    let packed = (120u32 | (220u32 << 16)) as isize;
    let down = window.take(WM_LBUTTONDOWN).expect("a press is posted");
    assert_eq!((down.wParam.0, down.lParam.0), (1, packed));
    let up = window.take(WM_LBUTTONUP).expect("a release is posted");
    assert_eq!((up.wParam.0, up.lParam.0), (0, packed));
}

#[test]
fn a_command_that_is_not_in_the_menu_is_ignored() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let buttons = [button(
        ToolbarItem::Action(ToolbarAction::Undo),
        Rect::new(10, 20, 30, 40),
    )];
    state.apply_command_menu_choice(
        window.0,
        &buttons,
        Ok(Some(ToolbarItem::Action(ToolbarAction::Save))),
    );
    assert!(!window.has(WM_LBUTTONDOWN));
}

#[test]
fn dismissing_the_command_menu_changes_nothing() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.apply_command_menu_choice(window.0, &[], Ok(None));
    assert!(!window.has(WM_LBUTTONDOWN));
    assert!(!state.scene_dirty);
    assert!(take_action_error().is_none());
}

#[test]
fn a_command_menu_failure_is_reported() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let failure = windows::core::Error::new(windows::core::HRESULT(-2147467259), "menu broke");
    state.apply_command_menu_choice(window.0, &[], Err(failure));
    let (title, message) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Command menu failed"));
    assert!(message.contains("menu broke"));
}

// ---- colour picker and notices ------------------------------------------------------

/// A 100x100 capture whose pixel (10, 20) is #FEAB10, with the picker active over a
/// selection that holds one rectangle.
fn picker_state() -> OverlayState {
    let mut pixels = vec![0u8; 100 * 100 * 4];
    let offset = (20 * 100 + 10) * 4;
    pixels[offset..offset + 4].copy_from_slice(&[0x10, 0xab, 0xfe, 0xff]);
    let capture = CaptureBuffer::from_original(100, 100, pixels);
    let mut state = OverlayState::create_test_state(Rc::new(capture));
    state.set_selection_active(Rect::new(5, 5, 60, 60));
    state.add_test_rectangle(Rect::new(10, 10, 30, 30));
    state.select_tool(ToolKind::ColorPicker);
    state
}

#[test]
fn the_picker_button_starts_the_picker_and_remembers_the_tool_before_it() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    activate(
        &mut state,
        &window,
        ToolbarItem::Tool(ToolKind::ColorPicker),
    );
    assert_eq!(state.active_tool, ToolKind::ColorPicker);
    assert_eq!(state.tool_before_pick, Some(ToolKind::Rectangle));
    assert_eq!(state.settings.last_tool, ToolKind::Rectangle);
    state.apply_command_menu_choice(window.0, &[], Ok(Some(ToolbarItem::Tool(ToolKind::Blur))));
    assert_eq!(state.active_tool, ToolKind::Blur);
    assert_eq!(state.tool_before_pick, None);
    state.apply_command_menu_choice(
        window.0,
        &[],
        Ok(Some(ToolbarItem::Tool(ToolKind::ColorPicker))),
    );
    assert_eq!(state.tool_before_pick, Some(ToolKind::Blur));
    assert_eq!(state.settings.last_tool, ToolKind::Blur);
}

#[test]
fn a_click_with_the_picker_copies_the_original_pixel_and_returns_to_the_tool() {
    let window = TestWindow::new();
    let mut state = picker_state();
    let selection = state.committed_selection();
    state.pick_color(window.0, (10, 20));
    assert_eq!(clipboard_text().as_deref(), Some("#FEAB10"));
    let notice = state.notice.clone().expect("a notice confirms the copy");
    assert!(notice.contains("#FEAB10"), "{notice}");
    assert_eq!(state.active_tool, ToolKind::Rectangle);
    assert_eq!(state.tool_before_pick, None);
    assert_eq!(state.committed_selection(), selection);
    assert_eq!(state.object_count(), 1);
    assert!(state.scene_dirty);
    // The only history entry is still the rectangle: picking recorded nothing.
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 0);
    assert!(!state.history.can_undo());
}

#[test]
fn a_failed_copy_keeps_the_picker_and_reports_the_error() {
    let window = TestWindow::new();
    let mut state = picker_state();
    let _hold = ClipboardHold::new();
    state.pick_color(window.0, (10, 20));
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Copy failed"));
    assert_eq!(state.active_tool, ToolKind::ColorPicker);
    assert!(state.notice.is_none());
}

#[test]
fn a_click_outside_the_capture_picks_nothing() {
    let window = TestWindow::new();
    let mut state = picker_state();
    for point in [(-1, 5), (5, -1), (100, 5), (5, 100)] {
        state.pick_color(window.0, point);
    }
    assert_eq!(state.active_tool, ToolKind::ColorPicker);
    assert!(state.notice.is_none());
}

#[test]
fn a_notice_goes_away_by_itself_and_a_newer_one_replaces_it() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.show_notice(window.0, "first".to_string());
    state.show_notice(window.0, "second".to_string());
    assert_eq!(state.notice.as_deref(), Some("second"));
    assert!(state.scene_dirty);
    assert!(window.wait_for_timer(
        NOTICE_TIMER,
        std::time::Duration::from_millis(u64::from(NOTICE_MS) + 2000)
    ));
    state.scene_dirty = false;
    state.clear_notice(window.0);
    assert_eq!(state.notice, None);
    assert!(state.scene_dirty, "the frame without the notice is drawn");
    state.scene_dirty = false;
    state.clear_notice(window.0);
    assert!(!state.scene_dirty, "nothing was showing");
}

#[test]
fn escape_cancels_the_picker_before_anything_else() {
    let mut state = picker_state();
    assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
    assert_eq!(state.active_tool, ToolKind::Rectangle);
    assert_eq!(state.mode(), OverlayMode::SelectionActive);
    assert_eq!(state.object_count(), 1);
}

#[test]
fn the_picker_keeps_the_plain_arrow_cursor_even_over_the_grips() {
    let mut state = picker_state();
    for point in [(5, 5), (30, 30), (80, 80)] {
        assert_eq!(state.editor_cursor(point), IDC_ARROW, "{point:?}");
    }
    state.select_tool(ToolKind::Rectangle);
    assert_eq!(state.editor_cursor((30, 30)), IDC_CROSS);
}

#[test]
fn the_picker_is_never_the_tool_a_new_editor_opens_with() {
    assert_eq!(initial_tool(ToolKind::ColorPicker), ToolKind::default());
    assert_eq!(initial_tool(ToolKind::Pen), ToolKind::Pen);
}

// ---- last region and preselection ---------------------------------------------------

#[test]
fn every_export_remembers_its_region_in_screen_coordinates() {
    let window = TestWindow::new();
    let folder = TempDir::new("last-region");
    for export in 0..3 {
        crate::capture::forget_last_region();
        let mut state = export_state(&folder);
        Rc::get_mut(&mut state.capture).expect("unshared capture").x = 40;
        let outcome = match export {
            0 => state.copy_selection_to_clipboard(window.0),
            1 => state.save_selection_to_file().map(|_| ()),
            _ => state.finish_upload(window.0),
        };
        assert_eq!(outcome, Ok(()), "export {export}");
        assert_eq!(
            crate::capture::last_region(),
            Some(Rect::new(140, 100, 540, 400)),
            "export {export}"
        );
        assert!(state.committed_result);
    }
}

#[test]
fn marking_an_export_uses_the_capture_origin_and_needs_a_selection() {
    crate::capture::forget_last_region();
    let mut state = plain_state();
    state.mark_exported();
    assert!(state.committed_result);
    assert_eq!(crate::capture::last_region(), None);
    Rc::get_mut(&mut state.capture).expect("unshared capture").x = -20;
    Rc::get_mut(&mut state.capture).expect("unshared capture").y = 30;
    state.set_selection_active(Rect::new(100, 100, 500, 400));
    state.mark_exported();
    assert_eq!(
        crate::capture::last_region(),
        Some(Rect::new(80, 130, 480, 430))
    );
}

#[test]
fn a_failed_export_leaves_the_last_region_alone() {
    let window = TestWindow::new();
    let folder = TempDir::new("last-region-failed");
    crate::capture::forget_last_region();
    let mut state = export_state(&folder);
    state.committed_selection = Some(Rect::new(1000, 1000, 1100, 1100));
    assert!(state.copy_selection_to_clipboard(window.0).is_err());
    assert_eq!(crate::capture::last_region(), None);
    assert!(!state.committed_result);
}

#[test]
fn a_preselected_region_opens_as_an_active_selection_for_the_first_paint() {
    let mut state = plain_state();
    state.preselect(Rect::new(10, 20, 110, 220));
    assert_eq!(state.mode(), OverlayMode::SelectionActive);
    assert_eq!(
        state.committed_selection(),
        Some(Rect::new(10, 20, 110, 220))
    );
    assert!(state.scene_dirty);
}
