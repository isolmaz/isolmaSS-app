//! Copy text (Ctrl+Shift+C) and Redact emails and phone numbers (Ctrl+Shift+R): how the
//! commands start, that a second request waits, and what a finished recognition changes.
//! The engine is canned, so no recognition language is needed.
use super::recognition::RecognitionJob;
use super::test_support::*;
use super::*;
use crate::ocr::{Engine, OcrFailure, OcrImage, OcrLine, OcrWord, Recognized, WordBox};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT;

const CTRL: isize = MOD_CONTROL.0 as isize;
const SHIFT: isize = MOD_SHIFT.0 as isize;

/// Answers every request with `result` and keeps the images it was given.
struct Canned {
    result: Recognized,
    seen: Mutex<Vec<OcrImage>>,
}

impl Engine for Canned {
    fn max_dimension(&self) -> u32 {
        u32::MAX
    }

    fn recognize(&self, image: &OcrImage) -> Recognized {
        self.seen.lock().unwrap().push(image.clone());
        self.result.clone()
    }
}

/// Waits for a signal before it answers, so a recognition can be held up.
struct Gated {
    release: Mutex<mpsc::Receiver<()>>,
    calls: AtomicUsize,
}

impl Engine for Gated {
    fn max_dimension(&self) -> u32 {
        u32::MAX
    }

    fn recognize(&self, _image: &OcrImage) -> Recognized {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.release.lock().unwrap().recv();
        Ok(Vec::new())
    }
}

fn word(text: &str, left: f32, top: f32, right: f32, bottom: f32) -> OcrLine {
    OcrLine {
        text: text.to_string(),
        words: vec![OcrWord {
            text: text.to_string(),
            bounds: WordBox::new(left, top, right, bottom),
        }],
    }
}

/// An e-mail address and a phone number, in pixels of the selection.
fn sensitive_lines() -> Vec<OcrLine> {
    vec![
        word("john@example.com", 10.0, 20.0, 110.0, 34.0),
        word("5551234567", 10.0, 50.0, 70.0, 64.0),
    ]
}

// The selection starts at (100, 100). A word box spans [left, right) x [top, bottom) of the
// selection's pixels, so the redaction is the box moved by that origin and grown by the
// 2 px of padding on every side: x 10..110 becomes 108..212, y 20..34 becomes 118..136.
const EMAIL_RECT: Rect = Rect::new(108, 118, 212, 136);
const PHONE_RECT: Rect = Rect::new(108, 148, 172, 166);

/// A state with a real back buffer, a selection and one rectangle, that stays open after
/// actions, recognizing with a canned engine.
fn state_with(result: Recognized) -> (OverlayState, Arc<Canned>) {
    let engine = Arc::new(Canned {
        result,
        seen: Mutex::new(Vec::new()),
    });
    let mut state = backbuffer_state(640, 480);
    state.set_selection_active(Rect::new(100, 100, 500, 400));
    state.add_test_rectangle(Rect::new(150, 150, 250, 250));
    state.settings.close_after_action = false;
    state.recognition.engine = engine.clone();
    (state, engine)
}

fn gated_state() -> (OverlayState, Arc<Gated>, mpsc::Sender<()>) {
    let (release, released) = mpsc::channel();
    let engine = Arc::new(Gated {
        release: Mutex::new(released),
        calls: AtomicUsize::new(0),
    });
    let mut state = backbuffer_state(640, 480);
    state.set_selection_active(Rect::new(100, 100, 500, 400));
    state.settings.close_after_action = false;
    state.recognition.engine = engine.clone();
    (state, engine, release)
}

fn press(state: &mut OverlayState, window: &TestWindow, letter: char, mods: isize) {
    let vk = letter.to_ascii_uppercase() as usize;
    assert_eq!(state.handle_key_down(window.0, vk, mods).0, 0);
}

/// The id the worker reports once it has finished.
fn wait_for_result(window: &TestWindow) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(message) = window.take(WM_EDITOR_RECOGNIZED) {
            return message.lParam.0 as u64;
        }
        assert!(Instant::now() < deadline, "the worker answers");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits for the running job and applies its result, as the window procedure does.
fn settle(state: &mut OverlayState, window: &TestWindow) {
    let id = wait_for_result(window);
    state.finish_recognition(window.0, id);
}

fn job_intent(state: &OverlayState) -> Option<Intent> {
    state.recognition.job.as_ref().map(|job| job.intent)
}

fn redact_rects(state: &OverlayState) -> Vec<Rect> {
    state
        .objects
        .iter()
        .filter_map(|object| match object.kind {
            AnnotationKind::Redact { rect } => Some(rect),
            _ => None,
        })
        .collect()
}

#[test]
fn ctrl_shift_c_copies_the_text_of_the_selection() {
    let window = TestWindow::new();
    let lines = vec![
        word("Hello", 0.0, 0.0, 30.0, 10.0),
        word("world", 0.0, 12.0, 30.0, 22.0),
    ];
    let (mut state, _) = state_with(Ok(lines));
    press(&mut state, &window, 'c', CTRL | SHIFT);
    assert_eq!(job_intent(&state), Some(Intent::CopyText));
    settle(&mut state, &window);
    assert_eq!(raw_clipboard_text().as_deref(), Some("Hello\r\nworld"));
    assert!(state.recognition.job.is_none());
    assert!(state.committed_result);
    assert!(window.exists());
    assert!(state.scene_dirty);
    assert_eq!(
        state.notice.as_deref(),
        Some(crate::i18n::t("Text copied")),
        "the open editor confirms in its own notice"
    );
}

#[test]
fn copying_text_closes_the_editor_when_asked_to() {
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(vec![word("Hello", 0.0, 0.0, 30.0, 10.0)]));
    state.settings.close_after_action = true;
    press(&mut state, &window, 'c', CTRL | SHIFT);
    settle(&mut state, &window);
    assert!(!window.exists());
    assert_eq!(state.notice, None, "a closing editor shows no notice");
}

#[test]
fn copied_text_that_cannot_reach_the_clipboard_is_reported() {
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(vec![word("Hello", 0.0, 0.0, 30.0, 10.0)]));
    press(&mut state, &window, 'c', CTRL | SHIFT);
    {
        let _held = ClipboardHold::new();
        settle(&mut state, &window);
    }
    let (title, _) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Copy failed"));
    assert!(!state.committed_result);
    assert!(window.exists());
}

#[test]
fn the_engine_reads_the_composed_selection_with_its_redactions() {
    let window = TestWindow::new();
    let (mut state, engine) = state_with(Ok(Vec::new()));
    state.objects.push(AnnotationObject::new(
        50,
        AnnotationKind::Redact {
            rect: Rect::new(150, 150, 250, 250),
        },
    ));
    state.start_recognition(window.0, Intent::CopyText);
    settle(&mut state, &window);
    let seen = engine.seen.lock().unwrap();
    let image = &seen[0];
    assert_eq!(
        (image.origin, image.width, image.height),
        ((100, 100), 400, 300)
    );
    let at = |x: usize, y: usize| &image.bgra[(y * 400 + x) * 4..][..4];
    // Inside the redaction only its opaque colour is left, never the pixels under it.
    assert_eq!(at(75, 75)[..3], crate::theme::BRAND_INK_BGRA[..3]);
    assert_ne!(at(10, 10)[..3], crate::theme::BRAND_INK_BGRA[..3]);
    assert!(
        image
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    // The editor's own scene is drawn again for the next frame.
    assert!(state.toolbar.is_some());
}

#[test]
fn ctrl_shift_r_covers_addresses_and_numbers_in_one_undo_step() {
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(sensitive_lines()));
    press(&mut state, &window, 'r', CTRL | SHIFT);
    assert_eq!(job_intent(&state), Some(Intent::RedactSensitive));
    settle(&mut state, &window);
    assert_eq!(redact_rects(&state), vec![EMAIL_RECT, PHONE_RECT]);
    assert_eq!(state.object_count(), 3);
    assert!(window.exists());
    assert!(state.scene_dirty);
    assert_eq!(
        state.notice,
        Some(crate::i18n::tf("Redacted {} items", &[&2]))
    );

    assert!(state.history.undo(&mut state.objects));
    assert_eq!(state.object_count(), 1, "one undo removes both redactions");
    assert!(state.history.redo(&mut state.objects));
    assert_eq!(redact_rects(&state), vec![EMAIL_RECT, PHONE_RECT]);
}

#[test]
fn nothing_found_changes_nothing_and_keeps_the_editor_open() {
    let _ = take_action_error();
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(vec![word("plain", 0.0, 0.0, 30.0, 10.0)]));
    state.settings.close_after_action = true;
    press(&mut state, &window, 'r', CTRL | SHIFT);
    settle(&mut state, &window);
    assert_eq!(state.object_count(), 1);
    assert_eq!(
        state.notice.as_deref(),
        Some(crate::i18n::t("No email addresses or phone numbers found"))
    );
    state.notice = None;
    state.recognition.engine = Arc::new(Canned {
        result: Ok(Vec::new()),
        seen: Mutex::new(Vec::new()),
    });
    press(&mut state, &window, 'c', CTRL | SHIFT);
    settle(&mut state, &window);
    assert_eq!(
        state.notice.as_deref(),
        Some(crate::i18n::t("No text found in the selection"))
    );
    assert!(!state.committed_result);
    assert!(window.exists());
    assert!(take_action_error().is_none());
}

#[test]
fn a_missing_language_opens_a_dialog_and_an_engine_failure_an_error() {
    let _ = take_action_error();
    let window = TestWindow::new();
    let (mut state, _) = state_with(Err(OcrFailure::NoLanguage));
    state.start_recognition(window.0, Intent::CopyText);
    settle(&mut state, &window);
    let (title, message) = take_action_error().expect("the missing language is reported");
    assert_eq!(title, crate::i18n::t("Copy text"));
    assert_eq!(
        message,
        crate::i18n::t(
            "Windows has no text recognition language installed. Add a language under Settings › Time & language › Language & region."
        )
    );
    assert_eq!(state.notice, None, "a notice would vanish unread");
    assert!(state.recognition.job.is_none());

    let (mut state, _) = state_with(Err(OcrFailure::Failed("boom".into())));
    state.start_recognition(window.0, Intent::RedactSensitive);
    settle(&mut state, &window);
    let (title, message) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Text recognition failed"));
    assert_eq!(message, "boom");
}

#[test]
fn a_second_request_while_one_runs_is_ignored() {
    let window = TestWindow::new();
    let (mut state, engine, release) = gated_state();
    press(&mut state, &window, 'c', CTRL | SHIFT);
    let id = state.recognition.job.as_ref().expect("a job runs").id;
    press(&mut state, &window, 'r', CTRL | SHIFT);
    state.activate_toolbar_item(
        window.0,
        ToolbarItem::Action(ToolbarAction::CopyText),
        (0, 0),
    );
    let job = state
        .recognition
        .job
        .as_ref()
        .expect("the first job runs on");
    assert_eq!((job.id, job.intent), (id, Intent::CopyText));
    release.send(()).unwrap();
    settle(&mut state, &window);
    assert_eq!(engine.calls.load(Ordering::SeqCst), 1);
    assert!(state.recognition.job.is_none());
}

#[test]
fn closing_the_editor_while_recognizing_drops_the_result() {
    let window = TestWindow::new();
    let (mut state, engine, release) = gated_state();
    state.start_recognition(window.0, Intent::CopyText);
    unsafe {
        DestroyWindow(window.0).expect("the window closes");
    }
    drop(state);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    // The worker lets go of the engine, and so of everything it held, once it is done.
    while Arc::strong_count(&engine) > 1 {
        assert!(Instant::now() < deadline, "the worker finishes");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(window.take(WM_EDITOR_RECOGNIZED).is_none());
}

#[test]
fn a_result_for_another_job_is_dropped_and_an_empty_one_frees_the_editor() {
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(Vec::new()));
    let job = |id: u64, outcome: Option<Recognized>| RecognitionJob {
        id,
        intent: Intent::RedactSensitive,
        area: Rect::new(100, 100, 500, 400),
        outcome: Arc::new(Mutex::new(outcome)),
    };
    state.recognition.job = Some(job(5, Some(Ok(sensitive_lines()))));
    state.finish_recognition(window.0, 6);
    assert!(
        state.recognition.job.is_some(),
        "another job's result is not ours"
    );
    assert_eq!(state.object_count(), 1);
    state.finish_recognition(window.0, 5);
    assert!(state.recognition.job.is_none());
    assert_eq!(state.object_count(), 3);

    state.recognition.job = Some(job(7, None));
    state.finish_recognition(window.0, 7);
    assert!(state.recognition.job.is_none());
    assert_eq!(state.object_count(), 3);
}

#[test]
fn redactions_are_dropped_once_the_selection_has_moved() {
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(sensitive_lines()));
    state.start_recognition(window.0, Intent::RedactSensitive);
    state.committed_selection = Some(Rect::new(110, 100, 510, 400));
    settle(&mut state, &window);
    assert!(redact_rects(&state).is_empty());
    assert!(state.recognition.job.is_none());
    assert_eq!(state.notice, None);
}

#[test]
fn nothing_starts_without_a_usable_selection() {
    let window = TestWindow::new();
    let mut state = plain_state();
    state.start_recognition(window.0, Intent::CopyText);
    assert!(state.recognition.job.is_none());

    let (mut state, engine) = state_with(Ok(Vec::new()));
    state.set_selection_active(Rect::new(100, 100, 100, 100));
    state.start_recognition(window.0, Intent::CopyText);
    assert!(state.recognition.job.is_none());
    assert!(engine.seen.lock().unwrap().is_empty());
}

#[test]
fn a_worker_that_cannot_start_is_reported() {
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(Vec::new()));
    let job = RecognitionJob {
        id: 1,
        intent: Intent::CopyText,
        area: Rect::new(100, 100, 500, 400),
        outcome: Arc::default(),
    };
    state.adopt_recognition(window.0, job, Err(std::io::Error::other("no thread")));
    assert!(state.recognition.job.is_none());
    let (title, message) = take_action_error().expect("the failure is reported");
    assert_eq!(title, crate::i18n::t("Text recognition failed"));
    assert_eq!(message, "no thread");
}

#[test]
fn the_toolbar_items_and_the_command_menu_start_the_commands() {
    let window = TestWindow::new();
    let (mut state, _) = state_with(Ok(Vec::new()));
    state.activate_toolbar_item(
        window.0,
        ToolbarItem::Action(ToolbarAction::RedactSensitive),
        (0, 0),
    );
    assert_eq!(job_intent(&state), Some(Intent::RedactSensitive));
    settle(&mut state, &window);

    state.apply_command_menu_choice(
        window.0,
        &[],
        Ok(Some(ToolbarItem::Action(ToolbarAction::CopyText))),
    );
    assert_eq!(job_intent(&state), Some(Intent::CopyText));
    settle(&mut state, &window);
}

#[test]
fn a_text_box_is_committed_before_the_selection_is_read() {
    let window = TestWindow::new();
    for (letter, intent) in [('c', Intent::CopyText), ('r', Intent::RedactSensitive)] {
        let (mut state, engine) = state_with(Ok(Vec::new()));
        state.set_text_edit(Some(TextEditState::new(
            (200, 200),
            "Note".to_string(),
            [0, 0, 0, 255],
            22,
            None,
        )));
        set_overlay_text_editing(true);
        press(&mut state, &window, letter, CTRL | SHIFT);
        assert!(state.text_edit().is_none(), "{letter}");
        assert!(!crate::hotkey::is_overlay_text_editing(), "{letter}");
        assert_eq!(state.object_count(), 2, "the note is part of the image");
        assert_eq!(job_intent(&state), Some(intent));
        settle(&mut state, &window);
        assert_eq!(engine.seen.lock().unwrap().len(), 1);
    }
}
