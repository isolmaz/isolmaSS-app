//! Upload of a flattened selection to the user's own Worker. The editor only
//! encodes the image ([`prepare`]); sending happens on a background thread
//! owned by the upload toast ([`crate::toast`]), so the editor closes at once.
use crate::capture::Rect;
use crate::cloudflare_setup::{self, CloudCredentials, RequestBody, UploadProgress};
use crate::save::save_buffer_to_image;
use crate::settings::{SaveFormat, Settings};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod glue;
pub use glue::{open_link, send};

pub const WM_UPLOAD_DONE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 41;
/// Posted to the toast while the file is written: WPARAM is the share of the
/// body already sent in per-mille, LPARAM the number of the attempt.
pub const WM_UPLOAD_PROGRESS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 42;

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
    bytes: u64,
    format: SaveFormat,
    credentials: CloudCredentials,
}

impl Prepared {
    /// The encoded image on disk, there for as long as this value or a clone of it lives.
    pub fn path(&self) -> &Path {
        &self.file.0
    }

    /// File extension of the encoded image, without the dot.
    pub fn extension(&self) -> &'static str {
        self.format.extension()
    }
}

#[derive(Deserialize)]
struct UploadReceipt {
    id: String,
    url: String,
}

/// `%TEMP%\isolmaSS\upload`, where encoded screenshots wait for their upload card.
pub fn staging_directory() -> PathBuf {
    std::env::temp_dir().join("isolmaSS").join("upload")
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
    let directory = staging_directory();
    let output = directory.join(format!(
        "isolmass-upload-{unique}.{}",
        settings.save_format.extension()
    ));
    let file = Arc::new(TemporaryScreenshot(output.clone()));
    let bytes = std::fs::create_dir_all(&directory)
        .map_err(crate::save::describe)
        .and_then(|_| {
            save_buffer_to_image(
                pixels,
                width,
                height,
                selection,
                &output,
                settings.save_format,
                settings.jpeg_quality,
            )
        })
        .and_then(|_| std::fs::metadata(&output).map_err(crate::save::describe))
        .map_err(|error| {
            crate::i18n::tf("Could not prepare the screenshot for upload: {}", &[&error])
        })?
        .len();
    Ok(Prepared {
        file,
        bytes,
        format: settings.save_format,
        credentials,
    })
}

/// The shape of [`cloudflare_setup::api_request`]: origin, path, method, bearer token,
/// content type, image password, body and progress callback; the status and body back.
type ApiRequest<'a> = dyn FnMut(
        &str,
        &str,
        &str,
        &str,
        Option<&str>,
        Option<&str>,
        RequestBody<'_>,
        UploadProgress<'_>,
    ) -> Result<(u32, Vec<u8>), String>
    + 'a;

/// Sends the prepared screenshot through `request`, the HTTPS call to the Worker
/// ([`send`] passes [`cloudflare_setup::api_request`]), and returns its share link.
fn send_with(
    prepared: &Prepared,
    progress: UploadProgress<'_>,
    request: &mut ApiRequest<'_>,
) -> Result<String, String> {
    let type_name = match prepared.format {
        SaveFormat::Png => "image/png",
        SaveFormat::Jpeg => "image/jpeg",
    };
    let origin = prepared.credentials.origin.as_str();
    let (status, body) = request(
        origin,
        "/api/upload",
        "POST",
        &prepared.credentials.upload_token,
        Some(type_name),
        prepared.credentials.share_password.as_deref(),
        RequestBody::File(&prepared.file.0),
        progress,
    )?;
    if status != 201 {
        return Err(cloudflare_setup::rejection(
            status,
            &body,
            Some(prepared.bytes),
        ));
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

/// The NUL-terminated UTF-16 form of `url` for the shell, only for `https://` links:
/// the shell would run any other scheme (`file:`, `ms-settings:`) just as readily.
fn browser_target(url: &str) -> Result<Vec<u16>, String> {
    if !url.starts_with("https://") {
        return Err(crate::i18n::t("Invalid link.").to_string());
    }
    Ok(url.encode_utf16().chain(Some(0)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_env::{Sandbox, TempDir, lock};
    use std::sync::Mutex;

    const ORIGIN: &str = "https://isolmass-share-abc.example.workers.dev";
    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn credentials(password: Option<&str>) -> CloudCredentials {
        CloudCredentials {
            origin: ORIGIN.to_string(),
            upload_token: cloudflare_setup::generate_token().expect("token"),
            admin_token: cloudflare_setup::generate_token().expect("token"),
            share_password: password.map(str::to_string),
            account_id: None,
            worker_name: None,
        }
    }

    fn connected(format: SaveFormat) -> Settings {
        Settings {
            cloud_url: Some(ORIGIN.to_string()),
            save_format: format,
            jpeg_quality: 70,
            ..Settings::default()
        }
    }

    /// BGRA pixels of a screen capture.
    fn pixels(width: i32, height: i32) -> Vec<u8> {
        (0..width * height * 4)
            .map(|index| (index * 5) as u8)
            .collect()
    }

    #[test]
    fn share_links_are_limited_to_the_installation() {
        let id = "A".repeat(32);
        assert!(is_share_link(ORIGIN, &format!("{ORIGIN}/i/{id}")));
        assert!(!is_share_link(ORIGIN, &format!("{ORIGIN}/i/{id}x")));
        assert!(!is_share_link(
            ORIGIN,
            &format!("https://evil.example/i/{id}")
        ));
        assert!(!is_share_link(ORIGIN, &format!("{ORIGIN}/x/{id}")));
        assert!(!is_share_link(
            ORIGIN,
            &format!("{ORIGIN}/i/{}", "A/".repeat(16))
        ));
        assert!(!is_share_link(
            "http://isolmass-share-abc.example.workers.dev",
            &format!("http://isolmass-share-abc.example.workers.dev/i/{id}")
        ));
    }

    #[test]
    fn only_https_links_reach_the_shell() {
        let wide = browser_target("https://example.com/i/ü").expect("https link");
        assert_eq!(wide.last(), Some(&0), "NUL-terminated for the shell");
        assert_eq!(
            String::from_utf16(&wide[..wide.len() - 1]).unwrap(),
            "https://example.com/i/ü"
        );
        for url in [
            "http://example.com",
            "HTTPS://example.com",
            "file:///C:/Windows/System32/cmd.exe",
            "ms-settings:",
            "",
        ] {
            assert_eq!(
                browser_target(url).unwrap_err(),
                crate::i18n::t("Invalid link."),
                "{url:?}"
            );
        }
    }

    #[test]
    fn prepare_encodes_the_selection_into_a_file_owned_by_the_prepared_upload() {
        let _sandbox = Sandbox::new("upload_prepare");
        cloudflare_setup::save_credentials(&credentials(None)).expect("store the keys");
        let prepared = prepare(
            &pixels(8, 6),
            8,
            6,
            &Rect::new(1, 1, 5, 4),
            &connected(SaveFormat::Png),
        )
        .expect("prepare");

        let path = prepared.file.0.clone();
        assert_eq!(prepared.path(), path.as_path());
        assert_eq!(prepared.extension(), "png");
        assert_eq!(path.parent(), Some(staging_directory().as_path()));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("isolmass-upload-") && name.ends_with(".png"),
            "{name}"
        );
        let bytes = std::fs::read(&path).expect("encoded file");
        assert_eq!(&bytes[..8], PNG_SIGNATURE);
        assert_eq!(prepared.bytes, bytes.len() as u64);
        assert_eq!(prepared.format, SaveFormat::Png);
        assert_eq!(prepared.credentials.origin, ORIGIN);

        // The toast and an in-flight attempt share the file; the last one deletes it.
        let attempt = prepared.clone();
        drop(prepared);
        assert!(path.exists(), "still owned by the running attempt");
        drop(attempt);
        assert!(!path.exists(), "removed with the last owner");
    }

    #[test]
    fn prepare_follows_the_configured_save_format() {
        let _sandbox = Sandbox::new("upload_prepare_jpeg");
        cloudflare_setup::save_credentials(&credentials(None)).expect("store the keys");
        let prepared = prepare(
            &pixels(8, 6),
            8,
            6,
            &Rect::new(0, 0, 8, 6),
            &connected(SaveFormat::Jpeg),
        )
        .expect("prepare");
        let path = prepared.file.0.clone();
        assert!(path.extension().is_some_and(|extension| extension == "jpg"));
        assert_eq!(&std::fs::read(&path).unwrap()[..2], [0xFF, 0xD8]);
        assert_eq!(prepared.format, SaveFormat::Jpeg);
        assert_eq!(prepared.extension(), "jpg");
    }

    #[test]
    fn prepare_needs_a_matching_cloudflare_connection() {
        let _sandbox = Sandbox::new("upload_prepare_missing");
        let selection = Rect::new(0, 0, 8, 6);
        let mut settings = connected(SaveFormat::Png);

        settings.cloud_url = None;
        assert_eq!(
            prepare(&pixels(8, 6), 8, 6, &selection, &settings).err(),
            Some(
                crate::i18n::t("Connect Cloudflare in Settings > Sharing before uploading.")
                    .to_string()
            )
        );

        settings.cloud_url = Some(ORIGIN.to_string());
        let error = prepare(&pixels(8, 6), 8, 6, &selection, &settings)
            .err()
            .expect("no keys stored yet");
        assert!(
            error.starts_with("Cloudflare setup is not complete: "),
            "{error}"
        );

        cloudflare_setup::save_credentials(&credentials(None)).expect("store the keys");
        settings.cloud_url = Some("https://other.example.workers.dev".to_string());
        assert_eq!(
            prepare(&pixels(8, 6), 8, 6, &selection, &settings).err(),
            Some(
                crate::i18n::t(
                    "The Cloudflare address changed. Connect this computer again before uploading."
                )
                .to_string()
            )
        );
    }

    #[test]
    fn prepare_reports_an_unusable_selection() {
        let _sandbox = Sandbox::new("upload_prepare_invalid");
        cloudflare_setup::save_credentials(&credentials(None)).expect("store the keys");
        let error = prepare(
            &pixels(8, 6),
            8,
            6,
            &Rect::new(2, 2, 2, 2),
            &connected(SaveFormat::Png),
        )
        .err()
        .expect("empty selection");
        assert_eq!(
            error,
            crate::i18n::tf(
                "Could not prepare the screenshot for upload: {}",
                &[&crate::i18n::t(
                    "The screenshot selection is empty or invalid."
                )]
            )
        );
    }

    #[test]
    fn the_temporary_screenshot_is_deleted_quietly() {
        let dir = TempDir::new("upload_cleanup");
        let file = dir.path().join("shot.png");
        std::fs::write(&file, b"image").unwrap();
        drop(TemporaryScreenshot(file.clone()));
        assert!(!file.exists());
        // Already gone: nothing to report.
        drop(TemporaryScreenshot(file));
        // Not deletable (a folder): the failure is recorded, the folder stays.
        let folder = dir.path().join("folder.png");
        std::fs::create_dir(&folder).unwrap();
        drop(TemporaryScreenshot(folder.clone()));
        assert!(folder.is_dir());
    }

    /// A prepared upload of `content` as a file below `dir`.
    fn prepared(
        dir: &TempDir,
        format: SaveFormat,
        password: Option<&str>,
        content: &[u8],
    ) -> Prepared {
        let path = dir.path().join("upload.bin");
        std::fs::write(&path, content).unwrap();
        Prepared {
            file: Arc::new(TemporaryScreenshot(path)),
            bytes: content.len() as u64,
            format,
            credentials: credentials(password),
        }
    }

    fn receipt(origin: &str, id: &str, url_id: &str) -> Vec<u8> {
        format!(r#"{{"id":"{id}","url":"{origin}/i/{url_id}"}}"#).into_bytes()
    }

    #[test]
    fn send_posts_the_file_with_the_upload_key_and_returns_the_share_link() {
        let dir = TempDir::new("upload_send");
        let id = "A1".repeat(16);
        for (format, expected_type) in [
            (SaveFormat::Png, "image/png"),
            (SaveFormat::Jpeg, "image/jpeg"),
        ] {
            let upload = prepared(
                &dir,
                format,
                Some("correct horse battery"),
                b"encoded image",
            );
            let reported = Mutex::new(Vec::new());
            let progress = |sent: u64, total: u64| lock(&reported).push((sent, total));
            let mut seen = None;
            let link = send_with(
                &upload,
                Some(&progress),
                &mut |origin, path, method, bearer, content_type, password, body, progress| {
                    let mut sent_file = None;
                    if let RequestBody::File(file) = body {
                        sent_file = Some(std::fs::read(file).unwrap());
                    }
                    seen = Some((
                        (origin.to_string(), path.to_string(), method.to_string()),
                        (bearer.to_string(), content_type.map(str::to_string)),
                        password.map(str::to_string),
                        sent_file,
                    ));
                    progress.expect("the progress callback is passed on")(13, 26);
                    Ok((201, receipt(ORIGIN, &id, &id)))
                },
            )
            .expect("share link");

            assert_eq!(link, format!("{ORIGIN}/i/{id}"));
            let (target, authorization, password, body) = seen.expect("the request was made");
            assert_eq!(
                target,
                (
                    ORIGIN.to_string(),
                    "/api/upload".to_string(),
                    "POST".to_string()
                )
            );
            assert_eq!(
                authorization,
                (
                    upload.credentials.upload_token.clone(),
                    Some(expected_type.to_string())
                )
            );
            assert_eq!(password.as_deref(), Some("correct horse battery"));
            assert_eq!(
                body.as_deref(),
                Some(b"encoded image".as_slice()),
                "the screenshot is sent from its file"
            );
            assert_eq!(*lock(&reported), [(13, 26)]);
        }
    }

    #[test]
    fn send_reports_network_failures_unchanged() {
        let dir = TempDir::new("upload_offline");
        let upload = prepared(&dir, SaveFormat::Png, None, b"image");
        let error = send_with(
            &upload,
            None,
            &mut |_, _, _, _, _, password, _, progress| {
                assert_eq!(password, None, "no image password configured");
                assert!(progress.is_none());
                Err("offline".to_string())
            },
        )
        .unwrap_err();
        assert_eq!(error, "offline");
    }

    #[test]
    fn send_explains_rejections() {
        let _sandbox = Sandbox::new("upload_rejected");
        let dir = TempDir::new("upload_rejected_file");
        let upload = prepared(&dir, SaveFormat::Png, None, &vec![0; 1_500_000]);
        let rejected = |status: u32, body: &[u8]| {
            send_with(&upload, None, &mut |_, _, _, _, _, _, _, _| {
                Ok((status, body.to_vec()))
            })
            .unwrap_err()
        };
        assert!(rejected(401, b"{}").contains("no longer matches your Worker"));
        let too_large = rejected(413, b"");
        assert!(
            too_large.contains("(1.5 MB)"),
            "the size of the capture is reported: {too_large}"
        );
        assert!(rejected(500, b"").contains("temporarily unavailable"));
        assert_eq!(
            rejected(403, br#"{"error":"Forbidden here"}"#),
            "Forbidden here"
        );
    }

    #[test]
    fn send_accepts_only_this_installations_links() {
        let dir = TempDir::new("upload_receipt");
        let upload = prepared(&dir, SaveFormat::Png, None, b"image");
        let answer = |body: Vec<u8>| {
            send_with(&upload, None, &mut |_, _, _, _, _, _, _, _| {
                Ok((201, body.clone()))
            })
            .unwrap_err()
        };
        let id = "B2".repeat(16);
        assert_eq!(
            answer(b"not json".to_vec()),
            crate::i18n::t("Cloudflare returned an invalid upload response.")
        );
        let foreign =
            crate::i18n::t("Cloudflare returned a link that does not belong to this installation.");
        assert_eq!(
            answer(receipt("https://evil.example.com", &id, &id)),
            foreign,
            "another origin"
        );
        assert_eq!(
            answer(receipt(ORIGIN, &id, &"C3".repeat(16))),
            foreign,
            "the link must name the id of the receipt"
        );
    }
}
