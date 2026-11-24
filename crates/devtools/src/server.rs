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

fn mailhog_log_dir(config_dir: &Path) -> PathBuf {
    log_dir(config_dir).join("mailhog")
}

/// Move this instance's old log directory, including its rotated log, before
/// opening a writer. If the destination already exists or the move fails, the
/// app can still list the legacy log independently.
pub fn prepare_mailhog_log(config_dir: &Path, id: &str) -> PathBuf {
    let id = file_name(id);
    let dir = mailhog_log_dir(config_dir);
    let instance = dir.join(&id);
    let legacy = config_dir.join("server-logs").join(&id);
    let old_log = legacy.join("mailhog.log");
    if !id.eq_ignore_ascii_case("developer-tools")
        && fs::symlink_metadata(&legacy).is_ok_and(|meta| meta.is_dir())
        && [&old_log, &rotated(&old_log)]
            .iter()
            .any(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()))
        && fs::symlink_metadata(&instance)
            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
        && fs::create_dir_all(&dir).is_ok()
    {
        let _ = fs::rename(legacy, &instance);
    }
    instance.join("mailhog.log")
}

/// The MailHog log of one instance, without moving an old one (see
/// [`prepare_mailhog_log`]).
pub fn mailhog_log_path(config_dir: &Path, id: &str) -> PathBuf {
    mailhog_log_dir(config_dir)
        .join(file_name(id))
        .join("mailhog.log")
}

fn redis_log_dir(config_dir: &Path) -> PathBuf {
    log_dir(config_dir).join("redis")
}

/// The Redis log of one instance: `server-logs/developer-tools/redis/<id>/redis.log`,
/// keyed by the tool's id like MailHog, so renaming the tool keeps it.
pub fn redis_log_path(config_dir: &Path, id: &str) -> PathBuf {
    redis_log_dir(config_dir)
        .join(file_name(id))
        .join("redis.log")
}

/// `server-data/developer-tools/`: every Developer Tools server keeps
/// its files under `<kind>/<tool id>/` here, next to the logs layout.
fn tools_data_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("server-data").join("developer-tools")
}

/// The folder a Redis instance runs in (its `dump.rdb`):
/// `server-data/developer-tools/redis/<id>`.
pub fn redis_data_dir(config_dir: &Path, id: &str) -> PathBuf {
    tools_data_dir(config_dir).join("redis").join(file_name(id))
}

/// Where a local MailHog keeps its mail (maildir storage), so the inbox
/// survives a server restart: `server-data/developer-tools/mailhog/<id>`.
pub fn mailhog_mail_dir(config_dir: &Path, id: &str) -> PathBuf {
    tools_data_dir(config_dir)
        .join("mailhog")
        .join(file_name(id))
}

/// Environment that makes MailHog store mail as files in `dir` instead of
/// memory. Flags typed in the command still win over these.
pub fn mailhog_storage_env(dir: &Path) -> Vec<(String, std::ffi::OsString)> {
    vec![
        ("MH_STORAGE".into(), "maildir".into()),
        ("MH_MAILDIR_PATH".into(), dir.as_os_str().to_owned()),
    ]
}

/// Log file and data folder of a Redis instance. Before 1.0.1 both were
/// named after the tool (so a rename started an empty database); the old
/// ones, found by the current name, are moved over once if the new place
/// does not exist yet.
pub fn prepare_redis(config_dir: &Path, id: &str, name: &str) -> (PathBuf, PathBuf) {
    let log = redis_log_path(config_dir, id);
    let data = redis_data_dir(config_dir, id);
    // A tool named like the new kind folders keeps its old folder.
    let reserved = ["redis", "mailhog"]
        .iter()
        .any(|kind| file_name(name).eq_ignore_ascii_case(kind));
    if !name.trim().is_empty() && !reserved {
        let old_data = data_dir(config_dir, name);
        if fs::symlink_metadata(&old_data).is_ok_and(|m| m.is_dir())
            && fs::symlink_metadata(&data).is_err()
        {
            if let Some(parent) = data.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::rename(&old_data, &data);
        }
        let old_log = log_path(config_dir, name);
        if fs::symlink_metadata(&old_log).is_ok_and(|m| m.is_file())
            && fs::symlink_metadata(&log).is_err()
        {
            if let Some(parent) = log.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::rename(&old_log, &log).is_ok() {
                let _ = fs::rename(rotated(&old_log), rotated(&log));
            }
        }
    }
    (log, data)
}

/// The folder a tool's server runs in, so files it writes by relative
/// path (Redis' dump.rdb) stay apart per tool and survive restarts.
pub fn data_dir(config_dir: &Path, tool_name: &str) -> PathBuf {
    config_dir
        .join("server-data")
        .join("developer-tools")
        .join(file_name(tool_name))
}

/// Per-instance PID journals for MailHog extension processes.
pub fn mailhog_data_dir(config: &Path, id: &str) -> PathBuf {
    config
        .join("server-data")
        .join("mailhog-extension")
        .join(file_name(id))
}

pub fn stop_mailhog_leftovers(config: &Path) {
    let root = config.join("server-data").join("mailhog-extension");
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            stop_leftovers(&entry.path());
        }
    }
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
        // A user can remove the log or its folder while the child is running.
        // Reopen it before appending to a file that is no longer reachable.
        match fs::metadata(&self.path) {
            Ok(meta) => self.size = meta.len(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Ok(writer) = Self::open(&self.path) else {
                    return;
                };
                *self = writer;
            }
            Err(_) => return,
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z");
        let line = format!("[{stamp}] {text}\n");
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
        Self::start_with_env(command, log_path, work_dir, &[])
    }

    /// Same, with extra environment variables for the server.
    pub fn start_with_env(
        command: &str,
        log_path: &Path,
        work_dir: &Path,
        env: &[(String, std::ffi::OsString)],
    ) -> Result<Self, String> {
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
            .envs(env.iter().map(|(k, v)| (k, v)))
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
                "--- Kilat started: {command} (pid {})",
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
            l.line("--- Kilat stopped the server");
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
    out.extend(instance_logs(
        &mailhog_log_dir(config_dir),
        "MailHog",
        "mailhog.log",
        false,
    ));
    out.extend(instance_logs(
        &redis_log_dir(config_dir),
        "Redis",
        "redis.log",
        false,
    ));
    out.extend(instance_logs(
        &config_dir.join("server-logs"),
        "MailHog",
        "mailhog.log",
        true,
    ));
    out.sort_by_key(|l| l.name.to_lowercase());
    out
}

fn instance_logs<'a>(
    dir: &Path,
    label: &'a str,
    file: &'a str,
    legacy: bool,
) -> impl Iterator<Item = LogFile> + 'a {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(move |entry| {
            let id = entry.file_name();
            if !entry.file_type().is_ok_and(|kind| kind.is_dir())
                || (legacy && id.eq_ignore_ascii_case("developer-tools"))
            {
                return None;
            }
            let path = entry.path().join(file);
            let meta = fs::metadata(&path).ok()?;
            if !meta.is_file() {
                return None;
            }
            let suffix = if legacy { " (legacy)" } else { "" };
            let name = format!("{label} ({}){suffix}", id.to_string_lossy());
            let size = meta.len() + fs::metadata(rotated(&path)).map(|m| m.len()).unwrap_or(0);
            Some(LogFile { name, path, size })
        })
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

    #[test]
    fn redis_moves_name_keyed_data_and_log_to_its_id() {
        let dir = temp();
        let cfg = dir.as_path();
        let old_data = data_dir(cfg, "local-redis");
        fs::create_dir_all(&old_data).unwrap();
        fs::write(old_data.join("dump.rdb"), b"x").unwrap();
        fs::create_dir_all(log_dir(cfg)).unwrap();
        fs::write(log_path(cfg, "local-redis"), b"log").unwrap();
        let (log, data) = prepare_redis(cfg, "t1", "local-redis");
        assert_eq!(data, cfg.join("server-data/developer-tools/redis/t1"));
        assert_eq!(log, log_dir(cfg).join("redis/t1/redis.log"));
        assert!(data.join("dump.rdb").is_file());
        assert!(log.is_file());
        assert!(!old_data.exists());
        // A rename later keeps the same folder and never moves another one in.
        fs::create_dir_all(data_dir(cfg, "Redis localhost")).unwrap();
        let (_, again) = prepare_redis(cfg, "t1", "Redis localhost");
        assert_eq!(again, data);
        assert!(data.join("dump.rdb").is_file());
        assert!(list_logs(cfg).iter().any(|l| l.name == "Redis (t1)"));
    }

    fn temp() -> PathBuf {
        // A counter, not a timestamp: tests run in parallel inside one
        // process and must never share (and delete) each other's folder.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let d = std::env::temp_dir().join(format!("kilat-server-{}-{n}", std::process::id()));
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
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(LOG_LIMIT - 3)
            .unwrap();
        w.line("hello");
        assert!(rotated(&path).exists());
        let text = fs::read_to_string(&path).unwrap();
        let (stamp, body) = text.strip_prefix('[').unwrap().split_once("] ").unwrap();
        let stamp = chrono::DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S %:z").unwrap();
        assert_eq!(stamp.offset(), stamp.with_timezone(&chrono::Local).offset());
        assert_eq!(body, "hello\n");
        w.line("second");
        assert!(tail(&path, 50).ends_with("] second\n"));
        clear_log(&path).unwrap();
        assert!(!rotated(&path).exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn logging_recreates_a_removed_log_folder() {
        let dir = temp();
        let logs = dir.join("logs");
        let path = logs.join("mailhog.log");
        let mut w = LogWriter::open(&path).unwrap();
        w.line("before removal");
        fs::remove_dir_all(&logs).unwrap();
        w.line("after removal");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("] after removal\n"), "{text}");
        assert!(!text.contains("before removal"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn moves_legacy_mailhog_logs_and_lists_them_with_redis() {
        let dir = temp();
        let legacy = dir.join("server-logs/tool-1/mailhog.log");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "current").unwrap();
        fs::write(rotated(&legacy), "older").unwrap();
        assert_eq!(list_logs(&dir)[0].path, legacy);

        let path = prepare_mailhog_log(&dir, "tool-1");
        assert_eq!(path, log_dir(&dir).join("mailhog/tool-1/mailhog.log"));
        assert!(!legacy.parent().unwrap().exists());
        assert_eq!(fs::read_to_string(rotated(&path)).unwrap(), "older");
        assert_eq!(prepare_mailhog_log(&dir, "tool-1"), path);

        let redis = log_path(&dir, "Redis");
        fs::write(&redis, "redis").unwrap();
        let logs = list_logs(&dir);
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].path, path);
        assert_eq!(logs[0].size, 12);
        assert_eq!(logs[1].path, redis);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn keeps_both_mailhog_logs_when_the_destination_exists() {
        let dir = temp();
        let path = prepare_mailhog_log(&dir, "tool-1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "new").unwrap();
        let legacy = dir.join("server-logs/tool-1/mailhog.log");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "old").unwrap();

        assert_eq!(prepare_mailhog_log(&dir, "tool-1"), path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::read_to_string(&legacy).unwrap(), "old");
        let logs = list_logs(&dir);
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].name, "MailHog (tool-1)");
        assert_eq!(logs[1].name, "MailHog (tool-1) (legacy)");
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
