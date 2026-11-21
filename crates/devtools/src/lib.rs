//! Backends of Kilat's Developer Tools: a Redis browser client and a
//! MailHog API client. Tools are not database connections: they have
//! their own saved instances and their own views in the app.
//!
//! Nothing here depends on GPUI; every call blocks and is meant to run on a
//! worker thread.

pub mod console;
pub mod mailhog;
pub mod mime;
pub mod redis_tool;
pub mod server;

/// rustls finds both ring and aws-lc-rs in this build and will not pick
/// one, so install ring as the process default before the first TLS
/// handshake. Harmless when one is installed already.
pub fn ensure_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}
