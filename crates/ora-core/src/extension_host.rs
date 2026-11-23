//! Talking to an extension program: the host side of the extension
//! protocol (see `docs/extension-protocol.md`).
//!
//! An extension is a separate program. Kilat starts it, and the two speak
//! newline-delimited JSON over the program's stdin and stdout, one request
//! at a time:
//!
//! ```text
//! host -> ext   {"id":2,"method":"query","params":{...}}
//! ext  -> host  {"id":2,"chunk":{...}}        zero or more, streaming
//! ext  -> host  {"id":2,"result":{...}}       or {"id":2,"error":{"code":-1,"message":"..."}}
//! ```
//!
//! Everything the program sends is untrusted: lines are size-capped,
//! every wait has a timeout, and a program that misbehaves (bad JSON, a
//! wrong id, silence) is killed and the process counts as broken. Its
//! stderr is only ever logged, never parsed.

use crate::store::{parse_json, to_json, Json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The protocol version this host speaks.
pub const PROTOCOL: u64 = 1;

/// Longest accepted line from the program. A bigger one kills it.
pub const MAX_LINE: usize = 8 * 1024 * 1024;

/// How much of the program's stderr is kept for error messages.
const STDERR_KEEP: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The extension answered with an error.
    Rpc(RpcError),
    /// No message within the timeout; the program was killed.
    Timeout,
    /// The program is gone (exited, or killed earlier). Carries the tail
    /// of its stderr.
    Closed(String),
    /// The program broke the protocol; it was killed.
    Protocol(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Rpc(e) => write!(f, "{}", e.message),
            CallError::Timeout => f.write_str("The extension did not answer in time."),
            CallError::Closed(log) if log.trim().is_empty() => {
                f.write_str("The extension stopped.")
            }
            CallError::Closed(log) => write!(f, "The extension stopped: {}", log.trim()),
            CallError::Protocol(why) => write!(f, "The extension broke the protocol: {why}"),
        }
    }
}

impl std::error::Error for CallError {}

/// What the extension said about itself in `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub id: String,
    pub version: String,
}

/// A running extension program.
pub struct ExtProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Result<String, String>>,
    stderr: Arc<Mutex<String>>,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
    next_id: u64,
    broken: bool,
    pub hello: Hello,
}

impl ExtProcess {
    /// Start `program` and run the handshake: the extension must answer
    /// `hello` with the same protocol version and the id it is installed
    /// under, within `timeout`.
    pub fn start(
        program: &Path,
        expected_id: &str,
        app_version: &str,
        timeout: Duration,
    ) -> Result<Self, CallError> {
        Self::start_command(Command::new(program), expected_id, app_version, timeout)
    }

    pub fn start_command(
        mut command: Command,
        expected_id: &str,
        app_version: &str,
        timeout: Duration,
    ) -> Result<Self, CallError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CallError::Closed(format!("could not start: {e}")))?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CallError::Closed("no stdout".into()))?;
        let stderr_pipe = child.stderr.take();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || read_lines(stdout, tx));
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_thread = stderr_pipe.map(|pipe| {
            let keep = Arc::clone(&stderr);
            std::thread::spawn(move || keep_stderr(pipe, keep))
        });
        let mut process = Self {
            child,
            stdin,
            lines,
            stderr,
            stderr_thread,
            next_id: 1,
            broken: false,
            hello: Hello {
                id: String::new(),
                version: String::new(),
            },
        };
        let reply = process.call(
            "hello",
            Json::Obj(vec![
                ("protocol".into(), Json::Num(PROTOCOL as f64)),
                ("app".into(), Json::Str(app_version.into())),
            ]),
            timeout,
        )?;
        let text = |k: &str| reply.get(k).and_then(Json::as_str).map(str::to_string);
        let protocol = reply.get("protocol").and_then(Json::as_u64);
        let (Some(id), Some(version)) = (text("id"), text("version")) else {
            return Err(process.fail("hello has no id or version"));
        };
        if protocol != Some(PROTOCOL) {
            return Err(process.fail(&format!("protocol {protocol:?}, expected {PROTOCOL}")));
        }
        if id != expected_id {
            return Err(process.fail(&format!("it is {id}, expected {expected_id}")));
        }
        process.hello = Hello { id, version };
        Ok(process)
    }

    /// The last part of what the program wrote to stderr.
    pub fn stderr_tail(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// One request with a single answer. `timeout` applies to each wait
    /// for the program.
    pub fn call(
        &mut self,
        method: &str,
        params: Json,
        timeout: Duration,
    ) -> Result<Json, CallError> {
        self.call_streaming(method, params, timeout, &mut |_| {
            Err("chunk sent to a call without streaming".to_string())
        })
    }

    /// A request whose answer may arrive in pieces: `on_chunk` gets each
    /// `chunk`, the final `result` is returned. `timeout` is the longest
    /// silence between two messages. When `on_chunk` returns an error the
    /// chunk was not acceptable (bad shape, too much data): the program is
    /// killed and the call fails as a protocol violation.
    pub fn call_streaming(
        &mut self,
        method: &str,
        params: Json,
        timeout: Duration,
        on_chunk: &mut dyn FnMut(Json) -> Result<(), String>,
    ) -> Result<Json, CallError> {
        if self.broken {
            return Err(self.closed());
        }
        let id = self.next_id;
        self.next_id += 1;
        let request = Json::Obj(vec![
            ("id".into(), Json::Num(id as f64)),
            ("method".into(), Json::Str(method.into())),
            ("params".into(), params),
        ]);
        let mut line = to_json(&request);
        line.push('\n');
        let wrote = self
            .stdin
            .as_mut()
            .map(|s| s.write_all(line.as_bytes()).and_then(|_| s.flush()));
        if !matches!(wrote, Some(Ok(()))) {
            self.kill();
            return Err(self.closed());
        }
        loop {
            let line = match self.lines.recv_timeout(timeout) {
                Ok(Ok(line)) => line,
                Ok(Err(why)) => return Err(self.fail(&why)),
                Err(RecvTimeoutError::Timeout) => {
                    self.kill();
                    return Err(CallError::Timeout);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.kill();
                    return Err(self.closed());
                }
            };
            let Ok(message) = parse_json(&line) else {
                return Err(self.fail("a line is not JSON"));
            };
            if message.get("id").and_then(Json::as_u64) != Some(id) {
                return Err(self.fail("the answer is for another request"));
            }
            if let Some(chunk) = message.get("chunk") {
                if let Err(why) = on_chunk(chunk.clone()) {
                    return Err(self.fail(&why));
                }
            } else if let Some(error) = message.get("error") {
                return Err(CallError::Rpc(RpcError {
                    code: error
                        .get("code")
                        .and_then(|c| match c {
                            Json::Num(n) => Some(*n as i64),
                            _ => None,
                        })
                        .unwrap_or(-1),
                    message: error
                        .get("message")
                        .and_then(Json::as_str)
                        .unwrap_or("unknown error")
                        .chars()
                        .take(2000)
                        .collect(),
                }));
            } else if let Some(result) = message.get("result") {
                return Ok(result.clone());
            } else {
                return Err(self.fail("a message has no result, error or chunk"));
            }
        }
    }

    /// Ask the program to quit, give it a moment, then kill it.
    pub fn stop(&mut self) {
        if !self.broken {
            if let Some(stdin) = self.stdin.as_mut() {
                let _ = stdin.write_all(b"{\"id\":0,\"method\":\"shutdown\"}\n");
                let _ = stdin.flush();
            }
            self.stdin = None;
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    self.broken = true;
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.kill();
    }

    /// "The program is gone", with what it last wrote to stderr. The
    /// stderr reader gets a moment to finish, so the message is not empty
    /// just because the program died faster than its output was read.
    fn closed(&mut self) -> CallError {
        if let Some(thread) = self.stderr_thread.take() {
            let deadline = Instant::now() + Duration::from_millis(500);
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
        CallError::Closed(self.stderr_tail())
    }

    /// The program sent something the protocol does not allow (found by
    /// the caller, e.g. a result of the wrong shape): kill it.
    pub fn violation(&mut self, why: &str) -> CallError {
        self.fail(why)
    }

    fn fail(&mut self, why: &str) -> CallError {
        self.kill();
        CallError::Protocol(why.to_string())
    }

    fn kill(&mut self) {
        self.broken = true;
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ExtProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reader thread: one message per line, capped at [`MAX_LINE`].
fn read_lines(stdout: impl Read, tx: mpsc::Sender<Result<String, String>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut buf = Vec::new();
        match reader
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.len() > MAX_LINE {
            let _ = tx.send(Err("a message is too large".into()));
            return;
        }
        let line = String::from_utf8_lossy(&buf).trim().to_string();
        if !line.is_empty() && tx.send(Ok(line)).is_err() {
            return;
        }
    }
}

fn keep_stderr(pipe: impl Read, keep: Arc<Mutex<String>>) {
    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
        if let Ok(mut text) = keep.lock() {
            text.push_str(&line);
            text.push('\n');
            if text.len() > STDERR_KEEP {
                let mut cut = text.len() - STDERR_KEEP;
                while !text.is_char_boundary(cut) {
                    cut += 1;
                }
                text.drain(..cut);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Run `script` (sh) as the extension.
    fn start(script: &str, id: &str) -> Result<ExtProcess, CallError> {
        let name = format!("kilat-exthost-{}", crate::store::new_profile_id());
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path: PathBuf = dir.join("ext.sh");
        std::fs::write(&path, script).unwrap();
        let mut cmd = Command::new("sh");
        cmd.arg(&path);
        let result = ExtProcess::start_command(cmd, id, "0.1.51", Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    const HELLO: &str =
        r#"read l; printf '{"id":1,"result":{"protocol":1,"id":"mock","version":"1.0.0"}}\n'"#;
    const T: Duration = Duration::from_secs(5);

    #[test]
    fn handshake_streaming_and_errors() {
        let script = format!(
            "{HELLO}\n\
             read l; printf '{{\"id\":2,\"chunk\":{{\"n\":1}}}}\\n{{\"id\":2,\"chunk\":{{\"n\":2}}}}\\n{{\"id\":2,\"result\":{{\"done\":true}}}}\\n'\n\
             read l; printf '{{\"id\":3,\"error\":{{\"code\":-5,\"message\":\"boom\"}}}}\\n'\n\
             read l\n"
        );
        let mut p = start(&script, "mock").unwrap();
        assert_eq!(
            p.hello,
            Hello {
                id: "mock".into(),
                version: "1.0.0".into()
            }
        );
        let mut chunks = 0;
        let done = p
            .call_streaming("query", Json::Null, T, &mut |_| {
                chunks += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(chunks, 2);
        assert_eq!(done.get("done").and_then(Json::as_bool), Some(true));
        match p.call("x", Json::Null, T) {
            Err(CallError::Rpc(e)) => assert_eq!((e.code, e.message.as_str()), (-5, "boom")),
            other => panic!("{other:?}"),
        }
        assert!(!p.is_broken(), "an error answer is not a protocol failure");
    }

    #[test]
    fn silence_times_out_and_kills() {
        let script = format!("{HELLO}\nread l; sleep 30\n");
        let mut p = start(&script, "mock").unwrap();
        let err = p
            .call("x", Json::Null, Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(err, CallError::Timeout);
        assert!(p.is_broken());
        assert!(matches!(
            p.call("y", Json::Null, T),
            Err(CallError::Closed(_))
        ));
    }

    #[test]
    fn handshake_is_strict() {
        let wrong_id =
            r#"read l; printf '{"id":1,"result":{"protocol":1,"id":"other","version":"1"}}\n'"#;
        assert!(matches!(
            start(wrong_id, "mock"),
            Err(CallError::Protocol(_))
        ));
        let wrong_proto =
            r#"read l; printf '{"id":1,"result":{"protocol":2,"id":"mock","version":"1"}}\n'"#;
        assert!(matches!(
            start(wrong_proto, "mock"),
            Err(CallError::Protocol(_))
        ));
        let missing = r#"read l; printf '{"id":1,"result":{}}\n'"#;
        assert!(matches!(
            start(missing, "mock"),
            Err(CallError::Protocol(_))
        ));
    }

    #[test]
    fn exit_reports_stderr_and_garbage_is_refused() {
        match start("echo oops >&2; exit 1", "mock") {
            Err(CallError::Closed(log)) => assert!(log.contains("oops"), "{log}"),
            other => panic!("{:?}", other.err()),
        }
        assert!(matches!(
            start("read l; echo not-json", "mock"),
            Err(CallError::Protocol(_))
        ));
        let wrong_call_id = r#"read l; printf '{"id":9,"result":{}}\n'"#;
        assert!(matches!(
            start(wrong_call_id, "mock"),
            Err(CallError::Protocol(_))
        ));
    }

    #[test]
    fn oversized_line_is_refused() {
        let script =
            format!("{HELLO}\nread l; head -c 9000000 /dev/zero | tr '\\0' a; echo\nsleep 30\n");
        let mut p = start(&script, "mock").unwrap();
        assert!(matches!(
            p.call("x", Json::Null, T),
            Err(CallError::Protocol(_))
        ));
        assert!(p.is_broken());
    }

    #[test]
    fn a_missing_program_is_a_clean_error() {
        let r = ExtProcess::start(Path::new("/no/such/kilat-ext"), "x", "1", T);
        assert!(matches!(r, Err(CallError::Closed(_))));
    }
}
