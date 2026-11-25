//! The parts of OAuth 2.0 for installed apps (RFC 8252) that need no
//! network: PKCE (RFC 7636), the authorization URL, form bodies, and
//! reading the redirect that arrives on the loopback address. The network
//! and the loopback listener live in the app crate.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::aead::{rand_core::RngCore, OsRng};
use sha2::{Digest, Sha256};

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// A PKCE code verifier and its S256 challenge.
pub fn pkce() -> (String, String) {
    let verifier = random_token(48);
    let challenge = pkce_challenge(&verifier);
    (verifier, challenge)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// An unguessable value that ties the redirect to the sign-in we started.
pub fn random_state() -> String {
    random_token(24)
}

/// `application/x-www-form-urlencoded` body.
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

/// The page the browser is sent to. `access_type=offline` and
/// `prompt=consent` make Google return a refresh token every time.
pub fn auth_url(
    endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    scope: &str,
) -> String {
    format!(
        "{endpoint}?{}",
        form_encode(&[
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", scope),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ])
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum Callback {
    /// The authorization code.
    Code(String),
    /// The user said no, or Google refused.
    Denied(String),
    /// Not our redirect (favicon, a stray request, a wrong state): keep
    /// waiting.
    Ignore,
}

/// Read the first line of the browser's request to the loopback address,
/// e.g. `GET /?code=4/0A&state=xyz HTTP/1.1`.
pub fn parse_callback(request_line: &str, expected_state: &str) -> Callback {
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        return Callback::Ignore;
    }
    let Some(target) = parts.next() else {
        return Callback::Ignore;
    };
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Ignore;
    };
    let mut code = None;
    let mut error = None;
    let mut state = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Callback::Ignore;
    }
    match (code, error) {
        (Some(code), _) if !code.is_empty() => Callback::Code(code),
        (_, Some(error)) => Callback::Denied(error),
        _ => Callback::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_example() {
        // RFC 7636 appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let (v, c) = pkce();
        assert!(v.len() >= 43 && v.len() <= 128);
        assert_eq!(pkce_challenge(&v), c);
        assert_ne!(random_state(), random_state());
    }

    #[test]
    fn auth_url_carries_pkce_and_offline_access() {
        let url = auth_url(
            "https://accounts.example/auth",
            "id 1",
            "http://127.0.0.1:5555",
            "st",
            "ch",
            "https://www.googleapis.com/auth/drive.file",
        );
        assert!(url.starts_with("https://accounts.example/auth?client_id=id+1&"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5555"));
        assert!(url.contains("code_challenge=ch&code_challenge_method=S256"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file"));
    }

    #[test]
    fn callback_needs_the_right_state() {
        assert_eq!(
            parse_callback("GET /?state=s&code=4%2F0AbC HTTP/1.1", "s"),
            Callback::Code("4/0AbC".into())
        );
        assert_eq!(
            parse_callback("GET /?state=s&error=access_denied HTTP/1.1", "s"),
            Callback::Denied("access_denied".into())
        );
        assert_eq!(
            parse_callback("GET /?state=other&code=x HTTP/1.1", "s"),
            Callback::Ignore
        );
        assert_eq!(
            parse_callback("GET /favicon.ico HTTP/1.1", "s"),
            Callback::Ignore
        );
        assert_eq!(
            parse_callback("POST /?state=s&code=x HTTP/1.1", "s"),
            Callback::Ignore
        );
        assert_eq!(parse_callback("garbage", "s"), Callback::Ignore);
    }

    #[test]
    fn form_bodies_are_escaped() {
        assert_eq!(form_encode(&[("a", "b c"), ("d", "e&f")]), "a=b+c&d=e%26f");
    }
}
