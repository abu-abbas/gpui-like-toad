//! The Dry-run extension: runs an UPDATE or DELETE in a transaction that is
//! never committed and reports what would change (see `docs/dry-run.md`).
//! Kilat starts it and talks to it over stdin and stdout. Nothing but
//! protocol messages may be written to stdout; anything for people goes to
//! stderr.

mod engine;
mod service;
mod sql;
mod view;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut service = service::Service::new(Box::new(ora_driver::ThinClient::new()), service::HOLD);
    let result = ora_core::extension_serve::serve_with(
        &mut |method, params, _emit| service.handle(method, params),
        "dryrun",
        env!("CARGO_PKG_VERSION"),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kilat-ext-dryrun: {e}");
            ExitCode::FAILURE
        }
    }
}
