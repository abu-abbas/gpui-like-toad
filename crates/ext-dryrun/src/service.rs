//! The methods of the Dry-run extension (see `docs/dry-run.md`).
//!
//! The Oracle session lives behind a mutex because a watchdog thread rolls
//! the transaction back when its time is up, while the protocol loop is
//! waiting for the next request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ora_core::client::DbClient;
use ora_core::extension_ui::{split_parts, Effect, Event, EventKind, Open, Slot, Text, Tick};
use ora_core::extension_wire as wire;
use ora_core::store::{parse_json, to_json, Json};
use serde_json::{json, Value};

use crate::engine::{self, Report};
use crate::view::{self, Phase, State};

/// How long an uncommitted dry run is kept.
pub const HOLD: Duration = Duration::from_secs(5 * 60);

struct Inner {
    client: Box<dyn DbClient>,
    phase: Phase,
    report: Option<Report>,
    deadline: Option<Instant>,
    /// When the statement ran: the rows are locked since then.
    started: Option<Instant>,
    hold: Duration,
    revision: u32,
    tab: String,
}

impl Inner {
    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1).max(1);
    }

    /// Roll back if a dry run is waiting. Returns whether one was.
    fn roll_back(&mut self, why: &str) -> Result<bool, String> {
        if self.phase != Phase::Pending {
            return Ok(false);
        }
        self.deadline = None;
        self.started = None;
        self.phase = Phase::RolledBack(why.to_string());
        self.bump();
        self.client.rollback().map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Commit the waiting dry run: exactly the rows shown are written.
    fn commit(&mut self) -> Result<(), String> {
        if self.phase != Phase::Pending {
            return Err("There is no dry run waiting to be committed.".into());
        }
        self.client.commit().map_err(|e| e.to_string())?;
        self.deadline = None;
        self.started = None;
        self.phase = Phase::Committed;
        if self.report.as_ref().is_some_and(|r| r.undo.is_some()) {
            self.tab = "undo".into();
        }
        self.bump();
        Ok(())
    }

    fn view(&self) -> Value {
        view::build(&State {
            revision: self.revision,
            phase: &self.phase,
            report: self.report.as_ref(),
            tab: &self.tab,
            remaining: self
                .deadline
                .map(|d| d.saturating_duration_since(Instant::now())),
            hold: self.hold,
            locked: self.started.map(|t| t.elapsed()),
        })
    }
}

/// `undo_<table>.sql`, with anything but letters, digits, `_` and `-`
/// in the table name turned into `_` (a schema dot, quotes).
fn undo_file_name(table: &str) -> String {
    let name: String = table
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = name.trim_matches('_');
    if name.is_empty() {
        "undo.sql".into()
    } else {
        format!("undo_{name}.sql")
    }
}

pub struct Service {
    inner: Arc<Mutex<Inner>>,
    stop: Arc<AtomicBool>,
    hold: Duration,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut inner = lock(&self.inner);
        // Closing the session would roll back anyway; say it explicitly.
        let _ = inner.roll_back("because Kilat closed it");
        inner.client.disconnect();
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

fn to_value(j: &Json) -> Result<Value, String> {
    serde_json::from_str(&to_json(j)).map_err(|e| e.to_string())
}

fn to_wire(v: &Value) -> Result<Json, String> {
    parse_json(&v.to_string()).map_err(|e| e.to_string())
}

impl Service {
    pub fn new(client: Box<dyn DbClient>, hold: Duration) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            client,
            phase: Phase::Idle,
            report: None,
            deadline: None,
            started: None,
            hold,
            revision: 1,
            tab: "changes".into(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (watch, quit) = (inner.clone(), stop.clone());
        std::thread::spawn(move || {
            while !quit.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                let mut inner = lock(&watch);
                if inner.deadline.is_some_and(|d| Instant::now() >= d) {
                    if let Err(error) = inner.roll_back("after 5 minutes") {
                        eprintln!("kilat-ext-dryrun: automatic rollback failed: {error}");
                    }
                }
            }
        });
        Self { inner, stop, hold }
    }

    pub fn handle(&mut self, method: &str, params: &Json) -> Result<Json, String> {
        let p = to_value(params)?;
        let mut inner = lock(&self.inner);
        let result = match method {
            "connect" => {
                let profile =
                    wire::profile_from_json(params.get("profile").ok_or("missing profile")?)?;
                let password = params
                    .get("password")
                    .and_then(Json::as_str)
                    .ok_or("missing password")?;
                if profile.kind != ora_core::DbKind::Oracle {
                    return Err("Dry-run works with Oracle connections only.".into());
                }
                inner.client.disconnect();
                let info = inner
                    .client
                    .connect(&profile, password)
                    .map_err(|e| e.to_string())?;
                return Ok(wire::db_info_to_json(&info));
            }
            "disconnect" => {
                inner.roll_back("because the connection closed")?;
                inner.client.disconnect();
                json!({})
            }
            "dryrun.run" => {
                let sql = p
                    .get("sql")
                    .and_then(Value::as_str)
                    .ok_or("missing sql")?
                    .to_string();
                if !inner.client.is_connected() {
                    return Err("not connected".into());
                }
                inner.roll_back("because a new dry run started")?;
                match engine::run(inner.client.as_mut(), &sql) {
                    Ok(report) => {
                        let summary = json!({
                            "affected": report.affected,
                            "changed": report.changed_rows,
                        });
                        inner.phase = Phase::Pending;
                        inner.deadline = Some(Instant::now() + self.hold);
                        inner.started = Some(Instant::now());
                        inner.report = Some(report);
                        inner.tab = "changes".into();
                        inner.bump();
                        summary
                    }
                    Err(error) => {
                        // A statement that failed halfway must not stay open.
                        let _ = inner.client.rollback();
                        return Err(error);
                    }
                }
            }
            "dryrun.rollback" => {
                inner.roll_back("by you")?;
                json!({})
            }
            "ui.open" => {
                let open: Open = serde_json::from_value(p).map_err(|e| e.to_string())?;
                if open.slot != Slot::Main {
                    return Err("Dry-run only has a main view.".into());
                }
                inner.view()
            }
            "ui.tick" => {
                serde_json::from_value::<Tick>(p).map_err(|e| e.to_string())?;
                inner.view()
            }
            "ui.event" => {
                let event: Event = serde_json::from_value(p).map_err(|e| e.to_string())?;
                let mut effects = Vec::new();
                match (event.node.as_str(), event.event) {
                    ("rollback", EventKind::Click { .. }) => {
                        inner.roll_back("by you")?;
                    }
                    ("copy-undo", EventKind::Click { .. }) => {
                        if let Some(script) = inner.report.as_ref().and_then(|r| r.undo.clone()) {
                            effects.push(if view::fits_view(&script) {
                                Effect::Copy {
                                    text: Text::new(script).map_err(str::to_owned)?,
                                }
                            } else {
                                Effect::Toast {
                                    text: Text::new(
                                        "The undo script is too long to copy. Use Export .sql."
                                            .into(),
                                    )
                                    .map_err(str::to_owned)?,
                                }
                            });
                        }
                    }
                    ("export-undo" | "export-undo-file", EventKind::Click { .. }) => {
                        if let Some(report) = inner.report.as_ref() {
                            if let Some(script) = &report.undo {
                                effects.push(Effect::Save {
                                    name: Text::new(undo_file_name(&report.table))
                                        .map_err(str::to_owned)?,
                                    parts: split_parts(script),
                                });
                            }
                        }
                    }
                    ("view", EventKind::Tab { item }) => {
                        inner.tab = item.as_str().to_string();
                        inner.bump();
                    }
                    ("commit", EventKind::Click { .. }) => {
                        inner.commit()?;
                        let rows = inner.report.as_ref().map_or(0, |r| r.affected);
                        effects.push(Effect::Toast {
                            text: Text::new(format!("Dry-run committed: {rows} row(s)."))
                                .map_err(str::to_owned)?,
                        });
                    }
                    _ => return Err("Unknown Dry-run event.".into()),
                }
                let mut view = inner.view();
                view["effects"] = serde_json::to_value(&effects).map_err(|e| e.to_string())?;
                view
            }
            other => return Err(format!("unknown method {other}")),
        };
        to_wire(&result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::{dept10, int, raise, text, Fake};
    use ora_core::extension_ui::{decode_update, Origin};

    fn service(hold: Duration) -> (Service, Arc<Mutex<Inner>>) {
        let service = Service::new(Box::new(Fake::new(dept10, raise)), hold);
        let inner = service.inner.clone();
        (service, inner)
    }

    fn call(s: &mut Service, method: &str, params: Value) -> Result<Value, String> {
        s.handle(method, &to_wire(&params)?)
            .and_then(|j| to_value(&j))
    }

    fn rolled_back(inner: &Arc<Mutex<Inner>>) -> bool {
        matches!(lock(inner).phase, Phase::RolledBack(_))
    }

    #[test]
    fn run_then_view_then_rollback() {
        let (mut s, inner) = service(HOLD);
        let summary = call(
            &mut s,
            "dryrun.run",
            json!({"sql": "UPDATE emp SET salary = salary + 10 WHERE dept = 10"}),
        )
        .unwrap();
        assert_eq!(summary, json!({"affected": 2, "changed": 2}));
        let view = call(&mut s, "ui.open", json!({"slot": "main"})).unwrap();
        let update = decode_update(&view.to_string(), Origin::Open).unwrap();
        assert_eq!(update.poll_ms, Some(1000));
        let revision = update.revision;
        let after = call(
            &mut s,
            "ui.event",
            json!({"revision": revision, "slot": "main", "node": "rollback",
                   "event": {"type": "click"}}),
        )
        .unwrap();
        let update = decode_update(&after.to_string(), Origin::UserEvent).unwrap();
        assert!(update.revision > revision);
        assert_eq!(update.poll_ms, None);
        assert!(rolled_back(&inner));
        assert!(after.to_string().contains("Rolled back by you"));
    }

    #[test]
    fn commit_writes_the_dry_run_and_stops_the_timer() {
        let (mut s, inner) = service(HOLD);
        call(
            &mut s,
            "dryrun.run",
            json!({"sql": "UPDATE emp SET salary = salary + 10 WHERE dept = 10"}),
        )
        .unwrap();
        let revision = lock(&inner).revision;
        let after = call(
            &mut s,
            "ui.event",
            json!({"revision": revision, "slot": "main", "node": "commit",
                   "event": {"type": "click"}}),
        )
        .unwrap();
        let update = decode_update(&after.to_string(), Origin::UserEvent).unwrap();
        assert_eq!(update.poll_ms, None);
        assert_eq!(lock(&inner).phase, Phase::Committed);
        assert!(lock(&inner).deadline.is_none());
        assert!(after.to_string().contains("Dry-run committed: 2 row(s)."));
        // Nothing is waiting any more: a second commit is refused.
        let revision = lock(&inner).revision;
        call(
            &mut s,
            "ui.event",
            json!({"revision": revision, "slot": "main", "node": "commit",
                   "event": {"type": "click"}}),
        )
        .unwrap_err();
    }

    #[test]
    fn the_watchdog_rolls_back_at_the_deadline() {
        let (mut s, inner) = service(Duration::from_millis(300));
        call(
            &mut s,
            "dryrun.run",
            json!({"sql": "DELETE FROM emp WHERE dept = 10"}),
        )
        .unwrap();
        assert!(!rolled_back(&inner));
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(
            lock(&inner).phase,
            Phase::RolledBack("after 5 minutes".into())
        );
    }

    #[test]
    fn a_refused_statement_is_an_error_and_leaves_nothing_pending() {
        let (mut s, inner) = service(HOLD);
        let error = call(
            &mut s,
            "dryrun.run",
            json!({"sql": "MERGE INTO t USING s ON (1=1)"}),
        )
        .unwrap_err();
        assert!(error.contains("MERGE"));
        assert_eq!(lock(&inner).phase, Phase::Idle);
    }

    #[test]
    fn copy_undo_sends_the_script_as_an_effect() {
        let (mut s, _) = service(HOLD);
        call(
            &mut s,
            "dryrun.run",
            json!({"sql": "DELETE FROM emp WHERE dept = 10"}),
        )
        .unwrap();
        let view = call(&mut s, "ui.open", json!({"slot": "main"})).unwrap();
        let revision = view["revision"].as_u64().unwrap();
        let reply = call(
            &mut s,
            "ui.event",
            json!({"revision": revision, "slot": "main", "node": "copy-undo",
                   "event": {"type": "click"}}),
        )
        .unwrap();
        assert_eq!(reply["effects"][0]["type"], "copy");
        assert!(reply["effects"][0]["text"]
            .as_str()
            .unwrap()
            .contains("INSERT INTO emp"));
    }

    #[test]
    fn export_undo_sends_the_whole_script_as_a_file() {
        let (mut s, _) = service(HOLD);
        call(
            &mut s,
            "dryrun.run",
            json!({"sql": "UPDATE emp SET salary = salary + 10 WHERE dept = 10"}),
        )
        .unwrap();
        let view = call(&mut s, "ui.open", json!({"slot": "main"})).unwrap();
        let revision = view["revision"].as_u64().unwrap();
        let reply = call(
            &mut s,
            "ui.event",
            json!({"revision": revision, "slot": "main", "node": "export-undo",
                   "event": {"type": "click"}}),
        )
        .unwrap();
        let update = decode_update(&reply.to_string(), Origin::UserEvent).unwrap();
        match &update.effects[0] {
            Effect::Save { name, parts } => {
                assert_eq!(name.as_str(), "undo_emp.sql");
                let script = Effect::joined(parts);
                assert!(script.contains("UPDATE emp SET"), "{script}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_large_script_is_previewed_and_exported_whole_within_the_reply_limit() {
        let mut db = Fake::new(|_| true, |rows| rows.len() as u64);
        // Quotes double when JSON-escaped, the worst case for the reply.
        let wide = "'\"".repeat(1400);
        db.rows = (0..650)
            .map(|n| (format!("R{n}"), vec![int(n), text(&wide), int(1), int(1)]))
            .collect();
        let mut s = Service::new(Box::new(db), HOLD);
        call(&mut s, "dryrun.run", json!({"sql": "DELETE FROM emp"})).unwrap();
        let script = lock(&s.inner)
            .report
            .as_ref()
            .unwrap()
            .undo
            .clone()
            .unwrap();
        assert!(script.len() > 2_000_000, "{}", script.len());
        let revision = lock(&s.inner).revision;
        let view = call(
            &mut s,
            "ui.event",
            json!({"revision": revision, "slot": "main", "node": "view",
                   "event": {"type": "tab", "item": "undo"}}),
        )
        .unwrap();
        let body = view.to_string();
        decode_update(&body, Origin::UserEvent).unwrap();
        assert!(body.contains("Export .sql for the whole script"));
        let revision = lock(&s.inner).revision;
        let reply = call(
            &mut s,
            "ui.event",
            json!({"revision": revision, "slot": "main",
                   "node": "export-undo-file", "event": {"type": "click"}}),
        )
        .unwrap();
        let update = decode_update(&reply.to_string(), Origin::UserEvent).unwrap();
        match &update.effects[0] {
            Effect::Save { parts, .. } => assert_eq!(Effect::joined(parts), script),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn undo_file_names_are_plain() {
        assert_eq!(undo_file_name("HR.EMP"), "undo_HR_EMP.sql");
        assert_eq!(undo_file_name("\"My Table\""), "undo_My_Table.sql");
        assert_eq!(undo_file_name("\"\""), "undo.sql");
    }

    #[test]
    fn an_unknown_event_is_an_error() {
        let (mut s, _) = service(HOLD);
        let error = call(
            &mut s,
            "ui.event",
            json!({"revision": 1, "slot": "main", "node": "nope", "event": {"type": "click"}}),
        )
        .unwrap_err();
        assert!(error.contains("Unknown"));
    }
}
