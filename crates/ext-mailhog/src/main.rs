//! The MailHog extension: the MailHog API client of `devtools` as a
//! separate program. Kilat starts it and talks to it over stdin and stdout
//! (see `docs/extension-protocol.md`). Nothing but protocol messages may be
//! written to stdout; anything for people goes to stderr.

mod inbox_ui;
mod service;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut service = service::Service::default();
    let result = ora_core::extension_serve::serve_with(
        &mut |method, params, emit| service.handle(method, params, emit),
        "mailhog",
        env!("CARGO_PKG_VERSION"),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kilat-ext-mailhog: {e}");
            ExitCode::FAILURE
        }
    }
}
