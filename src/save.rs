pub mod recent;
use crate::capture::Rect;
use crate::settings::SaveFormat;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::{GUID, PCWSTR};

pub fn choose_output_path(
    owner: windows::Win32::Foundation::HWND,
    format: SaveFormat,
) -> windows::core::Result<Option<PathBuf>> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FileSaveDialog, IFileSaveDialog,
        SIGDN_FILESYSPATH,
    };
    let dialog: IFileSaveDialog =
        unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)? };
    let name: Vec<u16> = generate_screenshot_filename_for(format)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let extension: Vec<u16> = format.extension().encode_utf16().chain(Some(0)).collect();
    let filter: Vec<u16> = format!("*.{}\0", format.extension())
        .encode_utf16()
        .collect();
    let description: Vec<u16> = format!(
        "{}\0",
        crate::i18n::tf("{} image", &[&format.extension().to_uppercase()])
    )
    .encode_utf16()
    .collect();
    unsafe {
        dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT)?;
        dialog.SetTitle(&windows::core::HSTRING::from(crate::i18n::t(
            "Save screenshot",
        )))?;
        dialog.SetFileName(PCWSTR(name.as_ptr()))?;
        dialog.SetDefaultExtension(PCWSTR(extension.as_ptr()))?;
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
            pszName: PCWSTR(description.as_ptr()),
            pszSpec: PCWSTR(filter.as_ptr()),
        }])?;
    }
    if let Err(error) = unsafe { dialog.Show(owner) } {
        if error.code() == windows::core::HRESULT::from_win32(1223) {
            return Ok(None);
        }
        return Err(error);
    }
    let raw = unsafe { dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)? };
    let path = unsafe { raw.to_string() };
    unsafe {
        CoTaskMemFree(Some(raw.0.cast()));
    }
    Ok(Some(PathBuf::from(path?)))
}

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
        if !self.0.is_null() {
            unsafe {
                let _ = GdipDisposeImage(self.0);
            }
        }
    }
}

pub fn generate_screenshot_filename() -> String {
    generate_screenshot_filename_for(SaveFormat::Png)
}

pub fn generate_screenshot_filename_for(format: SaveFormat) -> String {
    let st = unsafe { GetLocalTime() };
    format!(
        "Screenshot_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}-{:03}.{}",
        st.wYear,
        st.wMonth,
        st.wDay,
        st.wHour,
        st.wMinute,
        st.wSecond,
        st.wMilliseconds,
        format.extension()
    )
}

pub fn default_save_directory() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    if let Ok(path) = unsafe { SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) } {
        let value = unsafe { path.to_string() };
        unsafe {
            CoTaskMemFree(Some(path.0.cast()));
        }
        if let Ok(value) = value {
            return PathBuf::from(value).join("Screenshots");
        }
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|path| path.join("Pictures").join("Screenshots"))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn extract_selection(
    buffer: &[u8],
    full_width: i32,
    full_height: i32,
    selection: &Rect,
) -> Result<(Vec<u8>, i32, i32), String> {
    let clamped = selection.clamp(full_width, full_height);
    if clamped.is_empty() || full_width <= 0 || full_height <= 0 {
        return Err("The screenshot selection is empty or invalid.".to_string());
    }

    let width = clamped.width();
    let height = clamped.height();
    let source_stride = full_width as usize * 4;
    let destination_stride = width as usize * 4;
    let required = (full_width as usize)
        .checked_mul(full_height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "The screenshot dimensions are too large.".to_string())?;
    if buffer.len() < required {
        return Err("The screenshot buffer is smaller than its dimensions.".to_string());
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
    let directory = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let temporary = directory.join(format!(".isolmass-{}-{unique}.tmp", std::process::id()));
    let result = (|| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        encode_image(
            buffer,
            full_width,
            full_height,
            selection,
            &temporary,
            format,
            jpeg_quality,
        )?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        use windows::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = output_path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| error.to_string())?;
        Ok(output_path.to_owned())
    })();
    if result.is_err()
        && let Err(error) = std::fs::remove_file(&temporary)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::diagnostics::record("save cleanup", &error.to_string());
    }
    result
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
    if status != 0 || raw_bitmap.is_null() {
        return Err(format!(
            "GDI+ could not create the image (status {status})."
        ));
    }
    let bitmap = GdiPlusBitmap(raw_bitmap);

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
    if save_status != 0 {
        return Err(format!(
            "GDI+ could not save the image (status {save_status})."
        ));
    }
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

fn unique_output_path(directory: &Path, format: SaveFormat) -> PathBuf {
    let initial = directory.join(generate_screenshot_filename_for(format));
    if !initial.exists() {
        return initial;
    }

    let stem = initial
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Screenshot");
    for suffix in 1..=9999 {
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
    std::fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "Could not create the screenshot folder '{}': {error}",
            directory.display()
        )
    })?;
    if !directory.is_dir() {
        return Err(format!(
            "The screenshot destination '{}' is not a directory.",
            directory.display()
        ));
    }

    let output_path = unique_output_path(&directory, format);
    // This temp carries our pid + nanos and is removed on every handled failure below;
    // only a hard process kill can orphan it. Deliberately no age-based sweeper: it could
    // delete another process's in-flight temp or a user's own dotfile in a shared folder.
    let temp_path = directory.join(format!(
        ".isolmass-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));

    let result = (|| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| error.to_string())?;
        encode_image(
            buffer,
            full_width,
            full_height,
            selection,
            &temp_path,
            format,
            jpeg_quality,
        )?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&temp_path)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
        let source: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut destination = output_path;
        for _ in 0..128 {
            let target: Vec<u16> = destination
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            match unsafe {
                MoveFileExW(
                    PCWSTR(source.as_ptr()),
                    PCWSTR(target.as_ptr()),
                    MOVEFILE_WRITE_THROUGH,
                )
            } {
                Ok(()) => {
                    recent::saved(&destination);
                    return Ok(destination);
                }
                Err(error) if matches!(error.code().0 as u32, 0x80070050 | 0x800700b7) => {
                    destination = unique_output_path(&directory, format)
                }
                Err(error) => return Err(format!("Could not finalize the screenshot: {error}")),
            }
        }
        Err("Could not reserve a unique screenshot filename.".to_string())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

pub fn recent_screenshots(directory: &Path, limit: usize) -> std::io::Result<Vec<PathBuf>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut newest = std::collections::BinaryHeap::new();
    let started = std::time::Instant::now();
    for entry in std::fs::read_dir(directory)? {
        if recent::cancelled() || started.elapsed() > std::time::Duration::from_secs(2) {
            break;
        }
        // Only a directory-level failure aborts the scan; individual entries that were
        // removed or became unreadable since listing are skipped so one bad file cannot
        // discard every successful result.
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                crate::diagnostics::record(
                    "recent captures",
                    &format!("Skipping unreadable directory entry: {error}"),
                );
                continue;
            }
        };
        let path = entry.path();
        if !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(value.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg")
            })
        {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                crate::diagnostics::record(
                    "recent captures",
                    &format!(
                        "Skipping unreadable screenshot '{}': {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(value) => value,
            Err(error) => {
                crate::diagnostics::record(
                    "recent captures",
                    &format!(
                        "Skipping screenshot without timestamp '{}': {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        newest.push(std::cmp::Reverse((modified, path)));
        if newest.len() > limit.min(100) {
            newest.pop();
        }
    }
    let mut newest = newest.into_iter().map(|entry| entry.0).collect::<Vec<_>>();
    newest.sort_unstable_by(|a, b| b.cmp(a));
    Ok(newest.into_iter().map(|(_, path)| path).collect())
}
