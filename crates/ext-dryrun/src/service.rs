//! The methods of the Dry-run extension (see `docs/dry-run.md`).
//!
//! The Oracle session lives behind a mutex because a watchdog thread rolls
//! the transaction back when its time is up, while the protocol loop is
//! waiting for the next request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ora_core::client::DbClient;
use ora_core::extension_ui::{Effect, Event, EventKind, Open, Slot, Text, Tick};
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
        self.phase = Phase::RolledBack(why.to_string());
        self.bump();
        self.client.rollback().map_err(|e| e.to_string())?;
        Ok(true)
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
        })
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
                            effects.push(Effect::Copy {
                                text: Text::new(script).map_err(str::to_owned)?,
                            });
                        }
                    }
                    ("view", EventKind::Tab { item }) => {
                        inner.tab = item.as_str().to_string();
                        inner.bump();
                    }
                    // The host runs the statement for real; the extension
                    // has nothing to do.
                    ("run-for-real", EventKind::Click { .. }) => {}
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
    use crate::engine::tests::{dept10, raise, Fake};
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
