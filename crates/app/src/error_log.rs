//! Error log. Every error shown in the Info / Messages panel is also
//! appended, with a timestamp, to `errors.log` in the config dir, so it
//! survives a restart and can be copied from Settings > Logs for a bug
//! report. Panics go to the separate `crash.log` (see `crash_log`).

use std::fs::{self, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;

const LOG_FILE: &str = "errors.log";
/// The log is cut back to its newest half once it passes this size.
const MAX_LOG_BYTES: u64 = 256 * 1024;
/// How much of the end of each log Settings shows and copies.
const VIEW_BYTES: usize = 32 * 1024;

pub fn log_path() -> PathBuf {
    ora_core::store::config_dir().join(LOG_FILE)
}

/// Recognize driver errors and failure messages for logging and danger styling
/// in the Info panel.
pub fn is_error_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    message.contains("ORA-")
        || message.contains("DPY-")
        || message.starts_with("SQLite:")
        || lower.contains("error")
        || lower.contains("failed")
        || lower.starts_with("demo:")
}

/// Whether a message goes to `errors.log`. A `private` message (it quotes
/// data the user typed, such as the values of a cell edit) is shown in the
/// Info panel only, whatever words it contains.
pub fn should_log(message: &str, private: bool) -> bool {
    !private && is_error_message(message)
}

/// Append one error line. Best effort: logging must never break the app.
pub fn append(message: &str) {
    append_in(&ora_core::store::config_dir(), message);
}

fn append_in(dir: &std::path::Path, message: &str) {
    let _ = fs::create_dir_all(dir);
    let path = dir.join(LOG_FILE);
    // A proxy URL quoted by a network error must not leave its password here.
    let message = ora_core::redact::mask_url_credentials(message);
    trim(&path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        // One entry per line: continuation lines are indented.
        let text = message.trim_end().replace('\n', "\n    ");
        let _ = writeln!(file, "[{stamp}] {text}");
    }
}

fn trim(path: &std::path::Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    if let Ok(bytes) = fs::read(path) {
        let tail = &bytes[bytes.len() - MAX_LOG_BYTES as usize / 2..];
        // Start at a line boundary so the first entry is whole.
        let start = tail.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
        let _ = fs::write(path, &tail[start..]);
    }
}

pub fn clear(path: &std::path::Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// The newest part of a log file, or empty when there is none.
pub fn tail_of(path: &std::path::Path) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let Ok(end) = file.seek(SeekFrom::End(0)) else {
        return String::new();
    };
    // Include the preceding byte so a cut at a newline keeps the whole next entry.
    let start = end.saturating_sub(VIEW_BYTES as u64 + 1);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file
        .take(VIEW_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return String::new();
    }
    let skip = if end > VIEW_BYTES as u64 {
        bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| i + 1)
    } else {
        0
    };
    crate::diagnostics::redact(String::from_utf8_lossy(&bytes[skip..]).trim_end())
}

/// The newest errors, for the Settings view.
pub fn recent_errors() -> String {
    tail_of(&log_path())
}

/// Everything a bug report needs: app version, OS, then the newest
/// errors and panics.
pub fn report() -> String {
    let crashes = tail_of(&crate::crash_log::log_path());
    let errors = recent_errors();
    format!(
        "{} {} | {} {}\n\n== errors.log ==\n{}\n\n== crash.log ==\n{}\n",
        app_name!(),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if errors.is_empty() {
            "(empty)"
        } else {
            &errors
        },
        if crashes.is_empty() {
            "(empty)"
        } else {
            &crashes
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        // Test-only scratch dir, unique per process id and test name.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("ora-errlog-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn cell_values_never_reach_the_log_or_the_report() {
        // The value contains "failed" so the echo looks like an error.
        let marker = "MARKER-7731 failed";
        let echo = ora_core::edit::update_cell_sql(
            &ora_core::dialect::OracleDialect,
            "HR",
            "EMP",
            "NAME",
            "AAABBB",
            Some(marker),
        )
        .inlined();
        assert!(echo.contains(marker) && is_error_message(&echo));
        assert!(!should_log(&echo, true));
        let dir = temp("marker");
        for (text, private) in [
            (echo.as_str(), true),
            ("Failed to apply change #1 (ORA-00001)", false),
        ] {
            if should_log(text, private) {
                append_in(&dir, text);
            }
        }
        let log = tail_of(&dir.join(LOG_FILE));
        assert!(log.contains("Failed to apply change #1"));
        assert!(!log.contains("MARKER-7731"), "{log}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn proxy_passwords_are_masked_in_the_log() {
        let dir = temp("proxy");
        append_in(
            &dir,
            "Invalid proxy (http://bob:s3cret@proxy:8080): bad port",
        );
        let log = tail_of(&dir.join(LOG_FILE));
        assert!(fs::read_to_string(dir.join(LOG_FILE))
            .unwrap()
            .contains("http://bob:***@proxy:8080"));
        assert!(log.contains("http://[redacted]@proxy:8080"), "{log}");
        assert!(!log.contains("s3cret"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_are_recognized_by_driver_prefix_or_wording() {
        assert!(is_error_message("ORA-00942: table or view does not exist"));
        assert!(is_error_message("SQLite: no such table: x"));
        assert!(is_error_message("Failed to save connection"));
        assert!(!is_error_message("Connected: main @ toko.sqlite"));
        assert!(!is_error_message("40 rows in 0 ms"));
    }
}
