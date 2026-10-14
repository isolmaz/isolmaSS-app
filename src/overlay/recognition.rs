//! Text recognition in the editor: Copy text (Ctrl+Shift+C) puts the text of the selection on
//! the clipboard, Redact emails and phone numbers (Ctrl+Shift+R) covers them. The selection is
//! read from the scene the export renders, so annotations, redactions included, are part of
//! what is recognized and nothing hidden leaks. The engine runs on a worker thread; its
//! result returns to the editor window as a message.
use super::*;
use crate::ocr::{Engine, OcrImage, Recognized, Resolution};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Ids of recognition jobs. They never repeat, so a result for an editor that was closed,
/// whose window handle Windows may have reused, is told from one this editor waits for.
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

/// Where the worker leaves its result.
type Outcome = Arc<Mutex<Option<Recognized>>>;

/// A recognition running on a worker thread.
pub(super) struct RecognitionJob {
    pub(super) id: u64,
    pub(super) intent: Intent,
    /// The selection, in capture pixels, whose pixels were read.
    pub(super) area: Rect,
    pub(super) outcome: Outcome,
}

pub(super) struct RecognitionState {
    pub(super) job: Option<RecognitionJob>,
    pub(super) engine: Arc<dyn Engine>,
}

impl RecognitionState {
    /// Recognition by the Windows OCR engine.
    pub(super) fn system() -> Self {
        Self {
            job: None,
            engine: Arc::new(crate::ocr::SystemEngine),
        }
    }
}

impl OverlayState {
    /// Reads the selection as the export renders it and recognizes its text in the
    /// background; the result returns as `WM_EDITOR_RECOGNIZED`. Nothing happens without a
    /// selection or while an earlier recognition is still running.
    pub(super) fn start_recognition(&mut self, hwnd: HWND, intent: Intent) {
        if self.recognition.job.is_some() {
            return;
        }
        let Some(selection) = self.committed_selection else {
            return;
        };
        self.commit_text(hwnd);
        self.composite_scene_with(CompositionPolicy::EXPORT);
        let (width, height) = (self.capture.width, self.capture.height);
        let frame = unsafe {
            std::slice::from_raw_parts(self.bits_ptr, width as usize * height as usize * 4)
        };
        let image = OcrImage::from_selection(frame, width, height, selection);
        self.composite_scene();
        let Some(image) = image else {
            return;
        };

        let id = NEXT_JOB.fetch_add(1, Ordering::Relaxed);
        let area = image.area();
        let outcome = Outcome::default();
        let engine = Arc::clone(&self.recognition.engine);
        let worker_outcome = Arc::clone(&outcome);
        let window = hwnd.0 as usize;
        let spawned = std::thread::Builder::new()
            .name("isolmass-ocr".into())
            .spawn(move || {
                let recognized = crate::ocr::recognize(engine.as_ref(), &image);
                if let Ok(mut slot) = worker_outcome.lock() {
                    *slot = Some(recognized);
                }
                // The editor may be closed by now; then there is nobody to tell.
                unsafe {
                    let _ = PostMessageW(
                        HWND(window as *mut _),
                        WM_EDITOR_RECOGNIZED,
                        WPARAM(0),
                        LPARAM(id as isize),
                    );
                }
            })
            .map(drop);
        let job = RecognitionJob {
            id,
            intent,
            area,
            outcome,
        };
        self.adopt_recognition(hwnd, job, spawned);
    }

    /// Takes over a started worker, or reports why none could start.
    pub(super) fn adopt_recognition(
        &mut self,
        hwnd: HWND,
        job: RecognitionJob,
        spawned: std::io::Result<()>,
    ) {
        match spawned {
            Ok(()) => self.recognition.job = Some(job),
            Err(error) => Self::show_action_error(
                hwnd,
                crate::i18n::t("Text recognition failed"),
                &error.to_string(),
            ),
        }
    }

    /// Applies the result of job `id`. A result for any other job, such as one started in an
    /// editor that has since closed, is dropped.
    pub(super) fn finish_recognition(&mut self, hwnd: HWND, id: u64) {
        let Some(job) = self.recognition.job.take_if(|job| job.id == id) else {
            return;
        };
        let recognized = job.outcome.lock().ok().and_then(|mut slot| slot.take());
        if let Some(recognized) = recognized {
            let resolution = crate::ocr::resolve(job.intent, recognized, job.area);
            self.apply_resolution(hwnd, job.area, resolution);
        }
    }

    fn apply_resolution(&mut self, hwnd: HWND, area: Rect, resolution: Resolution) {
        match resolution {
            Resolution::Copy { text, message } => self.copy_recognized_text(hwnd, &text, message),
            Resolution::Redact { rects, message } => {
                self.redact_recognized(hwnd, area, rects, message)
            }
            Resolution::Notice(message) => self.show_notice(hwnd, message),
            Resolution::Error { title, message } => Self::show_action_error(hwnd, title, &message),
        }
    }

    /// Puts `text` on the clipboard and, like Copy, closes the editor when asked to. An
    /// editor that stays open confirms in a notice; one that closes has no room for it, so
    /// the confirmation goes to the notification area.
    fn copy_recognized_text(&mut self, hwnd: HWND, text: &str, message: &str) {
        match crate::clipboard::copy_text_to_clipboard(Some(hwnd), text) {
            Ok(()) => {
                self.committed_result = true;
                if self.settings.close_after_action {
                    crate::tray::show_notification(Intent::CopyText.title(), message);
                    let _ = unsafe { DestroyWindow(hwnd) };
                } else {
                    self.show_notice(hwnd, message.to_owned());
                }
            }
            Err(error) => {
                Self::show_action_error(hwnd, crate::i18n::t("Copy failed"), &error.to_string())
            }
        }
    }

    /// Adds one opaque redaction per rectangle as a single undo step. The rectangles belong
    /// to the selection that was read; once it has moved or gone they would cover the wrong
    /// pixels, so they are dropped.
    fn redact_recognized(&mut self, hwnd: HWND, area: Rect, rects: Vec<Rect>, message: String) {
        if self.committed_selection != Some(area) {
            return;
        }
        let objects: Vec<AnnotationObject> = rects
            .into_iter()
            .map(|rect| {
                let object = AnnotationObject::new(self.next_id, AnnotationKind::Redact { rect });
                self.next_id += 1;
                object
            })
            .collect();
        self.history.record(EditCommand::AddMany(objects.clone()));
        self.objects.extend(objects);
        self.selected_id = None;
        self.redraw(hwnd);
        self.show_notice(hwnd, message);
    }
}
