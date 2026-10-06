//! Browser-based Cloudflare consent and installation into the selected user's own account.
//! OAuth access tokens stay in memory; only Worker-specific keys enter Windows DPAPI.
use crate::cloudflare_setup::{self, CloudCredentials, RequestBody};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

const CLIENT_ID: &str = "aab1858e7c76bac3d9a341eef60d0bea";
const REQUIRED_SCOPES: &str = "memberships.read workers-scripts.write";
const REDIRECT_URI: &str = "http://127.0.0.1:38481/oauth/callback";
const CALLBACK_PORT: u16 = 38481;
const MAX_ACCOUNTS: usize = 200;
const WORKER_SOURCE: &str = include_str!("../cloudflare/worker.mjs");
/// Must equal `WORKER_VERSION` in `cloudflare/worker.mjs` (checked by a test).
pub const WORKER_VERSION: u64 = 3;
/// Per-account rate-limit namespace for share-link views (60 per IP per minute).
const VIEW_LIMITER: &str = "4917";

#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub name: String,
}

pub struct Authorization {
    pub access_token: String,
    pub accounts: Vec<Account>,
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for part in bytes.chunks(3) {
        let word = ((part[0] as usize) << 16)
            | ((part.get(1).copied().unwrap_or(0) as usize) << 8)
            | part.get(2).copied().unwrap_or(0) as usize;
        result.push(ALPHABET[(word >> 18) & 63] as char);
        result.push(ALPHABET[(word >> 12) & 63] as char);
        if part.len() > 1 {
            result.push(ALPHABET[(word >> 6) & 63] as char);
        }
        if part.len() > 2 {
            result.push(ALPHABET[word & 63] as char);
        }
    }
    result
}

fn sha256(bytes: &[u8]) -> Result<[u8; 32], String> {
    let mut digest = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut digest) }
        .ok()
        .map_err(|error| {
            crate::i18n::tf("Windows could not create the PKCE digest: {}", &[&error])
        })?;
    Ok(digest)
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(
                char::from_digit((byte >> 4) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            out.push(
                char::from_digit((byte & 15) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
    out
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(crate::i18n::t("Malformed Cloudflare callback.").into());
            }
            let high =
                hex(bytes[index + 1]).ok_or(crate::i18n::t("Malformed Cloudflare callback."))?;
            let low =
                hex(bytes[index + 2]).ok_or(crate::i18n::t("Malformed Cloudflare callback."))?;
            out.push(high << 4 | low);
            index += 3;
        } else {
            out.push(if bytes[index] == b'+' {
                b' '
            } else {
                bytes[index]
            });
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| crate::i18n::t("Malformed Cloudflare callback.").into())
}

/// Also used to tell a refusal apart from other callback errors, so compare it whole.
fn consent_denied_message() -> String {
    crate::i18n::t("Cloudflare permission was not granted; nothing was connected.").into()
}

fn parse_callback(request: &str, expected_state: &str) -> Result<Option<String>, String> {
    let mut lines = request.split("\r\n");
    let first = lines
        .next()
        .ok_or(crate::i18n::t("Missing Cloudflare callback."))?;
    let mut parts = first.split_whitespace();
    if parts.next() != Some("GET") {
        return Err(crate::i18n::t("Invalid Cloudflare callback method.").into());
    }
    let target = parts
        .next()
        .ok_or(crate::i18n::t("Invalid Cloudflare callback path."))?;
    if parts.next() != Some("HTTP/1.1") || parts.next().is_some() {
        return Err(crate::i18n::t("Invalid Cloudflare callback request.").into());
    }
    let host = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("Host").then(|| value.trim())
    });
    if host != Some("127.0.0.1:38481") {
        return Err(crate::i18n::t("Cloudflare callback host does not match.").into());
    }
    let (path, query) = target
        .split_once('?')
        .ok_or(crate::i18n::t("Cloudflare did not return a response."))?;
    if path != "/oauth/callback" {
        return Err(crate::i18n::t("Unrecognized Cloudflare callback path.").into());
    }
    let mut state = None;
    let mut code = None;
    let mut denied = false;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or(crate::i18n::t("Malformed Cloudflare callback."))?;
        match key {
            "state" if state.is_none() => state = Some(percent_decode(value)?),
            "code" if code.is_none() => code = Some(percent_decode(value)?),
            "error" if !denied => denied = true,
            "state" | "code" | "error" => {
                return Err(crate::i18n::t("Duplicate OAuth parameter.").into());
            }
            _ => {}
        }
    }
    let state = state.ok_or(crate::i18n::t(
        "Cloudflare did not return a request identifier.",
    ))?;
    if !constant_equal(state.as_bytes(), expected_state.as_bytes()) {
        return Err(crate::i18n::t("Cloudflare callback did not match this request.").into());
    }
    if denied {
        return Err(consent_denied_message());
    }
    let code = code.ok_or(crate::i18n::t(
        "Cloudflare did not return an authorization code.",
    ))?;
    if code.is_empty()
        || code.len() > 4096
        || !code.is_ascii()
        || code.chars().any(char::is_control)
    {
        return Err(crate::i18n::t("Cloudflare returned an invalid authorization code.").into());
    }
    Ok(Some(code))
}

fn constant_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (&a, &b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn callback_reply(socket: &mut TcpStream, accepted: bool) {
    use crate::i18n::t;
    let html = if accepted {
        format!(
            "<!doctype html><html lang={}><meta charset=utf-8><title>isolmaSS · {}</title><main><h1>{}</h1><p>{}</p></main><script>window.close()</script>",
            crate::i18n::code(),
            t("Connecting"),
            t("Permission granted"),
            t("Setup is finishing in the app. You can close this tab.")
        )
    } else {
        format!(
            "<!doctype html><html lang={}><meta charset=utf-8><title>isolmaSS · {}</title><main><h1>{}</h1><p>{}</p></main>",
            crate::i18n::code(),
            t("Stopped"),
            t("Could not connect"),
            t("Check the error message in the app.")
        )
    };
    let status = if accepted {
        "200 OK"
    } else {
        "400 Bad Request"
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        html.len()
    );
    let _ = socket
        .write_all(head.as_bytes())
        .and_then(|()| socket.write_all(html.as_bytes()));
}

/// Reads one HTTP request head (bounded to 8 KiB and 5 seconds) and leaves the
/// socket blocking for the reply. `None` means the connection carried no usable
/// request.
fn read_request(socket: &mut TcpStream, cancel: &AtomicBool) -> Option<Vec<u8>> {
    socket.set_nonblocking(true).ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut bytes = vec![0u8; 8192];
    let mut length = 0;
    while length < bytes.len() {
        match socket.read(&mut bytes[length..]) {
            Ok(0) => break,
            Ok(count) => {
                length += count;
                if bytes[..length].windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
    if length == 0 {
        return None;
    }
    socket.set_nonblocking(false).ok()?;
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    bytes.truncate(length);
    Some(bytes)
}

fn callback(listener: &TcpListener, state: &str, cancel: &AtomicBool) -> Result<String, String> {
    // Signing up, verifying e-mail or completing 2FA can take several minutes.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::i18n::t("The Cloudflare connection was cancelled.").into());
        }
        if Instant::now() >= deadline {
            return Err(crate::i18n::t("The Cloudflare sign-in timed out. Connect again.").into());
        }
        match listener.accept() {
            Ok((mut socket, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                // Browsers open speculative connections that send nothing, and a
                // Windows socket accepted from a non-blocking listener is itself
                // non-blocking. A quiet or broken connection is skipped instead of
                // failing the login; the real redirect arrives on another one.
                let Some(request) = read_request(&mut socket, cancel) else {
                    continue;
                };
                let parsed = std::str::from_utf8(&request)
                    .map_err(|_| crate::i18n::t("Malformed Cloudflare callback.").to_string())
                    .and_then(|text| parse_callback(text, state));
                callback_reply(&mut socket, parsed.as_ref().is_ok_and(Option::is_some));
                match parsed {
                    Ok(Some(code)) => return Ok(code),
                    Err(error) if error == consent_denied_message() => return Err(error),
                    _ => continue, // Noise from another local process cannot cancel a valid login.
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(error) => {
                return Err(crate::i18n::tf(
                    "Could not receive the Cloudflare sign-in: {}",
                    &[&error],
                ));
            }
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    /// Space-separated scopes actually granted; users may deselect optional ones.
    #[serde(default)]
    scope: Option<String>,
}

fn cloudflare_json(
    host: &str,
    path: &str,
    method: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> Result<Value, String> {
    let encoded = body
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|error| error.to_string())?;
    let (status, bytes) = cloudflare_setup::control_request(
        host,
        path,
        method,
        token,
        body.map(|_| "application/json"),
        RequestBody::Bytes(encoded.as_deref().unwrap_or_default()),
    )?;
    let response: Value = serde_json::from_slice(&bytes)
        .map_err(|_| crate::i18n::tf("Cloudflare API HTTP {}: invalid JSON.", &[&status]))?;
    if !(200..300).contains(&status) || response["success"] != true {
        return Err(format!("Cloudflare API {}.", api_error(status, &bytes)));
    }
    Ok(response["result"].clone())
}

pub fn authorize(cancel: &AtomicBool) -> Result<Authorization, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, CALLBACK_PORT)).map_err(|error| {
        crate::i18n::tf(
            "Could not open local port 38481 for the Cloudflare callback: {}",
            &[&error],
        )
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let verifier = cloudflare_setup::generate_token()?;
    let state = cloudflare_setup::generate_token()?;
    let challenge = base64url(&sha256(verifier.as_bytes())?);
    let url = format!(
        "https://dash.cloudflare.com/oauth2/auth?client_id={CLIENT_ID}&response_type=code&redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        percent_encode(REDIRECT_URI),
        percent_encode(REQUIRED_SCOPES)
    );
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let launched = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if launched.0 as isize <= 32 {
        return Err(
            crate::i18n::t("Could not open the Cloudflare sign-in page in the browser.").into(),
        );
    }
    let code = callback(&listener, &state, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::i18n::t("The Cloudflare connection was cancelled.").into());
    }
    let form = format!(
        "grant_type=authorization_code&client_id={CLIENT_ID}&code={}&redirect_uri={}&code_verifier={verifier}",
        percent_encode(&code),
        percent_encode(REDIRECT_URI)
    );
    let (status, bytes) = cloudflare_setup::control_request(
        "dash.cloudflare.com",
        "/oauth2/token",
        "POST",
        None,
        Some("application/x-www-form-urlencoded"),
        RequestBody::Bytes(form.as_bytes()),
    )?;
    if status != 200 {
        return Err(crate::i18n::tf(
            "Could not start the Cloudflare OAuth session (HTTP {}).",
            &[&status],
        ));
    }
    let issued: TokenResponse = serde_json::from_slice(&bytes).map_err(|_| {
        crate::i18n::t("Cloudflare returned an invalid OAuth response.").to_string()
    })?;
    if !issued.token_type.eq_ignore_ascii_case("Bearer")
        || issued.expires_in < 60
        || issued.access_token.len() < 20
        || issued.access_token.len() > 4096
        || !issued.access_token.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
        })
    {
        return Err(crate::i18n::t("Cloudflare returned an invalid access token.").into());
    }
    if let Some(granted) = issued.scope.as_deref() {
        let missing: Vec<&str> = REQUIRED_SCOPES
            .split(' ')
            .filter(|scope| !granted.split(' ').any(|given| given == *scope))
            .collect();
        if !missing.is_empty() {
            return Err(crate::i18n::tf(
                "The required permissions were not granted on the Cloudflare consent screen ({}). Connect again and approve all permissions.",
                &[&missing.join(", ")],
            ));
        }
    }
    let accounts = list_accounts(&issued.access_token, cancel)?;
    let accounts = authorized_accounts(&issued.access_token, accounts, cancel)?;
    Ok(Authorization {
        access_token: issued.access_token,
        accounts,
    })
}

/// Memberships list every account the user belongs to, but the consent page
/// grants Workers access only to the accounts picked there. Each candidate is
/// probed in parallel with a harmless read of its Workers subdomain: `200`/`404`
/// mean the token may manage Workers there, `401`/`403` mean it may not.
/// Accounts whose probe fails for network reasons are kept rather than hidden.
fn authorized_accounts(
    token: &str,
    accounts: Vec<Account>,
    cancel: &AtomicBool,
) -> Result<Vec<Account>, String> {
    const MAX_PROBES: usize = 25;
    if accounts.len() <= 1 || accounts.len() > MAX_PROBES {
        return Ok(accounts);
    }
    let verdicts: Vec<bool> = std::thread::scope(|scope| {
        let probes: Vec<_> = accounts
            .iter()
            .map(|account| {
                scope.spawn(move || {
                    match cloudflare_setup::control_request(
                        "api.cloudflare.com",
                        &account_path(account, "subdomain"),
                        "GET",
                        Some(token),
                        None,
                        RequestBody::Bytes(&[]),
                    ) {
                        Ok((status, _)) => !matches!(status, 401 | 403),
                        Err(_) => true,
                    }
                })
            })
            .collect();
        probes
            .into_iter()
            .map(|probe| probe.join().unwrap_or(true))
            .collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::i18n::t("The Cloudflare connection was cancelled.").into());
    }
    let usable: Vec<Account> = accounts
        .into_iter()
        .zip(verdicts)
        .filter_map(|(account, usable)| usable.then_some(account))
        .collect();
    if usable.is_empty() {
        return Err(crate::i18n::t("None of the authorized accounts give you Workers access. Connect again and pick an account where you can use Workers on the Cloudflare consent page; if it persists, make sure the app's OAuth client is public.").into());
    }
    Ok(usable)
}

fn list_accounts(token: &str, cancel: &AtomicBool) -> Result<Vec<Account>, String> {
    let mut accounts = Vec::new();
    for page in 1..=MAX_ACCOUNTS / 50 {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::i18n::t("The Cloudflare connection was cancelled.").into());
        }
        let result = cloudflare_json(
            "api.cloudflare.com",
            &format!("/client/v4/memberships?per_page=50&page={page}&status=accepted"),
            "GET",
            Some(token),
            None,
        )?;
        let rows = result.as_array().ok_or(crate::i18n::t(
            "Could not read the Cloudflare account list.",
        ))?;
        for row in rows {
            let Some(id) = row["account"]["id"].as_str() else {
                continue;
            };
            let Some(name) = row["account"]["name"].as_str() else {
                continue;
            };
            if id.len() == 32
                && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                && !accounts.iter().any(|account: &Account| account.id == id)
            {
                let safe_name = !name.is_empty()
                    && name.len() <= 100
                    && !name.chars().any(|character| {
                        character.is_control()
                            || matches!(character, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                    });
                accounts.push(Account {
                    id: id.to_ascii_lowercase(),
                    name: if safe_name {
                        name.into()
                    } else {
                        crate::i18n::t("Cloudflare account").into()
                    },
                });
            }
        }
        if rows.len() < 50 {
            break;
        }
        if page == MAX_ACCOUNTS / 50 {
            return Err(crate::i18n::t("There are more than 200 Cloudflare accounts; the list limit for safe account selection was exceeded.").into());
        }
    }
    if accounts.is_empty() {
        return Err(
            crate::i18n::t("No Cloudflare account can be used with this permission.").into(),
        );
    }
    Ok(accounts)
}

fn account_path(account: &Account, suffix: &str) -> String {
    format!("/client/v4/accounts/{}/workers/{suffix}", account.id)
}

/// The account's workers.dev subdomain, and whether this call registered it.
fn ensure_subdomain(
    token: &str,
    account: &Account,
    cancel: &AtomicBool,
) -> Result<(String, bool), String> {
    let path = account_path(account, "subdomain");
    let (status, bytes) = cloudflare_setup::control_request(
        "api.cloudflare.com",
        &path,
        "GET",
        Some(token),
        None,
        RequestBody::Bytes(&[]),
    )?;
    if status == 200 {
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| crate::i18n::t("Could not read the Cloudflare subdomain."))?;
        if response["success"] != true {
            return Err(
                crate::i18n::t("Access to the Cloudflare Workers subdomain was denied.").into(),
            );
        }
        return subdomain(&response["result"]["subdomain"]).map(|name| (name, false));
    }
    if status != 404 {
        return Err(crate::i18n::tf(
            "Could not check the Cloudflare Workers subdomain ({}).{}",
            &[
                &api_error(status, &bytes),
                &if status == 403 {
                    crate::i18n::t(
                        " You may lack Workers access in the chosen account, or the app's OAuth client may be private to its own account; try another account.",
                    )
                } else {
                    ""
                },
            ],
        ));
    }
    for _ in 0..3 {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::i18n::t("Cloudflare setup was cancelled.").into());
        }
        let name = format!("isolmass-{}", random_label(10)?);
        let result = cloudflare_json(
            "api.cloudflare.com",
            &path,
            "PUT",
            Some(token),
            Some(&json!({"subdomain": name})),
        );
        if let Ok(response) = result {
            return subdomain(&response["subdomain"]).map(|name| (name, true));
        }
        if let Err(error) = result
            && !error.contains("HTTP 409")
        {
            return Err(error);
        }
    }
    Err(
        crate::i18n::t("Cloudflare could not provide a free workers.dev subdomain; try again.")
            .into(),
    )
}

fn subdomain(value: &Value) -> Result<String, String> {
    let name = value.as_str().ok_or(crate::i18n::t(
        "Cloudflare returned an invalid Workers subdomain.",
    ))?;
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return Err(crate::i18n::t("Cloudflare returned an invalid Workers subdomain.").into());
    }
    Ok(name.to_string())
}

fn random_label(length: usize) -> Result<String, String> {
    let token = cloudflare_setup::generate_token()?;
    let label: String = token
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(length)
        .collect();
    if label.len() != length {
        return Err(crate::i18n::t(
            "Windows could not generate a valid Cloudflare resource name; try again.",
        )
        .into());
    }
    Ok(label.to_ascii_lowercase())
}

fn random_worker_name() -> Result<String, String> {
    Ok(format!("isolmass-share-{}", random_label(12)?))
}

fn vacant_worker(token: &str, account: &Account, cancel: &AtomicBool) -> Result<String, String> {
    for _ in 0..3 {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::i18n::t("Cloudflare setup was cancelled.").into());
        }
        let name = random_worker_name()?;
        let path = account_path(account, &format!("scripts/{name}"));
        let (status, _) = cloudflare_setup::control_request(
            "api.cloudflare.com",
            &path,
            "GET",
            Some(token),
            None,
            RequestBody::Bytes(&[]),
        )?;
        if status == 404 {
            return Ok(name);
        }
        if status != 200 {
            return Err(crate::i18n::tf(
                "Could not check existing Worker names (HTTP {}).",
                &[&status],
            ));
        }
    }
    Err(crate::i18n::t("No free Worker name was found; try again.").into())
}

/// Script-upload body. A new Worker declares its Durable Object migration; an
/// update of an existing Worker must not repeat it. The view rate limiter is
/// optional so an account that rejects the binding still gets a working Worker.
fn multipart(
    upload_token: &str,
    admin_token: &str,
    limiter: bool,
    new_worker: bool,
) -> Result<(String, Vec<u8>), String> {
    let boundary = format!("isolmass-{}", cloudflare_setup::generate_token()?);
    let mut bindings = vec![
        json!({"type":"durable_object_namespace", "name":"STORE", "class_name":"ShareStore"}),
        json!({"type":"secret_text", "name":"UPLOAD_TOKEN", "text":upload_token}),
        json!({"type":"secret_text", "name":"ADMIN_TOKEN", "text":admin_token}),
    ];
    if limiter {
        bindings.push(json!({
            "type":"ratelimit",
            "name":"VIEW_LIMITER",
            "namespace_id":VIEW_LIMITER,
            "simple":{"limit":60, "period":60}
        }));
    }
    let mut metadata = json!({
        "main_module": "worker.mjs",
        "compatibility_date": "2026-09-24",
        "bindings": bindings,
    });
    if new_worker {
        // Script-upload API shape (SingleStepMigration), not wrangler's `[[migrations]]` list.
        metadata["migrations"] = json!({"new_tag":"v1", "new_sqlite_classes":["ShareStore"]});
    }
    let encoded = serde_json::to_vec(&metadata).map_err(|error| error.to_string())?;
    let mut body = Vec::with_capacity(WORKER_SOURCE.len() + encoded.len() + 512);
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    body.extend_from_slice(&encoded);
    body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"worker.mjs\"; filename=\"worker.mjs\"\r\nContent-Type: application/javascript+module\r\n\r\n").as_bytes());
    body.extend_from_slice(WORKER_SOURCE.as_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), body))
}

/// Uploads the Worker script, first with the view rate limiter and, if
/// Cloudflare refuses that request, once more without it.
fn upload_script(
    token: &str,
    account: &Account,
    worker: &str,
    credentials: &CloudCredentials,
    new_worker: bool,
) -> Result<(), String> {
    let path = account_path(account, &format!("scripts/{worker}"));
    let mut last = String::new();
    for limiter in [true, false] {
        let (content_type, body) = multipart(
            &credentials.upload_token,
            &credentials.admin_token,
            limiter,
            new_worker,
        )?;
        let (status, bytes) = cloudflare_setup::control_request(
            "api.cloudflare.com",
            &path,
            "PUT",
            Some(token),
            Some(&content_type),
            RequestBody::Bytes(&body),
        )?;
        if (200..300).contains(&status)
            && serde_json::from_slice::<Value>(&bytes).is_ok_and(|value| value["success"] == true)
        {
            if !limiter {
                crate::diagnostics::record(
                    "cloudflare worker",
                    &format!("rate limiter binding refused, installed without it: {last}"),
                );
            }
            return Ok(());
        }
        last = api_error(status, &bytes);
        // Authorization problems will not improve without the binding.
        if matches!(status, 401 | 403) {
            break;
        }
    }
    Err(crate::i18n::tf(
        "Cloudflare rejected the Worker upload ({}).",
        &[&last],
    ))
}

/// A Worker that was created in the user's account. `ready` is false when its
/// workers.dev address did not answer yet; the pairing is still valid.
pub struct Installed {
    pub credentials: CloudCredentials,
    pub ready: bool,
}

pub fn install(token: &str, account: &Account, cancel: &AtomicBool) -> Result<Installed, String> {
    if account.id.len() != 32 || !account.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(crate::i18n::t("Invalid Cloudflare account ID.").into());
    }
    let (subdomain, created) = ensure_subdomain(token, account, cancel)?;
    let worker = vacant_worker(token, account, cancel)?;
    let origin = format!("https://{worker}.{subdomain}.workers.dev");
    let credentials = CloudCredentials {
        origin: origin.clone(),
        upload_token: cloudflare_setup::generate_token()?,
        admin_token: cloudflare_setup::generate_token()?,
        share_password: None,
        account_id: Some(account.id.clone()),
        worker_name: Some(worker.clone()),
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::i18n::t("Cloudflare setup was cancelled.").into());
    }
    upload_script(token, account, &worker, &credentials, true)?;
    let route = account_path(account, &format!("scripts/{worker}/subdomain"));
    let enabled = cloudflare_json(
        "api.cloudflare.com",
        &route,
        "POST",
        Some(token),
        Some(&json!({"enabled": true, "previews_enabled": false})),
    )
    .map_err(|error| {
        crate::i18n::tf(
            "Worker {} was created but workers.dev could not be enabled: {}",
            &[&worker, &error],
        )
    })?;
    if enabled["enabled"] != true {
        return Err(crate::i18n::tf(
            "Worker {} was created but its workers.dev address could not be opened.",
            &[&worker],
        ));
    }
    // A new workers.dev name needs DNS/TLS propagation (minutes for a brand-new
    // account subdomain). Asking too early also plants a negative DNS cache
    // entry in Windows, so a fresh subdomain gets a head start.
    let deadline = Instant::now() + Duration::from_secs(if created { 150 } else { 90 });
    if created {
        wait(Duration::from_secs(8), cancel);
    }
    loop {
        if cancel.load(Ordering::Relaxed) {
            // The Worker exists and its keys are known: keep the pairing rather
            // than orphaning the Worker in the user's account.
            return Ok(Installed {
                credentials,
                ready: false,
            });
        }
        if let Ok((200, reply)) = cloudflare_setup::api_request(
            &origin,
            "/api/setup",
            "POST",
            &credentials.admin_token,
            None,
            None,
            RequestBody::Bytes(&[]),
        ) && let Ok(value) = serde_json::from_slice::<Value>(&reply)
            && value["status"] == "ready"
            && value["origin"] == origin
        {
            return Ok(Installed {
                credentials,
                ready: true,
            });
        }
        if Instant::now() >= deadline {
            return Ok(Installed {
                credentials,
                ready: false,
            });
        }
        wait(Duration::from_secs(3), cancel);
    }
}

fn wait(duration: Duration, cancel: &AtomicBool) {
    let end = Instant::now() + duration;
    while Instant::now() < end && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Cloudflare's first error, shortened and stripped of control characters so it
/// can be shown in a dialog.
fn api_error(status: u32, bytes: &[u8]) -> String {
    let first = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value["errors"].as_array()?.first().cloned());
    let code = first.as_ref().and_then(|error| error["code"].as_u64());
    let message: Option<String> = first
        .as_ref()
        .and_then(|error| error["message"].as_str())
        .map(|text| {
            text.chars()
                .filter(|character| !character.is_control())
                .take(200)
                .collect()
        });
    match (code, message) {
        (Some(code), Some(message)) => {
            crate::i18n::tf("HTTP {}, code {}: {}", &[&status, &code, &message])
        }
        (Some(code), None) => crate::i18n::tf("HTTP {}, code {}", &[&status, &code]),
        (None, Some(message)) => format!("HTTP {status}: {message}"),
        (None, None) => format!("HTTP {status}"),
    }
}

/// Replaces the code of this computer's existing Worker with the bundled
/// version, keeping its name, address, keys, images and settings. The account
/// is the stored one, or the authorized account that holds a script of that name.
pub fn update_worker(
    token: &str,
    accounts: &[Account],
    credentials: &CloudCredentials,
    cancel: &AtomicBool,
) -> Result<CloudCredentials, String> {
    let worker = match credentials.worker_name.clone() {
        Some(name) => name,
        None => credentials
            .origin
            .strip_prefix("https://")
            .and_then(|host| host.split('.').next())
            .filter(|label| label.starts_with("isolmass-share-"))
            .map(str::to_owned)
            .ok_or(crate::i18n::t(
                "Could not determine this connection's Worker name.",
            ))?,
    };
    let stored = credentials
        .account_id
        .as_deref()
        .and_then(|id| accounts.iter().find(|account| account.id == id));
    let account = match stored {
        Some(account) => account.clone(),
        None => {
            let mut found = None;
            for account in accounts {
                if cancel.load(Ordering::Relaxed) {
                    return Err(crate::i18n::t("The Worker update was cancelled.").into());
                }
                let (status, _) = cloudflare_setup::control_request(
                    "api.cloudflare.com",
                    &account_path(account, &format!("scripts/{worker}")),
                    "GET",
                    Some(token),
                    None,
                    RequestBody::Bytes(&[]),
                )?;
                if status == 200 {
                    found = Some(account.clone());
                    break;
                }
            }
            found.ok_or(
                crate::i18n::t("This computer's Worker was not found in the authorized accounts. Pick the account that holds the Worker on the Cloudflare consent page."),
            )?
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::i18n::t("The Worker update was cancelled.").into());
    }
    let mut updated = credentials.clone();
    updated.account_id = Some(account.id.clone());
    updated.worker_name = Some(worker.clone());
    upload_script(token, &account, &worker, &updated, false)?;
    // The new code is live within seconds; confirm the version it reports.
    for attempt in 0..10 {
        if let Ok((200, reply)) = cloudflare_setup::api_request(
            &updated.origin,
            "/api/setup",
            "POST",
            &updated.admin_token,
            None,
            None,
            RequestBody::Bytes(&[]),
        ) && serde_json::from_slice::<Value>(&reply)
            .is_ok_and(|value| value["version"].as_u64() == Some(WORKER_VERSION))
        {
            return Ok(updated);
        }
        if attempt < 9 {
            wait(Duration::from_secs(2), cancel);
        }
    }
    Err(crate::i18n::t("The Worker was updated but the new version is not answering yet; press Refresh in a few seconds.").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_worker_reports_the_expected_version() {
        assert!(WORKER_SOURCE.contains(&format!("const WORKER_VERSION = {WORKER_VERSION};")));
    }

    #[test]
    fn oauth_callback_rejects_wrong_state_and_duplicate_parameters() {
        let line =
            "GET /oauth/callback?state=correct&code=abc HTTP/1.1\r\nHost: 127.0.0.1:38481\r\n\r\n";
        assert_eq!(
            parse_callback(line, "correct").unwrap().as_deref(),
            Some("abc")
        );
        assert!(parse_callback(line, "wrong").is_err());
        assert!(parse_callback("GET /oauth/callback?state=correct&state=correct&code=abc HTTP/1.1\r\nHost: 127.0.0.1:38481\r\n\r\n", "correct").is_err());
        assert!(
            parse_callback(
                "GET /oauth/callback?state=correct&code=abc HTTP/1.1\r\nHost: bad.test\r\n\r\n",
                "correct"
            )
            .is_err()
        );
    }

    #[test]
    fn denied_consent_is_recognized_in_every_language() {
        let _lock = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap();
        let line = "GET /oauth/callback?state=correct&error=access_denied HTTP/1.1\r\nHost: 127.0.0.1:38481\r\n\r\n";
        for language in [
            crate::i18n::LanguagePreference::English,
            crate::i18n::LanguagePreference::Turkish,
        ] {
            crate::i18n::set_preference(language);
            let error = parse_callback(line, "correct").unwrap_err();
            assert_eq!(error, consent_denied_message());
        }
        crate::i18n::set_preference(crate::i18n::LanguagePreference::System);
    }
}
