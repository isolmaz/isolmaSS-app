//! Upload of a flattened selection to the user's own Worker. The editor only
//! encodes the image ([`prepare`]); sending happens on a background thread
//! owned by the upload toast ([`crate::toast`]), so the editor closes at once.
use crate::capture::Rect;
use crate::cloudflare_setup::{self, CloudCredentials, RequestBody};
use crate::save::save_buffer_to_image;
use crate::settings::{SaveFormat, Settings};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

pub const WM_UPLOAD_DONE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 41;

/// The encoded screenshot on disk; removed when the last owner drops it.
struct TemporaryScreenshot(PathBuf);
impl Drop for TemporaryScreenshot {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::diagnostics::record("temporary upload cleanup", &error.to_string());
        }
    }
}

/// An encoded screenshot and the credentials to send it. Cheap to clone; the
/// file lives until the toast and every in-flight attempt are done with it.
#[derive(Clone)]
pub struct Prepared {
    file: Arc<TemporaryScreenshot>,
    format: SaveFormat,
    credentials: CloudCredentials,
}

#[derive(Deserialize)]
struct UploadReceipt {
    id: String,
    url: String,
}

/// Encodes the already-flattened editor selection to a private temporary file.
pub fn prepare(
    pixels: &[u8],
    width: i32,
    height: i32,
    selection: &Rect,
    settings: &Settings,
) -> Result<Prepared, String> {
    let origin = settings.cloud_url.as_deref().ok_or_else(|| {
        crate::i18n::t("Connect Cloudflare in Settings > Sharing before uploading.").to_string()
    })?;
    let credentials = cloudflare_setup::load_credentials(origin)?;
    let unique = cloudflare_setup::generate_token()?;
    let output = std::env::temp_dir().join(format!(
        "isolmass-upload-{unique}.{}",
        settings.save_format.extension()
    ));
    let file = Arc::new(TemporaryScreenshot(output.clone()));
    save_buffer_to_image(
        pixels,
        width,
        height,
        selection,
        &output,
        settings.save_format,
        settings.jpeg_quality,
    )
    .map_err(|error| {
        crate::i18n::tf("Could not prepare the screenshot for upload: {}", &[&error])
    })?;
    Ok(Prepared {
        file,
        format: settings.save_format,
        credentials,
    })
}

/// Sends the prepared screenshot (blocking) and returns its share link.
pub fn send(prepared: &Prepared) -> Result<String, String> {
    let type_name = match prepared.format {
        SaveFormat::Png => "image/png",
        SaveFormat::Jpeg => "image/jpeg",
    };
    let origin = prepared.credentials.origin.as_str();
    let (status, body) = cloudflare_setup::api_request(
        origin,
        "/api/upload",
        "POST",
        &prepared.credentials.upload_token,
        Some(type_name),
        prepared.credentials.share_password.as_deref(),
        RequestBody::File(&prepared.file.0),
    )?;
    if status != 201 {
        let detail = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|text| text.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| crate::i18n::tf("The upload failed (HTTP {}).", &[&status]));
        return Err(detail
            .chars()
            .filter(|character| !character.is_control())
            .take(240)
            .collect());
    }
    let receipt: UploadReceipt = serde_json::from_slice(&body).map_err(|_| {
        crate::i18n::t("Cloudflare returned an invalid upload response.").to_string()
    })?;
    if !is_share_link(origin, &receipt.url) || receipt.url != format!("{origin}/i/{}", receipt.id) {
        return Err(crate::i18n::t(
            "Cloudflare returned a link that does not belong to this installation.",
        )
        .to_string());
    }
    Ok(receipt.url)
}

/// True for `<origin>/i/<32-character id>`: the only links the app copies or
/// opens, so a compromised response cannot send the user elsewhere.
pub fn is_share_link(origin: &str, url: &str) -> bool {
    cloudflare_setup::valid_cloud_origin(origin)
        && url
            .strip_prefix(origin)
            .and_then(|rest| rest.strip_prefix("/i/"))
            .is_some_and(|id| {
                id.len() == 32
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
}

/// Opens a validated share link in the default browser.
pub fn open_link(url: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};
    if !url.starts_with("https://") {
        return Err(crate::i18n::t("Invalid link.").to_string());
    }
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            windows::Win32::Foundation::HWND::default(),
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(crate::i18n::t("The link could not be opened in the browser.").to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_links_are_limited_to_the_installation() {
        let origin = "https://isolmass-share-abc.example.workers.dev";
        let id = "A".repeat(32);
        assert!(is_share_link(origin, &format!("{origin}/i/{id}")));
        assert!(!is_share_link(origin, &format!("{origin}/i/{id}x")));
        assert!(!is_share_link(
            origin,
            &format!("https://evil.example/i/{id}")
        ));
        assert!(!is_share_link(origin, &format!("{origin}/x/{id}")));
        assert!(!is_share_link(
            origin,
            &format!("{origin}/i/{}", "A/".repeat(16))
        ));
    }
}
