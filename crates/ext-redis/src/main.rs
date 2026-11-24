//! The Redis extension: the Redis browser of `devtools` as a separate
//! program. Kilat starts it and talks to it over stdin and stdout (see
//! `docs/extension-protocol.md` and `docs/extension-ui.md`). Nothing but
//! protocol messages may be written to stdout; anything for people goes to
//! stderr.

mod service;
mod view;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut service = service::Service::default();
    let result = ora_core::extension_serve::serve_with(
        &mut |method, params, _emit| service.handle(method, params),
        "redis",
        env!("CARGO_PKG_VERSION"),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kilat-ext-redis: {e}");
            ExitCode::FAILURE
        }
    }
}
