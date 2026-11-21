//! Opt-in timing log shared by the driver and the GUI.
//!
//! Switched on with the environment variable `KILAT_TIMING=1`, or by an
//! empty file named `timing-on` in the config directory (handy for a macOS
//! `.app` started from Finder). Lines go to `timing.log` in the config
//! directory and carry only counts and durations: no SQL text, no cell
//! contents, no object names.

/// True when timing output is switched on.
pub fn enabled() -> bool {
    std::env::var_os("KILAT_TIMING").is_some_and(|v| !v.is_empty() && v != "0")
        || crate::store::config_dir().join("timing-on").exists()
}

/// Append one line (prefixed with the Unix time) to `timing.log`.
pub fn write_line(line: &str) {
    use std::io::Write as _;
    let dir = crate::store::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("timing.log"))
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "{secs} {line}");
    }
}
