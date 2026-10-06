use crate::capture::Rect;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{Error, Result, w};

pub const CF_DIB_FORMAT: u32 = 8; // Standard Windows CF_DIB format

pub fn read_text(owner: HWND) -> Result<String> {
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::System::Memory::GlobalSize;
    unsafe {
        OpenClipboard(owner)?;
    }
    let _clipboard = ClipboardGuard;
    let handle = unsafe { GetClipboardData(13)? };
    let memory = HGLOBAL(handle.0);
    let full_size = unsafe { GlobalSize(memory) };
    if full_size == 0 {
        return Err(Error::new(
            windows::core::HRESULT::from_win32(windows::Win32::Foundation::ERROR_INVALID_HANDLE.0),
            "Clipboard text memory is unavailable.",
        ));
    }
    let size = full_size.min(32_768);
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        return Err(Error::from_win32());
    }
    let units = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2) };
    let length = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    // Oversized input stays bounded, but a cut is never silent: report only
    // when the terminator really lies past the bound.
    let truncated = size < full_size && length == units.len();
    let text = String::from_utf16_lossy(&units[..length]);
    unsafe {
        let _ = GlobalUnlock(memory);
    }
    if truncated {
        crate::diagnostics::record(
            "clipboard",
            "Clipboard text exceeded 32768 bytes and was truncated.",
        );
    }
    Ok(text.replace(['\r', '\n'], " ").replace('\t', "    "))
}

pub(crate) struct ClipboardGuard;

struct GlobalMemory(HGLOBAL);

impl Drop for GlobalMemory {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = GlobalFree(self.0);
            }
        }
    }
}

struct ClipboardOwner(HWND);

impl Drop for ClipboardOwner {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Flattens a sub-rectangle from a full-screen 32-bit BGRA buffer into a
/// standalone DIB byte vector (BITMAPINFOHEADER + bottom-up BGRA pixels).
pub fn flatten_selection_to_dib(
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
) -> Result<Vec<u8>> {
    let clamped = selection.clamp(full_width, full_height);
    if clamped.is_empty() || full_width <= 0 || full_height <= 0 {
        return Err(Error::from_win32());
    }

    let w = clamped.width();
    let h = clamped.height();
    let pixel_bytes = (w as usize)
        .checked_mul(h as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(Error::from_win32)?;

    let header_size = std::mem::size_of::<BITMAPINFOHEADER>();
    let total_size = header_size + pixel_bytes;

    let mut dib_bytes = Vec::with_capacity(total_size);

    // 1. Create standard BITMAPINFOHEADER (positive height = bottom-up DIB)
    let header = BITMAPINFOHEADER {
        biSize: header_size as u32,
        biWidth: w,
        biHeight: h,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: pixel_bytes as u32,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };

    let header_slice = unsafe {
        std::slice::from_raw_parts(&header as *const BITMAPINFOHEADER as *const u8, header_size)
    };
    dib_bytes.extend_from_slice(header_slice);

    // 2. Copy scanlines in bottom-up order (row 0 in DIB = bottom row of image)
    let src_stride = full_width as usize * 4;
    let row_bytes = w as usize * 4;
    let col_offset = clamped.left as usize * 4;

    for y in 0..h {
        let src_y = clamped.bottom - 1 - y;
        let src_offset = (src_y as usize * src_stride) + col_offset;
        let end = src_offset + row_bytes;

        if end <= buffer.len() {
            dib_bytes.extend_from_slice(&buffer[src_offset..end]);
        } else {
            return Err(Error::from_win32());
        }
    }

    for pixel in dib_bytes[header_size..].as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok(dib_bytes)
}

/// Copies pre-formatted DIB data (BITMAPINFOHEADER + pixels) to the Windows Clipboard.
pub fn copy_dib_to_clipboard(hwnd: Option<HWND>, dib_data: &[u8]) -> Result<()> {
    if dib_data.is_empty() {
        return Err(Error::from_win32());
    }

    // A NULL owner cannot become the clipboard owner after EmptyClipboard.
    let temporary_owner = if hwnd.is_none_or(|owner| owner.is_invalid()) {
        Some(ClipboardOwner(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("isolmaSS Clipboard"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                None,
                None,
            )?
        }))
    } else {
        None
    };
    let owner = temporary_owner
        .as_ref()
        .map(|window| window.0)
        .or(hwnd)
        .unwrap_or_default();

    // Keep ownership on every failure path until SetClipboardData succeeds.
    let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, dib_data.len())? };
    let mut allocation = GlobalMemory(hmem);
    if hmem.is_invalid() {
        return Err(Error::from_win32());
    }

    // 2. Lock memory and copy DIB data
    let ptr = unsafe { GlobalLock(hmem) };
    if ptr.is_null() {
        return Err(Error::from_win32());
    }

    unsafe {
        std::ptr::copy_nonoverlapping(dib_data.as_ptr(), ptr as *mut u8, dib_data.len());
        let _ = GlobalUnlock(hmem);
    }

    // 3. Set clipboard data
    unsafe {
        OpenClipboard(owner)?;
    }
    let _clip_guard = ClipboardGuard;

    unsafe {
        EmptyClipboard()?;
        SetClipboardData(CF_DIB_FORMAT, HANDLE(hmem.0))?;
        allocation.0 = HGLOBAL::default();
    }

    // On success, Windows takes ownership of hmem.
    Ok(())
}

/// Copies a completed share URL as Unicode text. The clipboard is touched only
/// after the upload has succeeded and the complete text is in movable memory.
pub fn copy_text_to_clipboard(hwnd: Option<HWND>, text: &str) -> Result<()> {
    if text.is_empty() || text.contains('\0') {
        return Err(Error::new(
            windows::core::HRESULT::from_win32(windows::Win32::Foundation::ERROR_INVALID_DATA.0),
            "Clipboard text is empty or contains NUL.",
        ));
    }
    let encoded: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let size = encoded
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(Error::from_win32)?;
    let temporary_owner = if hwnd.is_none_or(|owner| owner.is_invalid()) {
        Some(ClipboardOwner(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("isolmaSS Clipboard"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                None,
                None,
            )?
        }))
    } else {
        None
    };
    let owner = temporary_owner
        .as_ref()
        .map(|window| window.0)
        .or(hwnd)
        .unwrap_or_default();
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, size)? };
    let mut allocation = GlobalMemory(memory);
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        return Err(Error::from_win32());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(encoded.as_ptr(), pointer.cast::<u16>(), encoded.len());
        let _ = GlobalUnlock(memory);
        OpenClipboard(owner)?;
    }
    let _clipboard = ClipboardGuard;
    unsafe {
        EmptyClipboard()?;
        SetClipboardData(13, HANDLE(memory.0))?; // CF_UNICODETEXT
    }
    allocation.0 = HGLOBAL::default(); // Windows now owns the allocation.
    Ok(())
}
