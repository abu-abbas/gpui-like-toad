//! TLS for the MySQL connection: used when the server offers it, without
//! checking its certificate (like `sslmode=prefer`). Servers with
//! self-signed certificates are the norm for MySQL.

use mysql::SslOpts;

pub(crate) fn options() -> SslOpts {
    SslOpts::default()
        .with_danger_accept_invalid_certs(true)
        .with_danger_skip_domain_validation(true)
}

/// A failure that means "this server does not do TLS" and so is worth a
/// second try without it.
pub(crate) fn is_tls_unavailable(err: &mysql::Error) -> bool {
    matches!(
        err,
        mysql::Error::DriverError(mysql::DriverError::TlsNotSupported)
    )
}
