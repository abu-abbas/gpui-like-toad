//! Workbench side of the Dry-run extension (see `docs/dry-run.md`): the
//! toolbar button's action and a dialog that draws the extension's view.
//! The extension runs the statement in its own Oracle session and never
//! commits; "Run for real" runs the statement through the editor as usual.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ora_core::credentials::{CredentialStore as _, ExposeSecret as _};
use ora_core::extension_host::ExtProcess;
use ora_core::extension_ui::{
    decode_update, Effect, Event, EventKind, Open, Origin, Slot, Tick, ViewUpdate,
};
use serde_json::json;

use super::extension_panel::ExtensionPanel;
use super::tools::spawn_tool_job;
use super::*;

pub(super) const ID: &str = "dryrun";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// A big UPDATE can take long; the statement itself is the slow call.
const RUN_TIMEOUT: Duration = Duration::from_secs(600);
const TIMEOUT: Duration = Duration::from_secs(20);

type Process = Arc<Mutex<ExtProcess>>;

enum Stage {
    Working,
    Ready,
    Failed(String),
}

pub(super) struct DryRun {
    sql: String,
    conn_id: u64,
    panel: Entity<ExtensionPanel>,
    process: Option<Process>,
    revision: u32,
    busy: bool,
    stage: Stage,
    polling: Option<gpui_kit::Task<()>>,
    _subscription: Subscription,
}

struct Started {
    process: Process,
    update: ViewUpdate,
}

fn call(
    process: &mut ExtProcess,
    method: &str,
    payload: serde_json::Value,
    timeout: Duration,
    origin: Origin,
) -> Result<ViewUpdate, ora_core::extension_host::CallError> {
    let params = store::parse_json(&payload.to_string()).map_err(|err| process.violation(&err))?;
    let reply = process.call(method, params, timeout)?;
    decode_update(&store::to_json(&reply), origin).map_err(|err| process.violation(&err))
}

/// Start the extension, open its session, run the statement and fetch the
/// first view. Everything blocking, so it runs on the background executor.
fn start(
    program: std::path::PathBuf,
    profile: ConnectionProfile,
    profile_id: String,
    sql: String,
) -> Result<Started, String> {
    let password = match crate::keychain::store().get(&profile_id) {
        Ok(Some(password)) => password,
        Ok(None) => return Err(crate::worker::NO_SAVED_PASSWORD.to_string()),
        Err(err) => return Err(err.to_string()),
    };
    let mut process = ExtProcess::start(&program, ID, env!("CARGO_PKG_VERSION"), START_TIMEOUT)
        .map_err(|err| err.to_string())?;
    let result = (|| {
        let params = store::Json::Obj(vec![
            (
                "profile".to_string(),
                ora_core::extension_wire::profile_to_json(&profile),
            ),
            (
                "password".to_string(),
                store::Json::Str(password.expose_secret().to_string()),
            ),
        ]);
        process
            .call("connect", params, CONNECT_TIMEOUT)
            .map_err(|err| err.to_string())?;
        process
            .call(
                "dryrun.run",
                store::parse_json(&json!({ "sql": sql }).to_string())?,
                RUN_TIMEOUT,
            )
            .map_err(|err| err.to_string())?;
        call(
            &mut process,
            "ui.open",
            json!(Open { slot: Slot::Main }),
            TIMEOUT,
            Origin::Open,
        )
        .map_err(|err| err.to_string())
    })();
    match result {
        Ok(update) => Ok(Started {
            process: Arc::new(Mutex::new(process)),
            update,
        }),
        Err(error) => {
            let tail = process.stderr_tail();
            let _ = process.call("disconnect", store::Json::Null, TIMEOUT);
            process.stop();
            Err(if tail.trim().is_empty() || error.contains(tail.trim()) {
                error
            } else {
                format!("{error}\n{}", tail.trim())
            })
        }
    }
}

/// Roll back and stop. `disconnect` rolls a waiting dry run back; the
/// session closing would do the same.
fn finish(process: Process) {
    let mut process = process.lock().unwrap_or_else(|err| err.into_inner());
    let _ = process.call("disconnect", store::Json::Null, TIMEOUT);
    process.stop();
}

impl Workbench {
    /// Whether the toolbar offers Dry-run: the extension is installed and
    /// the active connection is a real Oracle one.
    pub(super) fn dryrun_available(&self) -> bool {
        self.extension_ready(ID)
            && self.conn.session.is_some()
            && self
                .conn
                .last_connection
                .as_ref()
                .is_some_and(|last| !last.demo && last.profile.kind == DbKind::Oracle)
    }

    pub(super) fn open_dryrun(&mut self, cx: &mut Context<Self>) {
        if self.dryrun.is_some() || self.conn.busy || !self.dryrun_available() {
            return;
        }
        let sql = self.current_statement(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("The editor is empty.".to_string());
            cx.notify();
            return;
        }
        let Some(program) = store::remote_install(ID).map(|install| install.path) else {
            self.push_message("Install Dry-run in Settings > Extensions first.".to_string());
            cx.notify();
            return;
        };
        let Some(last) = self.conn.last_connection.as_ref() else {
            return;
        };
        let profile = last.profile.clone();
        let saved = last
            .profile_ix
            .and_then(|ix| self.profiles.get(ix))
            .filter(|saved| saved.save_password)
            .map(|saved| saved.id.clone());
        let Some(profile_id) = saved else {
            self.push_message(
                "Dry-run opens its own session, so it needs the password saved for this \
                 connection (tick Save password in the connection and connect again)."
                    .to_string(),
            );
            cx.notify();
            return;
        };
        let production = profile.access.environment == ConnectionEnvironment::Production;
        let read_only = profile.access.read_only;
        let conn_id = self.conn.id;
        let panel = cx.new(|_| ExtensionPanel::new(Slot::Main, read_only, production));
        let generation = panel.entity_id();
        let subscription = cx.subscribe(&panel, move |this, _, event: &Event, cx| {
            this.dryrun_event(generation, event, cx);
        });
        self.dryrun = Some(DryRun {
            sql: sql.clone(),
            conn_id,
            panel,
            process: None,
            revision: 0,
            busy: true,
            stage: Stage::Working,
            polling: None,
            _subscription: subscription,
        });
        spawn_tool_job(
            cx,
            move || start(program, profile, profile_id, sql),
            move |this, result, cx| {
                let Some(dry) = this
                    .dryrun
                    .as_mut()
                    .filter(|dry| dry.panel.entity_id() == generation)
                else {
                    // Closed while it was running: do not leave it open.
                    if let Ok(started) = result {
                        std::thread::spawn(move || finish(started.process));
                    }
                    return;
                };
                dry.busy = false;
                match result {
                    Ok(started) => {
                        dry.process = Some(started.process);
                        dry.stage = Stage::Ready;
                        this.apply_dryrun_update(started.update, cx);
                    }
                    Err(error) => dry.stage = Stage::Failed(error),
                }
            },
        );
        cx.notify();
    }

    /// Close the dialog and roll the dry run back.
    pub(super) fn close_dryrun(&mut self, cx: &mut Context<Self>) {
        if let Some(process) = self.dryrun.take().and_then(|dry| dry.process) {
            std::thread::spawn(move || finish(process));
        }
        cx.notify();
    }

    fn dryrun_event(
        &mut self,
        generation: gpui_kit::EntityId,
        event: &Event,
        cx: &mut Context<Self>,
    ) {
        let Some(dry) = self
            .dryrun
            .as_ref()
            .filter(|dry| dry.panel.entity_id() == generation && dry.revision == event.revision)
        else {
            return;
        };
        if dry.busy {
            return;
        }
        if event.node.as_str() == "run-for-real" && matches!(event.event, EventKind::Click { .. }) {
            self.dryrun_run_for_real(cx);
            return;
        }
        self.dryrun_request("ui.event", json!(event), Origin::UserEvent, cx);
    }

    /// Roll the dry run back first (it holds the row locks), then run the
    /// statement through the editor's normal path.
    fn dryrun_run_for_real(&mut self, cx: &mut Context<Self>) {
        let Some(dry) = self.dryrun.take() else {
            return;
        };
        let (sql, conn_id, process) = (dry.sql, dry.conn_id, dry.process);
        cx.notify();
        spawn_tool_job(
            cx,
            move || {
                if let Some(process) = process {
                    finish(process);
                }
            },
            move |this, (), cx| {
                if this.conn.id == conn_id {
                    this.run_sql(sql, cx);
                } else {
                    this.push_message(
                        "Switch back to the connection of the statement to run it.".to_string(),
                    );
                }
            },
        );
    }

    fn dryrun_request(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        let Some(dry) = self.dryrun.as_mut().filter(|dry| !dry.busy) else {
            return;
        };
        let Some(process) = dry.process.clone() else {
            return;
        };
        dry.busy = true;
        let generation = dry.panel.entity_id();
        spawn_tool_job(
            cx,
            move || {
                let mut process = process.lock().unwrap_or_else(|err| err.into_inner());
                let result = call(&mut process, method, params, TIMEOUT, origin);
                (result, process.is_broken(), process.stderr_tail())
            },
            move |this, (result, broken, stderr), cx| {
                let Some(dry) = this
                    .dryrun
                    .as_mut()
                    .filter(|dry| dry.panel.entity_id() == generation)
                else {
                    return;
                };
                dry.busy = false;
                match result {
                    Ok(update) => this.apply_dryrun_update(update, cx),
                    Err(error) if broken || origin == Origin::Tick => {
                        let error = if stderr.is_empty() {
                            error.to_string()
                        } else {
                            format!("{error}\n{stderr}")
                        };
                        this.dryrun_failed(error, cx);
                    }
                    Err(error) => this.push_message(error.to_string()),
                }
            },
        );
    }

    fn apply_dryrun_update(&mut self, update: ViewUpdate, cx: &mut Context<Self>) {
        let Some(dry) = self.dryrun.as_mut() else {
            return;
        };
        if update.revision < dry.revision {
            return;
        }
        dry.revision = update.revision;
        let tree = update
            .slots
            .into_iter()
            .find(|item| item.slot == Slot::Main)
            .map(|item| item.tree);
        dry.panel
            .update(cx, |panel, cx| panel.update(update.revision, tree, cx));
        for effect in update.effects {
            match effect {
                Effect::Copy { text } => {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                        text.as_str().to_owned(),
                    ));
                    self.push_message("Undo script copied.".to_string());
                }
                Effect::Toast { text } => self.push_message(text.as_str().to_owned()),
                Effect::OpenUrl { url } => cx.open_url(url.as_str()),
                Effect::Confirm { .. } => {}
            }
        }
        self.schedule_dryrun_poll(update.poll_ms, cx);
        cx.notify();
    }

    fn schedule_dryrun_poll(&mut self, poll_ms: Option<u32>, cx: &mut Context<Self>) {
        let Some(dry) = self.dryrun.as_mut() else {
            return;
        };
        let generation = dry.panel.entity_id();
        dry.polling = poll_ms.map(|ms| {
            cx.spawn(async move |view, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(ms.into()))
                    .await;
                let again = view.update(cx, |this, cx| {
                    let Some(dry) = this
                        .dryrun
                        .as_ref()
                        .filter(|dry| dry.panel.entity_id() == generation)
                    else {
                        return false;
                    };
                    if dry.process.is_none() {
                        return false;
                    }
                    if dry.busy {
                        return true;
                    }
                    let revision = dry.revision;
                    this.dryrun_request("ui.tick", json!(Tick { revision }), Origin::Tick, cx);
                    false
                });
                if !matches!(again, Ok(true)) {
                    break;
                }
            })
        });
    }

    fn dryrun_failed(&mut self, error: String, cx: &mut Context<Self>) {
        let Some(dry) = self.dryrun.as_mut() else {
            return;
        };
        crate::error_log::append(&format!("Dry-run extension: {error}"));
        dry.polling = None;
        dry.stage = Stage::Failed(error);
        if let Some(process) = dry.process.take() {
            std::thread::spawn(move || finish(process));
        }
        cx.notify();
    }

    pub(super) fn render_dryrun(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(dry) = self.dryrun.as_ref() else {
            return div().into_any_element();
        };
        let body = match &dry.stage {
            Stage::Working => {
                loading_state("Running the statement without committing…", theme).into_any_element()
            }
            Stage::Failed(error) => div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .p(px(SPACE_4))
                .child("Dry-run could not finish")
                .child(caption_text(error.clone(), theme))
                .into_any_element(),
            Stage::Ready => dry.panel.clone().into_any_element(),
        };
        let card = div()
            .id("dryrun-dialog")
            .flex()
            .flex_col()
            .w(px(860.))
            .max_w_full()
            .h(px(560.))
            .max_h_full()
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_dryrun(cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .p(px(SPACE_3))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Dry-run"),
                    )
                    .child(
                        Button::new("dryrun-close")
                            .small()
                            .ghost()
                            .label("Close")
                            .tooltip("Close and roll back")
                            .on_click(cx.listener(|this, _, _, cx| this.close_dryrun(cx))),
                    ),
            )
            .child(div().flex_1().min_h(px(0.)).child(body));
        div()
            .id("dryrun-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(60.))
            .pb(px(24.))
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
            .into_any_element()
    }
}
