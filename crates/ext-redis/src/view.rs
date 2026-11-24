//! The screens of the Redis extension as view trees (see
//! `docs/extension-ui.md`): the key browser in the explorer column and the
//! value and console in the main column. Redis state lives here; Kilat only
//! draws the trees and sends events back.

use devtools::redis_tool::{
    display, natural_cmp, pretty_json, ttl_label, ttl_short, KeyDetail, KeyEntry, KeyValue,
    RedisSession, ServerInfo,
};
use ora_core::extension_ui::{Event, EventKind, InputValue, Open, Slot, Tick, VERSION};
use serde_json::{json, Value};

/// Key types for the type dropdown: SCAN TYPE name and label.
const KEY_TYPES: [(&str, &str); 6] = [
    ("string", "String"),
    ("hash", "Hash"),
    ("list", "List"),
    ("set", "Set"),
    ("zset", "Sorted set"),
    ("stream", "Stream"),
];

/// Keys kept in the list. A pattern that matches more has to be narrowed.
const MAX_KEYS: usize = 1000;
/// Keys longer than this are not listed (their id would be too large).
const MAX_KEY_BYTES: usize = 4096;
/// Longest value text put in one tree node, well below the 64 KiB limit.
const TEXT_CAP: usize = 48 * 1024;
/// Longest element of a hash / list / set drawn in its cell.
const CELL_CAP: usize = 2_000;
/// Console entries kept.
const CONSOLE_LINES: usize = 500;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Value,
    Console,
}

struct ConsoleLine {
    db: u32,
    command: String,
    output: String,
}

#[derive(Default)]
pub struct View {
    revision: u32,
    info: ServerInfo,
    read_only: bool,
    allow_dangerous: bool,
    db: u32,
    kind: Option<&'static str>,
    pattern: String,
    keys: Vec<KeyEntry>,
    /// SCAN cursor to continue from; 0 once the scan is complete.
    cursor: u64,
    capped: bool,
    keyspace: Vec<(u32, u64)>,
    error: Option<String>,
    selected: Option<Vec<u8>>,
    detail: Option<KeyDetail>,
    detail_error: Option<String>,
    tab: Tab,
    console: Vec<ConsoleLine>,
}

fn clip(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A key as a row id: plain text when it is, hex otherwise, so any byte
/// string survives the round trip through JSON.
fn key_id(key: &[u8]) -> String {
    match std::str::from_utf8(key) {
        Ok(text) if !text.chars().any(char::is_control) => format!("k:{text}"),
        _ => {
            let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
            format!("x:{hex}")
        }
    }
}

fn key_from_id(id: &str) -> Option<Vec<u8>> {
    if let Some(text) = id.strip_prefix("k:") {
        return Some(text.as_bytes().to_vec());
    }
    let hex = id.strip_prefix("x:")?;
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

fn type_label(kind: &str) -> &str {
    KEY_TYPES
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(kind, |(_, label)| label)
}

/// Names from Kilat's interface icon set.
fn type_icon(kind: &str) -> &'static str {
    match kind {
        "string" => "type-string",
        "hash" => "type-hash",
        "list" => "type-list",
        "set" => "type-set",
        "zset" => "type-zset",
        "stream" => "type-stream",
        _ => "key",
    }
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.),
        n => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
    }
}

fn format_score(score: f64) -> String {
    if score.fract() == 0.0 && score.abs() < 1e15 {
        format!("{score:.0}")
    } else {
        score.to_string()
    }
}

/// The value as text for Copy value.
fn value_text(value: &KeyValue) -> String {
    match value {
        KeyValue::Str { bytes, .. } => display(bytes),
        KeyValue::Hash(pairs) => pairs
            .iter()
            .map(|(f, v)| format!("{}\t{}", display(f), display(v)))
            .collect::<Vec<_>>()
            .join("\n"),
        KeyValue::List(items) | KeyValue::Set(items) => items
            .iter()
            .map(|v| display(v))
            .collect::<Vec<_>>()
            .join("\n"),
        KeyValue::ZSet(items) => items
            .iter()
            .map(|(m, s)| format!("{}\t{}", format_score(*s), display(m)))
            .collect::<Vec<_>>()
            .join("\n"),
        KeyValue::Raw(text) => text.clone(),
        KeyValue::Missing => String::new(),
    }
}

fn table(columns: [&str; 2], rows: impl Iterator<Item = (String, String)>) -> Value {
    let rows: Vec<Value> = rows
        .map(|(a, b)| json!([clip(&a, CELL_CAP), clip(&b, CELL_CAP)]))
        .collect();
    json!({"type": "table", "columns": columns, "rows": rows})
}

impl View {
    pub fn new(db: u32, info: ServerInfo, read_only: bool, allow_dangerous: bool) -> Self {
        Self {
            db,
            info,
            read_only,
            allow_dangerous,
            ..Self::default()
        }
    }

    /// The first key list, after connecting.
    pub fn seed(&mut self, session: &mut RedisSession) {
        self.keyspace = session.keyspace().unwrap_or_default();
        self.rescan(session);
    }

    pub fn stopped(&mut self) -> Result<Value, String> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("View revision exhausted.")?;
        *self = Self {
            revision,
            ..Self::default()
        };
        Ok(json!({
            "version": VERSION,
            "revision": revision,
            "slots": [
                {"slot": "sidebar", "tree": null},
                {"slot": "explorer", "tree": {"type": "column", "children": [
                    {"type": "text", "text": "Redis", "style": "heading"},
                    {"type": "text", "text": "Server stopped.", "tone": "muted"},
                    {"type": "button", "id": "start-server", "label": "Start server", "variant": "primary"}
                ]}},
                {"slot": "main", "tree": null}
            ]
        }))
    }

    fn rescan(&mut self, session: &mut RedisSession) {
        self.keys.clear();
        self.cursor = 0;
        self.capped = false;
        self.scan_page(session);
    }

    fn scan_page(&mut self, session: &mut RedisSession) {
        match session.scan(self.db, &self.pattern, self.kind, self.cursor) {
            Ok(page) => {
                let mut seen: std::collections::HashSet<Vec<u8>> =
                    self.keys.iter().map(|k| k.key.clone()).collect();
                self.keys
                    .extend(page.keys.into_iter().filter(|k| seen.insert(k.key.clone())));
                self.keys.sort_by(|a, b| natural_cmp(&a.key, &b.key));
                self.cursor = page.cursor;
                if self.keys.len() > MAX_KEYS {
                    self.keys.truncate(MAX_KEYS);
                    self.cursor = 0;
                    self.capped = true;
                }
                if let Ok(keyspace) = session.keyspace() {
                    self.keyspace = keyspace;
                }
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
    }

    fn load_selected(&mut self, session: &mut RedisSession) {
        let Some(key) = self.selected.clone() else {
            return;
        };
        match session.load(self.db, &key) {
            Ok(detail) => {
                self.detail = Some(detail);
                self.detail_error = None;
            }
            Err(err) => {
                self.detail = None;
                self.detail_error = Some(err);
            }
        }
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.detail = None;
        self.detail_error = None;
    }

    fn key_count(&self, db: u32) -> u64 {
        self.keyspace
            .iter()
            .find(|(d, _)| *d == db)
            .map_or(0, |(_, n)| *n)
    }

    pub fn handle(
        &mut self,
        session: &mut RedisSession,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let mut effects = Vec::new();
        match method {
            "ui.open" => {
                serde_json::from_value::<Open>(params).map_err(|e| e.to_string())?;
            }
            "ui.tick" => {
                serde_json::from_value::<Tick>(params).map_err(|e| e.to_string())?;
            }
            "ui.event" => {
                let event: Event = serde_json::from_value(params).map_err(|e| e.to_string())?;
                if event.revision != self.revision {
                    return Err("The Redis view has changed.".into());
                }
                self.event(session, event, &mut effects)?;
            }
            _ => return Err("Unknown Redis UI method.".into()),
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("View revision exhausted.")?;
        Ok(json!({
            "version": VERSION,
            "revision": self.revision,
            "slots": [
                {"slot": "sidebar", "tree": {"type": "badge", "text": "Connected", "tone": "success"}},
                {"slot": "explorer", "tree": self.explorer()},
                {"slot": "main", "tree": self.main()}
            ],
            "effects": effects
        }))
    }

    fn event(
        &mut self,
        session: &mut RedisSession,
        event: Event,
        effects: &mut Vec<Value>,
    ) -> Result<(), String> {
        match (event.slot, event.node.as_str(), event.event) {
            (
                Slot::Explorer,
                "db",
                EventKind::Change {
                    value: InputValue::Text(value),
                },
            ) => {
                self.db = value.as_str().parse().map_err(|_| "Unknown database.")?;
                self.clear_selection();
                self.rescan(session);
            }
            (
                Slot::Explorer,
                "type",
                EventKind::Change {
                    value: InputValue::Text(value),
                },
            ) => {
                self.kind = match value.as_str() {
                    "all" => None,
                    other => Some(
                        KEY_TYPES
                            .iter()
                            .find(|(kind, _)| *kind == other)
                            .map(|(kind, _)| *kind)
                            .ok_or("Unknown key type.")?,
                    ),
                };
                self.rescan(session);
            }
            (
                Slot::Explorer,
                "pattern",
                EventKind::Change {
                    value: InputValue::Text(value),
                },
            ) => self.pattern = value.as_str().trim().to_owned(),
            (Slot::Explorer, "pattern", EventKind::Submit { value }) => {
                self.pattern = value.as_str().trim().to_owned();
                self.rescan(session);
            }
            (Slot::Explorer, "keys", EventKind::Select { item }) => {
                let key = key_from_id(item.as_str()).ok_or("Unknown key.")?;
                if !self.keys.iter().any(|entry| entry.key == key) {
                    return Err("The key is no longer in the list.".into());
                }
                self.selected = Some(key);
                self.tab = Tab::Value;
                self.load_selected(session);
            }
            (Slot::Explorer, "keys", EventKind::More) => self.scan_page(session),
            (Slot::Explorer, "refresh", EventKind::Click { .. }) => {
                self.keyspace = session.keyspace().unwrap_or_default();
                self.rescan(session);
                self.load_selected(session);
            }
            (Slot::Main, "main-tabs", EventKind::Tab { item }) => {
                self.tab = match item.as_str() {
                    "value" => Tab::Value,
                    "console" => Tab::Console,
                    _ => return Err("Unknown Redis tab.".into()),
                };
            }
            (Slot::Main, "copy-value", EventKind::Click { .. }) => {
                let detail = self.detail.as_ref().ok_or("Select a key first.")?;
                let text = value_text(&detail.value);
                effects.push(json!({"type": "copy", "text": clip(&text, 60 * 1024)}));
            }
            (Slot::Main, "delete-key", EventKind::Click { .. }) => {
                if self.read_only {
                    return Err("This Redis connection is read only.".into());
                }
                let key = self.selected.clone().ok_or("Select a key first.")?;
                session.delete_key(self.db, &key)?;
                self.keys.retain(|entry| entry.key != key);
                if let Some(entry) = self.keyspace.iter_mut().find(|(d, _)| *d == self.db) {
                    entry.1 = entry.1.saturating_sub(1);
                }
                self.clear_selection();
            }
            (Slot::Main, "console", EventKind::Submit { value }) => {
                let line = value.as_str().trim();
                if line.eq_ignore_ascii_case("clear") {
                    self.console.clear();
                } else if !line.is_empty() {
                    let output = session
                        .console(self.db, line, self.allow_dangerous, self.read_only)
                        .unwrap_or_else(|err| err);
                    self.console.push(ConsoleLine {
                        db: self.db,
                        command: clip(line, 4096).to_owned(),
                        output,
                    });
                    if self.console.len() > CONSOLE_LINES {
                        self.console.remove(0);
                    }
                }
            }
            _ => return Err("Unknown Redis UI event.".into()),
        }
        Ok(())
    }

    fn explorer(&self) -> Value {
        let count = self.info.databases.max(self.db + 1).clamp(1, 1024);
        let databases: Vec<Value> = (0..count)
            .map(|db| {
                let keys = self.key_count(db);
                let label = if keys > 0 {
                    format!("db{db}  ·  {keys} keys")
                } else {
                    format!("db{db}")
                };
                json!([db.to_string(), label])
            })
            .collect();
        let mut types = vec![json!(["all", "All types"])];
        types.extend(KEY_TYPES.iter().map(|(kind, label)| json!([kind, label])));
        let rows: Vec<Value> = self
            .keys
            .iter()
            .filter(|entry| entry.key.len() <= MAX_KEY_BYTES)
            .map(|entry| {
                json!({
                    "id": key_id(&entry.key),
                    "title": clip(&entry.name(), 512),
                    "trailing": ttl_short(entry.ttl_ms),
                    "icon": type_icon(&entry.kind),
                    "selected": self.selected.as_deref() == Some(entry.key.as_slice())
                })
            })
            .collect();
        let footer = if self.capped {
            format!(
                "First {} keys. Narrow the pattern to see others.",
                self.keys.len()
            )
        } else if self.cursor != 0 {
            format!("{} keys loaded, more on the server", self.keys.len())
        } else {
            format!("{} keys", self.keys.len())
        };
        let mut children = vec![
            json!({"type": "text", "text": "Redis", "style": "heading"}),
            json!({"type": "select", "id": "db", "value": self.db.to_string(), "options": databases}),
            json!({"type": "select", "id": "type", "value": self.kind.unwrap_or("all"), "options": types}),
            json!({"type": "input", "id": "pattern", "placeholder": "Pattern, e.g. session:*",
                   "value": clip(&self.pattern, 512)}),
            json!({"type": "button", "id": "refresh", "label": "Refresh", "variant": "ghost"}),
        ];
        if let Some(error) = &self.error {
            children.push(json!({"type": "text", "text": clip(error, 2000), "tone": "danger"}));
        }
        children
            .push(json!({"type": "list", "id": "keys", "rows": rows, "more": self.cursor != 0}));
        children.push(json!({"type": "text", "text": footer, "style": "caption", "tone": "muted"}));
        json!({"type": "column", "children": children})
    }

    fn main(&self) -> Value {
        json!({"type": "tabs", "id": "main-tabs",
        "active": if self.tab == Tab::Console { "console" } else { "value" },
        "items": [
            {"id": "value", "label": "Value", "child": self.value_tab()},
            {"id": "console", "label": "Console", "child": self.console_tab()}
        ]})
    }

    fn value_tab(&self) -> Value {
        if let Some(error) = &self.detail_error {
            return json!({"type": "text", "text": clip(error, 2000), "tone": "danger"});
        }
        let Some(detail) = &self.detail else {
            return json!({"type": "empty", "text": "Select a key to see its value."});
        };
        let mut meta = vec![
            type_label(&detail.kind).to_owned(),
            format!("TTL {}", ttl_label(detail.ttl_ms)),
        ];
        meta.push(if detail.kind == "string" {
            format!("{} bytes", detail.len)
        } else {
            format!("{} items", detail.len)
        });
        if let Some(encoding) = &detail.encoding {
            meta.push(encoding.clone());
        }
        if let Some(memory) = detail.memory {
            meta.push(format!("{} in memory", human_bytes(memory)));
        }
        let mut children = vec![
            json!({"type": "text", "text": clip(&display(&detail.key), 400), "style": "heading"}),
            json!({"type": "text", "text": meta.join("  ·  "), "style": "caption", "tone": "muted"}),
            json!({"type": "row", "children": [
                {"type": "button", "id": "copy-value", "label": "Copy value", "variant": "ghost"},
                {"type": "button", "id": "delete-key", "label": "Delete key", "variant": "danger",
                 "disabled": self.read_only, "writes": true,
                 "confirm": format!("Delete key {} from db{}? This cannot be undone.",
                                    clip(&display(&detail.key), 80), self.db)}
            ]}),
        ];
        children.extend(self.value_nodes(detail));
        json!({"type": "column", "children": children})
    }

    fn value_nodes(&self, detail: &KeyDetail) -> Vec<Value> {
        let note = |text: String| json!({"type": "text", "text": text, "style": "caption", "tone": "muted"});
        let shown_of = |shown: usize| {
            (detail.len > shown as u64).then(|| {
                note(format!(
                    "Showing the first {shown} of {} items.",
                    detail.len
                ))
            })
        };
        match &detail.value {
            KeyValue::Str { bytes, truncated } => {
                let text = display(bytes);
                let (text, json) = match pretty_json(&text) {
                    Some(pretty) => (pretty, true),
                    None => (text, false),
                };
                let cut = text.len() > TEXT_CAP;
                let mut nodes = vec![json!({"type": "code", "text": clip(&text, TEXT_CAP),
                                            "language": if json { "json" } else { "text" }})];
                if json {
                    nodes.push(note("JSON, shown indented.".into()));
                }
                if *truncated || cut {
                    nodes.push(note("Only the first part of this value is shown.".into()));
                }
                nodes
            }
            KeyValue::Hash(pairs) => std::iter::once(table(
                ["Field", "Value"],
                pairs.iter().map(|(f, v)| (display(f), display(v))),
            ))
            .chain(shown_of(pairs.len()))
            .collect(),
            KeyValue::List(items) => std::iter::once(table(
                ["Index", "Value"],
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), display(v))),
            ))
            .chain(shown_of(items.len()))
            .collect(),
            KeyValue::Set(items) => std::iter::once(table(
                ["#", "Member"],
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| ((i + 1).to_string(), display(v))),
            ))
            .chain(shown_of(items.len()))
            .collect(),
            KeyValue::ZSet(items) => std::iter::once(table(
                ["Score", "Member"],
                items.iter().map(|(m, s)| (format_score(*s), display(m))),
            ))
            .chain(shown_of(items.len()))
            .collect(),
            KeyValue::Raw(text) => vec![json!({"type": "code", "text": clip(text, TEXT_CAP)})],
            KeyValue::Missing => {
                vec![json!({"type": "empty", "text": "The key no longer exists."})]
            }
        }
    }

    fn console_tab(&self) -> Value {
        let rule = if self.read_only {
            "PROD is read only: only read commands run."
        } else if self.allow_dangerous {
            "Dangerous commands are allowed on this instance."
        } else {
            "FLUSHDB, FLUSHALL, KEYS, CONFIG and the like are blocked. Allow them in the connection settings."
        };
        let mut children = vec![json!({
            "type": "text", "style": "caption", "tone": "muted",
            "text": format!("Commands run on db{}. Switch databases with the dropdown. {rule} Type clear to empty the log.", self.db)
        })];
        if !self.console.is_empty() {
            children.push(json!({"type": "code", "text": self.console_text()}));
        }
        children.push(json!({"type": "row", "children": [
            {"type": "text", "text": format!("db{}>", self.db), "style": "mono", "tone": "primary"},
            {"type": "input", "id": "console", "value": "",
             "placeholder": "Command, e.g. HGETALL user:1", "clear_on_submit": true}
        ]}));
        json!({"type": "column", "children": children})
    }

    /// The log, newest last, cut from the top so it stays within a node.
    fn console_text(&self) -> String {
        let mut blocks: Vec<String> = self
            .console
            .iter()
            .map(|line| {
                format!(
                    "db{}> {}\n{}",
                    line.db,
                    line.command,
                    clip(&line.output, 16 * 1024)
                )
            })
            .collect();
        let mut total: usize = blocks.iter().map(|b| b.len() + 2).sum();
        let mut skip = 0;
        while total > TEXT_CAP && skip + 1 < blocks.len() {
            total -= blocks[skip].len() + 2;
            skip += 1;
        }
        blocks.drain(..skip);
        let text = blocks.join("\n\n");
        clip(&text, TEXT_CAP).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use devtools::console;
    use ora_core::extension_ui::{decode_update, Origin};

    use super::*;

    #[test]
    fn key_ids_round_trip_any_bytes() {
        for key in [
            b"user:1".to_vec(),
            "naïve:ключ".as_bytes().to_vec(),
            vec![0, 1, 2, 0xff, 0xfe],
            b"line\nbreak".to_vec(),
            b"k:x:".to_vec(),
            Vec::new(),
        ] {
            let id = key_id(&key);
            assert_eq!(key_from_id(&id), Some(key));
        }
        assert_eq!(key_from_id("x:abc"), None);
        assert_eq!(key_from_id("zzz"), None);
    }

    #[test]
    fn stopped_view_declares_start_and_validates() {
        let mut view = View {
            revision: 7,
            ..View::default()
        };
        let json = view.stopped().unwrap();
        let update = decode_update(&json.to_string(), Origin::UserEvent).unwrap();
        assert_eq!(update.revision, 8);
        assert_eq!(update.poll_ms, None);
    }

    #[test]
    fn full_view_stays_inside_the_protocol_limits() {
        let mut view = View::new(
            0,
            ServerInfo {
                version: "7".into(),
                databases: 16,
            },
            false,
            false,
        );
        view.keys = (0..MAX_KEYS)
            .map(|i| KeyEntry {
                key: format!("session:{i}").into_bytes(),
                kind: "hash".into(),
                ttl_ms: 90_000,
            })
            .collect();
        view.selected = Some(b"session:1".to_vec());
        view.detail = Some(KeyDetail {
            key: b"session:1".to_vec(),
            kind: "zset".into(),
            ttl_ms: -1,
            encoding: Some("skiplist".into()),
            memory: Some(2048),
            len: 5000,
            value: KeyValue::ZSet(
                (0..1000)
                    .map(|i| (format!("member-{i}").into_bytes(), i as f64 + 0.5))
                    .collect(),
            ),
        });
        view.console.push(ConsoleLine {
            db: 0,
            command: "PING".into(),
            output: "PONG".into(),
        });
        let json = json!({
            "version": VERSION,
            "revision": 1,
            "slots": [
                {"slot": "explorer", "tree": view.explorer()},
                {"slot": "main", "tree": view.main()}
            ]
        });
        decode_update(&json.to_string(), Origin::UserEvent).unwrap();
    }

    #[test]
    fn console_log_is_cut_from_the_top() {
        let mut view = View::default();
        for i in 0..40 {
            view.console.push(ConsoleLine {
                db: 0,
                command: format!("GET key{i}"),
                output: "x".repeat(10_000),
            });
        }
        let text = view.console_text();
        assert!(text.len() <= TEXT_CAP);
        assert!(text.contains("GET key39"));
        assert!(!text.contains("GET key0\n"));
    }

    #[test]
    fn read_only_view_disables_delete() {
        let mut view = View::new(0, ServerInfo::default(), true, false);
        view.detail = Some(KeyDetail {
            key: b"a".to_vec(),
            kind: "string".into(),
            ttl_ms: -1,
            encoding: None,
            memory: None,
            len: 1,
            value: KeyValue::Str {
                bytes: b"{\"a\":1}".to_vec(),
                truncated: false,
            },
        });
        let tree = view.value_tab().to_string();
        assert!(tree.contains("\"disabled\":true"));
        assert!(tree.contains("\"language\":\"json\""));
    }

    #[test]
    fn dangerous_commands_are_refused_before_the_server_sees_them() {
        let args = console::split_args("FLUSHALL").unwrap();
        assert!(console::refusal(&args, false, false).is_some());
        assert!(console::refusal(&args, true, true).is_some());
    }
}
