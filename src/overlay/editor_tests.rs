//! Editor state: text boxes, the selection, cursors, colours and widths, the opening fade,
//! timers and the monitor queries.
use super::test_support::*;
use super::*;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::UI::WindowsAndMessaging::GetLayeredWindowAttributes;

const INK: [u8; 4] = [9, 8, 7, 255];

fn text_object(id: usize, pos: (i32, i32), text: &str) -> AnnotationObject {
    AnnotationObject::new(
        id,
        AnnotationKind::Text {
            pos,
            text: text.to_string(),
            color: INK,
            font_size: 22,
        },
    )
}

fn text_of(object: &AnnotationObject) -> &str {
    match &object.kind {
        AnnotationKind::Text { text, .. } => text,
        other => panic!("expected text, found {other:?}"),
    }
}

/// A full-capture selection holding the text objects `5` ("Hello") at (100, 100).
fn state_with_text() -> OverlayState {
    let mut state = plain_state();
    state.set_selection_active(Rect::new(0, 0, 800, 600));
    let object = text_object(5, (100, 100), "Hello");
    state.history.record(EditCommand::Add(object.clone()));
    state.objects.push(object);
    state.next_id = 6;
    state
}

fn open_text(state: &mut OverlayState, text: &str, editing_id: Option<usize>) {
    state.set_text_edit(Some(TextEditState::new(
        (200, 200),
        text.to_string(),
        INK,
        22,
        editing_id,
    )));
}

fn primary_monitor() -> HMONITOR {
    unsafe {
        MonitorFromPoint(
            windows::Win32::Foundation::POINT { x: 0, y: 0 },
            MONITOR_DEFAULTTOPRIMARY,
        )
    }
}

// ---- text boxes --------------------------------------------------------------------

#[test]
fn committing_without_a_text_box_does_nothing() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    state.commit_text(window.0);
    assert_eq!(state.object_count(), 1);
    assert!(!state.scene_dirty);
}

#[test]
fn a_new_text_box_left_blank_leaves_no_object() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    open_text(&mut state, "  \t ", None);
    set_overlay_text_editing(true);
    state.commit_text(window.0);
    assert!(state.text_edit().is_none());
    assert_eq!(state.object_count(), 1);
    assert_eq!(state.selected_id(), None);
    assert!(!crate::hotkey::is_overlay_text_editing());
    assert!(state.scene_dirty);
}

#[test]
fn clearing_an_existing_text_deletes_it_and_undo_brings_it_back() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    state.set_selected_id(Some(5));
    open_text(&mut state, "   ", Some(5));
    state.commit_text(window.0);
    assert_eq!(state.object_count(), 0);
    assert_eq!(state.selected_id(), None);
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 1);
    assert_eq!(text_of(&state.objects[0]), "Hello");
}

#[test]
fn clearing_a_text_whose_object_is_gone_deletes_nothing() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    open_text(&mut state, "", Some(77));
    state.commit_text(window.0);
    assert_eq!(state.object_count(), 1);
}

#[test]
fn typed_text_becomes_an_object_with_the_next_id() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    open_text(&mut state, "Hello", None);
    state.commit_text(window.0);
    assert_eq!(state.object_count(), 2);
    let object = &state.objects[1];
    assert_eq!(object.id, 2);
    assert_eq!(
        object.kind,
        AnnotationKind::Text {
            pos: (200, 200),
            text: "Hello".to_string(),
            color: INK,
            font_size: 22,
        }
    );
    assert_eq!(state.next_id, 3);
    assert_eq!(state.selected_id(), None);
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 1);
}

#[test]
fn editing_a_text_records_the_change_and_keeps_it_selected_with_the_select_tool() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    state.active_tool = ToolKind::Select;
    state.set_text_edit(Some(TextEditState::new(
        (100, 100),
        "Goodbye".to_string(),
        INK,
        22,
        Some(5),
    )));
    state.commit_text(window.0);
    assert_eq!(text_of(&state.objects[0]), "Goodbye");
    assert_eq!(state.selected_id(), Some(5));
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(text_of(&state.objects[0]), "Hello");
}

#[test]
fn editing_a_text_with_a_drawing_tool_leaves_nothing_selected() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    state.set_text_edit(Some(TextEditState::new(
        (100, 100),
        "Goodbye".to_string(),
        INK,
        22,
        Some(5),
    )));
    state.commit_text(window.0);
    assert_eq!(text_of(&state.objects[0]), "Goodbye");
    assert_eq!(state.selected_id(), None);
}

#[test]
fn committing_an_unchanged_text_adds_no_history_entry() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    state.set_text_edit(Some(TextEditState::new(
        (100, 100),
        "Hello".to_string(),
        INK,
        22,
        Some(5),
    )));
    state.commit_text(window.0);
    // The newest entry is still the text's own creation: undoing it removes the object.
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 0);
}

#[test]
fn a_text_whose_object_vanished_is_put_back_as_a_modification() {
    let window = TestWindow::new();
    let mut state = state_with_rectangle();
    let mut edit = TextEditState::new((120, 130), "original".to_string(), INK, 22, Some(42));
    edit.text = "changed".to_string();
    state.set_text_edit(Some(edit));
    state.commit_text(window.0);
    assert_eq!(state.object_count(), 2);
    assert_eq!(state.objects[1].id, 42);
    assert_eq!(text_of(&state.objects[1]), "changed");
    assert!(state.history.can_undo());
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(text_of(&state.objects[1]), "original");
}

#[test]
fn double_clicking_a_text_opens_it_with_the_caret_at_the_end() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    state.set_selected_id(Some(5));
    state.dragging_object = Some(DragObjectState {
        id: 5,
        action: DragObjectAction::Move,
        last_pos: (0, 0),
        original_kind: state.objects[0].kind.clone(),
    });
    assert!(state.begin_text_reedit(window.0, (104, 108), false));
    let edit = state.text_edit().expect("the text box is open");
    assert_eq!(edit.text, "Hello");
    assert_eq!(edit.editing_id, Some(5));
    assert_eq!(edit.caret, 5);
    assert_eq!(edit.color, INK);
    assert_eq!(state.selected_id(), None);
    assert!(state.dragging_object.is_none());
    assert!(crate::hotkey::is_overlay_text_editing());
    assert!(state.scene_dirty);
}

#[test]
fn a_single_click_puts_the_caret_where_it_landed() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    let boundary = 100 + crate::drawing::measure_text("He", 22).0;
    assert!(state.begin_text_reedit(window.0, (boundary, 108), true));
    assert_eq!(state.text_edit().unwrap().caret, 2);
}

#[test]
fn only_text_objects_under_the_pointer_open_for_editing() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    state.add_test_rectangle(Rect::new(300, 300, 400, 400));
    assert!(!state.begin_text_reedit(window.0, (350, 300), false));
    assert!(!state.begin_text_reedit(window.0, (700, 500), false));
    assert!(state.text_edit().is_none());
}

#[test]
fn the_topmost_of_overlapping_texts_is_edited() {
    let window = TestWindow::new();
    let mut state = state_with_text();
    state.objects.push(text_object(6, (100, 100), "Above"));
    assert!(state.begin_text_reedit(window.0, (104, 108), false));
    assert_eq!(state.text_edit().unwrap().editing_id, Some(6));
}

// ---- selection ---------------------------------------------------------------------

#[test]
fn a_region_smaller_than_eight_pixels_is_not_a_selection() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.hover_snap_rect = Some(Rect::new(0, 0, 50, 50));
    state.commit_selection(window.0, Rect::new(10, 10, 17, 80));
    assert_eq!(state.mode(), OverlayMode::Hovering);
    assert_eq!(state.committed_selection(), None);
    state.commit_selection(window.0, Rect::new(10, 10, 80, 17));
    assert_eq!(state.mode(), OverlayMode::Hovering);
    assert!(state.scene_dirty);
}

#[test]
fn a_region_is_clamped_to_the_capture_and_becomes_the_selection() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.hover_snap_rect = Some(Rect::new(0, 0, 50, 50));
    state.set_selected_id(Some(3));
    state.commit_selection(window.0, Rect::new(-20, -20, 900, 700));
    assert_eq!(state.mode(), OverlayMode::SelectionActive);
    assert_eq!(state.committed_selection(), Some(Rect::new(0, 0, 800, 600)));
    assert_eq!(state.hover_snap_rect, None);
    assert_eq!(state.selected_id(), None);
    assert!(state.scene_dirty);
}

#[test]
fn a_full_redraw_drops_the_cached_base_layer() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.base_cache = vec![1, 2, 3];
    state.redraw(window.0);
    assert!(state.base_cache.is_empty());
    assert!(state.scene_dirty);
    assert!(!state.cache_requested);

    state.base_cache = vec![1, 2, 3];
    state.scene_dirty = false;
    state.redraw_region(window.0, Rect::new(0, 0, 10, 10));
    assert!(state.base_cache.is_empty());
    assert!(state.scene_dirty);
}

#[test]
fn a_chrome_redraw_keeps_the_cache_and_asks_for_a_new_one() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.base_cache = vec![1, 2, 3];
    state.redraw_chrome(window.0);
    assert_eq!(state.base_cache, [1, 2, 3]);
    assert!(state.scene_dirty);
    assert!(state.cache_requested);
}

#[test]
fn smoke_hooks_read_and_write_the_state() {
    let mut state = plain_state();
    assert_eq!(state.mode(), OverlayMode::Hovering);
    state.set_selection_active(Rect::new(10, 10, 200, 100));
    assert_eq!(state.mode(), OverlayMode::SelectionActive);
    state.set_selected_id(Some(4));
    assert_eq!(state.selected_id(), Some(4));
    open_text(&mut state, "x", None);
    assert!(state.text_edit().is_some());
    state.set_text_edit(None);
    assert!(state.text_edit().is_none());
    assert!(!state.has_active_gesture());
    state.begin_test_selection_move();
    assert!(state.has_active_gesture());
    assert_eq!(state.handle_escape_action(), EscapeAction::CancelledGesture);
}

#[test]
fn a_test_selection_move_needs_a_selection() {
    let mut state = plain_state();
    state.begin_test_selection_move();
    assert!(!state.has_active_gesture());
}

#[test]
fn action_errors_are_queued_for_the_window() {
    let window = TestWindow::new();
    OverlayState::show_action_error(window.0, "Copy failed", "no clipboard");
    assert_eq!(
        take_action_error(),
        Some(("Copy failed".to_string(), "no clipboard".to_string()))
    );
    assert!(window.take(WM_EDITOR_ERROR).is_some());
}

// ---- cursors -----------------------------------------------------------------------

fn cursor_state() -> OverlayState {
    let mut state = state_with_rectangle();
    state.active_tool = ToolKind::Select;
    state
}

#[test]
fn the_cursor_is_a_crosshair_until_a_region_is_chosen() {
    let mut state = plain_state();
    assert_eq!(state.editor_cursor((50, 50)), IDC_CROSS);
    state.mode = OverlayMode::SelectionActive;
    assert_eq!(state.editor_cursor((50, 50)), IDC_CROSS);
}

#[test]
fn the_cursor_is_an_i_beam_while_a_text_box_is_open() {
    let mut state = cursor_state();
    open_text(&mut state, "abc", None);
    assert_eq!(state.editor_cursor((300, 300)), IDC_IBEAM);
}

#[test]
fn the_cursor_shows_which_way_a_grip_resizes_the_selection() {
    let state = cursor_state();
    assert_eq!(state.editor_cursor((95, 95)), IDC_SIZENWSE);
    assert_eq!(state.editor_cursor((505, 405)), IDC_SIZENWSE);
    assert_eq!(state.editor_cursor((505, 95)), IDC_SIZENESW);
    assert_eq!(state.editor_cursor((95, 405)), IDC_SIZENESW);
    assert_eq!(state.editor_cursor((300, 95)), IDC_SIZENS);
    assert_eq!(state.editor_cursor((300, 405)), IDC_SIZENS);
    assert_eq!(state.editor_cursor((95, 250)), IDC_SIZEWE);
    assert_eq!(state.editor_cursor((505, 250)), IDC_SIZEWE);
}

#[test]
fn the_cursor_moves_the_selection_from_its_border_and_the_size_readout() {
    let mut state = cursor_state();
    assert_eq!(state.editor_cursor((200, 98)), IDC_SIZEALL);
    state.hud_rect = Some(Rect::new(120, 40, 256, 68));
    assert_eq!(state.editor_cursor((130, 50)), IDC_SIZEALL);
    assert_eq!(state.editor_cursor((10, 10)), IDC_CROSS);
}

#[test]
fn drawing_tools_only_act_inside_the_selection() {
    let mut state = cursor_state();
    state.active_tool = ToolKind::Rectangle;
    assert_eq!(state.editor_cursor((300, 300)), IDC_CROSS);
    assert_eq!(state.editor_cursor((10, 10)), IDC_ARROW);
    state.active_tool = ToolKind::Text;
    assert_eq!(state.editor_cursor((300, 300)), IDC_IBEAM);
    assert_eq!(state.editor_cursor((10, 10)), IDC_ARROW);
}

#[test]
fn the_select_tool_shows_move_over_objects_and_an_arrow_elsewhere() {
    let state = cursor_state();
    assert_eq!(state.editor_cursor((200, 200)), IDC_SIZEALL);
    assert_eq!(state.editor_cursor((400, 300)), IDC_ARROW);
}

#[test]
fn the_cursor_shows_which_way_a_selected_object_grip_resizes_it() {
    let mut state = cursor_state();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    // The 4 px stroke reaches 2 px beyond the rectangle and the gap adds 4 more.
    assert_eq!(state.editor_cursor((144, 144)), IDC_SIZENWSE);
    assert_eq!(state.editor_cursor((256, 256)), IDC_SIZENWSE);
    assert_eq!(state.editor_cursor((256, 144)), IDC_SIZENESW);
    assert_eq!(state.editor_cursor((144, 256)), IDC_SIZENESW);
    // The body of a selected object moves it; empty selection area is plain.
    assert_eq!(state.editor_cursor((200, 200)), IDC_SIZEALL);
    assert_eq!(state.editor_cursor((400, 300)), IDC_ARROW);
}

#[test]
fn the_cursor_moves_a_selected_arrow_from_either_end() {
    let mut state = cursor_state();
    state.objects.push(AnnotationObject::new(
        9,
        AnnotationKind::Arrow {
            start: (180, 300),
            end: (300, 350),
            color: INK,
            thickness: 4,
        },
    ));
    state.set_selected_id(Some(9));
    assert_eq!(state.editor_cursor((180, 300)), IDC_SIZEALL);
    assert_eq!(state.editor_cursor((300, 350)), IDC_SIZEALL);
}

#[test]
fn the_topmost_stroke_wins_over_the_interior_of_a_frame_below_it() {
    let mut state = state_with_rectangle();
    // A small rectangle drawn on top of the big hollow frame.
    state.add_test_rectangle(Rect::new(200, 200, 220, 220));
    let top = state.objects[1].id;
    let inside_both = (210, 210);
    assert_eq!(
        state.topmost_object_at(inside_both).map(|o| o.id),
        Some(top)
    );
    // On the big frame's stroke but outside the small one: the frame.
    let stroke = (150, 200);
    assert_eq!(
        state.topmost_object_at(stroke).map(|o| o.id),
        Some(state.objects[0].id)
    );
    assert!(state.topmost_object_at((700, 500)).is_none());
}

// ---- colour and width --------------------------------------------------------------

#[test]
fn a_new_colour_recolours_the_selected_object_as_one_undo_step() {
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    state.change_color([1, 2, 3, 255]);
    assert_eq!(state.active_color, [1, 2, 3, 255]);
    assert_eq!(state.objects[0].get_color(), Some([1, 2, 3, 255]));
    assert_eq!(state.settings.default_color, [1, 2, 3, 255]);
    assert!(state.preferences_dirty);
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.objects[0].get_color(), Some([49, 49, 224, 255]));
}

#[test]
fn picking_the_colour_an_object_already_has_records_nothing() {
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    state.change_color([49, 49, 224, 255]);
    // The newest history entry is still the rectangle's creation.
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 0);
}

#[test]
fn a_new_colour_without_a_selection_only_changes_the_pen() {
    let mut state = state_with_rectangle();
    state.change_color([5, 6, 7, 255]);
    assert_eq!(state.active_color, [5, 6, 7, 255]);
    assert_eq!(state.objects[0].get_color(), Some([49, 49, 224, 255]));
}

#[test]
fn a_new_width_is_clamped_and_applied_to_the_selected_object() {
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    state.change_thickness(500, true);
    assert_eq!(state.active_thickness, 64);
    assert_eq!(state.objects[0].get_thickness(), Some(64));
    assert_eq!(state.settings.default_thickness, 64);
    state.change_thickness(-3, true);
    assert_eq!(state.active_thickness, 1);
    // Widths changed in quick succession are one undo step.
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.objects[0].get_thickness(), Some(4));
}

#[test]
fn an_unrecorded_width_change_leaves_history_and_preferences_alone() {
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    state.change_thickness(9, false);
    assert_eq!(state.objects[0].get_thickness(), Some(9));
    assert!(!state.preferences_dirty);
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 0);
}

#[test]
fn a_width_that_the_object_already_has_records_nothing() {
    let mut state = state_with_rectangle();
    let id = state.objects[0].id;
    state.set_selected_id(Some(id));
    let current = state.active_thickness;
    state.change_thickness(current, true);
    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 0);
}

#[test]
fn a_width_without_a_selection_only_changes_the_pen() {
    let mut state = state_with_rectangle();
    state.change_thickness(7, true);
    assert_eq!(state.active_thickness, 7);
    assert_eq!(state.objects[0].get_thickness(), Some(4));
}

// ---- fade, timers, monitors ---------------------------------------------------------

fn window_alpha(window: &TestWindow) -> u8 {
    let mut alpha = 0u8;
    unsafe {
        GetLayeredWindowAttributes(window.0, None, Some(std::ptr::from_mut(&mut alpha)), None)
    }
    .expect("a layered window");
    alpha
}

#[test]
fn the_fade_steps_up_the_opacity_while_the_timer_keeps_running() {
    let window = TestWindow::new();
    let mut state = plain_state();
    // A start in the future saturates to zero elapsed time.
    state.fade_started = Some(Instant::now() + Duration::from_secs(3600));
    unsafe {
        SetTimer(window.0, FADE_TIMER, 10, None);
    }
    state.advance_fade(window.0);
    assert_eq!(window_alpha(&window), fade_alpha(0));
    assert!(state.fade_started.is_some());
    assert!(window.wait_for_timer(FADE_TIMER, Duration::from_millis(1000)));
}

#[test]
fn a_finished_fade_is_opaque_and_stops_its_timer() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.fade_started = Instant::now().checked_sub(Duration::from_secs(5));
    assert!(state.fade_started.is_some());
    unsafe {
        SetTimer(window.0, FADE_TIMER, 10, None);
    }
    state.advance_fade(window.0);
    assert_eq!(window_alpha(&window), 255);
    assert!(state.fade_started.is_none());
    assert!(!window.wait_for_timer(FADE_TIMER, Duration::from_millis(150)));
}

#[test]
fn a_fade_that_was_never_started_is_already_complete() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.advance_fade(window.0);
    assert_eq!(window_alpha(&window), 255);
}

#[test]
fn the_caret_timer_runs_at_the_given_interval_until_it_is_stopped() {
    let window = TestWindow::new();
    set_caret_timer(window.0, Some(15));
    assert!(window.wait_for_timer(CARET_TIMER, Duration::from_millis(1000)));
    set_caret_timer(window.0, None);
    assert!(!window.wait_for_timer(CARET_TIMER, Duration::from_millis(120)));
}

#[test]
fn ending_text_input_stops_the_caret_and_hands_back_the_keyboard() {
    let window = TestWindow::new();
    set_caret_timer(window.0, Some(15));
    set_overlay_text_editing(true);
    set_overlay_ime_composing(true);
    stop_text_input(window.0);
    assert!(!crate::hotkey::is_overlay_text_editing());
    assert!(!window.wait_for_timer(CARET_TIMER, Duration::from_millis(120)));
}

#[test]
fn a_selection_maps_to_screen_coordinates_through_the_capture_origin() {
    let mut capture = CaptureBuffer::dummy(800, 600);
    capture.x = -100;
    capture.y = 50;
    let screen = selection_screen_rect(&capture, Rect::new(10, 20, 110, 70));
    assert_eq!(
        (screen.left, screen.top, screen.right, screen.bottom),
        (-90, 70, 10, 120)
    );
}

#[test]
fn a_monitor_reports_its_dpi_and_an_unknown_one_the_fallback() {
    assert!(monitor_dpi(primary_monitor(), 77) >= 96);
    assert_eq!(monitor_dpi(HMONITOR::default(), 77), 77);
}

#[test]
fn the_selection_dpi_is_that_of_the_monitor_under_it() {
    let capture = CaptureBuffer::dummy(800, 600);
    let selection = Rect::new(10, 10, 100, 100);
    assert_eq!(
        selection_monitor_dpi(&capture, selection, 77),
        monitor_dpi(primary_monitor(), 77)
    );
}

#[test]
fn the_work_area_is_limited_to_the_capture_and_defaults_to_all_of_it() {
    let capture = CaptureBuffer::dummy(800, 600);
    let viewport = monitor_work_viewport(primary_monitor(), &capture);
    assert!(!viewport.is_empty());
    assert!(viewport.left >= 0 && viewport.top >= 0);
    assert!(viewport.right <= 800 && viewport.bottom <= 600);
    assert_eq!(
        selection_work_viewport(&capture, Rect::new(10, 10, 100, 100)),
        viewport
    );
    assert_eq!(
        monitor_work_viewport(HMONITOR::default(), &capture),
        Rect::new(0, 0, 800, 600)
    );
}

#[test]
fn dragging_starts_after_the_system_threshold_which_grows_with_the_dpi() {
    let normal = drag_threshold(96);
    assert!(normal.0 >= 1 && normal.1 >= 1);
    let large = drag_threshold(192);
    assert!(large.0 >= normal.0 && large.1 >= normal.1);
}

#[test]
fn object_grips_scale_with_the_dpi() {
    assert_eq!(object_grip_radius(96), 4);
    assert_eq!(object_grip_radius(192), 8);
}

#[test]
fn letters_select_tools() {
    for (letter, tool) in [
        ('v', ToolKind::Select),
        ('r', ToolKind::Rectangle),
        ('a', ToolKind::Arrow),
        ('e', ToolKind::Ellipse),
        ('l', ToolKind::Line),
        ('p', ToolKind::Pen),
        ('t', ToolKind::Text),
        ('h', ToolKind::Highlight),
        ('n', ToolKind::Step),
        ('b', ToolKind::Blur),
        ('m', ToolKind::Redact),
    ] {
        assert_eq!(tool_for_key(letter.to_ascii_uppercase() as u32), Some(tool));
    }
    assert_eq!(tool_for_key('Q' as u32), None);
    assert_eq!(tool_for_key(0), None);
}

#[test]
fn shortcut_keys_depend_on_modifiers_and_text_editing() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_WIN};
    // Alt and Win combinations belong to Windows.
    assert!(!is_editor_shortcut(0x43, MOD_CONTROL.0 | MOD_ALT.0, false));
    assert!(!is_editor_shortcut(0x43, MOD_WIN.0, false));
    // Ctrl+C, S, U, Z, Y, comma and the arrows always count.
    for vk in [0x43, 0x53, 0x55, 0x5a, 0x59, 0xbc, 37, 40] {
        assert!(is_editor_shortcut(vk, MOD_CONTROL.0, false), "vk {vk:#x}");
    }
    assert!(!is_editor_shortcut(0x51, MOD_CONTROL.0, false));
    // Plain keys: editing keys always, tool letters only outside text boxes.
    for vk in [8, 13, 27, 35, 40, 46] {
        assert!(is_editor_shortcut(vk, 0, true), "vk {vk:#x}");
    }
    assert!(is_editor_shortcut(0x52, 0, false));
    assert!(!is_editor_shortcut(0x52, 0, true));
    assert!(!is_editor_shortcut(0x51, 0, false));
}
