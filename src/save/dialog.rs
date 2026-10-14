//! The Save As dialog: a modal shell dialog that only a person at a desktop can answer.
use super::generate_screenshot_filename_for;
use crate::settings::SaveFormat;
use std::path::PathBuf;
use windows::core::PCWSTR;

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
