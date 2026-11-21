//! A small synchronous Redis client for the Developer Tools browser.
//!
//! Everything here blocks, so the app runs a [`RedisSession`] on its own
//! worker thread. Keys are always listed with SCAN (never KEYS), and every
//! collection read is capped so a huge key cannot freeze the UI.

use std::time::Duration;

use redis::{Connection, Value};

use crate::console;

/// Keys shown per page of the key list.
pub const SCAN_PAGE: usize = 200;
/// SCAN calls per page before giving up on a sparse pattern.
const SCAN_ROUNDS: usize = 40;
/// Items read from one hash / set / zset / list.
pub const ITEM_CAP: usize = 1000;
/// Bytes read from one string value.
pub const STRING_CAP: usize = 1 << 20;

/// Where and how to connect.
#[derive(Clone, Debug, Default)]
pub struct RedisTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub tls: bool,
}

/// What the server told us right after connecting.
#[derive(Clone, Debug, Default)]
pub struct ServerInfo {
    pub version: String,
    /// Number of logical databases (CONFIG GET databases, 16 when hidden).
    pub databases: u32,
}

/// One key of the key list.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyEntry {
    pub key: Vec<u8>,
    pub kind: String,
    /// Milliseconds left, -1 without expiry, -2 when the key is gone.
    pub ttl_ms: i64,
}

impl KeyEntry {
    pub fn name(&self) -> String {
        display(&self.key)
    }
}

/// A page of SCAN results.
#[derive(Clone, Debug, Default)]
pub struct ScanPage {
    pub keys: Vec<KeyEntry>,
    /// Cursor to continue from; 0 when the scan is complete.
    pub cursor: u64,
}

/// The value of one key, capped (see [`ITEM_CAP`], [`STRING_CAP`]).
#[derive(Clone, Debug, PartialEq)]
pub enum KeyValue {
    Str {
        bytes: Vec<u8>,
        truncated: bool,
    },
    Hash(Vec<(Vec<u8>, Vec<u8>)>),
    List(Vec<Vec<u8>>),
    Set(Vec<Vec<u8>>),
    ZSet(Vec<(Vec<u8>, f64)>),
    /// Streams and module types: the server's reply as text.
    Raw(String),
    Missing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyDetail {
    pub key: Vec<u8>,
    pub kind: String,
    pub ttl_ms: i64,
    pub encoding: Option<String>,
    pub memory: Option<u64>,
    /// Length of the whole value (fields, members, bytes), which can be
    /// more than what `value` holds.
    pub len: u64,
    pub value: KeyValue,
}

pub struct RedisSession {
    conn: Connection,
    db: u32,
}

/// Bytes as text for the UI: UTF-8 when valid, else escaped like redis-cli.
pub fn display(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let mut out = String::new();
            for &b in bytes {
                if b.is_ascii_graphic() || b == b' ' {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("\\x{b:02x}"));
                }
            }
            out
        }
    }
}

fn err(e: redis::RedisError) -> String {
    let text = e.to_string();
    if text.contains("NOAUTH") || text.contains("WRONGPASS") || text.contains("invalid password") {
        format!("Authentication failed: {text}")
    } else {
        text
    }
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn url(target: &RedisTarget, db: u32) -> String {
    let scheme = if target.tls { "rediss" } else { "redis" };
    let auth = match (&target.password, target.username.trim()) {
        (Some(pw), user) if !pw.is_empty() => format!("{}:{}@", pct(user), pct(pw)),
        (_, user) if !user.is_empty() => format!("{}@", pct(user)),
        _ => String::new(),
    };
    let host = if target.host.contains(':') && !target.host.starts_with('[') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    format!("{scheme}://{auth}{host}:{}/{db}", target.port)
}

impl RedisSession {
    /// Connect (5 s timeout) and select `db`. Replies are given 30 s, so a
    /// blocking command typed in the console cannot hang the worker.
    pub fn connect(target: &RedisTarget, db: u32) -> Result<(Self, ServerInfo), String> {
        if target.tls {
            crate::ensure_crypto_provider();
        }
        let client = redis::Client::open(url(target, db)).map_err(err)?;
        let mut conn = client
            .get_connection_with_timeout(Duration::from_secs(5))
            .map_err(err)?;
        conn.set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(err)?;
        conn.set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(err)?;
        let info: String = redis::cmd("INFO")
            .arg("server")
            .query(&mut conn)
            .unwrap_or_default();
        let version = info
            .lines()
            .find_map(|l| l.strip_prefix("redis_version:"))
            .unwrap_or("")
            .trim()
            .to_string();
        // Managed servers often hide CONFIG; 16 is the default.
        let databases = redis::cmd("CONFIG")
            .arg("GET")
            .arg("databases")
            .query::<Vec<String>>(&mut conn)
            .ok()
            .and_then(|v| v.get(1).and_then(|n| n.parse().ok()))
            .unwrap_or(16);
        Ok((RedisSession { conn, db }, ServerInfo { version, databases }))
    }

    fn select(&mut self, db: u32) -> Result<(), String> {
        if db != self.db {
            redis::cmd("SELECT")
                .arg(db)
                .exec(&mut self.conn)
                .map_err(err)?;
            self.db = db;
        }
        Ok(())
    }

    /// Key counts per database from INFO keyspace (empty ones are absent).
    pub fn keyspace(&mut self) -> Result<Vec<(u32, u64)>, String> {
        let info: String = redis::cmd("INFO")
            .arg("keyspace")
            .query(&mut self.conn)
            .map_err(err)?;
        Ok(parse_keyspace(&info))
    }

    /// One page of keys matching `pattern`, optionally of one type, with
    /// each key's type and TTL.
    pub fn scan(
        &mut self,
        db: u32,
        pattern: &str,
        kind: Option<&str>,
        cursor: u64,
    ) -> Result<ScanPage, String> {
        self.select(db)?;
        let pattern = if pattern.trim().is_empty() {
            "*"
        } else {
            pattern.trim()
        };
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut cursor = cursor;
        for _ in 0..SCAN_ROUNDS {
            let mut cmd = redis::cmd("SCAN");
            cmd.arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(500);
            if let Some(kind) = kind {
                cmd.arg("TYPE").arg(kind);
            }
            let (next, batch): (u64, Vec<Vec<u8>>) = cmd.query(&mut self.conn).map_err(err)?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 || keys.len() >= SCAN_PAGE {
                break;
            }
        }
        keys.sort_by(|a, b| natural_cmp(a, b));
        keys.dedup();
        let mut pipe = redis::pipe();
        for key in &keys {
            pipe.cmd("TYPE").arg(key).cmd("PTTL").arg(key);
        }
        let meta: Vec<Value> = if keys.is_empty() {
            Vec::new()
        } else {
            pipe.query(&mut self.conn).map_err(err)?
        };
        let entries = keys
            .into_iter()
            .enumerate()
            .map(|(i, key)| KeyEntry {
                key,
                kind: value_text(meta.get(i * 2)).unwrap_or_default(),
                ttl_ms: value_int(meta.get(i * 2 + 1)).unwrap_or(-1),
            })
            .collect();
        Ok(ScanPage {
            keys: entries,
            cursor,
        })
    }

    /// Type, TTL, size and (capped) value of one key.
    pub fn load(&mut self, db: u32, key: &[u8]) -> Result<KeyDetail, String> {
        self.select(db)?;
        let kind: String = redis::cmd("TYPE")
            .arg(key)
            .query(&mut self.conn)
            .map_err(err)?;
        let ttl_ms: i64 = redis::cmd("PTTL")
            .arg(key)
            .query(&mut self.conn)
            .map_err(err)?;
        let encoding: Option<String> = redis::cmd("OBJECT")
            .arg("ENCODING")
            .arg(key)
            .query(&mut self.conn)
            .ok();
        let memory: Option<u64> = redis::cmd("MEMORY")
            .arg("USAGE")
            .arg(key)
            .query(&mut self.conn)
            .ok();
        let c = &mut self.conn;
        let (len, value) = match kind.as_str() {
            "none" => (0, KeyValue::Missing),
            "string" => {
                let len: u64 = redis::cmd("STRLEN").arg(key).query(c).map_err(err)?;
                let bytes: Vec<u8> = redis::cmd("GETRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(STRING_CAP as i64 - 1)
                    .query(c)
                    .map_err(err)?;
                let truncated = (bytes.len() as u64) < len;
                (len, KeyValue::Str { bytes, truncated })
            }
            "hash" => {
                let len: u64 = redis::cmd("HLEN").arg(key).query(c).map_err(err)?;
                let mut out = Vec::new();
                let mut cursor = 0u64;
                loop {
                    let (next, items): (u64, Vec<Vec<u8>>) = redis::cmd("HSCAN")
                        .arg(key)
                        .arg(cursor)
                        .arg("COUNT")
                        .arg(500)
                        .query(c)
                        .map_err(err)?;
                    let mut it = items.into_iter();
                    while let (Some(f), Some(v)) = (it.next(), it.next()) {
                        out.push((f, v));
                    }
                    cursor = next;
                    if cursor == 0 || out.len() >= ITEM_CAP {
                        break;
                    }
                }
                out.truncate(ITEM_CAP);
                out.sort_by(|a, b| a.0.cmp(&b.0));
                (len, KeyValue::Hash(out))
            }
            "list" => {
                let len: u64 = redis::cmd("LLEN").arg(key).query(c).map_err(err)?;
                let items: Vec<Vec<u8>> = redis::cmd("LRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(ITEM_CAP as i64 - 1)
                    .query(c)
                    .map_err(err)?;
                (len, KeyValue::List(items))
            }
            "set" => {
                let len: u64 = redis::cmd("SCARD").arg(key).query(c).map_err(err)?;
                let mut out = Vec::new();
                let mut cursor = 0u64;
                loop {
                    let (next, items): (u64, Vec<Vec<u8>>) = redis::cmd("SSCAN")
                        .arg(key)
                        .arg(cursor)
                        .arg("COUNT")
                        .arg(500)
                        .query(c)
                        .map_err(err)?;
                    out.extend(items);
                    cursor = next;
                    if cursor == 0 || out.len() >= ITEM_CAP {
                        break;
                    }
                }
                out.truncate(ITEM_CAP);
                out.sort();
                (len, KeyValue::Set(out))
            }
            "zset" => {
                let len: u64 = redis::cmd("ZCARD").arg(key).query(c).map_err(err)?;
                let items: Vec<(Vec<u8>, f64)> = redis::cmd("ZREVRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(ITEM_CAP as i64 - 1)
                    .arg("WITHSCORES")
                    .query(c)
                    .map_err(err)?;
                (len, KeyValue::ZSet(items))
            }
            "stream" => {
                let len: u64 = redis::cmd("XLEN").arg(key).query(c).map_err(err)?;
                let v: Value = redis::cmd("XREVRANGE")
                    .arg(key)
                    .arg("+")
                    .arg("-")
                    .arg("COUNT")
                    .arg(100)
                    .query(c)
                    .map_err(err)?;
                (len, KeyValue::Raw(format_reply(&v)))
            }
            other => (
                0,
                KeyValue::Raw(format!(
                    "Keys of type \"{other}\" have no viewer yet. Use the console to read them."
                )),
            ),
        };
        Ok(KeyDetail {
            key: key.to_vec(),
            kind,
            ttl_ms,
            encoding,
            memory,
            len,
            value,
        })
    }

    /// Run one command typed in the console and format the reply like
    /// redis-cli. Refused commands never reach the server.
    pub fn console(
        &mut self,
        db: u32,
        line: &str,
        allow_dangerous: bool,
        read_only: bool,
    ) -> Result<String, String> {
        let args = console::split_args(line)?;
        if args.is_empty() {
            return Ok(String::new());
        }
        if let Some(reason) = console::refusal(&args, allow_dangerous, read_only) {
            return Err(reason);
        }
        self.select(db)?;
        let mut cmd = redis::cmd(&String::from_utf8_lossy(&args[0]));
        for a in &args[1..] {
            cmd.arg(a.as_slice());
        }
        match cmd.query::<Value>(&mut self.conn) {
            Ok(v) => Ok(format_reply(&v)),
            Err(e) => Err(format!("(error) {}", err(e))),
        }
    }

    /// Run a command built by the UI (key operations); returns the reply.
    pub fn exec(&mut self, db: u32, args: &[Vec<u8>]) -> Result<Value, String> {
        self.select(db)?;
        let Some(name) = args.first() else {
            return Ok(Value::Nil);
        };
        let mut cmd = redis::cmd(&String::from_utf8_lossy(name));
        for a in &args[1..] {
            cmd.arg(a.as_slice());
        }
        cmd.query::<Value>(&mut self.conn).map_err(err)
    }

    /// Delete one key (UNLINK, so a big value is freed in the background).
    pub fn delete_key(&mut self, db: u32, key: &[u8]) -> Result<bool, String> {
        self.select(db)?;
        let n: i64 = redis::cmd("UNLINK")
            .arg(key)
            .query(&mut self.conn)
            .map_err(err)?;
        Ok(n > 0)
    }

    pub fn ping(&mut self) -> Result<(), String> {
        redis::cmd("PING").exec(&mut self.conn).map_err(err)
    }
}

/// Parse `INFO keyspace`: `db0:keys=12,expires=0,avg_ttl=0`.
pub fn parse_keyspace(info: &str) -> Vec<(u32, u64)> {
    info.lines()
        .filter_map(|line| {
            let (db, rest) = line.trim().split_once(':')?;
            let db: u32 = db.strip_prefix("db")?.parse().ok()?;
            let keys = rest
                .split(',')
                .find_map(|kv| kv.strip_prefix("keys="))?
                .parse()
                .ok()?;
            Some((db, keys))
        })
        .collect()
}

fn value_text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::SimpleString(s) => Some(s.clone()),
        Value::BulkString(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::Okay => Some("OK".into()),
        _ => None,
    }
}

fn value_int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

/// A reply in redis-cli's text form.
pub fn format_reply(v: &Value) -> String {
    let mut out = String::new();
    write_reply(&mut out, v, 0);
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn quoted(bytes: &[u8]) -> String {
    let mut s = String::from("\"");
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            for c in text.chars() {
                match c {
                    '"' => s.push_str("\\\""),
                    '\\' => s.push_str("\\\\"),
                    '\n' => s.push_str("\\n"),
                    '\r' => s.push_str("\\r"),
                    '\t' => s.push_str("\\t"),
                    c if c.is_control() => s.push_str(&format!("\\x{:02x}", c as u32)),
                    c => s.push(c),
                }
            }
        }
        Err(_) => s.push_str(&display(bytes)),
    }
    s.push('"');
    s
}

fn write_reply(out: &mut String, v: &Value, indent: usize) {
    let items = |out: &mut String, items: &[Value], indent: usize| {
        if items.is_empty() {
            out.push_str("(empty array)\n");
            return;
        }
        let width = items.len().to_string().len();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(&" ".repeat(indent));
            }
            let label = format!("{:>width$}) ", i + 1);
            out.push_str(&label);
            write_reply(out, item, indent + label.len());
        }
    };
    match v {
        Value::Nil => out.push_str("(nil)\n"),
        Value::Int(n) => out.push_str(&format!("(integer) {n}\n")),
        Value::BulkString(b) => {
            out.push_str(&quoted(b));
            out.push('\n');
        }
        Value::SimpleString(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Value::Okay => out.push_str("OK\n"),
        Value::Double(d) => out.push_str(&format!("(double) {d}\n")),
        Value::Boolean(b) => out.push_str(&format!("({})\n", if *b { "true" } else { "false" })),
        Value::Array(list) | Value::Set(list) => items(out, list, indent),
        Value::Map(pairs) => {
            let flat: Vec<Value> = pairs
                .iter()
                .flat_map(|(k, v)| [k.clone(), v.clone()])
                .collect();
            items(out, &flat, indent)
        }
        Value::ServerError(e) => out.push_str(&format!("(error) {e:?}\n")),
        other => out.push_str(&format!("{other:?}\n")),
    }
}

/// Order keys like a person would: `item:2` before `item:10`.
pub fn natural_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na = a[si..i].iter().skip_while(|&&c| c == b'0').count();
            let nb = b[sj..j].iter().skip_while(|&&c| c == b'0').count();
            let ord = na
                .cmp(&nb)
                .then_with(|| a[i - na..i].cmp(&b[j - nb..j]))
                .then_with(|| (i - si).cmp(&(j - sj)));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            match a[i].cmp(&b[j]) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                ord => return ord,
            }
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// A value that is a JSON object or array, indented; `None` otherwise.
pub fn pretty_json(text: &str) -> Option<String> {
    let t = text.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::to_string_pretty(&v).ok()
}

/// TTL for the UI: "No expiry", "41 s", "56 min", "3 h", "2 d".
pub fn ttl_label(ms: i64) -> String {
    match ms {
        -2 => "Gone".into(),
        ms if ms < 0 => "No expiry".into(),
        ms => {
            let s = ms / 1000;
            if s < 120 {
                format!("{s} s")
            } else if s < 2 * 3600 {
                format!("{} min", s / 60)
            } else if s < 2 * 86400 {
                format!("{} h", s / 3600)
            } else {
                format!("{} d", s / 86400)
            }
        }
    }
}

/// Short TTL for the key list: "" without expiry.
pub fn ttl_short(ms: i64) -> String {
    match ms {
        ms if ms < 0 => String::new(),
        ms => {
            let s = ms / 1000;
            if s < 60 {
                format!("{s}s")
            } else if s < 3600 {
                format!("{}m", s / 60)
            } else if s < 86400 {
                format!("{}h", s / 3600)
            } else {
                format!("{}d", s / 86400)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut keys: Vec<&[u8]> = vec![b"p:10", b"p:2", b"p:1", b"a", b"p:02x", b"p:2x"];
        keys.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            keys,
            [&b"a"[..], b"p:1", b"p:2", b"p:2x", b"p:02x", b"p:10"]
        );
    }

    #[test]
    fn builds_urls() {
        let mut t = RedisTarget {
            host: "127.0.0.1".into(),
            port: 6379,
            ..Default::default()
        };
        assert_eq!(url(&t, 0), "redis://127.0.0.1:6379/0");
        t.password = Some("p@ss word".into());
        t.username = "default".into();
        t.tls = true;
        assert_eq!(
            url(&t, 3),
            "rediss://default:p%40ss%20word@127.0.0.1:6379/3"
        );
        t.host = "::1".into();
        t.password = None;
        t.username.clear();
        assert_eq!(url(&t, 0), "rediss://[::1]:6379/0");
    }

    #[test]
    fn parses_keyspace() {
        let info =
            "# Keyspace\r\ndb0:keys=12,expires=1,avg_ttl=0\r\ndb3:keys=4,expires=0,avg_ttl=0\r\n";
        assert_eq!(parse_keyspace(info), vec![(0, 12), (3, 4)]);
    }

    #[test]
    fn formats_like_redis_cli() {
        let v = Value::Array(vec![
            Value::BulkString(b"a".to_vec()),
            Value::Int(3),
            Value::Nil,
        ]);
        assert_eq!(format_reply(&v), "1) \"a\"\n2) (integer) 3\n3) (nil)");
        assert_eq!(format_reply(&Value::Okay), "OK");
        assert_eq!(format_reply(&Value::Array(vec![])), "(empty array)");
    }

    #[test]
    fn ttl_labels() {
        assert_eq!(ttl_label(-1), "No expiry");
        assert_eq!(ttl_label(41_000), "41 s");
        assert_eq!(ttl_short(3_412_000), "56m");
        assert_eq!(ttl_short(-1), "");
    }
}
