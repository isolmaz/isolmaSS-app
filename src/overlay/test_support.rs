//! Helpers shared by the overlay tests: hidden windows with a readable message queue, a
//! state that draws into a real DIB section, pixel access and the process-wide resources
//! (clipboard, `%LOCALAPPDATA%`) the editor actions touch.
use super::*;
use std::sync::mpsc;
use windows::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindow, MSG, PM_REMOVE, PeekMessageW, WS_EX_LAYERED,
};

/// A hidden window on the test thread. The editor posts messages and sets timers on its
/// window; this one is a plain `STATIC` control, so everything posted stays in the thread
/// queue where [`TestWindow::take`] finds it.
pub(super) struct TestWindow(pub(super) HWND);

impl TestWindow {
    pub(super) fn new() -> Self {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("isolmaSS overlay test"),
                WS_POPUP,
                0,
                0,
                64,
                64,
                None,
                None,
                None,
                None,
            )
        }
        .expect("a hidden test window");
        Self(hwnd)
    }

    pub(super) fn exists(&self) -> bool {
        unsafe { IsWindow(self.0).as_bool() }
    }

    /// Removes and returns the next queued `message` for this window.
    pub(super) fn take(&self, message: u32) -> Option<MSG> {
        let mut msg = MSG::default();
        unsafe { PeekMessageW(&mut msg, self.0, message, message, PM_REMOVE) }
            .as_bool()
            .then_some(msg)
    }

    /// Whether `message` is queued, leaving it there.
    pub(super) fn has(&self, message: u32) -> bool {
        let mut msg = MSG::default();
        unsafe {
            PeekMessageW(
                &mut msg,
                self.0,
                message,
                message,
                windows::Win32::UI::WindowsAndMessaging::PM_NOREMOVE,
            )
        }
        .as_bool()
    }

    /// Waits up to `limit` for a `WM_TIMER` of timer `id`; timers only fire while the
    /// queue is polled.
    pub(super) fn wait_for_timer(&self, id: usize, limit: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if self.take(WM_TIMER).is_some_and(|msg| msg.wParam.0 == id) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        if self.exists() {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
}

/// Colour of the dimmed capture in test states: dark grey, opaque.
pub(super) const DIMMED: [u8; 4] = [40, 41, 42, 255];

/// A capture whose dimmed layer is uniformly [`DIMMED`]. The undimmed layer of a dummy
/// capture is all zero bytes, so a pixel read back from a punched-out area is
/// `[0, 0, 0, 0]` until something is drawn over it.
pub(super) fn dimmed_capture(width: i32, height: i32) -> Rc<CaptureBuffer> {
    let mut capture = CaptureBuffer::dummy(width, height);
    for pixel in capture.dimmed.as_chunks_mut::<4>().0 {
        *pixel = DIMMED;
    }
    Rc::new(capture)
}

/// A state without a pixel buffer, over a dummy capture.
pub(super) fn plain_state() -> OverlayState {
    OverlayState::create_test_state(dimmed_capture(800, 600))
}

/// A plain state with an active selection and one rectangle inside it.
pub(super) fn state_with_rectangle() -> OverlayState {
    let mut state = plain_state();
    state.set_selection_active(Rect::new(100, 100, 500, 400));
    state.add_test_rectangle(Rect::new(150, 150, 250, 250));
    state
}

/// A state that owns a 32-bit top-down DIB section selected into a memory DC, exactly
/// what `run_session` sets up, so scenes can be composed and read back. Dropping the
/// state releases the bitmap and the DC.
pub(super) fn backbuffer_state(width: i32, height: i32) -> OverlayState {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        RGBQUAD,
    };
    let mut state = OverlayState::create_test_state(dimmed_capture(width, height));
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        bmiColors: [RGBQUAD::default()],
    };
    unsafe {
        let mem_dc = CreateCompatibleDC(None);
        assert!(!mem_dc.is_invalid(), "a memory DC");
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .expect("a DIB section");
        state.old_bmp = SelectObject(mem_dc, HGDIOBJ(dib.0));
        state.mem_dc = mem_dc;
        state.dib = dib;
        state.bits_ptr = bits.cast();
    }
    state
}

/// The pixel at (`x`, `y`) of the state's back buffer, after GDI has finished drawing.
pub(super) fn pixel(state: &OverlayState, x: i32, y: i32) -> [u8; 4] {
    unsafe {
        let _ = GdiFlush();
    }
    assert!(
        (0..state.capture.width).contains(&x) && (0..state.capture.height).contains(&y),
        "pixel ({x}, {y}) is outside the back buffer"
    );
    let offset = (y as usize * state.capture.width as usize + x as usize) * 4;
    let bytes = unsafe { std::slice::from_raw_parts(state.bits_ptr, offset + 4) };
    [
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]
}

/// How many pixels inside `area` have exactly the colour `color` (alpha ignored).
pub(super) fn count_color(state: &OverlayState, area: Rect, color: [u8; 4]) -> usize {
    let mut count = 0;
    for y in area.top..area.bottom {
        for x in area.left..area.right {
            let found = pixel(state, x, y);
            if found[..3] == color[..3] {
                count += 1;
            }
        }
    }
    count
}

/// The action failure the editor queued for its window, `(title, message)`.
pub(super) fn take_action_error() -> Option<(String, String)> {
    ACTION_ERROR.with(|pending| pending.borrow_mut().take())
}

/// A fresh temporary folder, removed with its contents on drop.
pub(super) struct TempDir(pub(super) std::path::PathBuf);

impl TempDir {
    pub(super) fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("isolmass-overlay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary folder");
        Self(dir)
    }

    /// The files in the folder, sorted by name.
    pub(super) fn entries(&self) -> Vec<std::path::PathBuf> {
        let mut paths: Vec<_> = std::fs::read_dir(&self.0)
            .expect("a readable temporary folder")
            .map(|entry| entry.expect("a directory entry").path())
            .collect();
        paths.sort();
        paths
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A state with a real back buffer, an active selection and one rectangle, that saves
/// into `save_dir` and keeps the editor open after an action.
pub(super) fn export_state(save_dir: &TempDir) -> OverlayState {
    let mut state = backbuffer_state(640, 480);
    state.set_selection_active(Rect::new(100, 100, 500, 400));
    state.add_test_rectangle(Rect::new(150, 150, 250, 250));
    state.settings.save_directory = save_dir.0.clone();
    state.settings.close_after_action = false;
    state
}

/// Points `%LOCALAPPDATA%` at a fresh temporary folder, where the editor looks for the
/// protected upload keys, and restores it on drop. The tests run on one thread.
pub(super) struct LocalAppData {
    previous: std::ffi::OsString,
    pub(super) dir: std::path::PathBuf,
}

impl LocalAppData {
    pub(super) fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("isolmass-overlay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary LOCALAPPDATA");
        let previous = std::env::var_os("LOCALAPPDATA").unwrap_or_default();
        // SAFETY: the test suite runs on a single thread (`--test-threads=1`), so no
        // other thread reads the environment while it changes.
        unsafe { std::env::set_var("LOCALAPPDATA", &dir) };
        Self { previous, dir }
    }
}

impl Drop for LocalAppData {
    fn drop(&mut self) {
        // SAFETY: as in `new`.
        unsafe { std::env::set_var("LOCALAPPDATA", &self.previous) };
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Keeps the Windows clipboard open from another thread, so any attempt by the editor
/// to read or replace its contents fails the way it does when another program holds it.
pub(super) struct ClipboardHold {
    release: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ClipboardHold {
    pub(super) fn new() -> Self {
        let (release, released) = mpsc::channel::<()>();
        let (ready_tx, ready) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let window = TestWindow::new();
            unsafe { OpenClipboard(window.0) }.expect("the test holds the clipboard");
            ready_tx.send(()).expect("the test is waiting");
            let _ = released.recv();
            unsafe {
                let _ = CloseClipboard();
            }
        });
        ready.recv().expect("the holder thread started");
        Self {
            release: Some(release),
            thread: Some(thread),
        }
    }
}

impl Drop for ClipboardHold {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the clipboard holder finished");
        }
    }
}

/// The text on the clipboard, `None` when it holds none.
pub(super) fn clipboard_text() -> Option<String> {
    crate::clipboard::read_text(HWND::default())
        .ok()
        .filter(|text| !text.is_empty())
}
