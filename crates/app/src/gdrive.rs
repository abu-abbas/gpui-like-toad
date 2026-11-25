//! Google Drive as a Settings Sync target.
//!
//! Sign-in is the installed-app OAuth flow (RFC 8252): the browser is sent
//! to Google, which redirects to a one-shot server on 127.0.0.1; PKCE
//! protects the code. Only the `drive.file` scope is asked, so Kilat sees
//! just the files it created, in a visible `Kilat Sync` folder.
//!
//! The sync itself never talks to Drive: [`pull`] copies `kilat-sync.json`
//! from Drive into a local cache folder, the normal folder sync runs on
//! that folder, and [`push`] uploads it again when it changed. A race with
//! another computer is healed by the next 3-way merge.
//!
//! The client id/secret come from the release build (`option_env!`), never
//! from the repository. Google treats a desktop client secret as public.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ora_core::credentials::{CredentialStore, ExposeSecret as _, SecretString};
use ora_core::oauth;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::release_fetch::{api_agent, net_err};

const CLIENT_ID: Option<&str> = option_env!("KILAT_GOOGLE_CLIENT_ID");
const CLIENT_SECRET: Option<&str> = option_env!("KILAT_GOOGLE_CLIENT_SECRET");
const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3";
const FOLDER_NAME: &str = "Kilat Sync";
const SYNC_FILE: &str = "kilat-sync.json";
/// Keychain account holding the refresh token.
pub const REFRESH_ACCOUNT: &str = "kilat-google-refresh";
/// Shown when Google no longer accepts the saved sign-in. In Testing mode
/// that happens every 7 days.
pub const SIGN_IN_AGAIN: &str = "Google sign-in expired. Sign in again on the Sync page.";

/// Whether this build has a Google client (release builds do).
pub fn available() -> bool {
    CLIENT_ID.is_some_and(|s| !s.is_empty()) && CLIENT_SECRET.is_some_and(|s| !s.is_empty())
}

fn client() -> Result<(&'static str, &'static str), String> {
    match (CLIENT_ID, CLIENT_SECRET) {
        (Some(i), Some(s)) if !i.is_empty() && !s.is_empty() => Ok((i, s)),
        _ => Err("This build of Kilat has no Google client. Use a release build.".into()),
    }
}

// ---- HTTP helpers -------------------------------------------------------

fn finish(
    resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<(u16, String), String> {
    let mut resp = resp.map_err(net_err)?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_string()
        .map_err(net_err)?;
    Ok((status, body))
}

fn api_error(status: u16, body: &str) -> String {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let reason = json["error"]["message"]
        .as_str()
        .or_else(|| json["error_description"].as_str())
        .unwrap_or("")
        .to_string();
    match status {
        401 => SIGN_IN_AGAIN.to_string(),
        403 if body.contains("storageQuotaExceeded") => "Google Drive storage is full.".to_string(),
        _ => format!("Google Drive error {status}: {reason}"),
    }
}

fn get_json(url: &str, token: &str) -> Result<Value, String> {
    let (status, body) = finish(
        api_agent()?
            .get(url)
            .header("Authorization", &format!("Bearer {token}"))
            .call(),
    )?;
    if status >= 300 {
        return Err(api_error(status, &body));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

// ---- Sign-in ------------------------------------------------------------

/// A sign-in waiting for the browser. `url` is opened by the caller.
pub struct Pending {
    listener: TcpListener,
    pub url: String,
    verifier: String,
    state: String,
    redirect: String,
}

pub fn begin() -> Result<Pending, String> {
    let (id, _) = client()?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");
    let (verifier, challenge) = oauth::pkce();
    let state = oauth::random_state();
    let url = oauth::auth_url(AUTH_ENDPOINT, id, &redirect, &state, &challenge, SCOPE);
    Ok(Pending {
        listener,
        url,
        verifier,
        state,
        redirect,
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Kilat</title>\
<body style=\"font-family:system-ui;text-align:center;margin-top:20vh\">\
<h2>Signed in</h2><p>You can close this tab and go back to Kilat.</p></body>";

/// Wait (up to 5 minutes) for the browser, then trade the code for tokens.
/// Saves the refresh token in the keychain and returns the account email.
pub fn finish_login(
    pending: Pending,
    keychain: &dyn CredentialStore,
    cancelled: impl Fn() -> bool,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(300);
    let code = loop {
        if cancelled() {
            return Err("Sign-in cancelled.".into());
        }
        if Instant::now() > deadline {
            return Err("Sign-in timed out. Try again.".into());
        }
        match pending.listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                let line = text.lines().next().unwrap_or("");
                let found = oauth::parse_callback(line, &pending.state);
                if found != oauth::Callback::Ignore {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        DONE_PAGE.len(),
                        DONE_PAGE
                    );
                }
                match found {
                    oauth::Callback::Code(code) => break code,
                    oauth::Callback::Denied(why) => {
                        return Err(format!("Google sign-in was not completed ({why})."))
                    }
                    oauth::Callback::Ignore => {}
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let (id, secret) = client()?;
    let body = oauth::form_encode(&[
        ("code", &code),
        ("client_id", id),
        ("client_secret", secret),
        ("redirect_uri", &pending.redirect),
        ("grant_type", "authorization_code"),
        ("code_verifier", &pending.verifier),
    ]);
    let tokens = token_request(&body)?;
    let refresh = tokens["refresh_token"]
        .as_str()
        .ok_or("Google did not return a refresh token. Remove Kilat from your Google account permissions and try again.")?;
    keychain
        .set(REFRESH_ACCOUNT, &SecretString::from(refresh.to_string()))
        .map_err(|e| e.to_string())?;
    let access = tokens["access_token"].as_str().unwrap_or_default();
    remember_access(access, tokens["expires_in"].as_u64().unwrap_or(3600));
    Ok(account_email(access).unwrap_or_else(|_| "Google account".into()))
}

fn token_request(body: &str) -> Result<Value, String> {
    let (status, text) = finish(
        api_agent()?
            .post(TOKEN_ENDPOINT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body),
    )?;
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status >= 300 {
        if json["error"].as_str() == Some("invalid_grant") {
            return Err(SIGN_IN_AGAIN.into());
        }
        return Err(format!(
            "Google sign-in failed: {}",
            json["error_description"]
                .as_str()
                .or(json["error"].as_str())
                .unwrap_or("unknown error")
        ));
    }
    Ok(json)
}

static ACCESS: Mutex<Option<(String, Instant)>> = Mutex::new(None);

fn remember_access(token: &str, expires_in: u64) {
    if let Ok(mut slot) = ACCESS.lock() {
        let life = Duration::from_secs(expires_in.saturating_sub(60));
        *slot = Some((token.to_string(), Instant::now() + life));
    }
}

fn access_token(keychain: &dyn CredentialStore) -> Result<String, String> {
    if let Some((token, until)) = ACCESS.lock().ok().and_then(|s| s.clone()) {
        if Instant::now() < until {
            return Ok(token);
        }
    }
    let refresh = keychain
        .get(REFRESH_ACCOUNT)
        .map_err(|e| e.to_string())?
        .ok_or(SIGN_IN_AGAIN)?;
    let (id, secret) = client()?;
    let body = oauth::form_encode(&[
        ("client_id", id),
        ("client_secret", secret),
        ("refresh_token", refresh.expose_secret()),
        ("grant_type", "refresh_token"),
    ]);
    let tokens = token_request(&body)?;
    let access = tokens["access_token"]
        .as_str()
        .ok_or("Google returned no access token.")?
        .to_string();
    remember_access(&access, tokens["expires_in"].as_u64().unwrap_or(3600));
    Ok(access)
}

fn account_email(token: &str) -> Result<String, String> {
    let json = get_json(&format!("{API}/about?fields=user(emailAddress)"), token)?;
    json["user"]["emailAddress"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "no email".into())
}

/// Forget the sign-in on this computer (the grant stays in the Google
/// account until the user removes it there).
pub fn sign_out(keychain: &dyn CredentialStore) {
    let _ = keychain.delete(REFRESH_ACCOUNT);
    if let Ok(mut slot) = ACCESS.lock() {
        *slot = None;
    }
}

// ---- Drive files --------------------------------------------------------

/// What we know about the file on Drive, so a sync only transfers when
/// something changed. Kept next to the cache.
#[derive(Default)]
struct State {
    folder_id: String,
    file_id: String,
    /// md5 Drive reported after our last download or upload.
    remote_md5: String,
    /// sha256 of the cache file after our last download or upload.
    local_sha: String,
}

impl State {
    fn path(cache: &Path) -> PathBuf {
        cache.join("drive-state.json")
    }

    fn load(cache: &Path) -> Self {
        let json: Value = std::fs::read_to_string(Self::path(cache))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let s = |k: &str| json[k].as_str().unwrap_or_default().to_string();
        Self {
            folder_id: s("folder_id"),
            file_id: s("file_id"),
            remote_md5: s("remote_md5"),
            local_sha: s("local_sha"),
        }
    }

    fn save(&self, cache: &Path) -> Result<(), String> {
        let json = serde_json::json!({
            "folder_id": self.folder_id,
            "file_id": self.file_id,
            "remote_md5": self.remote_md5,
            "local_sha": self.local_sha,
        });
        std::fs::write(Self::path(cache), json.to_string()).map_err(|e| e.to_string())
    }
}

fn sha_of(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        Err(_) => String::new(),
    }
}

fn q(url_base: &str, query: &str, fields: &str) -> String {
    format!(
        "{url_base}/files?{}",
        oauth::form_encode(&[
            ("q", query),
            ("fields", fields),
            ("spaces", "drive"),
            ("pageSize", "10"),
        ])
    )
}

fn ensure_folder(token: &str, state: &mut State) -> Result<(), String> {
    if !state.folder_id.is_empty() {
        return Ok(());
    }
    let query = format!(
        "name = '{FOLDER_NAME}' and mimeType = 'application/vnd.google-apps.folder' and trashed = false"
    );
    let found = get_json(&q(API, &query, "files(id)"), token)?;
    if let Some(id) = found["files"][0]["id"].as_str() {
        state.folder_id = id.to_string();
        return Ok(());
    }
    let meta = serde_json::json!({
        "name": FOLDER_NAME,
        "mimeType": "application/vnd.google-apps.folder",
    });
    let (status, body) = finish(
        api_agent()?
            .post(&format!("{API}/files?fields=id"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send(meta.to_string()),
    )?;
    if status >= 300 {
        return Err(api_error(status, &body));
    }
    let json: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    state.folder_id = json["id"]
        .as_str()
        .ok_or("Drive returned no folder id")?
        .into();
    Ok(())
}

/// The sync file on Drive: (id, md5), or `None` if it is not there yet.
fn find_file(token: &str, state: &State) -> Result<Option<(String, String)>, String> {
    let query = format!(
        "name = '{SYNC_FILE}' and '{}' in parents and trashed = false",
        state.folder_id
    );
    let found = get_json(&q(API, &query, "files(id,md5Checksum)"), token)?;
    Ok(found["files"][0]["id"].as_str().map(|id| {
        (
            id.to_string(),
            found["files"][0]["md5Checksum"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    }))
}

/// Before a sync: make the cache folder hold what Drive has. Returns the
/// signed-in account's token so [`push`] does not refresh again.
pub fn pull(cache: &Path, keychain: &dyn CredentialStore) -> Result<(), String> {
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let token = access_token(keychain)?;
    let mut state = State::load(cache);
    ensure_folder(&token, &mut state)?;
    match find_file(&token, &state)? {
        Some((id, md5)) => {
            state.file_id = id.clone();
            let local = cache.join(SYNC_FILE);
            if md5 != state.remote_md5 || !local.exists() {
                let (status, body) = finish(
                    api_agent()?
                        .get(&format!("{API}/files/{id}?alt=media"))
                        .header("Authorization", &format!("Bearer {token}"))
                        .call(),
                )?;
                if status >= 300 {
                    return Err(api_error(status, &body));
                }
                let tmp = cache.join("kilat-sync.json.tmp");
                std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &local).map_err(|e| e.to_string())?;
                state.remote_md5 = md5;
                state.local_sha = sha_of(&local);
            }
        }
        None => state.file_id.clear(),
    }
    state.save(cache)
}

/// After a sync: upload the cache file if the merge changed it.
/// Returns whether something was uploaded.
pub fn push(cache: &Path, keychain: &dyn CredentialStore) -> Result<bool, String> {
    let local = cache.join(SYNC_FILE);
    let mut state = State::load(cache);
    let sha = sha_of(&local);
    if sha.is_empty() || (sha == state.local_sha && !state.file_id.is_empty()) {
        return Ok(false);
    }
    let token = access_token(keychain)?;
    ensure_folder(&token, &mut state)?;
    let bytes = std::fs::read(&local).map_err(|e| e.to_string())?;
    let agent = api_agent()?;
    let auth = format!("Bearer {token}");
    let (status, body) = if state.file_id.is_empty() {
        let boundary = format!("kilat{}", oauth::random_state());
        let meta = serde_json::json!({
            "name": SYNC_FILE,
            "parents": [state.folder_id],
            "mimeType": "application/json",
        });
        let mut payload = Vec::new();
        payload.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n\
                 --{boundary}\r\nContent-Type: application/json\r\n\r\n"
            )
            .as_bytes(),
        );
        payload.extend_from_slice(&bytes);
        payload.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
        finish(
            agent
                .post(&format!(
                    "{UPLOAD}/files?uploadType=multipart&fields=id,md5Checksum"
                ))
                .header("Authorization", &auth)
                .header(
                    "Content-Type",
                    &format!("multipart/related; boundary={boundary}"),
                )
                .send(&payload[..]),
        )?
    } else {
        finish(
            agent
                .patch(&format!(
                    "{UPLOAD}/files/{}?uploadType=media&fields=id,md5Checksum",
                    state.file_id
                ))
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(&bytes[..]),
        )?
    };
    if status >= 300 {
        return Err(api_error(status, &body));
    }
    let json: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    state.file_id = json["id"].as_str().unwrap_or(&state.file_id).to_string();
    state.remote_md5 = json["md5Checksum"].as_str().unwrap_or_default().to_string();
    state.local_sha = sha;
    state.save(cache)?;
    Ok(true)
}

/// Where the cache folder lives.
pub fn cache_dir() -> PathBuf {
    ora_core::store::config_dir().join("drive-cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_are_readable() {
        assert_eq!(api_error(401, "{}"), SIGN_IN_AGAIN);
        assert!(api_error(403, "storageQuotaExceeded").contains("full"));
        assert!(api_error(500, r#"{"error":{"message":"boom"}}"#).contains("boom"));
    }

    #[test]
    fn state_round_trips_and_missing_file_is_empty() {
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("kilat-gd-{}", oauth::random_state()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(State::load(&dir).file_id, "");
        let s = State {
            folder_id: "f".into(),
            file_id: "i".into(),
            remote_md5: "m".into(),
            local_sha: "s".into(),
        };
        s.save(&dir).unwrap();
        let back = State::load(&dir);
        assert_eq!(
            (back.folder_id.as_str(), back.remote_md5.as_str()),
            ("f", "m")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
