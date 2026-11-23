//! The PostgreSQL extension: `postgres-driver` as a separate program. Kilat
//! starts it and talks to it over stdin and stdout (see
//! `docs/extension-protocol.md`). Nothing but protocol messages may be
//! written to stdout; anything for people goes to stderr.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut client = postgres_driver::PostgresClient::new();
    let result = ora_core::extension_serve::serve(
        &mut client,
        "postgres",
        env!("CARGO_PKG_VERSION"),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kilat-ext-postgres: {e}");
            ExitCode::FAILURE
        }
    }
}
