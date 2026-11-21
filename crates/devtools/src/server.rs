//! Local servers started from Kilat (`redis-server`, `MailHog`): they live
//! as child processes, their output goes to a log file with size-based
//! rotation, and they are stopped when Kilat quits. Pids are recorded so a
//! server left behind by a crash can be stopped on the next start.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A log file grows to this size, then moves to `<name>.log.1`.
pub const LOG_LIMIT: u64 = 5 * 1024 * 1024;

/// Where servers started by Kilat write their logs:
/// `<config>/server-logs/developer-tools/`.
pub fn log_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("server-logs").join("developer-tools")
}

/// The log file of a tool, named after it (characters a file name cannot
/// hold become `_`).
pub fn log_path(config_dir: &Path, tool_name: &str) -> PathBuf {
    log_dir(config_dir).join(format!("{}.log", file_name(tool_name)))
}

/// The folder a tool's server runs in, so files it writes by relative
/// path (Redis' dump.rdb) stay apart per tool and survive restarts.
pub fn data_dir(config_dir: &Path, tool_name: &str) -> PathBuf {
    config_dir
        .join("server-data")
        .join("developer-tools")
        .join(file_name(tool_name))
}

fn file_name(tool_name: &str) -> String {
    let mut name: String = tool_name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    if name.is_empty() || name.starts_with('.') {
        name.insert(0, '_');
    }
    name
}

/// The command a tool starts by default.
pub fn default_command(redis: bool, port: u16) -> String {
    if redis {
        format!("redis-server --port {port}")
    } else if port == 8025 {
        "MailHog".into()
    } else {
        format!("MailHog -ui-bind-addr 127.0.0.1:{port} -api-bind-addr 127.0.0.1:{port}")
    }
}

/// Whether `host` is this machine: only then can Kilat start its server.
pub fn is_local(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h == "::1" || h.starts_with("127.")
}

/// Folders searched for a program besides `PATH`: apps opened from the
/// macOS Dock do not get the shell's `PATH`, so Homebrew is not on it.
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/opt/local/bin",
        "/snap/bin",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join("go").join("bin"));
        dirs.push(home.join(".local").join("bin"));
    }
    dirs
}

/// The full path of `program`: as given when it has a path, else the
/// first match on `PATH` or in the usual install folders.
pub fn resolve_program(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let names: Vec<String> = if cfg!(windows) && p.extension().is_none() {
        vec![format!("{program}.exe"), program.to_string()]
    } else {
        vec![program.to_string()]
    };
    let path_dirs = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(extra_dirs())
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|c| c.is_file())
}

/// Appends lines to a log file and rotates it past [`LOG_LIMIT`].
struct LogWriter {
    path: PathBuf,
    file: File,
    size: u64,
}

impl LogWriter {
    fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut w = LogWriter {
            path: path.to_path_buf(),
            file,
            size,
        };
        if w.size >= LOG_LIMIT {
            w.rotate()?;
        }
        Ok(w)
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        let old = rotated(&self.path);
        let _ = fs::remove_file(&old);
        fs::rename(&self.path, &old)?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.size = 0;
        Ok(())
    }

    fn line(&mut self, text: &str) {
        let line = format!("{text}\n");
        if self.size + line.len() as u64 > LOG_LIMIT && self.size > 0 {
            let _ = self.rotate();
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.size += line.len() as u64;
        }
        let _ = self.file.flush();
    }
}

fn rotated(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// A server started by Kilat.
pub struct ServerProcess {
    child: Child,
    log: Arc<Mutex<LogWriter>>,
    pub log_path: PathBuf,
    pub command: String,
}

impl ServerProcess {
    /// Start `command` (split like a shell line, no shell involved) in
    /// `work_dir`, with its output in `log_path`.
    pub fn start(command: &str, log_path: &Path, work_dir: &Path) -> Result<Self, String> {
        let args = crate::console::split_args(command)?;
        let args: Vec<String> = args
            .into_iter()
            .map(|a| String::from_utf8_lossy(&a).into_owned())
            .collect();
        let Some(program) = args.first() else {
            return Err("Enter the command that starts the server.".into());
        };
        let Some(exe) = resolve_program(program) else {
            return Err(format!(
                "Could not find {program}. Install it or enter its full path in the command, e.g. /opt/homebrew/bin/{program}."
            ));
        };
        let log = LogWriter::open(log_path).map_err(|e| format!("Cannot write the log: {e}"))?;
        let log = Arc::new(Mutex::new(log));
        let mut cmd = Command::new(&exe);
        cmd.args(&args[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Relative files (dump.rdb) land in the tool's own folder, not in
        // whatever folder Kilat was started from.
        if std::fs::create_dir_all(work_dir).is_ok() {
            cmd.current_dir(work_dir);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Could not start {}: {e}", exe.display()))?;
        if let Ok(mut l) = log.lock() {
            l.line(&format!(
                "--- {} Kilat started: {command} (pid {})",
                stamp(),
                child.id()
            ));
        }
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let log = log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(mut l) = log.lock() {
                        l.line(&line);
                    }
                }
            });
        }
        Ok(ServerProcess {
            child,
            log,
            log_path: log_path.to_path_buf(),
            command: command.to_string(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// `None` while running, else how it ended.
    pub fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(status.to_string()),
            Err(e) => Some(e.to_string()),
        }
    }

    /// Send the stop signal without waiting (quit path).
    pub fn request_stop(&mut self) {
        if self.exited().is_none() {
            terminate(self.child.id());
        }
    }

    /// Ask the server to stop (SIGTERM; Redis saves first when persistence
    /// is on), then kill it after `grace`. Blocks up to `grace`.
    pub fn stop(&mut self, grace: Duration) {
        if self.exited().is_some() {
            return;
        }
        terminate(self.child.id());
        let start = Instant::now();
        while start.elapsed() < grace {
            if self.exited().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.exited().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Ok(mut l) = self.log.lock() {
            l.line(&format!("--- {} Kilat stopped the server", stamp()));
        }
    }
}

#[cfg(unix)]
fn terminate(pid: u32) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(unix))]
fn terminate(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Wait until `host:port` accepts connections, the server exits, or
/// `timeout` passes.
pub fn wait_port(
    host: &str,
    port: u16,
    timeout: Duration,
    mut exited: impl FnMut() -> Option<String>,
) -> Result<(), String> {
    let start = Instant::now();
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    loop {
        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map(|a| a.collect())
            .unwrap_or_default();
        if addrs
            .iter()
            .any(|a| TcpStream::connect_timeout(a, Duration::from_millis(300)).is_ok())
        {
            return Ok(());
        }
        if let Some(how) = exited() {
            return Err(format!(
                "The server stopped right away ({how}). See its Server log."
            ));
        }
        if start.elapsed() > timeout {
            return Err(format!(
                "The server started but {host}:{port} does not answer. Check the port in the command and the Server log."
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------
// Pids of running servers, to clean up after a crash
// ---------------------------------------------------------------------

fn pid_file(config_dir: &Path) -> PathBuf {
    log_dir(config_dir).join("running.pids")
}

fn read_pids(config_dir: &Path) -> Vec<(u32, String)> {
    fs::read_to_string(pid_file(config_dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, program) = l.split_once(' ')?;
            Some((pid.parse().ok()?, program.to_string()))
        })
        .collect()
}

fn write_pids(config_dir: &Path, pids: &[(u32, String)]) {
    let text: String = pids.iter().map(|(p, n)| format!("{p} {n}\n")).collect();
    let _ = fs::create_dir_all(log_dir(config_dir));
    let _ = fs::write(pid_file(config_dir), text);
}

/// Remember a running server (program = file name of the executable).
pub fn record_pid(config_dir: &Path, pid: u32, command: &str) {
    let program = program_name(command);
    let mut pids = read_pids(config_dir);
    pids.retain(|(p, _)| *p != pid);
    pids.push((pid, program));
    write_pids(config_dir, &pids);
}

pub fn forget_pid(config_dir: &Path, pid: u32) {
    let mut pids = read_pids(config_dir);
    pids.retain(|(p, _)| *p != pid);
    write_pids(config_dir, &pids);
}

fn program_name(command: &str) -> String {
    let first = crate::console::split_args(command)
        .ok()
        .and_then(|a| a.into_iter().next())
        .map(|a| String::from_utf8_lossy(&a).into_owned())
        .unwrap_or_default();
    Path::new(&first)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or(first)
}

/// Stop servers a previous Kilat started and never stopped (it crashed or
/// was killed). A recorded pid is stopped only while it still runs the
/// same program, so a reused pid is never touched. Returns how many.
pub fn stop_leftovers(config_dir: &Path) -> usize {
    let pids = read_pids(config_dir);
    let mut stopped = 0;
    for (pid, program) in &pids {
        if process_name(*pid).is_some_and(|n| names_match(&n, program)) {
            terminate(*pid);
            stopped += 1;
        }
    }
    if !pids.is_empty() {
        write_pids(config_dir, &[]);
    }
    stopped
}

fn names_match(running: &str, program: &str) -> bool {
    let a = running.trim().to_ascii_lowercase();
    let b = program.trim().to_ascii_lowercase();
    // `ps` may cut long names (15 chars on Linux).
    !a.is_empty() && !b.is_empty() && (a.starts_with(&b) || b.starts_with(&a))
}

#[cfg(unix)]
fn process_name(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then(|| {
        Path::new(&name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(name)
    })
}

#[cfg(not(unix))]
fn process_name(_pid: u32) -> Option<String> {
    None
}

// ---------------------------------------------------------------------
// Reading and managing logs
// ---------------------------------------------------------------------

/// The last `max` bytes of a log, starting at a line.
pub fn tail(path: &Path, max: u64) -> String {
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    // Read one byte early to know whether `start` is a line start.
    let from = start.saturating_sub(1);
    if f.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start == 0 {
        return text;
    }
    match text.find('\n') {
        Some(i) => text[i + 1..].to_string(),
        None => String::new(),
    }
}

/// A log file in the Developer Tools log folder.
#[derive(Clone, Debug)]
pub struct LogFile {
    pub name: String,
    pub path: PathBuf,
    /// Bytes, the rotated `.1` file included.
    pub size: u64,
}

pub fn list_logs(config_dir: &Path) -> Vec<LogFile> {
    let dir = log_dir(config_dir);
    let mut out: Vec<LogFile> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path
                .file_name()?
                .to_str()?
                .strip_suffix(".log")?
                .to_string();
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                + fs::metadata(rotated(&path)).map(|m| m.len()).unwrap_or(0);
            Some(LogFile { name, path, size })
        })
        .collect();
    out.sort_by_key(|l| l.name.to_lowercase());
    out
}

/// Empty a log and drop its rotated part. A running server keeps writing
/// to the (now empty) file.
pub fn clear_log(path: &Path) -> std::io::Result<()> {
    let _ = fs::remove_file(rotated(path));
    if path.exists() {
        OpenOptions::new().write(true).truncate(true).open(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kilat-server-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn names_and_paths() {
        let dir = Path::new("/cfg");
        assert_eq!(
            log_path(dir, "Local Redis"),
            Path::new("/cfg/server-logs/developer-tools/Local Redis.log")
        );
        assert_eq!(
            log_path(dir, "a/b:c"),
            Path::new("/cfg/server-logs/developer-tools/a_b_c.log")
        );
        assert!(is_local("localhost") && is_local("127.0.0.1") && is_local("[::1]"));
        assert!(!is_local("10.0.0.5") && !is_local("redis.example.com"));
        assert_eq!(default_command(true, 6390), "redis-server --port 6390");
        assert_eq!(default_command(false, 8025), "MailHog");
        assert!(names_match("redis-server", "redis-server"));
        assert!(names_match("MailHog", "mailhog"));
        assert!(!names_match("bash", "redis-server"));
        assert_eq!(
            program_name("/opt/homebrew/bin/redis-server --port 1"),
            "redis-server"
        );
    }

    #[test]
    fn log_rotates_and_clears() {
        let dir = temp();
        let path = dir.join("t.log");
        let mut w = LogWriter::open(&path).unwrap();
        w.size = LOG_LIMIT - 3;
        w.line("hello");
        assert!(rotated(&path).exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
        w.line("second");
        assert_eq!(tail(&path, 7), "second\n");
        clear_log(&path).unwrap();
        assert!(!rotated(&path).exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn runs_logs_and_stops_a_process() {
        let dir = temp();
        let path = dir.join("sleep.log");
        let mut p = ServerProcess::start("sh -c 'echo ready; sleep 30'", &path, &dir).unwrap();
        record_pid(&dir, p.pid(), &p.command);
        std::thread::sleep(Duration::from_millis(300));
        assert!(p.exited().is_none());
        p.stop(Duration::from_secs(3));
        assert!(p.exited().is_some());
        forget_pid(&dir, p.pid());
        let log = fs::read_to_string(&path).unwrap();
        assert!(log.contains("ready"), "{log}");
        assert!(log.contains("Kilat stopped"), "{log}");
        assert!(ServerProcess::start("no-such-program-kilat", &path, &dir).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
