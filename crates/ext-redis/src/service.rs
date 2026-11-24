//! The methods of the Redis extension: `connect`, `disconnect`,
//! `server.stop` and the `ui.*` calls (see `docs/extension-ui.md`).

use std::time::Duration;

use devtools::redis_tool::{RedisSession, RedisTarget};
use devtools::server::{self, ServerProcess};
use ora_core::store::{parse_json, to_json, Json};
use serde_json::{json, Value};

use crate::view::View;

/// How long `redis-server` gets to save and exit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Service {
    session: Option<RedisSession>,
    view: View,
    server: Option<ManagedServer>,
}

/// A `redis-server` this extension started. Dropping it stops the server.
struct ManagedServer {
    process: ServerProcess,
    config: std::path::PathBuf,
}

impl Drop for ManagedServer {
    fn drop(&mut self) {
        self.process.stop(STOP_GRACE);
        server::forget_pid(&self.config, self.process.pid());
    }
}

fn to_value(j: &Json) -> Result<Value, String> {
    serde_json::from_str(&to_json(j)).map_err(|e| e.to_string())
}

fn to_wire(v: &Value) -> Result<Json, String> {
    parse_json(&v.to_string()).map_err(|e| e.to_string())
}

fn text(p: &Value, key: &str) -> String {
    p.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn flag(p: &Value, key: &str) -> bool {
    p.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn target_of(p: &Value) -> Result<RedisTarget, String> {
    let target = p.get("target").ok_or("missing target")?;
    let port = target
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok())
        .ok_or("missing port")?;
    let host = text(target, "host");
    if host.trim().is_empty() {
        return Err("missing host".into());
    }
    Ok(RedisTarget {
        host,
        port,
        username: text(target, "username"),
        password: target
            .get("password")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        tls: flag(target, "tls"),
    })
}

/// Start `redis-server` when the connection asks for it and nothing
/// answers on the port yet. Returns `None` when something already does:
/// that server is used as it is and never touched.
fn start_local(target: &RedisTarget, p: &Value) -> Result<Option<ManagedServer>, String> {
    if !flag(p, "start_server") {
        return Ok(None);
    }
    if !server::is_local(&target.host) {
        return Err("A managed Redis server needs a loopback address.".into());
    }
    if server::wait_port(&target.host, target.port, Duration::ZERO, || None).is_ok() {
        return Ok(None);
    }
    let name = text(p, "name");
    let id = p
        .get("instance_id")
        .and_then(Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or("Invalid Redis instance ID.")?;
    let config = ora_core::store::config_dir();
    let (log, data) = server::prepare_redis(&config, id, &name);
    let command = Some(text(p, "server_command"))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| server::default_command(true, target.port));
    let process = ServerProcess::start(&command, &log, &data)?;
    server::record_pid(&config, process.pid(), &command);
    let mut managed = ManagedServer { process, config };
    server::wait_port(&target.host, target.port, Duration::from_secs(15), || {
        managed.process.exited()
    })?;
    Ok(Some(managed))
}

impl Service {
    pub fn handle(&mut self, method: &str, params: &Json) -> Result<Json, String> {
        let p = to_value(params)?;
        let result = match method {
            "connect" => {
                let target = target_of(&p)?;
                let managed = if self.server.is_none() {
                    start_local(&target, &p)?
                } else {
                    None
                };
                let database = p
                    .get("database")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0);
                let (mut session, info) = RedisSession::connect(&target, database)?;
                if let Some(managed) = managed {
                    self.server = Some(managed);
                }
                self.view = View::new(
                    database,
                    info.clone(),
                    flag(&p, "read_only"),
                    flag(&p, "allow_dangerous"),
                );
                self.view.seed(&mut session);
                self.session = Some(session);
                json!({
                    "version": info.version,
                    "databases": info.databases,
                    "managed_server": self.server.is_some(),
                })
            }
            "disconnect" => {
                self.session = None;
                self.server = None;
                json!({})
            }
            "server.stop" => {
                let server = self
                    .server
                    .take()
                    .ok_or("Kilat did not start this server.")?;
                self.session = None;
                drop(server);
                self.view.stopped()?
            }
            "ui.open" | "ui.event" | "ui.tick" => {
                let session = self.session.as_mut().ok_or("not connected")?;
                self.view.handle(session, method, p)?
            }
            other => return Err(format!("unknown method {other}")),
        };
        to_wire(&result)
    }
}
