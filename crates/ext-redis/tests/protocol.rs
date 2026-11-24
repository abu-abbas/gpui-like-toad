//! End to end: the real extension program against a real `redis-server`.
//! The tests are skipped (with a note) where `redis-server` is not
//! installed.

use ora_core::extension_host::ExtProcess;
use ora_core::extension_ui::{decode_update, Node, Origin, Slot, ViewUpdate};
use ora_core::store::{parse_json, to_json, Json};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

fn program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kilat-ext-redis"))
}

fn params(v: Value) -> Json {
    parse_json(&v.to_string()).unwrap()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A throwaway `redis-server`, or `None` when the program is not installed.
fn redis() -> Option<Server> {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("kilat-ext-redis-test-{port}"));
    std::fs::create_dir_all(&dir).ok()?;
    let child = Command::new("redis-server")
        .args([
            "--port",
            &port.to_string(),
            "--save",
            "",
            "--appendonly",
            "no",
        ])
        .current_dir(&dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let server = Server { child, port };
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(start.elapsed() < WAIT, "redis-server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(server)
}

fn skip() {
    eprintln!("skipped: redis-server is not installed");
}

struct Session {
    ext: ExtProcess,
    revision: u32,
}

impl Session {
    fn connect(server: &Server, extra: Value) -> Self {
        let mut ext = ExtProcess::start(&program(), "redis", "test", WAIT).unwrap();
        let mut payload = json!({
            "target": {"host": "127.0.0.1", "port": server.port, "username": "", "tls": false},
            "database": 0,
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        ext.call("connect", params(payload), WAIT).unwrap();
        Self { ext, revision: 0 }
    }

    fn ask(&mut self, method: &str, payload: Value, origin: Origin) -> Result<ViewUpdate, String> {
        let reply = self
            .ext
            .call(method, params(payload), WAIT)
            .map_err(|e| e.to_string())?;
        let update = decode_update(&to_json(&reply), origin)?;
        self.revision = update.revision;
        Ok(update)
    }

    fn open(&mut self) -> ViewUpdate {
        self.ask("ui.open", json!({"slot": "explorer"}), Origin::Open)
            .unwrap()
    }

    fn event(&mut self, slot: &str, node: &str, event: Value) -> Result<ViewUpdate, String> {
        let revision = self.revision;
        self.ask(
            "ui.event",
            json!({"revision": revision, "slot": slot, "node": node, "event": event}),
            Origin::UserEvent,
        )
    }

    fn redis(&mut self, line: &str) -> Value {
        // Seed data through the console of the extension itself.
        let update = self
            .event("main", "console", json!({"type": "submit", "value": line}))
            .unwrap();
        serde_json::to_value(&update).unwrap()
    }
}

fn slot_json(update: &ViewUpdate, slot: Slot) -> String {
    let tree = update
        .slots
        .iter()
        .find(|s| s.slot == slot)
        .and_then(|s| s.tree.as_ref())
        .expect("slot has a tree");
    serde_json::to_string(tree).unwrap()
}

fn key_rows(update: &ViewUpdate) -> Vec<String> {
    fn walk(node: &Node, out: &mut Vec<String>) {
        match node {
            Node::Column { children } => children.iter().for_each(|c| walk(c, out)),
            Node::List { rows, .. } => out.extend(rows.iter().map(|r| r.id.as_str().to_owned())),
            _ => {}
        }
    }
    let mut out = Vec::new();
    let tree = update
        .slots
        .iter()
        .find(|s| s.slot == Slot::Explorer)
        .and_then(|s| s.tree.as_ref())
        .unwrap();
    walk(tree, &mut out);
    out
}

#[test]
fn hello_names_the_extension() {
    let ext = ExtProcess::start(&program(), "redis", "test", WAIT).unwrap();
    assert_eq!(ext.hello.id, "redis");
    assert_eq!(ext.hello.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn browse_read_and_delete_keys() {
    let Some(server) = redis() else {
        return skip();
    };
    let mut s = Session::connect(&server, json!({}));
    s.open();
    s.event(
        "main",
        "main-tabs",
        json!({"type": "tab", "item": "console"}),
    )
    .unwrap();
    s.redis("SET user:1 {\"name\":\"Ada\"}");
    s.redis("HSET profile:1 name Ada city London");
    s.redis("RPUSH queue a b c");
    let update = s
        .event("explorer", "refresh", json!({"type": "click"}))
        .unwrap();
    let rows = key_rows(&update);
    assert_eq!(rows, ["k:profile:1", "k:queue", "k:user:1"]);

    // A string that is JSON is shown indented.
    let update = s
        .event(
            "explorer",
            "keys",
            json!({"type": "select", "item": "k:user:1"}),
        )
        .unwrap();
    let main = slot_json(&update, Slot::Main);
    assert!(main.contains("\"language\":\"json\""), "{main}");
    assert!(main.contains("Ada"));

    // A hash is a table.
    let update = s
        .event(
            "explorer",
            "keys",
            json!({"type": "select", "item": "k:profile:1"}),
        )
        .unwrap();
    let main = slot_json(&update, Slot::Main);
    assert!(main.contains("\"type\":\"table\""));
    assert!(main.contains("London"));

    // Filter by type, then by pattern.
    let update = s
        .event(
            "explorer",
            "type",
            json!({"type": "change", "value": "list"}),
        )
        .unwrap();
    assert_eq!(key_rows(&update), ["k:queue"]);
    s.event(
        "explorer",
        "type",
        json!({"type": "change", "value": "all"}),
    )
    .unwrap();
    let update = s
        .event(
            "explorer",
            "pattern",
            json!({"type": "submit", "value": "user:*"}),
        )
        .unwrap();
    assert_eq!(key_rows(&update), ["k:user:1"]);

    // Copy and delete.
    s.event(
        "explorer",
        "keys",
        json!({"type": "select", "item": "k:user:1"}),
    )
    .unwrap();
    let copied = s
        .event("main", "copy-value", json!({"type": "click"}))
        .unwrap();
    assert!(serde_json::to_string(&copied.effects)
        .unwrap()
        .contains("Ada"));
    let update = s
        .event("main", "delete-key", json!({"type": "click"}))
        .unwrap();
    assert!(key_rows(&update).is_empty());
    assert!(slot_json(&update, Slot::Main).contains("Select a key"));
}

#[test]
fn the_console_blocks_dangerous_commands_and_clears_itself() {
    let Some(server) = redis() else {
        return skip();
    };
    let mut s = Session::connect(&server, json!({}));
    s.open();
    let update = s
        .event(
            "main",
            "console",
            json!({"type": "submit", "value": "FLUSHALL"}),
        )
        .unwrap();
    let main = slot_json(&update, Slot::Main);
    assert!(main.contains("db0> FLUSHALL"));
    assert!(main.contains("blocked") || main.contains("not allowed") || main.contains("Allow"));
    let update = s
        .event(
            "main",
            "console",
            json!({"type": "submit", "value": "clear"}),
        )
        .unwrap();
    assert!(!slot_json(&update, Slot::Main).contains("db0> FLUSHALL"));
    assert!(slot_json(&update, Slot::Main).contains("\"clear_on_submit\":true"));
}

#[test]
fn a_read_only_connection_cannot_write_or_delete() {
    let Some(server) = redis() else {
        return skip();
    };
    let mut writer = Session::connect(&server, json!({}));
    writer.open();
    writer.redis("SET prod:key 1");
    let mut s = Session::connect(&server, json!({"read_only": true}));
    s.open();
    let update = s
        .event(
            "main",
            "console",
            json!({"type": "submit", "value": "SET prod:key 2"}),
        )
        .unwrap();
    assert!(slot_json(&update, Slot::Main).contains("read only"));
    s.event("explorer", "refresh", json!({"type": "click"}))
        .unwrap();
    s.event(
        "explorer",
        "keys",
        json!({"type": "select", "item": "k:prod:key"}),
    )
    .unwrap();
    let error = s
        .event("main", "delete-key", json!({"type": "click"}))
        .unwrap_err();
    assert!(error.contains("read only"), "{error}");
    let value = writer.redis("GET prod:key").to_string();
    assert!(value.contains("GET prod:key\\n\\\"1\\\""), "{value}");
}

#[test]
fn a_stale_revision_is_refused() {
    let Some(server) = redis() else {
        return skip();
    };
    let mut s = Session::connect(&server, json!({}));
    s.open();
    s.revision = s.revision.wrapping_sub(1);
    let error = s
        .event("explorer", "refresh", json!({"type": "click"}))
        .unwrap_err();
    assert!(error.contains("has changed"), "{error}");
}

#[test]
fn an_existing_server_is_not_owned_by_the_extension() {
    let Some(server) = redis() else {
        return skip();
    };
    let mut ext = ExtProcess::start(&program(), "redis", "test", WAIT).unwrap();
    let reply = ext
        .call(
            "connect",
            params(json!({
                "target": {"host": "127.0.0.1", "port": server.port, "username": "", "tls": false},
                "database": 0, "start_server": true, "name": "test",
            })),
            WAIT,
        )
        .unwrap();
    assert_eq!(
        reply.get("managed_server").and_then(Json::as_bool),
        Some(false)
    );
    let error = ext.call("server.stop", Json::Null, WAIT).unwrap_err();
    assert!(error
        .to_string()
        .contains("Kilat did not start this server"));
}

#[test]
fn connecting_to_a_closed_port_fails_cleanly() {
    let mut ext = ExtProcess::start(&program(), "redis", "test", WAIT).unwrap();
    let error = ext
        .call(
            "connect",
            params(json!({
                "target": {"host": "127.0.0.1", "port": free_port(), "username": "", "tls": false},
                "database": 0,
            })),
            WAIT,
        )
        .unwrap_err();
    assert!(!error.to_string().is_empty());
    let error = ext
        .call("ui.open", params(json!({"slot": "explorer"})), WAIT)
        .unwrap_err();
    assert!(error.to_string().contains("not connected"));
}

#[test]
fn the_extension_starts_stops_and_restarts_its_own_server() {
    if Command::new("redis-server")
        .arg("--version")
        .output()
        .is_err()
    {
        return skip();
    }
    let port = free_port();
    let config = std::env::temp_dir().join(format!("kilat-ext-redis-config-{port}"));
    let mut command = Command::new(program());
    command.env("XDG_CONFIG_HOME", &config).env("HOME", &config);
    let mut ext = ExtProcess::start_command(command, "redis", "test", WAIT).unwrap();
    let payload = json!({
        "target": {"host": "127.0.0.1", "port": port, "username": "", "tls": false},
        "database": 0, "start_server": true, "name": "Local Redis",
        "server_command": format!("redis-server --port {port} --save \"\" --appendonly no"),
    });
    let reply = ext.call("connect", params(payload), WAIT).unwrap();
    assert_eq!(
        reply.get("managed_server").and_then(Json::as_bool),
        Some(true)
    );
    let update = ext
        .call("ui.open", params(json!({"slot": "explorer"})), WAIT)
        .unwrap();
    assert!(to_json(&update).contains("Connected"));
    // The server's output goes to the Developer Tools log.
    let log = config.join("Kilat/server-logs/developer-tools/Local Redis.log");
    assert!(log.is_file(), "no log at {}", log.display());

    let stopped = ext.call("server.stop", Json::Null, WAIT).unwrap();
    let stopped = decode_update(&to_json(&stopped), Origin::UserEvent).unwrap();
    assert!(slot_json(&stopped, Slot::Explorer).contains("start-server"));
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    assert!(ext
        .call("ui.open", params(json!({"slot": "explorer"})), WAIT)
        .is_err());
    let _ = std::fs::remove_dir_all(&config);
}
