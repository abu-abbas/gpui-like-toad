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
    /// The editor's session has changes that are not committed. Dry-run has
    /// its own session, so rows they lock would make it wait; ask first.
    Uncommitted,
    Working,
    Ready,
    Failed(String),
}

pub(super) struct DryRun {
    sql: String,
    conn_id: u64,
    /// The editor tab whose result pane shows it.
    tab_id: u64,
    panel: Entity<ExtensionPanel>,
    process: Option<Process>,
    revision: u32,
    busy: bool,
    stage: Stage,
    polling: Option<gpui_kit::Task<()>>,
    pending: Option<Pending>,
    _subscription: Subscription,
}

impl DryRun {
    /// Whether this run is over: it failed, or the extension showed its
    /// final view (it polls only while the rows are held, so a ready view
    /// with no polling is committed or rolled back).
    fn finished(&self) -> bool {
        match self.stage {
            Stage::Failed(_) => true,
            Stage::Ready => !self.busy && self.polling.is_none(),
            Stage::Uncommitted | Stage::Working => false,
        }
    }
}

/// What the background start needs, kept while the dialog asks about
/// uncommitted changes.
struct Pending {
    program: std::path::PathBuf,
    profile: ConnectionProfile,
    profile_id: String,
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

    /// The editor tab that has the Dry-run result tab, while one is open.
    pub(super) fn dryrun_tab(&self) -> Option<u64> {
        self.dryrun.as_ref().map(|dry| dry.tab_id)
    }

    pub(super) fn open_dryrun(&mut self, cx: &mut Context<Self>) {
        // A finished dry run (committed, rolled back or failed) holds no
        // locks, so pressing Dry-run again replaces it with a fresh run.
        if self.dryrun.as_ref().is_some_and(DryRun::finished) {
            self.close_dryrun(cx);
        }
        if let Some(tab_id) = self.dryrun_tab() {
            if tab_id == self.active_tab().id {
                self.active_tab_mut().results_tab = ResultsTab::DryRun;
            } else {
                self.push_message(
                    "A dry run is still open in another tab. Close it there first.".to_string(),
                );
            }
            cx.notify();
            return;
        }
        if self.conn.busy || !self.dryrun_available() {
            return;
        }
        let sql = self.current_statement(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("The editor is empty.".to_string());
            cx.notify();
            return;
        }
        if self.needs_binds(&sql) {
            return self.ask_binds(sql, BindThen::DryRun, cx);
        }
        self.open_dryrun_resolved(sql, cx);
    }

    pub(super) fn open_dryrun_resolved(&mut self, sql: String, cx: &mut Context<Self>) {
        if self.conn.busy || !self.dryrun_available() {
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
        let uncommitted = self.conn.uncommitted;
        let tab_id = self.active_tab().id;
        self.active_tab_mut().results_tab = ResultsTab::DryRun;
        self.dryrun = Some(DryRun {
            sql,
            conn_id,
            tab_id,
            panel,
            process: None,
            revision: 0,
            busy: !uncommitted,
            stage: if uncommitted {
                Stage::Uncommitted
            } else {
                Stage::Working
            },
            polling: None,
            pending: Some(Pending {
                program,
                profile,
                profile_id,
            }),
            _subscription: subscription,
        });
        if !uncommitted {
            self.start_dryrun(cx);
        }
        cx.notify();
    }

    /// Start the extension and run the statement (after the uncommitted
    /// question, when there was one).
    pub(super) fn start_dryrun(&mut self, cx: &mut Context<Self>) {
        let Some(dry) = self.dryrun.as_mut() else {
            return;
        };
        let Some(Pending {
            program,
            profile,
            profile_id,
        }) = dry.pending.take()
        else {
            return;
        };
        dry.busy = true;
        dry.stage = Stage::Working;
        let sql = dry.sql.clone();
        let generation = dry.panel.entity_id();
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
        let Some(dry) = self.dryrun.take() else {
            return;
        };
        self.leave_dryrun_tab(dry.tab_id);
        if let Some(process) = dry.process {
            std::thread::spawn(move || finish(process));
        }
        cx.notify();
    }

    /// Show Messages again in the tab that showed the dry run.
    fn leave_dryrun_tab(&mut self, tab_id: u64) {
        for tab in self
            .tabs
            .iter_mut()
            .chain(std::iter::once(&mut self.scratch_tab))
        {
            if tab.id == tab_id && tab.results_tab == ResultsTab::DryRun {
                tab.results_tab = ResultsTab::Messages;
            }
        }
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
        // Commit: the extension commits its own session. Record the
        // statement in the history once it succeeds, as a normal run does.
        let committed = (event.node.as_str() == "commit"
            && matches!(event.event, EventKind::Click { .. }))
        .then(|| (dry.conn_id, dry.sql.clone()));
        self.dryrun_request("ui.event", json!(event), Origin::UserEvent, committed, cx);
    }

    fn dryrun_request(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        origin: Origin,
        committed: Option<(u64, String)>,
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
                    Ok(update) => {
                        if let Some((conn_id, sql)) = committed {
                            this.record_run_for(conn_id, &sql);
                        }
                        this.apply_dryrun_update(update, cx)
                    }
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
                Effect::Save { name, parts } => {
                    self.save_text_file(name.as_str().to_owned(), Effect::joined(&parts), cx)
                }
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
                    this.dryrun_request(
                        "ui.tick",
                        json!(Tick { revision }),
                        Origin::Tick,
                        None,
                        cx,
                    );
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
            Stage::Uncommitted => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_3))
                .p(px(SPACE_4))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("This connection has changes that are not committed yet"),
                )
                .child(caption_text(
                    "Dry-run runs in its own session. If your uncommitted changes touch the \
                     same rows, it waits for them and stops after 10 seconds. Commit or Roll \
                     back in the editor toolbar first, or try anyway when the changes are on \
                     other rows."
                        .to_string(),
                    theme,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(SPACE_2))
                        .child(
                            Button::new("dryrun-cancel")
                                .small()
                                .label("Close")
                                .on_click(cx.listener(|this, _, _, cx| this.close_dryrun(cx))),
                        )
                        .child(
                            Button::new("dryrun-anyway")
                                .small()
                                .primary()
                                .label("Dry-run anyway")
                                .on_click(cx.listener(|this, _, _, cx| this.start_dryrun(cx))),
                        ),
                )
                .into_any_element(),
            Stage::Working => loading_state("Running the statement without committing…", theme)
                .size_full()
                .into_any_element(),
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
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_none()
            .px(px(SPACE_3))
            .py(px(SPACE_1))
            .border_b_1()
            .border_color(theme.border)
            .child(caption_text(
                "Runs in its own Oracle session. Closing rolls back anything not committed."
                    .to_string(),
                theme,
            ))
            .child(
                // Sized to the caption beside it, not to the toolbar.
                Button::new("dryrun-close")
                    .xsmall()
                    .ghost()
                    .icon(IconName::X)
                    .label("Close")
                    .on_click(cx.listener(|this, _, _, cx| this.close_dryrun(cx))),
            );
        div()
            .id("dryrun-pane")
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h(px(0.)).child(body))
            .into_any_element()
    }
}
