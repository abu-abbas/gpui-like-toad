//! End to end: the real extension program against a small fake MailHog.

use ora_core::extension_host::{CallError, ExtProcess};
use ora_core::store::{parse_json, Json};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

fn program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kilat-ext-mailhog"))
}

fn start() -> ExtProcess {
    ExtProcess::start(&program(), "mailhog", "test", WAIT).unwrap()
}

fn params(v: Value) -> Json {
    parse_json(&v.to_string()).unwrap()
}

/// One MailHog message in the shape of its API.
fn api_message(id: &str, subject: &str, body: &str) -> Value {
    json!({
        "ID": id,
        "From": {"Mailbox": "ada", "Domain": "example.com"},
        "Content": {
            "Headers": {
                "Subject": [subject],
                "From": ["Ada <ada@example.com>"],
                "To": ["bob@example.com"],
            },
            "Body": body,
            "Size": body.len(),
        },
        "Created": "2026-11-23T10:00:00Z",
        "MIME": null,
        "Raw": {"Data": format!("Subject: {subject}\r\n\r\n{body}")},
    })
}

/// A MailHog that serves canned answers and remembers every request line.
struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn new(messages: Vec<Value>) -> Self {
        Self::with_messages(Some(messages))
    }

    fn with_messages(messages: Option<Vec<Value>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(stream, messages.as_deref(), &log);
            }
        });
        Self { port, seen }
    }

    fn target(&self) -> Value {
        json!({"host": "127.0.0.1", "port": self.port, "https": false})
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, messages: Option<&[Value]>, log: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    // Drain the headers.
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            break;
        }
    }
    let request = line.trim().to_string();
    log.lock().unwrap().push(request.clone());
    let path = request.split(' ').nth(1).unwrap_or("");
    let (status, body) = if messages.is_none() {
        ("503 Service Unavailable", String::new())
    } else if request.starts_with("DELETE") {
        ("200 OK", String::new())
    } else if path.starts_with("/api/v2/messages") || path.starts_with("/api/v2/search") {
        let messages = messages.unwrap_or_default();
        let page = json!({"total": messages.len(), "count": messages.len(), "start": 0, "items": messages});
        ("200 OK", page.to_string())
    } else {
        ("404 Not Found", String::new())
    };
    let mut stream = reader.into_inner();
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    let mut sink = Vec::new();
    let _ = stream.read_to_end(&mut sink);
}

fn connect(ext: &mut ExtProcess, fake: &Fake) -> Json {
    ext.call("connect", params(json!({"target": fake.target()})), WAIT)
        .unwrap()
}

/// Every message of a streamed answer, and the closing result.
fn stream(ext: &mut ExtProcess, method: &str, p: Value) -> (Vec<Json>, Json) {
    let mut messages = Vec::new();
    let result = ext
        .call_streaming(method, params(p), WAIT, &mut |chunk| {
            let list = chunk
                .get("messages")
                .and_then(Json::as_arr)
                .ok_or("a chunk has no messages")?;
            messages.extend(list.iter().cloned());
            Ok(())
        })
        .unwrap();
    (messages, result)
}

fn subject(m: &Json) -> &str {
    m.get("subject").and_then(Json::as_str).unwrap()
}

#[test]
fn hello_names_the_extension() {
    let ext = start();
    assert_eq!(ext.hello.id, "mailhog");
    assert_eq!(ext.hello.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn connect_reports_the_total_and_list_streams_the_messages() {
    let fake = Fake::new(vec![
        api_message("1", "First", "hello"),
        api_message("2", "Second", "world"),
    ]);
    let mut ext = start();
    let reply = connect(&mut ext, &fake);
    assert_eq!(reply.get("total").and_then(Json::as_u64), Some(2));
    let (messages, result) = stream(&mut ext, "list", json!({"start": 0}));
    assert_eq!(messages.len(), 2);
    assert_eq!(subject(&messages[0]), "First");
    assert_eq!(
        messages[1].get("text").and_then(Json::as_str),
        Some("world")
    );
    assert_eq!(result.get("total").and_then(Json::as_u64), Some(2));
    assert_eq!(result.get("count").and_then(Json::as_u64), Some(2));
    assert!(fake
        .requests()
        .iter()
        .any(|r| r.starts_with("GET /api/v2/messages?start=0")));
}

#[test]
fn an_existing_server_is_not_owned_by_the_extension() {
    let fake = Fake::new(vec![]);
    let mut ext = start();
    let reply = ext
        .call(
            "connect",
            params(json!({"target": fake.target(), "start_server": true})),
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
    ext.call("disconnect", params(json!({})), WAIT).unwrap();
    assert_eq!(
        connect(&mut ext, &fake).get("total").and_then(Json::as_u64),
        Some(0)
    );
}

#[test]
fn search_sends_the_kind_and_the_encoded_query() {
    let fake = Fake::new(vec![api_message("1", "Reset", "x")]);
    let mut ext = start();
    connect(&mut ext, &fake);
    let (messages, _) = stream(
        &mut ext,
        "search",
        json!({"kind": "from", "query": "a b&c", "start": 5}),
    );
    assert_eq!(messages.len(), 1);
    let sent = fake.requests();
    let search = sent.iter().find(|r| r.contains("/api/v2/search")).unwrap();
    assert!(search.contains("kind=from"), "{search}");
    assert!(search.contains("query=a%20b%26c"), "{search}");
    assert!(search.contains("start=5"), "{search}");
}

#[test]
fn a_search_kind_that_does_not_exist_is_an_error() {
    let fake = Fake::new(vec![]);
    let mut ext = start();
    connect(&mut ext, &fake);
    let err = ext
        .call(
            "search",
            params(json!({"kind": "subject", "query": "x"})),
            WAIT,
        )
        .unwrap_err();
    assert!(matches!(err, CallError::Rpc(_)), "{err}");
    // The program is still usable after an error answer.
    assert!(ext.call("list", params(json!({"start": 0})), WAIT).is_ok());
}

#[test]
fn delete_and_delete_all_reach_mailhog() {
    let fake = Fake::new(vec![]);
    let mut ext = start();
    connect(&mut ext, &fake);
    ext.call("delete", params(json!({"id": "a b"})), WAIT)
        .unwrap();
    ext.call("delete_all", params(json!({})), WAIT).unwrap();
    let sent = fake.requests();
    assert!(sent.contains(&"DELETE /api/v1/messages/a%20b HTTP/1.1".to_string()));
    assert!(sent.contains(&"DELETE /api/v1/messages HTTP/1.1".to_string()));
}

#[test]
fn calls_before_connect_and_after_disconnect_say_not_connected() {
    let fake = Fake::new(vec![]);
    let mut ext = start();
    let err = ext
        .call("list", params(json!({"start": 0})), WAIT)
        .unwrap_err();
    assert!(err.to_string().contains("not connected"), "{err}");
    connect(&mut ext, &fake);
    ext.call("disconnect", params(json!({})), WAIT).unwrap();
    let err = ext
        .call("list", params(json!({"start": 0})), WAIT)
        .unwrap_err();
    assert!(err.to_string().contains("not connected"), "{err}");
}

#[test]
fn connect_reports_an_unavailable_server() {
    let fake = Fake::with_messages(None);
    let mut ext = start();
    let err = ext
        .call("connect", params(json!({"target": fake.target()})), WAIT)
        .unwrap_err();
    assert!(
        err.to_string().contains("MailHog answered HTTP 503"),
        "{err}"
    );
}

#[test]
fn connect_needs_a_target_and_unknown_methods_fail() {
    let mut ext = start();
    assert!(ext.call("connect", params(json!({})), WAIT).is_err());
    assert!(ext
        .call("connect", params(json!({"target": {"host": "h"}})), WAIT)
        .is_err());
    let err = ext.call("frobnicate", params(json!({})), WAIT).unwrap_err();
    assert!(err.to_string().contains("unknown method"), "{err}");
}

#[test]
fn a_huge_message_arrives_without_its_raw_source() {
    let big = "x".repeat(3 * 1024 * 1024);
    let fake = Fake::new(vec![
        api_message("1", "Big", &big),
        api_message("2", "Small", "ok"),
    ]);
    let mut ext = start();
    connect(&mut ext, &fake);
    let (messages, _) = stream(&mut ext, "list", json!({"start": 0}));
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].get("raw").and_then(Json::as_str), Some(""));
    let text = messages[0].get("text").and_then(Json::as_str).unwrap();
    assert!(text.len() <= 512 * 1024, "{}", text.len());
    assert_eq!(subject(&messages[1]), "Small");
}

#[test]
fn declarative_inbox_selects_and_copies_without_native_mailhog_ui() {
    use ora_core::extension_ui::{decode_update, Effect, Origin, Slot};
    use ora_core::store::to_json;

    let fake = Fake::new(vec![api_message("mail-1", "Welcome", "Hello")]);
    let mut ext = start();
    ext.call(
        "connect",
        params(json!({"target": fake.target(), "read_only": true})),
        WAIT,
    )
    .unwrap();
    let reply = ext
        .call("ui.open", params(json!({"slot": "explorer"})), WAIT)
        .unwrap();
    let update = decode_update(&to_json(&reply), Origin::Open).unwrap();
    assert_eq!(update.slots.len(), 3);
    assert_eq!(update.poll_ms, Some(5000));

    let reply = ext
        .call(
            "ui.event",
            params(json!({
                "revision": update.revision,
                "slot": "explorer",
                "node": "messages",
                "event": {"type": "select", "item": "mail-1"}
            })),
            WAIT,
        )
        .unwrap();
    let update = decode_update(&to_json(&reply), Origin::UserEvent).unwrap();
    assert!(update
        .slots
        .iter()
        .any(|slot| slot.slot == Slot::Main && slot.tree.is_some()));

    let action = |node: &str, revision| {
        params(json!({
            "revision": revision,
            "slot": "main",
            "node": node,
            "event": {"type": "click"}
        }))
    };
    let reply = ext
        .call("ui.event", action("copy-subject", update.revision), WAIT)
        .unwrap();
    let update = decode_update(&to_json(&reply), Origin::UserEvent).unwrap();
    assert!(matches!(&update.effects[..], [Effect::Copy { text }] if text.as_str() == "Welcome"));
    assert!(ext
        .call("ui.event", action("delete-message", update.revision), WAIT)
        .unwrap_err()
        .to_string()
        .contains("read-only"));
    assert!(ext
        .call("delete_all", params(json!({})), WAIT)
        .unwrap_err()
        .to_string()
        .contains("read-only"));
    assert!(fake
        .requests()
        .iter()
        .all(|request| !request.starts_with("DELETE")));
}
