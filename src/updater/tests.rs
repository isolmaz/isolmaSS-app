use super::*;
use crate::i18n::{t, tf};
use serde_json::{Value, json};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, LazyLock};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, SetLastError};
use windows::Win32::Security::Cryptography::{
    BCRYPT_KEY_HANDLE, BCRYPT_PAD_PKCS1, BCRYPT_PKCS1_PADDING_INFO, BCRYPT_RSA_ALG_HANDLE,
    BCRYPT_RSAPUBLIC_BLOB, BCRYPT_SHA256_ALGORITHM, BCryptExportKey, BCryptFinalizeKeyPair,
    BCryptGenerateKeyPair, BCryptSignHash,
};
use windows::Win32::Storage::FileSystem::VS_FIXEDFILEINFO;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, REG_VALUE_TYPE, RegDeleteTreeW, RegSetKeyValueW,
};

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const RELEASE_URL: &str = "https://github.com/isolmaz/isolmaSS-app/releases/tag/v99.0.0";
const INSTALLER_URL: &str =
    "https://github.com/isolmaz/isolmaSS-app/releases/download/v99.0.0/isolmass-setup.exe";
const SIGNATURE_URL: &str =
    "https://github.com/isolmaz/isolmaSS-app/releases/download/v99.0.0/isolmass-setup.exe.sig";
const SIGNATURE_PATH: &str = "/dl/signature";
const INSTALLER_PATH: &str = "/dl/installer";

/// A message up to its first placeholder, for messages that end in an operating system error.
fn lead(template: &'static str) -> &'static str {
    t(template).split("{}").next().unwrap_or_default()
}

/// A fresh folder under the system temp folder, removed again on drop.
pub(super) struct Scratch(PathBuf);

impl Scratch {
    pub(super) fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("isolmass-updater-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch folder");
        Self(path)
    }

    pub(super) fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The file names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read folder")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

// ---- A loopback HTTP server --------------------------------------------------------------

struct Reply {
    status: u16,
    headers: Vec<String>,
    body: Vec<u8>,
}

impl Reply {
    fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn redirect(status: u16, location: &str) -> Self {
        Self {
            status,
            headers: vec![format!("Location: {location}")],
            body: Vec::new(),
        }
    }
}

/// Answers every request with whatever the handler makes of its path, one connection at a
/// time, and remembers the request heads it saw.
struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn serve(mut stream: TcpStream, handler: &dyn Fn(&str) -> Reply, requests: &Mutex<Vec<String>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => head.extend_from_slice(&chunk[..count]),
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    requests.lock().expect("request log").push(head);
    let reply = handler(&path);
    let mut response = format!(
        "HTTP/1.1 {} Reply\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for header in &reply.headers {
        response.push_str(header);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    let _ = stream
        .write_all(response.as_bytes())
        .and_then(|_| stream.write_all(&reply.body));
}

impl Server {
    fn start(handler: impl Fn(&str) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (requests, stop) = (requests.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    serve(stream, &handler, &requests);
                }
            })
        };
        Self {
            port,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    /// Plain HTTP to this server, whatever host the URL names.
    fn route(&self) -> Route<'static> {
        Route {
            host: Some("127.0.0.1"),
            port: self.port,
            flags: WINHTTP_OPEN_REQUEST_FLAGS(0),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("request log").clone()
    }

    fn paths(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|head| head.split_whitespace().nth(1).unwrap_or("").to_string())
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Serves each known path with its status and body; anything else is a 404.
fn serving(routes: Vec<(&'static str, u16, Vec<u8>)>) -> Server {
    Server::start(
        move |path| match routes.iter().find(|(known, _, _)| *known == path) {
            Some((_, status, body)) => Reply {
                status: *status,
                headers: Vec::new(),
                body: body.clone(),
            },
            None => Reply::status(404),
        },
    )
}

// ---- A signing key the tests hold ------------------------------------------------------------

struct TestKey {
    /// The CNG key handle, kept as an integer so the key can live in a static.
    handle: usize,
    /// The matching `BCRYPT_RSAPUBLIC_BLOB`.
    public: Vec<u8>,
}

fn generate_test_key() -> TestKey {
    let mut key = BCRYPT_KEY_HANDLE::default();
    unsafe { BCryptGenerateKeyPair(BCRYPT_RSA_ALG_HANDLE, &mut key, 3072, 0) }
        .ok()
        .expect("generate an RSA key");
    unsafe { BCryptFinalizeKeyPair(key, 0) }
        .ok()
        .expect("finalize the RSA key");
    let mut size = 0u32;
    let export = |buffer: Option<&mut [u8]>, size: &mut u32| {
        unsafe {
            BCryptExportKey(
                key,
                BCRYPT_KEY_HANDLE::default(),
                BCRYPT_RSAPUBLIC_BLOB,
                buffer,
                size,
                0,
            )
        }
        .ok()
        .expect("export the public key");
    };
    export(None, &mut size);
    let mut public = vec![0u8; size as usize];
    export(Some(&mut public), &mut size);
    TestKey {
        handle: key.0 as usize,
        public,
    }
}

/// One RSA-3072 key pair per test run, like the publisher's.
fn test_key() -> &'static TestKey {
    static KEY: LazyLock<TestKey> = LazyLock::new(generate_test_key);
    &KEY
}

/// A detached signature the way `scripts/sign-update.ps1` makes it: RSA PKCS#1 SHA-256 over
/// the version and the installer digest.
fn sign(version: &str, digest: &str) -> Vec<u8> {
    let message = format!("isolmaSS-update-v1\n{version}\n{digest}\n");
    let hash = sha256_digest(message.as_bytes()).expect("hash the message");
    let padding = BCRYPT_PKCS1_PADDING_INFO {
        pszAlgId: BCRYPT_SHA256_ALGORITHM,
    };
    let mut signature = vec![0u8; 384];
    let mut size = 0u32;
    unsafe {
        BCryptSignHash(
            BCRYPT_KEY_HANDLE(test_key().handle as *mut c_void),
            Some((&raw const padding).cast()),
            &hash,
            Some(&mut signature),
            &mut size,
            BCRYPT_PAD_PKCS1,
        )
    }
    .ok()
    .expect("sign the message");
    assert_eq!(size, 384);
    signature
}

// ---- Executables with a known version ----------------------------------------------------------

/// A small system executable with the offset of its fixed version block.
fn system_executable() -> (Vec<u8>, usize) {
    let system =
        PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot")).join("System32");
    let marker: Vec<u8> = "VS_VERSION_INFO\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    [
        "hostname.exe",
        "find.exe",
        "attrib.exe",
        "notepad.exe",
        "cmd.exe",
    ]
    .iter()
    .find_map(|name| {
        let bytes = std::fs::read(system.join(name)).ok()?;
        let key = bytes
            .windows(marker.len())
            .position(|window| window == marker)?;
        // The block starts 6 bytes before its key; the value follows the key, 4-aligned.
        let info = key - 6 + 40;
        (bytes.get(info..info + 4)? == 0xfeef04bdu32.to_le_bytes()).then_some((bytes, info))
    })
    .expect("a system executable with a version resource")
}

/// A copy of a system executable whose version resource says `version`.
fn executable_with_version(version: [u32; 3]) -> Vec<u8> {
    let (mut bytes, info) = system_executable();
    bytes[info + 8..info + 12].copy_from_slice(&((version[0] << 16) | version[1]).to_le_bytes());
    bytes[info + 12..info + 16].copy_from_slice(&(version[2] << 16).to_le_bytes());
    bytes
}

/// Writes `bytes` as the staged installer for `name_version` with a signature for
/// `signed_version`, and returns its path.
fn write_signed(dir: &Path, name_version: &str, bytes: &[u8], signed_version: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create folder");
    let path = dir.join(format!("isolmass-setup-{name_version}.exe"));
    std::fs::write(&path, bytes).expect("write installer");
    let digest = sha256_hex(bytes).expect("digest");
    std::fs::write(
        path.with_extension("exe.sig"),
        sign(signed_version, &digest),
    )
    .expect("write signature");
    path
}

// ---- Helpers that were already here ----------------------------------------------------------

#[test]
fn semantic_version_comparison_and_sha256_are_stable() {
    assert_eq!(parse_version("v1.2.3"), Some([1, 2, 3]));
    assert!(parse_version("1.2").is_none());
    for value in [
        "1.2.3/evil",
        "1.2.3-beta",
        "1.2.3.4",
        "01.2.3",
        "1..3",
        "../../x",
    ] {
        assert!(parse_version(value).is_none());
    }
    for url in [
        "http://github.com/x",
        "https://github.com.evil.test/x",
        "https://github.com@evil.test/x",
        "https://github.com/x#fragment",
        "https://evil.test/x",
    ] {
        assert!(parse_https_url(url).is_err(), "{url}");
    }
    assert!(parse_https_url("https://github.com/owner/release").is_ok());
    for key in [0x56, 0x4d, 0x52, 0x54, 0x42] {
        assert!(crate::overlay::is_editor_shortcut(key, 0, false));
    }
    for key in [0x43, 0x53, 0x5a, 0x59] {
        assert!(crate::overlay::is_editor_shortcut(
            key,
            windows::Win32::UI::Input::KeyboardAndMouse::MOD_CONTROL.0,
            true
        ));
    }
    assert!(!crate::overlay::is_editor_shortcut(0x41, 0, true));

    assert_eq!(
        sha256_hex(b"abc").expect("SHA-256"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn only_installers_for_the_running_or_older_versions_and_interrupted_downloads_are_stale() {
    let current = [0, 6, 3];
    for name in [
        "isolmass-setup-0.6.3.exe",
        "isolmass-setup-0.6.3.exe.sig",
        "isolmass-setup-0.5.12.exe",
        "isolmass-setup-0.6.2.exe.sig",
        ".download-4120-0.6.4-1760000000.tmp",
        ".download-4120-0.6.4-1760000000.sig.tmp",
    ] {
        assert!(is_stale_update_file(name, current), "{name}");
    }
    for name in [
        "isolmass-setup-0.6.4.exe",
        "isolmass-setup-0.6.4.exe.sig",
        "isolmass-setup-1.0.0.exe",
        "isolmass-setup-latest.exe",
        "isolmass-setup-0.6.3.exe.bak",
        "isolmass-setup-.exe",
        "notes.txt",
        "download-1.tmp",
        ".download-4120-0.6.4-1760000000.part",
    ] {
        assert!(!is_stale_update_file(name, current), "{name}");
    }
}

#[test]
fn only_the_registered_folder_with_an_uninstaller_counts_as_installed() {
    let installed = Path::new(r"C:\Users\me\AppData\Local\isolmaSS");
    assert!(is_installed_copy(
        installed,
        Some(Path::new(r"c:\users\ME\appdata\local\ISOLMASS")),
        true
    ));
    assert!(!is_installed_copy(installed, Some(installed), false));
    assert!(!is_installed_copy(
        Path::new(r"D:\Tools\isolmaSS"),
        Some(installed),
        true
    ));
    assert!(!is_installed_copy(installed, None, true));
}

#[test]
fn sha256_and_hex_match_known_vectors() {
    assert_eq!(
        sha256_hex(b"").expect("SHA-256"),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
}

#[test]
fn the_app_checks_github_over_https_and_trusts_the_published_key() {
    assert_eq!(
        parse_https_url(&format!("https://{RELEASE_API_HOST}{RELEASE_API_PATH}")),
        Ok((RELEASE_API_HOST, RELEASE_API_PATH.to_string()))
    );
    assert_eq!(LIVE.route.host, None);
    assert_eq!(LIVE.route.port, 443);
    assert_eq!(LIVE.route.flags.0, WINHTTP_FLAG_SECURE.0);
    assert_eq!(LIVE.public_key, PINNED_PUBLIC_KEY);
    // The pinned blob is the RSA-3072 public key `sign-update.ps1` insists on: the "RSA1"
    // magic, a 3-byte exponent and a 384-byte modulus.
    assert_eq!(&PINNED_PUBLIC_KEY[..4], b"RSA1");
    assert_eq!(PINNED_PUBLIC_KEY.len(), 24 + 3 + 384);
}

#[test]
fn every_github_download_host_is_accepted_and_paths_default_to_the_root() {
    for host in [
        "api.github.com",
        "github.com",
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
    ] {
        assert_eq!(
            parse_https_url(&format!("https://{host}/a/b?c=d")),
            Ok((host, "/a/b?c=d".to_string()))
        );
    }
    assert_eq!(
        parse_https_url("https://github.com"),
        Ok(("github.com", "/".to_string()))
    );
    for url in [
        "https://github.com/a\\b",
        "https://github.com/a\nb",
        "ftp://github.com/a",
    ] {
        assert!(parse_https_url(url).is_err(), "{url}");
    }
}

// ---- HTTP -------------------------------------------------------------------------------------

fn far() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn get(server: &Server, host: &str, path: &str, maximum: usize) -> Result<Vec<u8>, String> {
    http_get_inner(server.route(), host, path, maximum, far(), 0)
}

#[test]
fn a_get_returns_the_body_and_announces_a_github_api_client() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::ok("hello"));
    assert_eq!(
        get(&server, "api.github.com", "/repos/a/b", 1024),
        Ok(b"hello".to_vec())
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let head = requests[0].to_ascii_lowercase();
    assert!(head.starts_with("get /repos/a/b http/1.1\r\n"), "{head}");
    assert!(head.contains("accept: application/vnd.github+json\r\n"));
    assert!(head.contains("x-github-api-version: 2022-11-28\r\n"));
    assert!(head.contains("user-agent: isolmass"));
    assert!(head.contains(&format!("host: 127.0.0.1:{}", server.port)));
}

#[test]
fn http_get_applies_its_own_deadline_and_the_routes_target() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::ok("ok"));
    assert_eq!(
        http_get(
            server.route(),
            "github.com",
            "/x",
            16,
            Duration::from_secs(60)
        ),
        Ok(b"ok".to_vec())
    );
    assert_eq!(
        http_get(server.route(), "github.com", "/x", 16, Duration::ZERO),
        Err(t("The update request exceeded its time limit.").to_string())
    );
    assert_eq!(server.paths(), ["/x"]);
}

#[test]
fn every_redirect_status_is_followed_to_an_allowed_host() {
    let _state = job::exclusive();
    let server = Server::start(|path| match path.trim_start_matches('/').parse::<u16>() {
        Ok(status) => Reply::redirect(status, "https://github.com/done/here"),
        Err(_) => Reply::ok("arrived"),
    });
    for status in [301, 302, 303, 307, 308] {
        assert_eq!(
            get(&server, "api.github.com", &format!("/{status}"), 64),
            Ok(b"arrived".to_vec()),
            "{status}"
        );
    }
    assert_eq!(server.paths().len(), 10);
    assert_eq!(server.paths()[..2], ["/301", "/done/here"]);
}

#[test]
fn redirects_to_other_hosts_or_plain_http_are_refused_without_following() {
    let _state = job::exclusive();
    for (location, message) in [
        (
            "https://evil.test/x",
            t("The update URL contains an unsupported host."),
        ),
        (
            "https://github.com.evil.test/x",
            t("The update URL contains an unsupported host."),
        ),
        ("http://github.com/x", t("Update downloads must use HTTPS.")),
    ] {
        let server = Server::start(move |_| Reply::redirect(302, location));
        assert_eq!(
            get(&server, "api.github.com", "/start", 64),
            Err(message.to_string()),
            "{location}"
        );
        assert_eq!(server.paths(), ["/start"], "{location}");
    }
}

#[test]
fn redirect_loops_stop_after_five_hops() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::redirect(302, "https://github.com/again"));
    assert_eq!(
        get(&server, "api.github.com", "/start", 64),
        Err(t("Too many update redirects.").to_string())
    );
    // The first request plus five followed redirects; the sixth redirect is not followed.
    assert_eq!(server.paths().len(), 6);
}

#[test]
fn a_redirect_without_a_location_is_an_error() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::status(302));
    let error = get(&server, "api.github.com", "/start", 64).expect_err("no Location header");
    assert!(
        error.starts_with(lead("The update redirect is invalid: {}")),
        "{error}"
    );
}

#[test]
fn only_success_statuses_deliver_a_body() {
    let _state = job::exclusive();
    for status in [404u32, 500] {
        let server = Server::start(move |_| Reply::status(status as u16));
        assert_eq!(
            get(&server, "api.github.com", "/x", 64),
            Err(tf("The update server returned HTTP {}.", &[&status])),
            "{status}"
        );
    }
    let server = Server::start(|_| Reply::status(204));
    assert_eq!(get(&server, "api.github.com", "/x", 64), Ok(Vec::new()));
}

#[test]
fn responses_over_the_size_limit_are_rejected_at_the_boundary() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::ok(vec![b'x'; 100]));
    assert_eq!(
        get(&server, "api.github.com", "/x", 100),
        Ok(vec![b'x'; 100])
    );
    assert_eq!(
        get(&server, "api.github.com", "/x", 99),
        Err(tf("The update response exceeded {} bytes.", &[&99usize]))
    );
}

#[test]
fn a_large_body_arrives_complete_over_several_reads() {
    let _state = job::exclusive();
    let body: Vec<u8> = (0..400_000u32).map(|index| (index % 251) as u8).collect();
    let served = body.clone();
    let server = Server::start(move |_| Reply::ok(served.clone()));
    assert_eq!(get(&server, "api.github.com", "/big", 1_000_000), Ok(body));
}

#[test]
fn an_unreachable_server_is_reported_as_a_failed_request() {
    let _state = job::exclusive();
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("address")
        .port();
    let route = Route {
        host: Some("127.0.0.1"),
        port,
        flags: WINHTTP_OPEN_REQUEST_FLAGS(0),
    };
    let error =
        http_get_inner(route, "api.github.com", "/x", 10, far(), 0).expect_err("nothing listens");
    assert!(
        error.starts_with(lead("The update request failed: {}")),
        "{error}"
    );
}

#[test]
fn urls_are_validated_before_any_connection_is_made() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::ok("never"));
    assert_eq!(
        get(&server, "evil.test", "/x", 10),
        Err(t("The update URL contains an unsupported host.").to_string())
    );
    assert_eq!(
        get(&server, "api.github.com", "/a\\b", 10),
        Err(t("The update URL contains an unsupported host.").to_string())
    );
    assert!(server.requests().is_empty());
}

#[test]
fn cancellation_and_the_deadline_stop_a_request_before_it_is_sent() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::ok("never"));
    let future = far();
    assert_eq!(check_cancelled(future), Ok(()));
    assert_eq!(
        check_cancelled(Instant::now()),
        Err(t("The update request exceeded its time limit.").to_string())
    );
    assert_eq!(
        http_get_inner(server.route(), "github.com", "/x", 10, Instant::now(), 0),
        Err(t("The update request exceeded its time limit.").to_string())
    );
    CANCELLED.store(true, Ordering::Release);
    assert_eq!(
        check_cancelled(future),
        Err(t("Update operation cancelled.").to_string())
    );
    assert_eq!(
        http_get_inner(server.route(), "github.com", "/x", 10, future, 0),
        Err(t("Update operation cancelled.").to_string())
    );
    CANCELLED.store(false, Ordering::Release);
    assert!(server.requests().is_empty());
}

#[test]
fn a_failed_winhttp_call_reports_the_threads_last_error_and_a_good_handle_is_kept() {
    unsafe { SetLastError(ERROR_ACCESS_DENIED) };
    let failure = InternetHandle::adopt(std::ptr::null_mut(), |error| {
        format!("{:#x}", error.code().0 as u32)
    });
    assert_eq!(failure.err(), Some("0x80070005".to_string()));

    let mut value = 0u8;
    let raw = (&raw mut value).cast::<c_void>();
    let adopted = InternetHandle::adopt(raw, |_| String::new()).expect("a non-null handle");
    assert_eq!(adopted.0, raw);
    // The test handle is not a real WinHTTP handle: do not let drop close it.
    std::mem::forget(adopted);
}

// ---- Release metadata --------------------------------------------------------------------------

fn asset(name: &str, url: &str, digest: Option<&str>) -> Value {
    json!({ "name": name, "browser_download_url": url, "digest": digest })
}

fn installer_asset() -> Value {
    asset(
        "isolmass-setup.exe",
        INSTALLER_URL,
        Some(&format!("sha256:{}", DIGEST.to_uppercase())),
    )
}

fn signature_asset() -> Value {
    asset("isolmass-setup.exe.sig", SIGNATURE_URL, None)
}

fn release(tag: &str, assets: Vec<Value>) -> Value {
    json!({ "tag_name": tag, "html_url": RELEASE_URL, "assets": assets })
}

fn complete_release(tag: &str) -> Value {
    release(tag, vec![installer_asset(), signature_asset()])
}

fn fetch(body: impl Into<Vec<u8>>, current: &str) -> Result<Option<UpdateInfo>, String> {
    let body = body.into();
    let server = Server::start(move |_| Reply::ok(body.clone()));
    fetch_release(server.route(), current)
}

fn fetch_json(value: Value, current: &str) -> Result<Option<UpdateInfo>, String> {
    fetch(value.to_string(), current)
}

#[test]
fn a_newer_complete_release_becomes_an_update_offer() {
    let _state = job::exclusive();
    let body = release(
        "v99.0.0",
        vec![
            asset("notes.txt", "https://evil.test/notes", None),
            installer_asset(),
            asset("isolmass-portable.zip", "https://evil.test/zip", None),
            signature_asset(),
        ],
    )
    .to_string();
    let server = Server::start(move |_| Reply::ok(body.clone()));
    assert_eq!(
        fetch_release(server.route(), "0.6.3"),
        Ok(Some(UpdateInfo {
            version: "99.0.0".to_string(),
            download_url: INSTALLER_URL.to_string(),
            sha256: DIGEST.to_string(),
            signature_url: SIGNATURE_URL.to_string(),
            release_url: RELEASE_URL.to_string(),
        }))
    );
    assert_eq!(
        server.paths(),
        ["/repos/isolmaz/isolmaSS-app/releases/latest"]
    );
}

#[test]
fn only_releases_newer_than_the_running_version_are_offered() {
    let _state = job::exclusive();
    for (tag, offered) in [
        ("v0.6.4", true),
        ("v0.10.0", true),
        ("1.0.0", true),
        ("v0.6.3", false),
        ("v0.6.2", false),
        ("v0.5.99", false),
    ] {
        let update = fetch_json(complete_release(tag), "0.6.3").expect("a valid release");
        assert_eq!(update.is_some(), offered, "{tag}");
    }
}

#[test]
fn drafts_and_prereleases_are_never_offered() {
    let _state = job::exclusive();
    for flag in ["draft", "prerelease"] {
        let mut value = complete_release("v99.0.0");
        value[flag] = json!(true);
        assert_eq!(fetch_json(value, "0.6.3"), Ok(None), "{flag}");
    }
}

#[test]
fn incomplete_or_suspicious_releases_are_rejected_with_their_reason() {
    let _state = job::exclusive();
    let digest_of = |digest: Option<&str>| {
        release(
            "v99.0.0",
            vec![
                asset("isolmass-setup.exe", INSTALLER_URL, digest),
                signature_asset(),
            ],
        )
    };
    let mut other_repository = complete_release("v99.0.0");
    other_repository["html_url"] =
        json!("https://github.com/other/repository/releases/tag/v99.0.0");
    let cases = [
        (
            "tag is not a version",
            complete_release("latest"),
            t("The release tag is not a semantic version.").to_string(),
        ),
        (
            "tag has a suffix",
            complete_release("v1.2.3-beta"),
            t("The release tag is not a semantic version.").to_string(),
        ),
        (
            "two installers",
            release(
                "v99.0.0",
                vec![installer_asset(), installer_asset(), signature_asset()],
            ),
            t("The release contains duplicate installers.").to_string(),
        ),
        (
            "two signatures",
            release(
                "v99.0.0",
                vec![installer_asset(), signature_asset(), signature_asset()],
            ),
            t("The release contains duplicate signatures.").to_string(),
        ),
        (
            "no installer",
            release("v99.0.0", vec![signature_asset()]),
            tf(
                "Release {} has no {} asset.",
                &[&"v99.0.0", &"isolmass-setup.exe"],
            ),
        ),
        (
            "no signature",
            release("v99.0.0", vec![installer_asset()]),
            tf(
                "Release {} has no {} asset.",
                &[&"v99.0.0", &"isolmass-setup.exe.sig"],
            ),
        ),
        (
            "no digest",
            digest_of(None),
            t("The release installer has no valid SHA-256 digest.").to_string(),
        ),
        (
            "another digest algorithm",
            digest_of(Some(&format!("md5:{DIGEST}"))),
            t("The release installer has no valid SHA-256 digest.").to_string(),
        ),
        (
            "a short digest",
            digest_of(Some("sha256:abcd")),
            t("The release installer has no valid SHA-256 digest.").to_string(),
        ),
        (
            "a digest that is not hex",
            digest_of(Some(&format!("sha256:{}", "z".repeat(64)))),
            t("The release installer has no valid SHA-256 digest.").to_string(),
        ),
        (
            "installer on another host",
            release(
                "v99.0.0",
                vec![
                    asset(
                        "isolmass-setup.exe",
                        "https://evil.test/isolmass-setup.exe",
                        Some(&format!("sha256:{DIGEST}")),
                    ),
                    signature_asset(),
                ],
            ),
            t("The update URL contains an unsupported host.").to_string(),
        ),
        (
            "signature over plain http",
            release(
                "v99.0.0",
                vec![
                    installer_asset(),
                    asset(
                        "isolmass-setup.exe.sig",
                        "http://github.com/isolmass-setup.exe.sig",
                        None,
                    ),
                ],
            ),
            t("Update downloads must use HTTPS.").to_string(),
        ),
        (
            "release page of another repository",
            other_repository,
            t("Unexpected release information URL.").to_string(),
        ),
    ];
    for (case, value, message) in cases {
        assert_eq!(fetch_json(value, "0.6.3"), Err(message), "{case}");
    }
}

#[test]
fn a_running_version_that_is_not_semantic_is_an_error() {
    let _state = job::exclusive();
    assert_eq!(
        fetch_json(complete_release("v99.0.0"), "dev"),
        Err(t("The installed application version is invalid.").to_string())
    );
}

#[test]
fn unreadable_release_metadata_and_server_errors_are_reported() {
    let _state = job::exclusive();
    for body in ["{", "[]", r#"{"tag_name":"v1.0.0"}"#] {
        let error = fetch(body, "0.6.3").expect_err("invalid metadata");
        assert!(
            error.starts_with(lead("The update metadata is invalid: {}")),
            "{body}: {error}"
        );
    }
    let server = Server::start(|_| Reply::status(500));
    assert_eq!(
        fetch_release(server.route(), "0.6.3"),
        Err(tf("The update server returned HTTP {}.", &[&500]))
    );
}

// ---- Downloading and staging --------------------------------------------------------------------

/// Release metadata for `installer` signed as `signed_version`, and the detached signature.
fn publication(installer: &[u8], signed_version: &str) -> (UpdateInfo, Vec<u8>) {
    let digest = sha256_hex(installer).expect("digest");
    let update = UpdateInfo {
        version: signed_version.to_string(),
        download_url: format!("https://github.com{INSTALLER_PATH}"),
        signature_url: format!("https://github.com{SIGNATURE_PATH}"),
        sha256: digest.clone(),
        release_url: RELEASE_URL.to_string(),
    };
    (update, sign(signed_version, &digest))
}

fn download(
    server: &Server,
    update: &UpdateInfo,
    staging: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<PathBuf, DownloadError> {
    let channel = Channel {
        route: server.route(),
        public_key: &test_key().public,
    };
    download_from(&channel, update, staging)
}

fn failure(result: Result<PathBuf, DownloadError>) -> (DownloadFailure, String) {
    let error = result.expect_err("the download must fail");
    (error.kind, error.message)
}

#[test]
fn a_verified_download_is_staged_beside_its_signature() {
    let _state = job::exclusive();
    let scratch = Scratch::new("download-ok");
    let root = scratch.join("updates");
    let installer = executable_with_version([7, 8, 9]);
    let (update, signature) = publication(&installer, "7.8.9");
    let server = serving(vec![
        (SIGNATURE_PATH, 200, signature.clone()),
        (INSTALLER_PATH, 200, installer.clone()),
    ]);
    let staged = download(&server, &update, &|| Ok(root.clone())).expect("download");
    assert_eq!(staged, root.join("isolmass-setup-7.8.9.exe"));
    assert_eq!(std::fs::read(&staged).expect("staged installer"), installer);
    assert_eq!(
        std::fs::read(root.join("isolmass-setup-7.8.9.exe.sig")).expect("staged signature"),
        signature
    );
    assert_eq!(
        names(&root),
        ["isolmass-setup-7.8.9.exe", "isolmass-setup-7.8.9.exe.sig"]
    );
    assert_eq!(server.paths(), [SIGNATURE_PATH, INSTALLER_PATH]);
    // What was staged passes the checks the installer launch applies.
    assert_eq!(
        verify_artifact(&test_key().public, &staged),
        Ok("7.8.9".to_string())
    );
    assert_eq!(
        verify_staged_installer(&test_key().public, &root, &staged),
        Ok(())
    );
    // A second run replaces the staged files in place.
    assert_eq!(
        download(&server, &update, &|| Ok(root.clone())).expect("repeat"),
        staged
    );
    assert_eq!(names(&root).len(), 2);
}

#[test]
fn malformed_update_metadata_is_rejected_before_any_request() {
    let _state = job::exclusive();
    let server = Server::start(|_| Reply::ok("never"));
    let (good, _) = publication(b"installer", "1.2.3");
    for (case, update) in [
        (
            "version",
            UpdateInfo {
                version: "1.2".to_string(),
                ..good.clone()
            },
        ),
        (
            "short digest",
            UpdateInfo {
                sha256: DIGEST[..63].to_string(),
                ..good.clone()
            },
        ),
        (
            "digest that is not hex",
            UpdateInfo {
                sha256: "z".repeat(64),
                ..good.clone()
            },
        ),
    ] {
        assert_eq!(
            failure(download(&server, &update, &|| Ok(PathBuf::new()))),
            (
                DownloadFailure::Integrity,
                t("Invalid signed update metadata.").to_string()
            ),
            "{case}"
        );
    }
    assert!(server.requests().is_empty());
}

#[test]
fn urls_outside_the_allowed_hosts_are_integrity_failures() {
    let _state = job::exclusive();
    let installer = executable_with_version([7, 8, 9]);
    let (good, signature) = publication(&installer, "7.8.9");
    let server = serving(vec![(SIGNATURE_PATH, 200, signature)]);
    let hostile = t("The update URL contains an unsupported host.").to_string();
    let bad_signature_url = UpdateInfo {
        signature_url: "https://evil.test/sig".to_string(),
        ..good.clone()
    };
    assert_eq!(
        failure(download(
            &server,
            &bad_signature_url,
            &|| Ok(PathBuf::new())
        )),
        (DownloadFailure::Integrity, hostile.clone())
    );
    assert!(server.requests().is_empty());
    // A validly signed release still cannot point the installer at another host.
    let bad_download_url = UpdateInfo {
        download_url: "https://evil.test/installer".to_string(),
        ..good
    };
    assert_eq!(
        failure(download(&server, &bad_download_url, &|| Ok(PathBuf::new()))),
        (DownloadFailure::Integrity, hostile)
    );
    assert_eq!(server.paths(), [SIGNATURE_PATH]);
}

#[test]
fn missing_assets_are_network_failures() {
    let _state = job::exclusive();
    let installer = executable_with_version([7, 8, 9]);
    let (update, signature) = publication(&installer, "7.8.9");
    let not_found = tf("The update server returned HTTP {}.", &[&404]);
    let no_signature = serving(vec![(INSTALLER_PATH, 200, installer)]);
    assert_eq!(
        failure(download(&no_signature, &update, &|| Ok(PathBuf::new()))),
        (DownloadFailure::Network, not_found.clone())
    );
    let no_installer = serving(vec![(SIGNATURE_PATH, 200, signature)]);
    assert_eq!(
        failure(download(&no_installer, &update, &|| Ok(PathBuf::new()))),
        (DownloadFailure::Network, not_found)
    );
}

#[test]
fn a_signature_for_another_version_stops_the_download_before_the_installer() {
    let _state = job::exclusive();
    let installer = executable_with_version([7, 8, 9]);
    let (update, _) = publication(&installer, "7.8.9");
    let wrong = sign("7.8.8", &update.sha256);
    let server = serving(vec![
        (SIGNATURE_PATH, 200, wrong),
        (INSTALLER_PATH, 200, installer),
    ]);
    assert_eq!(
        failure(download(&server, &update, &|| Ok(PathBuf::new()))),
        (
            DownloadFailure::Integrity,
            t("The update signature does not match the pinned publisher key.").to_string()
        )
    );
    assert_eq!(server.paths(), [SIGNATURE_PATH]);
}

#[test]
fn an_installer_that_differs_from_its_signed_digest_is_never_staged() {
    let _state = job::exclusive();
    let scratch = Scratch::new("download-tampered");
    let root = scratch.join("updates");
    let installer = executable_with_version([7, 8, 9]);
    let (update, signature) = publication(&installer, "7.8.9");
    let mut tampered = installer;
    tampered[0] ^= 1;
    let server = serving(vec![
        (SIGNATURE_PATH, 200, signature),
        (INSTALLER_PATH, 200, tampered),
    ]);
    assert_eq!(
        failure(download(&server, &update, &|| Ok(root.clone()))),
        (
            DownloadFailure::Integrity,
            t("The downloaded installer does not match its signed SHA-256 digest.").to_string()
        )
    );
    assert!(!root.exists(), "nothing is created for a rejected download");
}

#[test]
fn an_unusable_staging_folder_is_a_storage_failure() {
    let _state = job::exclusive();
    let scratch = Scratch::new("download-storage");
    let installer = executable_with_version([7, 8, 9]);
    let (update, signature) = publication(&installer, "7.8.9");
    let server = serving(vec![
        (SIGNATURE_PATH, 200, signature),
        (INSTALLER_PATH, 200, installer),
    ]);
    assert_eq!(
        failure(download(&server, &update, &|| Err("no folder".to_string()))),
        (DownloadFailure::Storage, "no folder".to_string())
    );
    let file = scratch.join("file");
    std::fs::write(&file, b"x").expect("write file");
    let below_a_file = file.join("updates");
    let (kind, message) = failure(download(&server, &update, &|| Ok(below_a_file.clone())));
    assert_eq!(kind, DownloadFailure::Storage);
    assert!(
        message.starts_with(lead("Could not create the update folder: {}")),
        "{message}"
    );
}

#[test]
fn an_installer_whose_embedded_version_differs_from_the_signed_one_is_discarded() {
    let _state = job::exclusive();
    let scratch = Scratch::new("download-version");
    let root = scratch.join("updates");
    let installer = executable_with_version([7, 8, 9]);
    // Signed as 9.9.9 but carrying 7.8.9: a downgrade smuggled in under a newer signature.
    let (update, signature) = publication(&installer, "9.9.9");
    let server = serving(vec![
        (SIGNATURE_PATH, 200, signature),
        (INSTALLER_PATH, 200, installer),
    ]);
    assert_eq!(
        failure(download(&server, &update, &|| Ok(root.clone()))),
        (
            DownloadFailure::Integrity,
            t("The installer file version does not match the signed release.").to_string()
        )
    );
    assert!(
        names(&root).is_empty(),
        "no temporary files are left behind"
    );
}

#[test]
fn a_signed_file_without_a_version_resource_is_an_integrity_failure() {
    let _state = job::exclusive();
    let scratch = Scratch::new("download-no-version");
    let root = scratch.join("updates");
    let installer = b"not an executable".to_vec();
    let (update, signature) = publication(&installer, "1.2.3");
    let server = serving(vec![
        (SIGNATURE_PATH, 200, signature),
        (INSTALLER_PATH, 200, installer),
    ]);
    assert_eq!(
        failure(download(&server, &update, &|| Ok(root.clone()))),
        (
            DownloadFailure::Integrity,
            t("The executable has no valid version resource.").to_string()
        )
    );
    assert!(names(&root).is_empty());
}

// ---- Staging files ------------------------------------------------------------------------------

#[test]
fn a_staged_file_is_written_once_and_never_overwritten() {
    let scratch = Scratch::new("write-staged");
    let path = scratch.join("staged.bin");
    assert_eq!(write_staged_file(&path, b"first"), Ok(()));
    assert_eq!(std::fs::read(&path).expect("read"), b"first");
    let error = write_staged_file(&path, b"second").expect_err("the file exists");
    assert!(
        error.starts_with(lead("Could not stage the update: {}")),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).expect("read"), b"first");
    let missing_parent = scratch.join("missing").join("staged.bin");
    assert!(write_staged_file(&missing_parent, b"x").is_err());
}

#[test]
fn replacing_a_staged_file_overwrites_the_target_and_consumes_the_source() {
    let scratch = Scratch::new("replace-staged");
    let (source, target) = (scratch.join("source"), scratch.join("target"));
    std::fs::write(&source, b"new").expect("write source");
    std::fs::write(&target, b"old").expect("write target");
    assert_eq!(replace_staged_file(&source, &target), Ok(()));
    assert_eq!(std::fs::read(&target).expect("read"), b"new");
    assert!(!source.exists());
    let error = replace_staged_file(&source, &target).expect_err("the source is gone");
    assert!(
        error.starts_with(lead("Could not finalize the staged update: {}")),
        "{error}"
    );
    assert_eq!(std::fs::read(&target).expect("read"), b"new");
}

#[test]
fn the_update_folder_lives_under_local_app_data() {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA"));
    assert_eq!(
        update_directory(),
        Ok(local.join("isolmaSS").join("updates"))
    );
}

#[test]
fn the_sweep_removes_only_stale_files_and_survives_what_it_cannot_delete() {
    let scratch = Scratch::new("sweep");
    let root = scratch.join("updates");
    std::fs::create_dir_all(&root).expect("create folder");
    let stale = [
        "isolmass-setup-0.6.3.exe",
        "isolmass-setup-0.6.3.exe.sig",
        "isolmass-setup-0.5.0.exe",
        ".download-1-0.6.4-5.tmp",
    ];
    let fresh = [
        "isolmass-setup-0.6.4.exe",
        "isolmass-setup-0.6.4.exe.sig",
        "notes.txt",
    ];
    for name in stale.iter().chain(&fresh) {
        std::fs::write(root.join(name), b"x").expect("write file");
    }
    // A stale name that cannot be deleted as a file: the sweep logs it and carries on.
    std::fs::create_dir(root.join("isolmass-setup-0.1.0.exe")).expect("create folder");
    remove_stale_files_in(&root, [0, 6, 3]);
    let mut expected: Vec<&str> = fresh.to_vec();
    expected.push("isolmass-setup-0.1.0.exe");
    expected.sort_unstable();
    assert_eq!(names(&root), expected);
}

#[test]
fn the_sweep_tolerates_a_missing_folder_and_a_path_that_is_not_a_folder() {
    let scratch = Scratch::new("sweep-missing");
    let missing = scratch.join("never-created");
    remove_stale_files_in(&missing, [0, 6, 3]);
    assert!(!missing.exists());
    let file = scratch.join("file");
    std::fs::write(&file, b"keep").expect("write file");
    remove_stale_files_in(&file, [0, 6, 3]);
    assert_eq!(std::fs::read(&file).expect("read"), b"keep");
}

// ---- The installation check -----------------------------------------------------------------

/// A throwaway key under HKEY_CURRENT_USER, deleted again on drop.
struct RegistryScratch(Vec<u16>);

impl RegistryScratch {
    fn new(name: &str) -> Self {
        Self(wide(&format!(
            "Software\\isolmaSS-updater-test-{}-{name}",
            std::process::id()
        )))
    }

    fn key(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }

    fn set(&self, kind: REG_VALUE_TYPE, data: &[u8]) {
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                self.key(),
                PCWSTR::null(),
                kind.0,
                Some(data.as_ptr().cast()),
                data.len() as u32,
            )
        }
        .ok()
        .expect("write the test registry value");
    }

    fn set_string(&self, value: &str) {
        let data: Vec<u8> = value
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        self.set(REG_SZ, &data);
    }
}

impl Drop for RegistryScratch {
    fn drop(&mut self) {
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, self.key());
        }
    }
}

#[test]
fn the_registered_executable_is_the_default_string_value_when_it_is_sane() {
    let registry = RegistryScratch::new("registered");
    assert_eq!(registered_executable_at(registry.key()), None);
    registry.set_string(r"C:\Apps\isolmaSS\isolmass.exe");
    assert_eq!(
        registered_executable_at(registry.key()),
        Some(PathBuf::from(r"C:\Apps\isolmaSS\isolmass.exe"))
    );
    // A value of 80 000 bytes is no path.
    registry.set_string(&"a".repeat(40_000));
    assert_eq!(registered_executable_at(registry.key()), None);
    // Only strings count.
    registry.set(REG_DWORD, &1u32.to_le_bytes());
    assert_eq!(registered_executable_at(registry.key()), None);
}

#[test]
fn a_copy_is_an_installation_only_beside_its_uninstaller_in_the_registered_folder() {
    let scratch = Scratch::new("installation");
    let (app, other) = (scratch.join("app"), scratch.join("other"));
    for dir in [&app, &other] {
        std::fs::create_dir_all(dir).expect("create folder");
        std::fs::write(dir.join("isolmass.exe"), b"x").expect("write executable");
    }
    let exe = app.join("isolmass.exe");
    assert!(!is_installation(Some(exe.clone()), Some(exe.clone())));
    std::fs::write(app.join("uninstall.exe"), b"x").expect("write uninstaller");
    assert!(is_installation(Some(exe.clone()), Some(exe.clone())));
    // The registration may spell the same folder differently.
    let respelled = app.join(".").join("isolmass.exe");
    assert!(is_installation(Some(exe.clone()), Some(respelled)));
    assert!(!is_installation(
        Some(exe.clone()),
        Some(other.join("isolmass.exe"))
    ));
    assert!(!is_installation(Some(exe.clone()), None));
    assert!(!is_installation(None, Some(exe.clone())));
    assert!(!is_installation(Some(app.join("gone.exe")), Some(exe)));
}

#[test]
fn a_build_running_from_the_target_folder_is_not_an_installation() {
    assert!(!runs_from_installation());
}

// ---- Signatures and versions ------------------------------------------------------------------

#[test]
fn signatures_verify_only_for_the_exact_version_digest_and_bytes() {
    let key = &test_key().public;
    let signature = sign("1.2.3", DIGEST);
    assert_eq!(
        verify_release_signature(key, "1.2.3", DIGEST, &signature),
        Ok(())
    );
    // The digest is signed in lower case, however it is spelled.
    assert_eq!(
        verify_release_signature(key, "1.2.3", &DIGEST.to_uppercase(), &signature),
        Ok(())
    );
    let mismatch =
        Err(t("The update signature does not match the pinned publisher key.").to_string());
    assert_eq!(
        verify_release_signature(key, "1.2.4", DIGEST, &signature),
        mismatch
    );
    let other_digest = format!("f{}", &DIGEST[1..]);
    assert_eq!(
        verify_release_signature(key, "1.2.3", &other_digest, &signature),
        mismatch
    );
    let mut flipped = signature.clone();
    flipped[100] ^= 1;
    assert_eq!(
        verify_release_signature(key, "1.2.3", DIGEST, &flipped),
        mismatch
    );
    // A signature from any other key is rejected by the pinned one.
    assert_eq!(
        verify_release_signature(PINNED_PUBLIC_KEY, "1.2.3", DIGEST, &signature),
        mismatch
    );
}

#[test]
fn malformed_signature_inputs_and_keys_are_rejected() {
    let key = &test_key().public;
    let signature = sign("1.2.3", DIGEST);
    let invalid = Err(t("Invalid update signature metadata.").to_string());
    let non_hex = "z".repeat(64);
    for (version, digest, bytes) in [
        ("1.2", DIGEST, &signature[..]),
        ("1.2.3", &DIGEST[..63], &signature[..]),
        ("1.2.3", non_hex.as_str(), &signature[..]),
        ("1.2.3", DIGEST, &signature[..383]),
    ] {
        assert_eq!(
            verify_release_signature(key, version, digest, bytes),
            invalid,
            "{version} {digest} {}",
            bytes.len()
        );
    }
    let error = verify_release_signature(&[1, 2, 3], "1.2.3", DIGEST, &signature)
        .expect_err("not a key blob");
    assert!(
        error.starts_with(lead("Could not load the pinned update key: {}")),
        "{error}"
    );
}

#[test]
fn the_fixed_version_block_formats_as_major_minor_patch() {
    let info = VS_FIXEDFILEINFO {
        dwSignature: 0xfeef04bd,
        dwFileVersionMS: 10 << 16,
        dwFileVersionLS: (26100 << 16) | 1,
        ..VS_FIXEDFILEINFO::default()
    };
    assert_eq!(fixed_version(&info), Ok("10.0.26100".to_string()));
    let unsigned = VS_FIXEDFILEINFO {
        dwSignature: 0x1234,
        ..info
    };
    assert_eq!(
        fixed_version(&unsigned),
        Err(t("The executable version signature is invalid.").to_string())
    );
}

#[test]
fn an_executables_version_comes_from_its_version_resource() {
    let scratch = Scratch::new("file-version");
    for (index, version) in [[7, 8, 9], [65_535, 1, 4_000], [0, 0, 1]]
        .into_iter()
        .enumerate()
    {
        let path = scratch.join(&format!("tool-{index}.exe"));
        std::fs::write(&path, executable_with_version(version)).expect("write executable");
        assert_eq!(
            file_version(&path),
            Ok(format!("{}.{}.{}", version[0], version[1], version[2]))
        );
    }
    let no_resource = Err(t("The executable has no valid version resource.").to_string());
    assert_eq!(file_version(&scratch.join("missing.exe")), no_resource);
    let text = scratch.join("text.exe");
    std::fs::write(&text, b"just text").expect("write file");
    assert_eq!(file_version(&text), no_resource);

    // A version block whose value is empty is a block without version numbers.
    let (mut bytes, info) = system_executable();
    bytes[info - 40 + 2..info - 40 + 4].copy_from_slice(&0u16.to_le_bytes());
    let empty = scratch.join("empty.exe");
    std::fs::write(&empty, bytes).expect("write executable");
    assert_eq!(
        file_version(&empty),
        Err(t("The executable has an invalid version resource.").to_string())
    );
}

// ---- Verifying staged installers --------------------------------------------------------------

#[test]
fn an_artifact_verifies_when_the_signature_matches_its_bytes_and_version() {
    let scratch = Scratch::new("artifact");
    let key = &test_key().public;
    let exe = executable_with_version([7, 8, 9]);
    let path = write_signed(&scratch.join("a"), "7.8.9", &exe, "7.8.9");
    assert_eq!(verify_artifact(key, &path), Ok("7.8.9".to_string()));

    // The published key refuses an installer signed with any other key.
    assert_eq!(
        verify_update_artifact(&path),
        Err(t("The update signature does not match the pinned publisher key.").to_string())
    );

    // One changed byte breaks the signed digest.
    let mut tampered = exe.clone();
    *tampered.last_mut().expect("bytes") ^= 1;
    let path = write_signed(&scratch.join("b"), "7.8.9", &exe, "7.8.9");
    std::fs::write(&path, tampered).expect("tamper");
    assert_eq!(
        verify_artifact(key, &path),
        Err(t("The update signature does not match the pinned publisher key.").to_string())
    );

    // Signed for another version than the file carries.
    let path = write_signed(&scratch.join("c"), "7.8.9", &exe, "7.8.8");
    assert_eq!(
        verify_artifact(key, &path),
        Err(t("The update signature does not match the pinned publisher key.").to_string())
    );

    // A signature of the wrong size is rejected up front.
    let path = write_signed(&scratch.join("d"), "7.8.9", &exe, "7.8.9");
    std::fs::write(path.with_extension("exe.sig"), b"short").expect("write signature");
    assert_eq!(
        verify_artifact(key, &path),
        Err(t("Invalid update signature metadata.").to_string())
    );

    // A signed file that is not an executable has no version to verify.
    let path = write_signed(&scratch.join("e"), "7.8.9", b"MZ but not really", "7.8.9");
    assert_eq!(
        verify_artifact(key, &path),
        Err(t("The executable has no valid version resource.").to_string())
    );
}

#[test]
fn an_artifact_that_is_missing_oversized_or_unsigned_is_rejected() {
    let scratch = Scratch::new("artifact-missing");
    let key = &test_key().public;
    let missing = scratch.join("isolmass-setup-7.8.9.exe");
    assert_eq!(
        verify_artifact(key, &missing),
        Err(std::fs::metadata(&missing)
            .expect_err("the file is missing")
            .to_string())
    );

    let huge = scratch.join("huge.exe");
    std::fs::File::create(&huge)
        .and_then(|file| file.set_len(MAX_INSTALLER_BYTES as u64 + 1))
        .expect("create an oversized file");
    assert_eq!(
        verify_artifact(key, &huge),
        Err(t("The staged installer exceeds the allowed size.").to_string())
    );

    let unsigned = scratch.join("unsigned.exe");
    std::fs::write(&unsigned, executable_with_version([7, 8, 9])).expect("write executable");
    let error = verify_artifact(key, &unsigned).expect_err("no detached signature");
    assert!(
        error.starts_with(lead("The staged update signature is unavailable: {}")),
        "{error}"
    );
}

#[test]
fn only_a_correctly_named_signed_installer_in_the_staging_folder_may_launch() {
    let scratch = Scratch::new("staged");
    let key = &test_key().public;
    let root = scratch.join("updates");
    let exe = executable_with_version([7, 8, 9]);
    let staged = write_signed(&root, "7.8.9", &exe, "7.8.9");
    assert_eq!(verify_staged_installer(key, &root, &staged), Ok(()));

    // The folder or the file does not exist.
    let error = verify_staged_installer(key, &scratch.join("nowhere"), &staged)
        .expect_err("no staging folder");
    assert!(
        error.starts_with(lead("Could not find the update folder: {}")),
        "{error}"
    );
    let error = verify_staged_installer(key, &root, &root.join("isolmass-setup-1.0.0.exe"))
        .expect_err("no installer");
    assert!(
        error.starts_with(lead("Could not find the staged installer: {}")),
        "{error}"
    );

    // A correctly signed installer elsewhere is not trusted.
    let elsewhere = write_signed(&scratch.join("elsewhere"), "7.8.9", &exe, "7.8.9");
    assert_eq!(
        verify_staged_installer(key, &root, &elsewhere),
        Err(t("The installer is not in the trusted update staging folder.").to_string())
    );

    // The file name must carry a version.
    for name in [
        "setup.exe",
        "isolmass-setup-latest.exe",
        "isolmass-setup-1.2.exe",
    ] {
        let renamed = root.join(name);
        std::fs::copy(&staged, &renamed).expect("copy installer");
        assert_eq!(
            verify_staged_installer(key, &root, &renamed),
            Err(t("Unexpected staged installer filename.").to_string()),
            "{name}"
        );
    }

    // The name must agree with the signed version inside.
    let mislabeled = write_signed(&root, "1.2.3", &exe, "7.8.9");
    assert_eq!(
        verify_staged_installer(key, &root, &mislabeled),
        Err(t("The staged installer version differs from its signed release.").to_string())
    );

    // And the signature must hold.
    let forged = write_signed(
        &root,
        "7.8.10",
        &executable_with_version([7, 8, 10]),
        "7.8.9",
    );
    assert_eq!(
        verify_staged_installer(key, &root, &forged),
        Err(t("The update signature does not match the pinned publisher key.").to_string())
    );
}
