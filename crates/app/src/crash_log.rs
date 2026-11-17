//! Panic log. A release app started from Finder/Dock has no terminal, so a
//! Rust panic used to vanish: macOS only showed "quit unexpectedly" (most
//! panics end in an abort there because they unwind through AppKit
//! callbacks). The hook below writes every panic, with a backtrace, to
//! `crash.log` in the config dir before the process dies, and the next
//! launch reports it in Messages.
//!
//! Crashes that are not Rust panics (a segfault inside a system framework)
//! never reach this hook; those only show up in the OS crash report.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

/// Every panic, appended. Trimmed to the newest part at startup.
const LOG_FILE: &str = "crash.log";
/// The newest panic, until the next launch has reported it.
const PENDING_FILE: &str = "crash-pending.txt";
const MAX_LOG_BYTES: u64 = 256 * 1024;

pub fn log_path() -> PathBuf {
    ora_core::store::config_dir().join(LOG_FILE)
}

fn pending_path() -> PathBuf {
    ora_core::store::config_dir().join(PENDING_FILE)
}

/// Install the hook. The default hook still runs first, so a launch from
/// Terminal keeps printing the panic to stderr as before.
pub fn install() {
    trim_log();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let report = format_report(info);
        // Best effort: a failing write must not panic inside the hook.
        let dir = ora_core::store::config_dir();
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(LOG_FILE))
        {
            let _ = file.write_all(report.as_bytes());
        }
        let _ = fs::write(dir.join(PENDING_FILE), first_line(&report));
    }));
}

fn format_report(info: &std::panic::PanicHookInfo<'_>) -> String {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_string());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "(unknown)".to_string());
    let thread = std::thread::current();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "=== panic at unix {} | {} {} | {} {} | thread '{}'",
        ora_core::store::now_unix_secs(),
        app_name!(),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        thread.name().unwrap_or("<unnamed>"),
    );
    let _ = writeln!(out, "message: {message}");
    let _ = writeln!(out, "location: {location}");
    let _ = writeln!(
        out,
        "backtrace:\n{}",
        std::backtrace::Backtrace::force_capture()
    );
    out.push('\n');
    out
}

/// "message (location)" for the one-line note shown at the next launch.
fn first_line(report: &str) -> String {
    let field = |key: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    format!("{} ({})", field("message:"), field("location:"))
}

/// Keep only the newest part of the log so it never grows without bound.
fn trim_log() {
    let path = log_path();
    let Ok(meta) = fs::metadata(&path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    if let Ok(bytes) = fs::read(&path) {
        let tail = &bytes[bytes.len() - MAX_LOG_BYTES as usize / 2..];
        let _ = fs::write(&path, tail);
    }
}

/// The panic that ended the previous run, if it has not been reported
/// yet. Reading it clears it, so it is shown once.
pub fn take_previous() -> Option<String> {
    let path = pending_path();
    let text = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_picks_message_and_location() {
        let report = "=== panic\nmessage: boom\nlocation: src/x.rs:1:2\nbacktrace:\n...";
        assert_eq!(first_line(report), "boom (src/x.rs:1:2)");
    }
}
