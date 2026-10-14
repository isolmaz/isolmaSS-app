use serde::Deserialize;
use std::ffi::c_void;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WinHttpCloseHandle,
};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::{PCWSTR, w};

const RELEASE_API_HOST: &str = "api.github.com";
const RELEASE_API_PATH: &str = "/repos/isolmaz/isolmaSS-app/releases/latest";
const INSTALLER_ASSET_NAME: &str = "isolmass-setup.exe";
const SIGNATURE_ASSET_NAME: &str = "isolmass-setup.exe.sig";
const PINNED_PUBLIC_KEY: &[u8] = include_bytes!("../resources/update-public-key.blob");
const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_INSTALLER_BYTES: usize = 32 * 1024 * 1024;
/// Overall limits per request. Stalls are caught earlier by the per-operation WinHTTP timeouts.
const METADATA_DEADLINE: Duration = Duration::from_secs(45);
const INSTALLER_DEADLINE: Duration = Duration::from_secs(300);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: String,
    pub download_url: String,
    pub sha256: String,
    pub signature_url: String,
    pub release_url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

/// Where requests really go. The app talks HTTPS to each URL's own host on the default port;
/// the tests aim the same code at a plain-HTTP server on the loopback interface.
#[derive(Clone, Copy)]
struct Route<'a> {
    /// Connect here instead of to the URL's host.
    host: Option<&'a str>,
    port: u16,
    flags: WINHTTP_OPEN_REQUEST_FLAGS,
}

/// What an update run talks to and trusts. The app uses [`LIVE`]; the tests substitute a
/// loopback server and a key whose private half they hold.
#[derive(Clone, Copy)]
struct Channel<'a> {
    route: Route<'a>,
    /// `BCRYPT_RSAPUBLIC_BLOB` that release signatures must verify under.
    public_key: &'a [u8],
}

const LIVE: Channel<'static> = Channel {
    route: Route {
        host: None,
        port: INTERNET_DEFAULT_HTTPS_PORT,
        flags: WINHTTP_FLAG_SECURE,
    },
    public_key: PINNED_PUBLIC_KEY,
};

struct InternetHandle(*mut c_void);

impl InternetHandle {
    /// Takes ownership of a handle returned by WinHTTP. A null handle means the call failed;
    /// the reason is the thread's last error, filled in after the `context` arguments of the
    /// translated `template`.
    fn adopt(
        raw: *mut c_void,
        template: &'static str,
        context: &[&dyn std::fmt::Display],
    ) -> Result<Self, String> {
        if raw.is_null() {
            let error = windows::core::Error::from_win32();
            let mut arguments = context.to_vec();
            arguments.push(&error);
            Err(crate::i18n::tf(template, &arguments))
        } else {
            Ok(Self(raw))
        }
    }
}

/// Words a failed call: fills the error into the translated `template`. Callers translate the
/// literal with `crate::i18n::t` so that the interface strings stay visible to the
/// translation check.
fn failed<E: std::fmt::Display>(template: &'static str) -> impl FnOnce(E) -> String {
    move |error| crate::i18n::tf(template, &[&error])
}

impl Drop for InternetHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn parse_https_url(url: &str) -> Result<(&str, String), String> {
    let remainder = url
        .strip_prefix("https://")
        .ok_or_else(|| crate::i18n::t("Update downloads must use HTTPS.").to_string())?;
    let (host, path) = remainder
        .split_once('/')
        .map(|(host, path)| (host, format!("/{path}")))
        .unwrap_or((remainder, "/".to_string()));
    if !matches!(
        host,
        "api.github.com"
            | "github.com"
            | "release-assets.githubusercontent.com"
            | "objects.githubusercontent.com"
    ) || url.chars().any(|ch| ch.is_control() || ch == '\\')
        || url.contains('#')
    {
        return Err(crate::i18n::t("The update URL contains an unsupported host.").to_string());
    }
    Ok((host, path))
}

fn http_get(
    route: Route<'_>,
    host: &str,
    path: &str,
    maximum_size: usize,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    http_get_inner(
        route,
        host,
        path,
        maximum_size,
        Instant::now() + deadline,
        0,
    )
}

fn check_cancelled(deadline: Instant) -> Result<(), String> {
    if CANCELLED.load(Ordering::Acquire) {
        Err(crate::i18n::t("Update operation cancelled.").to_string())
    } else if Instant::now() >= deadline {
        Err(crate::i18n::t("The update request exceeded its time limit.").to_string())
    } else {
        Ok(())
    }
}

fn http_get_inner(
    route: Route<'_>,
    host: &str,
    path: &str,
    maximum_size: usize,
    deadline: Instant,
    redirects: u8,
) -> Result<Vec<u8>, String> {
    use crate::i18n::t;
    use windows::Win32::Networking::WinHttp::*;
    check_cancelled(deadline)?;
    if redirects > 5 {
        return Err(t("Too many update redirects.").to_string());
    }
    parse_https_url(&format!("https://{host}{path}"))?;
    let agent = wide(&format!("isolmaSS/{}", env!("CARGO_PKG_VERSION")));
    let session = unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    };
    let session = InternetHandle::adopt(session, t("WinHTTP could not start: {}"), &[])?;

    unsafe { WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 5_000) }
        .map_err(failed(t("Could not set update timeouts: {}")))?;
    let host_wide = wide(route.host.unwrap_or(host));
    let connection =
        unsafe { WinHttpConnect(session.0, PCWSTR(host_wide.as_ptr()), route.port, 0) };
    let connection = InternetHandle::adopt(connection, t("Could not connect to {}: {}"), &[&host])?;

    let path_wide = wide(path);
    let request = unsafe {
        WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            route.flags,
        )
    };
    let request = InternetHandle::adopt(request, t("Could not create the HTTPS request: {}"), &[])?;

    unsafe {
        WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes()),
        )
    }
    .map_err(failed(t("Could not enforce the HTTPS redirect policy: {}")))?;
    let headers: Vec<u16> = concat!(
        "Accept: application/vnd.github+json\r\n",
        "User-Agent: isolmaSS\r\n",
        "X-GitHub-Api-Version: 2022-11-28\r\n"
    )
    .encode_utf16()
    .collect();
    unsafe {
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0)
            .and_then(|_| WinHttpReceiveResponse(request.0, std::ptr::null_mut()))
    }
    .map_err(failed(t("The update request failed: {}")))?;

    let mut status = 0u32;
    let mut status_size = std::mem::size_of::<u32>() as u32;
    let mut index = 0u32;
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut status_size,
            &mut index,
        )
    }
    .map_err(failed(t("Could not read the update response status: {}")))?;
    check_cancelled(deadline)?;
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        let mut location = [0u16; 4096];
        let mut size = (location.len() * 2) as u32;
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_LOCATION,
                PCWSTR::null(),
                Some(location.as_mut_ptr().cast()),
                &mut size,
                std::ptr::null_mut(),
            )
        }
        .map_err(failed(t("The update redirect is invalid: {}")))?;
        let length = location
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(location.len());
        let location = String::from_utf16(&location[..length])
            .map_err(failed(t("Invalid redirect encoding")))?;
        let (host, path) = parse_https_url(&location)?;
        return http_get_inner(route, host, &path, maximum_size, deadline, redirects + 1);
    }
    if !(200..300).contains(&status) {
        return Err(crate::i18n::tf(
            "The update server returned HTTP {}.",
            &[&status],
        ));
    }

    let mut response = Vec::new();
    loop {
        check_cancelled(deadline)?;
        let mut available = 0u32;
        unsafe { WinHttpQueryDataAvailable(request.0, &mut available) }
            .map_err(failed(t("Could not read update data: {}")))?;
        if available == 0 {
            break;
        }
        let new_length = grown_length(response.len(), available, maximum_size)?;
        let offset = response.len();
        response.resize(new_length, 0);
        let mut read = 0u32;
        unsafe {
            WinHttpReadData(
                request.0,
                response[offset..].as_mut_ptr().cast(),
                available,
                &mut read,
            )
        }
        .map_err(failed(t("Could not receive update data: {}")))?;
        response.truncate(offset + read as usize);
    }
    Ok(response)
}

/// The response length after `added` more bytes, which must stay within `maximum`.
fn grown_length(current: usize, added: u32, maximum: usize) -> Result<usize, String> {
    let grown = current
        .checked_add(added as usize)
        .ok_or_else(|| crate::i18n::t("The update response is too large.").to_string())?;
    if grown > maximum {
        return Err(crate::i18n::tf(
            "The update response exceeded {} bytes.",
            &[&maximum],
        ));
    }
    Ok(grown)
}

fn parse_version(value: &str) -> Option<[u64; 3]> {
    let core = value.strip_prefix('v').unwrap_or(value);
    if core.is_empty()
        || !core
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || core
            .split('.')
            .any(|part| part.len() > 1 && part.starts_with('0'))
    {
        return None;
    }
    let mut components = core.split('.');
    let parsed = [
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
    ];
    if components.next().is_some() {
        return None;
    }
    Some(parsed)
}

/// The newest published release when it is newer than `current_version` and complete.
fn fetch_release(route: Route<'_>, current_version: &str) -> Result<Option<UpdateInfo>, String> {
    let bytes = http_get(
        route,
        RELEASE_API_HOST,
        RELEASE_API_PATH,
        MAX_METADATA_BYTES,
        METADATA_DEADLINE,
    )?;
    let release: GithubRelease = serde_json::from_slice(&bytes)
        .map_err(|error| crate::i18n::tf("The update metadata is invalid: {}", &[&error]))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }

    let current = parse_version(current_version).ok_or_else(|| {
        crate::i18n::t("The installed application version is invalid.").to_string()
    })?;
    let available = parse_version(&release.tag_name)
        .ok_or_else(|| crate::i18n::t("The release tag is not a semantic version.").to_string())?;
    if available <= current {
        return Ok(None);
    }

    let mut installer = None;
    let mut signature = None;
    for asset in release.assets {
        if asset.name == INSTALLER_ASSET_NAME {
            if installer.replace(asset).is_some() {
                return Err(
                    crate::i18n::t("The release contains duplicate installers.").to_string()
                );
            }
        } else if asset.name == SIGNATURE_ASSET_NAME && signature.replace(asset).is_some() {
            return Err(crate::i18n::t("The release contains duplicate signatures.").to_string());
        }
    }
    let asset = installer.ok_or_else(|| {
        crate::i18n::tf(
            "Release {} has no {} asset.",
            &[&release.tag_name, &INSTALLER_ASSET_NAME],
        )
    })?;
    let signature = signature.ok_or_else(|| {
        crate::i18n::tf(
            "Release {} has no {} asset.",
            &[&release.tag_name, &SIGNATURE_ASSET_NAME],
        )
    })?;
    let digest = asset
        .digest
        .and_then(|digest| digest.strip_prefix("sha256:").map(str::to_owned))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| {
            crate::i18n::t("The release installer has no valid SHA-256 digest.").to_string()
        })?
        .to_ascii_lowercase();
    parse_https_url(&asset.browser_download_url)?;
    parse_https_url(&signature.browser_download_url)?;
    if !release
        .html_url
        .starts_with("https://github.com/isolmaz/isolmaSS-app/releases/tag/")
    {
        return Err(crate::i18n::t("Unexpected release information URL.").to_string());
    }
    Ok(Some(UpdateInfo {
        version: release.tag_name.trim_start_matches('v').to_string(),
        download_url: asset.browser_download_url,
        signature_url: signature.browser_download_url,
        sha256: digest,
        release_url: release.html_url,
    }))
}

/// Why staging an update failed. Each kind gets its own wording, so a dropped connection is
/// never reported like a tampered installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadFailure {
    /// A release asset could not be fetched.
    Network,
    /// The signature, digest or embedded version does not match the signed release.
    Integrity,
    /// The verified installer could not be written to the staging folder.
    Storage,
}

#[derive(Debug)]
pub struct DownloadError {
    pub kind: DownloadFailure,
    pub message: String,
}

impl DownloadError {
    fn network(message: String) -> Self {
        Self {
            kind: DownloadFailure::Network,
            message,
        }
    }

    fn integrity(message: String) -> Self {
        Self {
            kind: DownloadFailure::Integrity,
            message,
        }
    }

    fn storage(message: String) -> Self {
        Self {
            kind: DownloadFailure::Storage,
            message,
        }
    }
}

/// Fetches, verifies and stages the installer of `update` in the folder `staging` names.
/// The folder is resolved last, so a failed download never reports a storage problem.
fn download_from(
    channel: &Channel<'_>,
    update: &UpdateInfo,
    staging: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<PathBuf, DownloadError> {
    if parse_version(&update.version).is_none()
        || update.sha256.len() != 64
        || !update.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(DownloadError::integrity(
            crate::i18n::t("Invalid signed update metadata.").to_string(),
        ));
    }
    let (signature_host, signature_path) =
        parse_https_url(&update.signature_url).map_err(DownloadError::integrity)?;
    let signature = http_get(
        channel.route,
        signature_host,
        &signature_path,
        512,
        METADATA_DEADLINE,
    )
    .map_err(DownloadError::network)?;
    verify_release_signature(
        channel.public_key,
        &update.version,
        &update.sha256,
        &signature,
    )
    .map_err(DownloadError::integrity)?;
    let (host, path) = parse_https_url(&update.download_url).map_err(DownloadError::integrity)?;
    let bytes = http_get(
        channel.route,
        host,
        &path,
        MAX_INSTALLER_BYTES,
        INSTALLER_DEADLINE,
    )
    .map_err(DownloadError::network)?;
    if sha256_hex(&bytes).map_err(DownloadError::integrity)? != update.sha256 {
        return Err(DownloadError::integrity(
            crate::i18n::t("The downloaded installer does not match its signed SHA-256 digest.")
                .to_string(),
        ));
    }
    let root = staging().map_err(DownloadError::storage)?;
    std::fs::create_dir_all(&root).map_err(|error| {
        DownloadError::storage(crate::i18n::tf(
            "Could not create the update folder: {}",
            &[&error],
        ))
    })?;
    let destination = root.join(format!("isolmass-setup-{}.exe", update.version));
    let signature_destination = destination.with_extension("exe.sig");
    let temporary = root.join(format!(
        ".download-{}-{}-{}.tmp",
        std::process::id(),
        update.version,
        std::time::UNIX_EPOCH
            .elapsed()
            .unwrap_or_default()
            .as_nanos()
    ));
    let signature_temporary = temporary.with_extension("sig.tmp");
    let result = (|| {
        write_staged_file(&temporary, &bytes).map_err(DownloadError::storage)?;
        check_cancelled(Instant::now() + Duration::from_secs(1)).map_err(DownloadError::network)?;
        if file_version(&temporary).map_err(DownloadError::integrity)? != update.version {
            return Err(DownloadError::integrity(
                crate::i18n::t("The installer file version does not match the signed release.")
                    .to_string(),
            ));
        }
        write_staged_file(&signature_temporary, &signature).map_err(DownloadError::storage)?;
        replace_staged_file(&temporary, &destination).map_err(DownloadError::storage)?;
        replace_staged_file(&signature_temporary, &signature_destination)
            .map_err(DownloadError::storage)?;
        Ok(destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
        let _ = std::fs::remove_file(&signature_temporary);
    }
    result
}

fn update_directory() -> Result<PathBuf, String> {
    update_directory_in(std::env::var_os("LOCALAPPDATA"))
}

/// The folder staged installers live in, below the user's local application data folder.
fn update_directory_in(local_app_data: Option<std::ffi::OsString>) -> Result<PathBuf, String> {
    local_app_data
        .map(PathBuf::from)
        .ok_or_else(|| "%LOCALAPPDATA% is unavailable.".to_string())
        .map(|root| root.join("isolmaSS").join("updates"))
}

/// Whether `name` in the update folder is a leftover: a staged installer or signature for the
/// running or an older version, or an interrupted download.
fn is_stale_update_file(name: &str, current: [u64; 3]) -> bool {
    if name.starts_with(".download-") && name.ends_with(".tmp") {
        return true;
    }
    name.strip_prefix("isolmass-setup-")
        .and_then(|rest| {
            rest.strip_suffix(".exe.sig")
                .or_else(|| rest.strip_suffix(".exe"))
        })
        .and_then(parse_version)
        .is_some_and(|version| version <= current)
}

/// Whether a failed cleanup is worth a log line. A file that is already gone needs nothing, and
/// one still in use (the installer that just started this version is still finishing) is
/// removed on a later start.
fn cleanup_failure_matters(error: &std::io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    error.kind() != std::io::ErrorKind::NotFound
        && error.raw_os_error() != Some(ERROR_SHARING_VIOLATION)
}

/// Deletes the leftovers in `root`, which may not exist yet.
fn remove_stale_files_in(root: &Path, current: [u64; 3]) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            if cleanup_failure_matters(&error) {
                crate::diagnostics::record("update cleanup", &error.to_string());
            }
            return;
        }
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| is_stale_update_file(name, current))
            && let Err(error) = std::fs::remove_file(entry.path())
            && cleanup_failure_matters(&error)
        {
            crate::diagnostics::record("update cleanup", &error.to_string());
        }
    }
}

const APP_PATHS_KEY: PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\isolmass.exe");

/// The executable the installer registered under App Paths.
fn registered_executable() -> Option<PathBuf> {
    registered_executable_at(APP_PATHS_KEY)
}

/// The default string value of `key` under HKEY_CURRENT_USER, read as a path.
fn registered_executable_at(key: PCWSTR) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    if !(2..=65_536).contains(&size) {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    let length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    Some(PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length],
    )))
}

/// An installed copy runs from the folder the installer registered and has its uninstaller
/// beside it. The portable ZIP never does, so it must not install over or register anything.
fn is_installed_copy(
    exe_dir: &Path,
    registered_dir: Option<&Path>,
    uninstaller_present: bool,
) -> bool {
    uninstaller_present
        && registered_dir
            .is_some_and(|dir| dir.as_os_str().eq_ignore_ascii_case(exe_dir.as_os_str()))
}

/// False for the portable build, whose update offer opens the release page instead.
pub fn runs_from_installation() -> bool {
    is_installation(std::env::current_exe().ok(), registered_executable())
}

/// Whether the executable `running` is the installed copy, given the one the installer
/// `registered`. Both are resolved first, so links and relative spellings compare equal.
fn is_installation(running: Option<PathBuf>, registered: Option<PathBuf>) -> bool {
    let parent_of = |path: PathBuf| {
        std::fs::canonicalize(path)
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
    };
    let Some(exe_dir) = running.and_then(parent_of) else {
        return false;
    };
    let registered_dir = registered.and_then(parent_of);
    is_installed_copy(
        &exe_dir,
        registered_dir.as_deref(),
        exe_dir.join("uninstall.exe").is_file(),
    )
}

fn write_staged_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(failed(crate::i18n::t("Could not stage the update: {}")))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(failed(crate::i18n::t(
            "Could not write the staged update: {}",
        )))
}

fn replace_staged_file(source: &Path, target: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| crate::i18n::tf("Could not finalize the staged update: {}", &[&error]))
}

/// Signature covers both the exact semantic version and the downloaded executable's digest.
/// `public_key` is a `BCRYPT_RSAPUBLIC_BLOB`; the app only ever passes [`PINNED_PUBLIC_KEY`].
fn verify_release_signature(
    public_key: &[u8],
    version: &str,
    digest: &str,
    signature: &[u8],
) -> Result<(), String> {
    use windows::Win32::Security::Cryptography::{
        BCRYPT_KEY_HANDLE, BCRYPT_PAD_PKCS1, BCRYPT_PKCS1_PADDING_INFO, BCRYPT_RSA_ALG_HANDLE,
        BCRYPT_RSAPUBLIC_BLOB, BCRYPT_SHA256_ALGORITHM, BCryptDestroyKey, BCryptImportKeyPair,
        BCryptVerifySignature,
    };
    if parse_version(version).is_none()
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || signature.len() != 384
    {
        return Err(crate::i18n::t("Invalid update signature metadata.").to_string());
    }
    let message = format!(
        "isolmaSS-update-v1\n{version}\n{}\n",
        digest.to_ascii_lowercase()
    );
    let hash = sha256_digest(message.as_bytes())?;
    let mut key = BCRYPT_KEY_HANDLE::default();
    unsafe {
        BCryptImportKeyPair(
            BCRYPT_RSA_ALG_HANDLE,
            BCRYPT_KEY_HANDLE::default(),
            BCRYPT_RSAPUBLIC_BLOB,
            &mut key,
            public_key,
            0,
        )
    }
    .ok()
    .map_err(|error| crate::i18n::tf("Could not load the pinned update key: {}", &[&error]))?;
    let padding = BCRYPT_PKCS1_PADDING_INFO {
        pszAlgId: BCRYPT_SHA256_ALGORITHM,
    };
    let result = unsafe {
        BCryptVerifySignature(
            key,
            Some((&raw const padding).cast()),
            &hash,
            signature,
            BCRYPT_PAD_PKCS1,
        )
    }
    .ok();
    unsafe {
        let _ = BCryptDestroyKey(key);
    }
    result.map_err(|_| {
        crate::i18n::t("The update signature does not match the pinned publisher key.").to_string()
    })
}
/// Read the version embedded in an executable, independent of its filename or timestamp.
pub fn file_version(path: &Path) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
    };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
    let missing = crate::i18n::t("The executable has no valid version resource.");
    if size == 0 || size > 1024 * 1024 {
        return Err(missing.to_string());
    }
    // u32 alignment is sufficient for VS_FIXEDFILEINFO and the Win32 version block.
    let mut data = vec![0u32; (size as usize).div_ceil(4)];
    unsafe { GetFileVersionInfoW(PCWSTR(path.as_ptr()), 0, size, data.as_mut_ptr().cast()) }
        .map_err(failed(missing))?;
    let mut pointer = std::ptr::null_mut();
    let mut length = 0;
    if !unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            windows::core::w!("\\"),
            &mut pointer,
            &mut length,
        )
        .as_bool()
    } || pointer.is_null()
        || length < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
    {
        return Err(crate::i18n::t("The executable has an invalid version resource.").to_string());
    }
    fixed_version(&unsafe { std::ptr::read_unaligned(pointer.cast::<VS_FIXEDFILEINFO>()) })
}

/// The `major.minor.patch` of a version resource's fixed block. Windows only hands out blocks
/// carrying the signature, so the check guards against a changed API contract.
fn fixed_version(
    info: &windows::Win32::Storage::FileSystem::VS_FIXEDFILEINFO,
) -> Result<String, String> {
    if info.dwSignature != 0xfeef04bd {
        return Err(crate::i18n::t("The executable version signature is invalid.").to_string());
    }
    Ok(format!(
        "{}.{}.{}",
        info.dwFileVersionMS >> 16,
        info.dwFileVersionMS & 0xffff,
        info.dwFileVersionLS >> 16
    ))
}

/// Checks that `path` is a signed installer in the staging folder `root`, named after the
/// version it carries.
fn verify_staged_installer(public_key: &[u8], root: &Path, path: &Path) -> Result<(), String> {
    let root = std::fs::canonicalize(root)
        .map_err(|error| crate::i18n::tf("Could not find the update folder: {}", &[&error]))?;
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| crate::i18n::tf("Could not find the staged installer: {}", &[&error]))?;
    if canonical.parent() != Some(root.as_path()) {
        return Err(
            crate::i18n::t("The installer is not in the trusted update staging folder.")
                .to_string(),
        );
    }
    let version = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("isolmass-setup-"))
        .and_then(|name| name.strip_suffix(".exe"))
        .filter(|version| parse_version(version).is_some())
        .ok_or_else(|| crate::i18n::t("Unexpected staged installer filename.").to_string())?;
    let verified_version = verify_artifact(public_key, path)?;
    if verified_version != version {
        return Err(crate::i18n::t(
            "The staged installer version differs from its signed release.",
        )
        .to_string());
    }
    Ok(())
}

/// Verify a local installer and detached signature against the pinned publisher key without
/// executing either.
pub fn verify_update_artifact(path: &Path) -> Result<String, String> {
    verify_artifact(PINNED_PUBLIC_KEY, path)
}

fn verify_artifact(public_key: &[u8], path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > MAX_INSTALLER_BYTES as u64 {
        return Err(crate::i18n::t("The staged installer exceeds the allowed size.").to_string());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let digest = sha256_hex(&bytes)?;
    let signature = std::fs::read(path.with_extension("exe.sig")).map_err(|error| {
        crate::i18n::tf("The staged update signature is unavailable: {}", &[&error])
    })?;
    let version = file_version(path)?;
    verify_release_signature(public_key, &version, &digest, &signature)?;
    Ok(version)
}

mod job;
mod live;
use job::CANCELLED;
pub use job::{configure, is_checking, shutdown};
pub use live::{check_for_update, poll, run_manual_update_check, run_startup_work};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_digest(input: &[u8]) -> Result<[u8; 32], String> {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut digest = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, input, &mut digest) }
        .ok()
        .map_err(failed(crate::i18n::t("Windows SHA-256 failed: {}")))?;
    Ok(digest)
}

fn sha256_hex(input: &[u8]) -> Result<String, String> {
    Ok(hex(&sha256_digest(input)?))
}

#[cfg(test)]
mod tests;
