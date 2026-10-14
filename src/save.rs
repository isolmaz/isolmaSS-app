mod dialog;
pub mod recent;

pub use dialog::choose_output_path;

use crate::capture::Rect;
use crate::settings::SaveFormat;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Storage::FileSystem::{
    MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::{GUID, PCWSTR};

#[repr(C)]
struct GdiplusStartupInput {
    gdiplus_version: u32,
    debug_event_callback: usize,
    suppress_background_thread: i32,
    suppress_external_codecs: i32,
}

#[repr(C)]
struct EncoderParameter {
    guid: GUID,
    number_of_values: u32,
    value_type: u32,
    value: *mut c_void,
}

#[repr(C)]
struct EncoderParameters {
    count: u32,
    parameter: [EncoderParameter; 1],
}

#[link(name = "gdiplus")]
unsafe extern "system" {
    fn GdiplusStartup(
        token: *mut usize,
        input: *const GdiplusStartupInput,
        output: *mut c_void,
    ) -> i32;
    fn GdiplusShutdown(token: usize);
    fn GdipCreateBitmapFromScan0(
        width: i32,
        height: i32,
        stride: i32,
        format: i32,
        scan0: *mut u8,
        bitmap: *mut *mut c_void,
    ) -> i32;
    fn GdipSaveImageToFile(
        image: *mut c_void,
        filename: PCWSTR,
        clsid_encoder: *const GUID,
        encoder_params: *const c_void,
    ) -> i32;
    fn GdipDisposeImage(image: *mut c_void) -> i32;
}

pub const CLSID_PNG: GUID = GUID::from_u128(0x557cf406_1a04_11d3_9a73_0000f81ef32e);
const CLSID_JPEG: GUID = GUID::from_u128(0x557cf401_1a04_11d3_9a73_0000f81ef32e);
const ENCODER_QUALITY: GUID = GUID::from_u128(0x1d5be4b5_fa4a_452d_9cdd_5db35105e7eb);
const ENCODER_PARAMETER_VALUE_TYPE_LONG: u32 = 4;
const PIXEL_FORMAT_32BPP_ARGB: i32 = 0x0026200A;

pub(crate) struct GdiPlusToken(usize);

impl GdiPlusToken {
    pub(crate) fn start() -> Result<Self, String> {
        let mut token = 0usize;
        let input = GdiplusStartupInput {
            gdiplus_version: 1,
            debug_event_callback: 0,
            suppress_background_thread: 0,
            suppress_external_codecs: 0,
        };
        let status = unsafe { GdiplusStartup(&mut token, &input, std::ptr::null_mut()) };
        Self::from_startup(status, token)
    }

    /// The token of a startup that returned `status` (0 is success).
    fn from_startup(status: i32, token: usize) -> Result<Self, String> {
        if status == 0 {
            Ok(Self(token))
        } else {
            Err(format!("GDI+ startup failed with status {status}"))
        }
    }
}

impl Drop for GdiPlusToken {
    fn drop(&mut self) {
        unsafe { GdiplusShutdown(self.0) };
    }
}

pub(crate) struct GdiPlusBitmap(pub(crate) *mut c_void);

impl Drop for GdiPlusBitmap {
    fn drop(&mut self) {
        // GDI+ answers a null image with an error status, which is ignored here.
        unsafe {
            let _ = GdipDisposeImage(self.0);
        }
    }
}

/// Turns a GDI+ status (0 is success) into an error naming the failed `action`.
fn check_status(status: i32, action: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("GDI+ could not {action} (status {status})."))
    }
}

pub fn generate_screenshot_filename() -> String {
    generate_screenshot_filename_for(SaveFormat::Png)
}

pub fn generate_screenshot_filename_for(format: SaveFormat) -> String {
    screenshot_filename(&unsafe { GetLocalTime() }, format)
}

/// `Screenshot_<date>_<time>-<milliseconds>.<extension>` for the given local time.
fn screenshot_filename(time: &SYSTEMTIME, format: SaveFormat) -> String {
    format!(
        "Screenshot_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}-{:03}.{}",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds,
        format.extension()
    )
}

pub fn default_save_directory() -> PathBuf {
    screenshots_directory(pictures_folder(), std::env::var_os("USERPROFILE"))
}

/// The user's Pictures folder as the shell reports it, if it can.
fn pictures_folder() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) }.ok()?;
    let value = unsafe { path.to_string() };
    unsafe {
        CoTaskMemFree(Some(path.0.cast()));
    }
    value.ok().map(PathBuf::from)
}

/// `Screenshots` inside the Pictures folder, else inside `Pictures` of the user
/// profile, else a folder relative to the working directory.
fn screenshots_directory(
    pictures: Option<PathBuf>,
    user_profile: Option<std::ffi::OsString>,
) -> PathBuf {
    match (pictures, user_profile) {
        (Some(pictures), _) => pictures.join("Screenshots"),
        (None, Some(profile)) => PathBuf::from(profile).join("Pictures").join("Screenshots"),
        (None, None) => PathBuf::from("."),
    }
}

/// Bytes of a 32-bit `width` x `height` image, or an error when the size overflows.
fn pixel_bytes(width: usize, height: usize) -> Result<usize, String> {
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| crate::i18n::t("The screenshot dimensions are too large.").to_string())
}

fn extract_selection(
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
) -> Result<(Vec<u8>, i32, i32), String> {
    let clamped = selection.clamp(full_width, full_height);
    if clamped.is_empty() || full_width <= 0 || full_height <= 0 {
        return Err(crate::i18n::t("The screenshot selection is empty or invalid.").to_string());
    }

    let width = clamped.width();
    let height = clamped.height();
    let source_stride = full_width as usize * 4;
    let destination_stride = width as usize * 4;
    let required = pixel_bytes(full_width as usize, full_height as usize)?;
    if buffer.len() < required {
        return Err(
            crate::i18n::t("The screenshot buffer is smaller than its dimensions.").to_string(),
        );
    }

    let mut pixels = vec![0u8; destination_stride * height as usize];
    for row in 0..height {
        let source_start = (clamped.top + row) as usize * source_stride + clamped.left as usize * 4;
        let destination_start = row as usize * destination_stride;
        pixels[destination_start..destination_start + destination_stride]
            .copy_from_slice(&buffer[source_start..source_start + destination_stride]);
    }
    // Screenshots are opaque. GDI does not preserve alpha when drawing into a DIB.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok((pixels, width, height))
}

/// Renames `source` to `destination` with the given `MoveFileExW` flags.
fn move_file(
    source: &Path,
    destination: &Path,
    flags: MOVE_FILE_FLAGS,
) -> windows::core::Result<()> {
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr()), flags) }
}

/// Deletes a temporary file that is no longer needed. A file that is already gone is
/// fine; any other failure is recorded, since it leaves the temporary behind.
fn remove_temporary(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::diagnostics::record("save cleanup", &error.to_string());
    }
}

/// Encodes the selection into a new temporary file in `directory` and flushes it to
/// disk. The temporary carries this process id and the time in nanoseconds and is
/// removed again whenever a step fails; only a hard process kill can orphan it.
/// There is deliberately no age-based sweeper: it could delete another process's
/// in-flight temporary or a user's own dotfile in a shared folder.
fn write_temporary(
    temporary: &Path,
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
    format: SaveFormat,
    jpeg_quality: u8,
) -> Result<(), String> {
    // A name that is already taken belongs to someone else and is never removed.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .map_err(describe)?;
    let result = encode_image(
        buffer,
        full_width,
        full_height,
        selection,
        temporary,
        format,
        jpeg_quality,
    )
    .and_then(|_| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(temporary)
            .and_then(|file| file.sync_all())
            .map_err(describe)
    });
    if result.is_err() {
        remove_temporary(temporary);
    }
    result
}

/// The name of a new temporary file in `directory`, unique to this process and instant.
fn temporary_path(directory: &Path) -> PathBuf {
    directory.join(format!(
        ".isolmass-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ))
}

/// An I/O error as the message shown to the user.
pub(crate) fn describe(error: std::io::Error) -> String {
    error.to_string()
}

/// The folder that holds `path`; the current folder for a bare file name.
fn parent_directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Save As replaces the destination only after encoding and flushing a complete image.
#[allow(clippy::too_many_arguments)]
pub fn save_buffer_to_image(
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
    output_path: &Path,
    format: SaveFormat,
    jpeg_quality: u8,
) -> Result<PathBuf, String> {
    let temporary = temporary_path(parent_directory(output_path));
    write_temporary(
        &temporary,
        buffer,
        full_width,
        full_height,
        selection,
        format,
        jpeg_quality,
    )?;
    match move_file(
        &temporary,
        output_path,
        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
    ) {
        Ok(()) => Ok(output_path.to_owned()),
        Err(error) => {
            remove_temporary(&temporary);
            Err(error.to_string())
        }
    }
}

fn encode_image(
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
    output_path: &Path,
    format: SaveFormat,
    jpeg_quality: u8,
) -> Result<PathBuf, String> {
    let (mut pixels, width, height) =
        extract_selection(buffer, full_width, full_height, selection)?;
    let _token = GdiPlusToken::start()?;

    let mut raw_bitmap = std::ptr::null_mut();
    let status = unsafe {
        GdipCreateBitmapFromScan0(
            width,
            height,
            width * 4,
            PIXEL_FORMAT_32BPP_ARGB,
            pixels.as_mut_ptr(),
            &mut raw_bitmap,
        )
    };
    let bitmap = GdiPlusBitmap(raw_bitmap);
    check_status(status, "create the image")?;

    let wide_path: Vec<u16> = output_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    let mut quality = u32::from(jpeg_quality.clamp(1, 100));
    let encoder_parameters = EncoderParameters {
        count: 1,
        parameter: [EncoderParameter {
            guid: ENCODER_QUALITY,
            number_of_values: 1,
            value_type: ENCODER_PARAMETER_VALUE_TYPE_LONG,
            value: (&mut quality as *mut u32).cast(),
        }],
    };
    let (encoder, parameters) = match format {
        SaveFormat::Png => (&CLSID_PNG, std::ptr::null()),
        SaveFormat::Jpeg => (
            &CLSID_JPEG,
            (&encoder_parameters as *const EncoderParameters).cast::<c_void>(),
        ),
    };

    let save_status =
        unsafe { GdipSaveImageToFile(bitmap.0, PCWSTR(wide_path.as_ptr()), encoder, parameters) };
    check_status(save_status, "save the image")?;
    Ok(output_path.to_path_buf())
}

pub fn save_buffer_to_png(
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
    output_path: &Path,
) -> Result<PathBuf, String> {
    save_buffer_to_image(
        buffer,
        full_width,
        full_height,
        selection,
        output_path,
        SaveFormat::Png,
        100,
    )
}

/// How many `_NNNN` suffixes are tried before the process id names the file.
const NAME_SUFFIXES: u32 = 9999;

fn unique_output_path(directory: &Path, format: SaveFormat) -> PathBuf {
    unique_path(
        directory,
        &generate_screenshot_filename_for(format),
        format,
        NAME_SUFFIXES,
    )
}

/// `filename` inside `directory` if it is free; otherwise the first free
/// `<name>_0001`, `<name>_0002`, ... up to `suffixes`, then `<name>_<process id>`.
fn unique_path(directory: &Path, filename: &str, format: SaveFormat, suffixes: u32) -> PathBuf {
    let initial = directory.join(filename);
    if !initial.exists() {
        return initial;
    }

    let stem = initial
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Screenshot");
    for suffix in 1..=suffixes {
        let candidate = directory.join(format!("{stem}_{suffix:04}.{}", format.extension()));
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(format!(
        "{stem}_{}.{}",
        std::process::id(),
        format.extension()
    ))
}

/// How many names are tried when moving a finished screenshot into place.
const PLACE_ATTEMPTS: u32 = 128;

/// Moves the `temporary` file to `destination` without replacing anything. A name that
/// someone else took in the meantime is swapped for a fresh unique name in `directory`,
/// at most `attempts` times in total. The temporary is removed when it cannot be placed.
fn place_unique(
    temporary: &Path,
    mut destination: PathBuf,
    directory: &Path,
    format: SaveFormat,
    attempts: u32,
) -> Result<PathBuf, String> {
    for _ in 0..attempts {
        match move_file(temporary, &destination, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => return Ok(destination),
            // ERROR_FILE_EXISTS and ERROR_ALREADY_EXISTS.
            Err(error) if matches!(error.code().0 as u32, 0x80070050 | 0x800700b7) => {
                destination = unique_output_path(directory, format)
            }
            Err(error) => {
                remove_temporary(temporary);
                return Err(crate::i18n::tf(
                    "Could not finalize the screenshot: {}",
                    &[&error],
                ));
            }
        }
    }
    remove_temporary(temporary);
    Err(crate::i18n::t("Could not reserve a unique screenshot filename.").to_string())
}

pub fn save_screenshot(
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
    custom_dir: Option<&Path>,
    format: SaveFormat,
    jpeg_quality: u8,
) -> Result<PathBuf, String> {
    let directory = custom_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(default_save_directory);
    if directory.exists() && !directory.is_dir() {
        return Err(crate::i18n::tf(
            "The screenshot destination '{}' is not a directory.",
            &[&directory.display()],
        ));
    }
    std::fs::create_dir_all(&directory).map_err(|error| {
        crate::i18n::tf(
            "Could not create the screenshot folder '{}': {}",
            &[&directory.display(), &error],
        )
    })?;

    let output_path = unique_output_path(&directory, format);
    let temporary = temporary_path(&directory);
    write_temporary(
        &temporary,
        buffer,
        full_width,
        full_height,
        selection,
        format,
        jpeg_quality,
    )?;
    let destination = place_unique(&temporary, output_path, &directory, format, PLACE_ATTEMPTS)?;
    recent::saved(&destination);
    Ok(destination)
}

pub fn recent_screenshots(directory: &Path, limit: usize) -> std::io::Result<Vec<PathBuf>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    Ok(newest_screenshots(
        &mut std::fs::read_dir(directory)?,
        limit,
    ))
}

/// The `limit` (at most 100) most recently modified screenshots among `entries`, newest
/// first. Only a directory-level failure aborts a scan, and that happens before this
/// runs; entries that were removed or became unreadable since listing are skipped so
/// one bad file cannot discard every successful result.
fn newest_screenshots(
    entries: &mut dyn Iterator<Item = std::io::Result<std::fs::DirEntry>>,
    limit: usize,
) -> Vec<PathBuf> {
    let mut newest = std::collections::BinaryHeap::new();
    let started = std::time::Instant::now();
    for entry in entries {
        if recent::cancelled() || started.elapsed() > std::time::Duration::from_secs(2) {
            break;
        }
        match screenshot_candidate(entry) {
            Ok(Some(candidate)) => {
                newest.push(std::cmp::Reverse(candidate));
                if newest.len() > limit.min(100) {
                    newest.pop();
                }
            }
            Ok(None) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => crate::diagnostics::record(
                "recent captures",
                &format!("Skipping unreadable screenshot entry: {error}"),
            ),
        }
    }
    let mut newest = newest.into_iter().map(|entry| entry.0).collect::<Vec<_>>();
    newest.sort_unstable_by(|a, b| b.cmp(a));
    newest.into_iter().map(|(_, path)| path).collect()
}

/// The modification time and path of a PNG or JPEG file entry; `None` for anything else.
fn screenshot_candidate(
    entry: std::io::Result<std::fs::DirEntry>,
) -> std::io::Result<Option<(std::time::SystemTime, PathBuf)>> {
    let entry = entry?;
    let path = entry.path();
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
    {
        return Ok(None);
    }
    let metadata = entry.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    Ok(Some((metadata.modified()?, path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_env::TempDir;
    use std::io::ErrorKind;
    use std::time::{Duration, UNIX_EPOCH};

    #[link(name = "gdiplus")]
    unsafe extern "system" {
        fn GdipLoadImageFromFile(filename: PCWSTR, image: *mut *mut c_void) -> i32;
        fn GdipBitmapGetPixel(image: *mut c_void, x: i32, y: i32, color: *mut u32) -> i32;
        fn GdipGetImageWidth(image: *mut c_void, width: *mut u32) -> i32;
        fn GdipGetImageHeight(image: *mut c_void, height: *mut u32) -> i32;
    }

    /// A decoded image: size and the ARGB value of every pixel, row by row.
    struct Decoded {
        width: u32,
        height: u32,
        argb: Vec<u32>,
    }

    fn decode(path: &Path) -> Decoded {
        let _token = GdiPlusToken::start().expect("GDI+ starts");
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut raw = std::ptr::null_mut();
        assert_eq!(
            unsafe { GdipLoadImageFromFile(PCWSTR(wide.as_ptr()), &mut raw) },
            0,
            "GDI+ opens {path:?}"
        );
        let image = GdiPlusBitmap(raw);
        let (mut width, mut height) = (0u32, 0u32);
        assert_eq!(unsafe { GdipGetImageWidth(image.0, &mut width) }, 0);
        assert_eq!(unsafe { GdipGetImageHeight(image.0, &mut height) }, 0);
        let mut argb = Vec::new();
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let mut color = 0u32;
                assert_eq!(unsafe { GdipBitmapGetPixel(image.0, x, y, &mut color) }, 0);
                argb.push(color);
            }
        }
        Decoded {
            width,
            height,
            argb,
        }
    }

    /// BGRA pixels like a screen capture: every pixel differs and the alpha byte is 0.
    fn capture(width: i32, height: i32) -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend([
                    (x * 7 + 3) as u8,
                    (y * 11 + 5) as u8,
                    (x * 3 + y * 5) as u8,
                    0,
                ]);
            }
        }
        pixels
    }

    /// The opaque ARGB value that `capture` makes of the pixel at (x, y), as GDI+ reads it.
    fn expected_argb(x: i32, y: i32) -> u32 {
        let (blue, green, red) = (x * 7 + 3, y * 11 + 5, x * 3 + y * 5);
        0xFF00_0000 | (red as u32) << 16 | (green as u32) << 8 | blue as u32
    }

    /// Sorted names of the entries in `directory`.
    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .expect("list the folder")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn everything(width: i32, height: i32) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn png_export_keeps_the_selected_pixels_and_makes_them_opaque() {
        let dir = TempDir::new("save_png");
        let output = dir.path().join("shot.png");
        let saved = save_buffer_to_image(
            &capture(8, 6),
            8,
            6,
            &Rect::new(2, 1, 7, 5),
            &output,
            SaveFormat::Png,
            100,
        )
        .expect("PNG export");
        assert_eq!(saved, output);
        assert_eq!(
            &std::fs::read(&output).unwrap()[..8],
            b"\x89PNG\r\n\x1a\n",
            "PNG signature"
        );
        let image = decode(&output);
        assert_eq!((image.width, image.height), (5, 4));
        for y in 0..4 {
            for x in 0..5 {
                assert_eq!(
                    image.argb[(y * 5 + x) as usize],
                    expected_argb(x + 2, y + 1),
                    "pixel ({x}, {y}) of the selection"
                );
            }
        }
        assert_eq!(names(dir.path()), ["shot.png"], "no temporary file is left");
    }

    #[test]
    fn png_wrapper_exports_the_whole_capture_losslessly() {
        let dir = TempDir::new("save_png_wrapper");
        let output = dir.path().join("whole.png");
        let saved = save_buffer_to_png(&capture(8, 6), 8, 6, &everything(8, 6), &output)
            .expect("PNG export");
        assert_eq!(saved, output);
        let image = decode(&output);
        assert_eq!((image.width, image.height), (8, 6));
        for y in 0..6 {
            for x in 0..8 {
                assert_eq!(image.argb[(y * 8 + x) as usize], expected_argb(x, y));
            }
        }
    }

    #[test]
    fn save_as_replaces_an_existing_file() {
        let dir = TempDir::new("save_replace");
        let output = dir.path().join("shot.png");
        std::fs::write(&output, b"previous image").unwrap();
        save_buffer_to_png(&capture(4, 4), 4, 4, &everything(4, 4), &output).expect("export");
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "the old file was replaced"
        );
        assert_eq!(names(dir.path()), ["shot.png"]);
    }

    /// Deterministic noise: it compresses badly, so the JPEG quality changes the size.
    fn noise(width: i32, height: i32) -> Vec<u8> {
        let mut state = 0x1234_5678u32;
        (0..width * height * 4)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    fn jpeg_size(directory: &Path, pixels: &[u8], quality: u8) -> u64 {
        let output = directory.join(format!("quality{quality}.jpg"));
        save_buffer_to_image(
            pixels,
            64,
            64,
            &everything(64, 64),
            &output,
            SaveFormat::Jpeg,
            quality,
        )
        .expect("JPEG export");
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(&bytes[..2], [0xFF, 0xD8], "JPEG signature");
        bytes.len() as u64
    }

    #[test]
    fn jpeg_quality_trades_size_for_fidelity_within_one_to_one_hundred() {
        let dir = TempDir::new("save_jpeg_quality");
        let pixels = noise(64, 64);
        let high = jpeg_size(dir.path(), &pixels, 100);
        let low = jpeg_size(dir.path(), &pixels, 1);
        assert!(
            high > low * 3,
            "quality 100 ({high} B) vs quality 1 ({low} B)"
        );
        assert_eq!(jpeg_size(dir.path(), &pixels, 0), low, "0 is raised to 1");
        assert_eq!(
            jpeg_size(dir.path(), &pixels, 200),
            high,
            "200 is lowered to 100"
        );
    }

    #[test]
    fn jpeg_export_keeps_a_flat_colour() {
        let dir = TempDir::new("save_jpeg_colour");
        let output = dir.path().join("flat.jpg");
        let pixels: Vec<u8> = (0..16 * 16).flat_map(|_| [30, 120, 200, 0]).collect();
        save_buffer_to_image(
            &pixels,
            16,
            16,
            &everything(16, 16),
            &output,
            SaveFormat::Jpeg,
            100,
        )
        .expect("JPEG export");
        let image = decode(&output);
        assert_eq!((image.width, image.height), (16, 16));
        let pixel = image.argb[8 * 16 + 8];
        assert_eq!(pixel >> 24, 0xFF, "opaque");
        for (shift, expected) in [(16, 200u32), (8, 120), (0, 30)] {
            let channel = (pixel >> shift) & 0xFF;
            assert!(
                channel.abs_diff(expected) <= 6,
                "channel at bit {shift}: {channel} vs {expected}"
            );
        }
    }

    #[test]
    fn failed_exports_report_why_and_leave_nothing_behind() {
        let dir = TempDir::new("save_failures");
        let pixels = capture(8, 6);
        let output = dir.path().join("shot.png");
        let export = |buffer: &[u8], selection: Rect, path: &Path| {
            save_buffer_to_image(buffer, 8, 6, &selection, path, SaveFormat::Png, 100)
        };

        assert_eq!(
            export(&pixels, Rect::new(3, 3, 3, 3), &output).expect_err("empty selection"),
            crate::i18n::t("The screenshot selection is empty or invalid.")
        );
        assert_eq!(
            export(&pixels[..10], everything(8, 6), &output).expect_err("short buffer"),
            crate::i18n::t("The screenshot buffer is smaller than its dimensions.")
        );
        assert!(
            names(dir.path()).is_empty(),
            "failed encodes remove their temporary file"
        );

        let missing = dir.path().join("missing").join("shot.png");
        export(&pixels, everything(8, 6), &missing).expect_err("missing folder");
        assert!(
            !dir.path().join("missing").exists(),
            "no folder is invented"
        );

        std::fs::create_dir(&output).unwrap();
        export(&pixels, everything(8, 6), &output).expect_err("a folder is not replaceable");
        assert_eq!(
            names(dir.path()),
            ["shot.png"],
            "temporary file removed again"
        );
        assert!(output.is_dir());
    }

    #[test]
    fn encoder_failures_name_the_gdi_plus_status() {
        let dir = TempDir::new("save_encoder");
        let error = encode_image(
            &capture(4, 4),
            4,
            4,
            &everything(4, 4),
            &dir.path().join("missing").join("shot.png"),
            SaveFormat::Png,
            100,
        )
        .expect_err("GDI+ cannot create a file in a missing folder");
        assert!(
            error.starts_with("GDI+ could not save the image (status "),
            "{error}"
        );
        assert_eq!(check_status(0, "do it"), Ok(()));
        assert_eq!(
            check_status(2, "create the image"),
            Err("GDI+ could not create the image (status 2).".to_string())
        );
    }

    #[test]
    fn a_failed_gdi_plus_startup_reports_its_status() {
        // A failed startup has no token to shut down, so it is never wrapped in one.
        let startup = GdiPlusToken::from_startup(18, 0);
        assert!(startup.is_err(), "status 18 is a failure");
        let error = startup.err().unwrap_or_default();
        assert!(
            error.starts_with("GDI+ startup failed with status "),
            "{error}"
        );
        assert!(GdiPlusToken::start().is_ok());
    }

    #[test]
    fn selection_is_cropped_clamped_and_made_opaque() {
        let pixels = capture(8, 6);
        let (cropped, width, height) =
            extract_selection(&pixels, 8, 6, &Rect::new(2, 1, 4, 3)).expect("crop");
        assert_eq!((width, height), (2, 2));
        let mut expected = Vec::new();
        for y in 1..3 {
            for x in 2..4 {
                expected.extend([
                    (x * 7 + 3) as u8,
                    (y * 11 + 5) as u8,
                    (x * 3 + y * 5) as u8,
                    255,
                ]);
            }
        }
        assert_eq!(cropped, expected);

        let (clamped, width, height) =
            extract_selection(&pixels, 8, 6, &Rect::new(-5, -5, 100, 100)).expect("clamp");
        assert_eq!((width, height), (8, 6));
        assert_eq!(clamped.len(), pixels.len());
    }

    #[test]
    fn invalid_selections_and_buffers_are_rejected() {
        let pixels = capture(8, 6);
        let empty = crate::i18n::t("The screenshot selection is empty or invalid.");
        for (width, height, selection) in [
            (8, 6, Rect::new(4, 4, 4, 5)),
            (8, 6, Rect::new(9, 0, 12, 3)),
            (0, 6, everything(8, 6)),
            (8, -1, everything(8, 6)),
        ] {
            assert_eq!(
                extract_selection(&pixels, width, height, &selection).unwrap_err(),
                empty,
                "{width}x{height} {selection:?}"
            );
        }
        assert_eq!(
            extract_selection(&pixels, 8, 7, &everything(8, 7)).unwrap_err(),
            crate::i18n::t("The screenshot buffer is smaller than its dimensions.")
        );
        // Dimensions far beyond the buffer are refused before anything is allocated.
        assert_eq!(
            extract_selection(&pixels, i32::MAX, i32::MAX, &everything(1, 1)).unwrap_err(),
            crate::i18n::t("The screenshot buffer is smaller than its dimensions.")
        );
    }

    #[test]
    fn filenames_carry_date_time_milliseconds_and_extension() {
        let time = SYSTEMTIME {
            wYear: 2026,
            wMonth: 3,
            wDayOfWeek: 6,
            wDay: 7,
            wHour: 9,
            wMinute: 5,
            wSecond: 2,
            wMilliseconds: 45,
        };
        assert_eq!(
            screenshot_filename(&time, SaveFormat::Png),
            "Screenshot_2026-03-07_09-05-02-045.png"
        );
        assert_eq!(
            screenshot_filename(&time, SaveFormat::Jpeg),
            "Screenshot_2026-03-07_09-05-02-045.jpg"
        );

        let live = generate_screenshot_filename_for(SaveFormat::Jpeg);
        let shape = "Screenshot_dddd-dd-dd_dd-dd-dd-ddd.jpg";
        assert_eq!(live.len(), shape.len(), "{live}");
        for (actual, expected) in live.chars().zip(shape.chars()) {
            match expected {
                'd' => assert!(actual.is_ascii_digit(), "{live}"),
                other => assert_eq!(actual, other, "{live}"),
            }
        }
        let png = generate_screenshot_filename();
        assert!(
            png.starts_with("Screenshot_") && png.ends_with(".png"),
            "{png}"
        );
    }

    #[test]
    fn default_folder_is_screenshots_in_pictures_or_the_profile() {
        let pictures = PathBuf::from(r"D:\Media\Pictures");
        assert_eq!(
            screenshots_directory(Some(pictures.clone()), Some(r"C:\Users\a".into())),
            pictures.join("Screenshots")
        );
        assert_eq!(
            screenshots_directory(None, Some(r"C:\Users\a".into())),
            PathBuf::from(r"C:\Users\a\Pictures\Screenshots")
        );
        assert_eq!(screenshots_directory(None, None), PathBuf::from("."));

        let live = default_save_directory();
        assert!(live.is_absolute(), "{live:?}");
        assert_eq!(live.file_name().unwrap(), "Screenshots");
    }

    #[test]
    fn taken_names_get_numbered_suffixes() {
        let dir = TempDir::new("save_unique");
        let free = dir.path().join("Screenshot_fixed.png");
        assert_eq!(
            unique_path(dir.path(), "Screenshot_fixed.png", SaveFormat::Png, 3),
            free
        );
        std::fs::write(&free, b"1").unwrap();
        for expected in ["Screenshot_fixed_0001.png", "Screenshot_fixed_0002.png"] {
            let next = unique_path(dir.path(), "Screenshot_fixed.png", SaveFormat::Png, 3);
            assert_eq!(next, dir.path().join(expected));
            std::fs::write(&next, b"x").unwrap();
        }
        assert_eq!(
            unique_path(dir.path(), "Screenshot_fixed.png", SaveFormat::Png, 3),
            dir.path().join("Screenshot_fixed_0003.png")
        );
        // The extension of a suffixed name follows the requested format.
        assert_eq!(
            unique_path(dir.path(), "Screenshot_fixed.png", SaveFormat::Jpeg, 3),
            dir.path().join("Screenshot_fixed_0001.jpg")
        );
        // When every suffix is taken, the process id still gives a distinct name.
        std::fs::write(dir.path().join("Screenshot_fixed_0003.png"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "Screenshot_fixed.png", SaveFormat::Png, 3),
            dir.path()
                .join(format!("Screenshot_fixed_{}.png", std::process::id()))
        );

        let generated = unique_output_path(dir.path(), SaveFormat::Png);
        assert_eq!(generated.parent(), Some(dir.path()));
        assert!(!generated.exists());
    }

    #[test]
    fn placing_a_file_never_overwrites_and_retries_taken_names() {
        let dir = TempDir::new("save_place");
        let source = dir.path().join("source.tmp");
        let taken = dir.path().join("taken.png");
        std::fs::write(&source, b"new image").unwrap();
        std::fs::write(&taken, b"existing image").unwrap();

        let placed = place_unique(&source, taken.clone(), dir.path(), SaveFormat::Png, 2)
            .expect("placed under the next name");
        assert_ne!(placed, taken);
        assert_eq!(placed.parent(), Some(dir.path()));
        let name = placed.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("Screenshot_") && name.ends_with(".png"),
            "{name}"
        );
        assert_eq!(std::fs::read(&placed).unwrap(), b"new image");
        assert_eq!(
            std::fs::read(&taken).unwrap(),
            b"existing image",
            "never replaced"
        );
        assert!(!source.exists(), "the source was moved");

        // A free name is used as given.
        std::fs::write(&source, b"second image").unwrap();
        let wanted = dir.path().join("direct.png");
        let direct = place_unique(&source, wanted.clone(), dir.path(), SaveFormat::Png, 1)
            .expect("placed directly");
        assert_eq!(direct, wanted);
        assert_eq!(std::fs::read(direct).unwrap(), b"second image");
    }

    #[test]
    fn placing_gives_up_when_the_attempts_run_out_and_reports_other_failures() {
        let dir = TempDir::new("save_place_fail");
        let source = dir.path().join("source.tmp");
        let taken = dir.path().join("taken.png");
        std::fs::write(&source, b"image").unwrap();
        std::fs::write(&taken, b"existing").unwrap();

        let error = place_unique(&source, taken.clone(), dir.path(), SaveFormat::Png, 1)
            .expect_err("the only attempt hits a taken name");
        assert_eq!(
            error,
            crate::i18n::t("Could not reserve a unique screenshot filename.")
        );
        assert!(!source.exists(), "the temporary is removed");
        assert_eq!(std::fs::read(&taken).unwrap(), b"existing");
        assert_eq!(names(dir.path()), ["taken.png"]);

        std::fs::write(&source, b"image").unwrap();
        let missing = dir.path().join("missing").join("shot.png");
        let error = place_unique(&source, missing, dir.path(), SaveFormat::Png, 5)
            .expect_err("missing folder");
        assert!(
            error.starts_with(
                crate::i18n::tf("Could not finalize the screenshot: {}", &[&""]).as_str()
            ),
            "{error}"
        );
        assert!(!source.exists(), "the temporary is removed");
    }

    #[test]
    fn a_taken_temporary_name_is_reported_and_left_alone() {
        let dir = TempDir::new("save_temporary_taken");
        let temporary = temporary_path(dir.path());
        assert_eq!(temporary.parent(), Some(dir.path()));
        let name = temporary.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(&format!(".isolmass-{}-", std::process::id()))
                && name.ends_with(".tmp"),
            "{name}"
        );
        std::fs::write(&temporary, b"someone else's").unwrap();

        let error = write_temporary(
            &temporary,
            &capture(4, 4),
            4,
            4,
            &everything(4, 4),
            SaveFormat::Png,
            90,
        )
        .unwrap_err();
        let reason = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .unwrap_err();
        assert_eq!(error, describe(reason));
        assert_eq!(std::fs::read(&temporary).unwrap(), b"someone else's");
    }

    #[test]
    fn a_bare_file_name_is_saved_in_the_current_folder() {
        assert_eq!(parent_directory(Path::new("shot.png")), Path::new("."));
        assert_eq!(
            parent_directory(Path::new(r"C:\a\shot.png")),
            Path::new(r"C:\a")
        );
        assert_eq!(parent_directory(Path::new(r"C:\")), Path::new("."));
    }

    #[test]
    fn overflowing_image_sizes_are_refused() {
        assert_eq!(pixel_bytes(3, 5), Ok(60));
        assert_eq!(
            pixel_bytes(usize::MAX, 2),
            Err(crate::i18n::t("The screenshot dimensions are too large.").to_string())
        );
        assert_eq!(
            pixel_bytes(usize::MAX / 2, 1),
            Err(crate::i18n::t("The screenshot dimensions are too large.").to_string())
        );
    }

    #[test]
    fn temporary_files_are_removed_quietly() {
        let dir = TempDir::new("save_temporary");
        let file = dir.path().join("a.tmp");
        std::fs::write(&file, b"x").unwrap();
        remove_temporary(&file);
        assert!(!file.exists());
        // Already gone: nothing to do.
        remove_temporary(&file);
        // Not removable (a folder): the failure is recorded and the folder is untouched.
        let folder = dir.path().join("b.tmp");
        std::fs::create_dir(&folder).unwrap();
        remove_temporary(&folder);
        assert!(folder.is_dir());
    }

    #[test]
    fn save_screenshot_creates_the_folder_and_names_the_file_by_time() {
        let _state = recent::isolated();
        let dir = TempDir::new("save_screenshot");
        let target = dir.path().join("a").join("b");
        let path = save_screenshot(
            &capture(8, 6),
            8,
            6,
            &Rect::new(1, 1, 5, 4),
            Some(&target),
            SaveFormat::Png,
            90,
        )
        .expect("save");
        assert_eq!(path.parent(), Some(target.as_path()));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("Screenshot_") && name.ends_with(".png"),
            "{name}"
        );
        let image = decode(&path);
        assert_eq!((image.width, image.height), (4, 3));
        assert_eq!(image.argb[0], expected_argb(1, 1));
        assert_eq!(names(&target), [name], "no temporary file is left");
        assert_eq!(
            recent::last_folder(),
            Some(target),
            "the folder is remembered for 'open screenshots folder'"
        );
    }

    #[test]
    fn save_screenshot_never_overwrites_an_earlier_capture() {
        let _state = recent::isolated();
        let dir = TempDir::new("save_screenshot_twice");
        let pixels = capture(4, 4);
        let save = |format| {
            save_screenshot(
                &pixels,
                4,
                4,
                &everything(4, 4),
                Some(dir.path()),
                format,
                80,
            )
            .expect("save")
        };
        let first = save(SaveFormat::Jpeg);
        let second = save(SaveFormat::Jpeg);
        assert_ne!(first, second);
        assert!(
            first
                .extension()
                .is_some_and(|extension| extension == "jpg")
        );
        assert_eq!(&std::fs::read(&first).unwrap()[..2], [0xFF, 0xD8]);
        assert_eq!(names(dir.path()).len(), 2);
    }

    #[test]
    fn save_screenshot_refuses_bad_destinations_and_failed_encodes() {
        let _state = recent::isolated();
        let dir = TempDir::new("save_screenshot_bad");
        let pixels = capture(4, 4);
        let file = dir.path().join("taken");
        std::fs::write(&file, b"x").unwrap();
        let save = |folder: &Path, selection: Rect| {
            save_screenshot(&pixels, 4, 4, &selection, Some(folder), SaveFormat::Png, 90)
        };

        assert_eq!(
            save(&file, everything(4, 4)).unwrap_err(),
            crate::i18n::tf(
                "The screenshot destination '{}' is not a directory.",
                &[&file.display()]
            )
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"x");

        let below_file = file.join("sub");
        let reason = std::fs::create_dir_all(&below_file).unwrap_err();
        assert_eq!(
            save(&below_file, everything(4, 4)).unwrap_err(),
            crate::i18n::tf(
                "Could not create the screenshot folder '{}': {}",
                &[&below_file.display(), &reason]
            )
        );

        let folder = dir.path().join("shots");
        assert_eq!(
            save(&folder, Rect::new(1, 1, 1, 1)).unwrap_err(),
            crate::i18n::t("The screenshot selection is empty or invalid.")
        );
        assert!(
            names(&folder).is_empty(),
            "a failed save leaves no temporary file"
        );
        assert_eq!(
            recent::last_folder(),
            None,
            "failed saves are not remembered"
        );
    }

    /// Creates `name` in `directory` with the given modification time (seconds after a
    /// fixed instant, so the order of the tests' files does not depend on the clock).
    fn touch(directory: &Path, name: &str, seconds: u64) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, b"image").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds))
            .unwrap();
        path
    }

    fn file_names(paths: &[PathBuf]) -> Vec<&str> {
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect()
    }

    fn folder_with_captures() -> TempDir {
        let dir = TempDir::new("save_recent");
        touch(dir.path(), "a.png", 0);
        touch(dir.path(), "f.png", 5);
        touch(dir.path(), "b.JPG", 10);
        touch(dir.path(), "c.jpeg", 20);
        touch(dir.path(), "notes.txt", 30);
        touch(dir.path(), "e.png", 40);
        std::fs::create_dir(dir.path().join("folder.png")).unwrap();
        dir
    }

    #[test]
    fn recent_screenshots_are_the_newest_images_first() {
        let _state = recent::isolated();
        let dir = folder_with_captures();
        let top = recent_screenshots(dir.path(), 3).expect("scan");
        assert_eq!(file_names(&top), ["e.png", "c.jpeg", "b.JPG"]);
        let all = recent_screenshots(dir.path(), 1000).expect("scan");
        assert_eq!(
            file_names(&all),
            ["e.png", "c.jpeg", "b.JPG", "f.png", "a.png"],
            "text files and folders never count, extensions are case-insensitive"
        );
        assert!(recent_screenshots(dir.path(), 0).expect("scan").is_empty());
    }

    #[test]
    fn recent_screenshots_report_an_unlistable_folder() {
        let _state = recent::isolated();
        let dir = TempDir::new("save_recent_missing");
        let error = recent_screenshots(&dir.path().join("missing"), 5).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        let file = touch(dir.path(), "file.png", 0);
        assert!(recent_screenshots(&file, 5).is_err());
    }

    #[test]
    fn a_cancelled_scan_returns_what_it_has() {
        let _state = recent::isolated();
        let dir = folder_with_captures();
        recent::shutdown();
        assert!(recent::cancelled());
        assert!(recent_screenshots(dir.path(), 5).expect("scan").is_empty());
    }

    #[test]
    fn unreadable_entries_do_not_discard_the_other_results() {
        let _state = recent::isolated();
        let dir = folder_with_captures();
        let failures = [
            Err(std::io::Error::from(ErrorKind::NotFound)),
            Err(std::io::Error::from(ErrorKind::PermissionDenied)),
        ];
        let mut entries = failures
            .into_iter()
            .chain(std::fs::read_dir(dir.path()).unwrap());
        let found = newest_screenshots(&mut entries, 2);
        assert_eq!(file_names(&found), ["e.png", "c.jpeg"]);
    }

    #[test]
    fn candidates_are_image_files_with_their_modification_time() {
        let dir = folder_with_captures();
        let mut seen = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            seen.insert(name, screenshot_candidate(Ok(entry)).unwrap());
        }
        assert_eq!(
            seen["e.png"],
            Some((
                UNIX_EPOCH + Duration::from_secs(1_700_000_040),
                dir.path().join("e.png")
            ))
        );
        assert_eq!(seen["notes.txt"], None, "not an image");
        assert_eq!(seen["folder.png"], None, "a folder named like an image");
        assert_eq!(
            screenshot_candidate(Err(std::io::Error::from(ErrorKind::TimedOut)))
                .unwrap_err()
                .kind(),
            ErrorKind::TimedOut
        );
    }
}
