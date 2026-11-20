//! Plain-text diagnostics for the Settings preview and clipboard report.
use chrono::{DateTime, Local, NaiveDateTime};
use regex::Regex;
use std::sync::LazyLock;

pub fn redact(text: &str) -> String {
    // Keep credentials out of previews even when reading logs from older installs.
    static USERINFO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)([a-z][a-z0-9+.\-]*://)[^/\s"'<>]+@"#).unwrap());
    static HOME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)(/Users/|/home/|[A-Z]:[\\/]Users[\\/])[^\\/\r\n:"']+"#).unwrap()
    });
    // A value can contain spaces and quotes. Drop the remainder of that line
    // rather than expose a suffix by guessing where the value ends.
    static SECRET: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?im)(\b(?:authorization|proxy-authorization|password|passwd|pwd|(?:access[_-]?|refresh[_-]?)?token|(?:x[_-]?)?api[_-]?key|client[_-]?secret|cookie|set-cookie)["']?\s*[:=][ \t]*|\b(?:bearer|basic)[ \t]+)[^\r\n]+"#).unwrap()
    });
    let text = USERINFO.replace_all(text, "${1}[redacted]@");
    let text = HOME.replace_all(&text, "${1}[redacted]");
    SECRET.replace_all(&text, "${1}[redacted]").into_owned()
}

pub struct Entry {
    pub timestamp: Option<NaiveDateTime>,
    pub message: String,
}

impl Entry {
    pub fn summary(&self) -> &str {
        self.message
            .lines()
            .find_map(|s| s.strip_prefix("message: "))
            .or_else(|| self.message.lines().next())
            .unwrap_or("Log entry")
    }
}

pub fn entries(text: &str, crashes: bool) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for line in text.lines() {
        let header = if crashes {
            line.strip_prefix("=== panic at unix ").and_then(|rest| {
                let (secs, _) = rest.split_once(" | ")?;
                let stamp = DateTime::from_timestamp(secs.parse().ok()?, 0)?;
                Some((stamp.with_timezone(&Local).naive_local(), line))
            })
        } else {
            line.strip_prefix('[').and_then(|rest| {
                let (stamp, body) = rest.split_once("] ")?;
                Some((
                    NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").ok()?,
                    body,
                ))
            })
        };
        if let Some((time, body)) = header {
            entries.push(Entry {
                timestamp: Some(time),
                message: body.into(),
            });
        } else if let Some(entry) = entries.last_mut() {
            entry.message.push('\n');
            entry.message.push_str(if crashes {
                line
            } else {
                line.strip_prefix("    ").unwrap_or(line)
            });
        } else if !line.is_empty() {
            // A tail or an older format may start without a timestamp.
            entries.push(Entry {
                timestamp: None,
                message: line.into(),
            });
        }
    }
    entries.reverse();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_and_home_names_while_preserving_diagnostic_locations() {
        let cleaned = redact("https://me:pw@server/a\nAuthorization: Bearer abc\n{\"api_key\": \"secret\"}\npassword=my long secret\n/Users/My Name/src.rs:5:12\nC:\\Users\\someone\\src.rs:3:1");
        for value in ["me:pw", "abc", "secret", "My Name", "someone"] {
            assert!(!cleaned.contains(value));
        }
        assert!(cleaned.contains("/src.rs:5:12"));
        assert!(cleaned.contains("\\src.rs:3:1"));
    }

    #[test]
    fn preserves_multiline_errors_and_partial_first_entry() {
        let rows = entries(
            "old detail\n[2026-11-19 20:12:55] First\n    A\n    B\n[2026-11-20 06:38:50] Second",
            false,
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].message, "First\nA\nB");
        assert!(rows[0].timestamp > rows[1].timestamp);
        assert!(rows[2].timestamp.is_none());
    }

    #[test]
    fn keeps_separate_panics_at_the_same_timestamp() {
        let log = "=== panic at unix 1794988464 | old version\nmessage: first\nbacktrace:\n   0: frame\n=== panic at unix 1794988464 | old version\nmessage: second";
        let rows = entries(log, true);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].summary(), "second");
        assert_eq!(rows[0].timestamp, rows[1].timestamp);
        assert!(rows[1].message.contains("   0: frame"));
    }
}
