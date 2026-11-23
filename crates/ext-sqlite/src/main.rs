//! The SQLite extension: `sqlite-driver` as a separate program. Kilat
//! starts it and talks to it over stdin and stdout (see
//! `docs/extension-protocol.md`). Nothing but protocol messages may be
//! written to stdout; anything for people goes to stderr.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut client = sqlite_driver::SqliteClient::new();
    let result = ora_core::extension_serve::serve(
        &mut client,
        "sqlite",
        env!("CARGO_PKG_VERSION"),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kilat-ext-sqlite: {e}");
            ExitCode::FAILURE
        }
    }
}
