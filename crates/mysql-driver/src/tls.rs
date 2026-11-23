//! TLS for the MySQL connection: used when the server offers it, without
//! checking its certificate (like `sslmode=prefer`). Servers with
//! self-signed certificates are the norm for MySQL.

use mysql::SslOpts;

pub(crate) fn options() -> SslOpts {
    // The TLS library needs a crypto provider chosen once per process;
    // without it the first handshake panics. Another one may already be
    // installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
    SslOpts::default()
        .with_danger_accept_invalid_certs(true)
        .with_danger_skip_domain_validation(true)
}

/// A failure that means "this server does not do TLS" and so is worth a
/// second try without it.
pub(crate) fn is_tls_unavailable(err: &mysql::Error) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("ssl") || text.contains("tls") || text.contains("secure connection")
}
