//! Workbench side of a developer tool that runs as an extension: starts its
//! program, forwards view events and applies the trees it sends back.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use devtools::mailhog::MailHogTarget;
use devtools::mailhog_wire::target_to_value;
use ora_core::extension_host::{CallError, ExtProcess};
use ora_core::extension_ui::{decode_update, Effect, Event, Open, Origin, Slot, Tick, ViewUpdate};
use ora_core::store::{ToolInstance, ToolKind};
use serde_json::json;

use super::extension_panel::ExtensionPanel;
use super::tools::{resolve_password, spawn_tool_job};
use super::*;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const TIMEOUT: Duration = Duration::from_secs(20);

type Process = Arc<Mutex<ExtProcess>>;
type Generation = gpui_kit::EntityId;

impl super::tools::ToolsState {
    /// The session whose panels belong to `generation` (a stale reply for
    /// a closed or reopened session finds none).
    fn pilot_by_gen(&mut self, generation: Generation) -> Option<&mut ToolPilot> {
        self.pilots
            .values_mut()
            .find(|pilot| pilot.explorer.entity_id() == generation)
    }

    /// The session of the tool shown now.
    fn active_pilot(&self) -> Option<&ToolPilot> {
        self.active.as_deref().and_then(|id| self.pilots.get(id))
    }
}

pub(super) struct ToolPilot {
    pub instance: String,
    pub kind: ToolKind,
    program: PathBuf,
    sidebar: Entity<ExtensionPanel>,
    explorer: Entity<ExtensionPanel>,
    main: Entity<ExtensionPanel>,
    process: Option<Process>,
    server: ServerOwnership,
    stopped: bool,
    revision: u32,
    busy: bool,
    failure: Option<String>,
    polling: Option<gpui_kit::Task<()>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ServerOwnership {
    Unreported,
    External,
    Managed,
}

impl From<Option<bool>> for ServerOwnership {
    fn from(reported: Option<bool>) -> Self {
        match reported {
            None => Self::Unreported,
            Some(false) => Self::External,
            Some(true) => Self::Managed,
        }
    }
}

impl ToolPilot {
    fn panel(&self, slot: Slot) -> &Entity<ExtensionPanel> {
        match slot {
            Slot::Sidebar => &self.sidebar,
            Slot::Explorer => &self.explorer,
            Slot::Main => &self.main,
        }
    }

    pub fn reachable(&self) -> bool {
        self.process.is_some() && self.failure.is_none()
    }

    pub(super) fn manages_server(&self) -> bool {
        self.server_ownership() == Some(ServerOwnership::Managed)
    }

    pub(super) fn server_ownership(&self) -> Option<ServerOwnership> {
        self.reachable().then_some(self.server)
    }

    pub(super) fn needs_reopen(&self, program: &std::path::Path) -> bool {
        self.program != program && !self.connecting()
    }

    pub(super) fn connecting(&self) -> bool {
        !self.reachable() && self.busy
    }
}

struct Connected {
    process: Process,
    update: ViewUpdate,
    server: ServerOwnership,
}

fn call(
    process: &mut ExtProcess,
    method: &str,
    payload: serde_json::Value,
    origin: Origin,
) -> Result<ViewUpdate, CallError> {
    let params = store::parse_json(&payload.to_string()).map_err(|err| process.violation(&err))?;
    let reply = process.call(method, params, TIMEOUT)?;
    decode_update(&store::to_json(&reply), origin).map_err(|err| process.violation(&err))
}

/// What `connect` needs for this tool: the target, its limits and the
/// local-server settings.
fn connect_payload(
    tool: &ToolInstance,
    password: Option<String>,
    start_server: bool,
) -> serde_json::Value {
    let mut payload = match tool.kind {
        ToolKind::MailHog => json!({
            "target": target_to_value(&MailHogTarget {
                host: tool.host.clone(),
                port: tool.port,
                https: tool.tls,
                username: tool.username.clone(),
                password,
            }),
        }),
        ToolKind::Redis => json!({
            "target": {
                "host": tool.host,
                "port": tool.port,
                "username": tool.username,
                "password": password,
                "tls": tool.tls,
            },
            "database": tool.database,
            "allow_dangerous": tool.allow_dangerous,
        }),
    };
    if let Some(fields) = payload.as_object_mut() {
        fields.insert("read_only".into(), json!(tool.read_only()));
        fields.insert("start_server".into(), json!(start_server));
        fields.insert("server_command".into(), json!(tool.server_command));
        fields.insert("instance_id".into(), json!(tool.id));
        fields.insert("name".into(), json!(tool.name));
    }
    payload
}

/// The form's Test button for a tool that only exists as an extension:
/// connect once through its program, then disconnect. Never starts a server.
pub(super) fn test_connection(
    tool: &ToolInstance,
    password: Option<String>,
) -> Result<String, String> {
    let id = tool.kind.extension_id();
    let install = store::remote_install(id).ok_or_else(|| {
        format!(
            "Install the {} extension in Settings > Extensions to test this connection.",
            tool.kind.label()
        )
    })?;
    let mut process =
        ExtProcess::start(&install.path, id, env!("CARGO_PKG_VERSION"), START_TIMEOUT)
            .map_err(|err| err.to_string())?;
    let payload = connect_payload(tool, password, false);
    let reply = process
        .call(
            "connect",
            store::parse_json(&payload.to_string())?,
            Duration::from_secs(30),
        )
        .map_err(|err| err.to_string());
    let _ = process.call("disconnect", store::Json::Null, TIMEOUT);
    process.stop();
    let reply = reply?;
    let version = reply
        .get("version")
        .and_then(store::Json::as_str)
        .unwrap_or("?");
    Ok(format!(
        "Connected to {} {version} at {}.",
        tool.kind.label(),
        tool.address()
    ))
}

fn start(program: PathBuf, tool: ToolInstance, typed: Option<String>) -> Result<Connected, String> {
    let password = resolve_password(&tool, typed);
    let id = tool.kind.extension_id();
    let payload = connect_payload(&tool, password, tool.start_server);
    let mut process = ExtProcess::start(&program, id, env!("CARGO_PKG_VERSION"), START_TIMEOUT)
        .map_err(|err| err.to_string())?;
    let reply = process
        .call(
            "connect",
            store::parse_json(&payload.to_string())?,
            Duration::from_secs(30),
        )
        .map_err(|err| err.to_string())?;
    // Older extensions omit this field. Only offer Stop when ownership is known.
    let server = ServerOwnership::from(reply.get("managed_server").and_then(store::Json::as_bool));
    let update = call(
        &mut process,
        "ui.open",
        json!(Open {
            slot: Slot::Explorer
        }),
        Origin::Open,
    )
    .map_err(|err| err.to_string())?;
    Ok(Connected {
        process: Arc::new(Mutex::new(process)),
        update,
        server,
    })
}

fn disconnect(process: Process) {
    let mut process = process.lock().unwrap_or_else(|err| err.into_inner());
    // Both released builds acknowledge disconnect after dropping their child.
    let _ = process.call("disconnect", store::Json::Null, TIMEOUT);
    process.stop();
}

impl Workbench {
    /// The downloaded program of an installed tool extension, if any.
    pub(super) fn tool_extension(&self, tool: &ToolInstance) -> Option<store::RemoteInstall> {
        if self.extensions.tool_enabled(tool.kind) {
            store::remote_install(tool.kind.extension_id())
        } else {
            None
        }
    }

    pub(super) fn open_tool_pilot(
        &mut self,
        tool: ToolInstance,
        program: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let previous = self
            .tools
            .pilots
            .remove(&tool.id)
            .and_then(|pilot| pilot.process);
        let start_requested = super::tools_server::can_start(&tool);
        let label = tool.kind.label();
        let read_only = tool.read_only();
        let prod = tool.environment == ConnectionEnvironment::Production;
        let sidebar = cx.new(|_| ExtensionPanel::new(Slot::Sidebar, read_only, prod));
        let explorer =
            cx.new(|_| ExtensionPanel::new(Slot::Explorer, read_only, prod).layout(tool.kind));
        let main = cx.new(|_| ExtensionPanel::new(Slot::Main, read_only, prod).layout(tool.kind));
        let generation = explorer.entity_id();
        let subscriptions = [&sidebar, &explorer, &main]
            .into_iter()
            .map(|panel| {
                cx.subscribe(panel, move |this, _, event: &Event, cx| {
                    this.pilot_event(generation, event, cx);
                })
            })
            .collect();
        let typed = self.tools.session_passwords.get(&tool.id).cloned();
        self.tools.pilots.insert(
            tool.id.clone(),
            ToolPilot {
                instance: tool.id.clone(),
                kind: tool.kind,
                program: program.clone(),
                sidebar,
                explorer,
                main,
                process: None,
                server: ServerOwnership::Unreported,
                stopped: false,
                revision: 0,
                busy: true,
                failure: None,
                polling: None,
                _subscriptions: subscriptions,
            },
        );
        spawn_tool_job(
            cx,
            move || {
                if let Some(previous) = previous {
                    disconnect(previous);
                }
                start(program, tool, typed)
            },
            move |this, result, cx| {
                let Some(pilot) = this.tools.pilot_by_gen(generation) else {
                    return;
                };
                pilot.busy = false;
                match result {
                    Ok(connected) => {
                        pilot.process = Some(connected.process);
                        pilot.server = connected.server;
                        this.apply_pilot_update(generation, connected.update, cx);
                        if start_requested && connected.server == ServerOwnership::External {
                            this.push_message(format!(
                                "{label} was already running. Connected to the existing server."
                            ));
                        }
                    }
                    Err(error) => this.pilot_failed(generation, error, cx),
                }
            },
        );
        cx.notify();
    }

    pub(super) fn stop_pilot_server(&mut self, id: &str, restart: bool, cx: &mut Context<Self>) {
        let Some(pilot) = self
            .tools
            .pilots
            .get_mut(id)
            .filter(|pilot| pilot.manages_server())
        else {
            return;
        };
        let Some(process) = pilot.process.take() else {
            return;
        };
        let generation = pilot.explorer.entity_id();
        pilot.polling = None;
        pilot.busy = true;
        let id = id.to_owned();
        self.tools.pilot_stopping.insert(id.clone());
        spawn_tool_job(
            cx,
            move || {
                let mut process = process.lock().unwrap_or_else(|err| err.into_inner());
                call(
                    &mut process,
                    "server.stop",
                    serde_json::Value::Null,
                    Origin::UserEvent,
                )
                .map_err(|err| err.to_string())
            },
            move |this, result, cx| {
                this.tools.pilot_stopping.remove(&id);
                if let Some(pilot) = this.tools.pilot_by_gen(generation) {
                    pilot.busy = false;
                    match result {
                        Ok(update) => {
                            pilot.stopped = true;
                            this.apply_pilot_update(generation, update, cx);
                            if restart {
                                if let Some(tool) = this.tools.get(&id).cloned() {
                                    if let Some(install) = this.tool_extension(&tool) {
                                        this.open_tool_pilot(tool, install.path, cx);
                                    }
                                }
                            }
                        }
                        Err(error) => this.pilot_failed(generation, error, cx),
                    }
                }
            },
        );
        cx.notify();
    }

    /// Disconnect tool `id`'s extension session, if it has one.
    pub(super) fn close_tool_pilot(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(pilot) = self.tools.pilots.remove(id) {
            if let Some(process) = pilot.process {
                self.retire_pilot_process(pilot.instance, process, cx);
            }
        }
    }

    fn retire_pilot_process(&mut self, id: String, process: Process, cx: &mut Context<Self>) {
        self.tools.pilot_stopping.insert(id.clone());
        spawn_tool_job(
            cx,
            move || disconnect(process),
            move |this, (), _| {
                this.tools.pilot_stopping.remove(&id);
            },
        );
    }

    /// Close the open view of the tool extension `id` before it is removed.
    /// `false` while one of its requests is still running.
    pub(in crate::workbench) fn stop_pilot_before_uninstall(&mut self, id: &str) -> bool {
        let ours = |pilot: &ToolPilot| pilot.kind.extension_id() == id;
        if self.tools.instances.iter().any(|tool| {
            tool.kind.extension_id() == id && self.tools.pilot_stopping.contains(&tool.id)
        }) || self
            .tools
            .pilots
            .values()
            .any(|pilot| ours(pilot) && pilot.busy)
        {
            return false;
        }
        let closing: Vec<String> = self
            .tools
            .pilots
            .values()
            .filter(|pilot| ours(pilot))
            .map(|pilot| pilot.instance.clone())
            .collect();
        for id in closing {
            self.tools.pilots.remove(&id);
            if self.tools.active.as_deref() == Some(id.as_str()) {
                self.tools.active = None;
            }
        }
        true
    }

    fn pilot_event(
        &mut self,
        generation: gpui_kit::EntityId,
        event: &Event,
        cx: &mut Context<Self>,
    ) {
        let Some(pilot) = self.tools.active_pilot().filter(|pilot| {
            pilot.explorer.entity_id() == generation && pilot.revision == event.revision
        }) else {
            return;
        };
        if pilot.busy {
            if !matches!(
                event.event,
                ora_core::extension_ui::EventKind::Change { .. }
            ) {
                let label = pilot.kind.label();
                self.push_message(format!(
                    "{label} is still loading. Try again when it finishes."
                ));
                cx.notify();
            }
            return;
        }
        if pilot.stopped {
            if event.slot == Slot::Explorer
                && event.node.as_str() == "start-server"
                && matches!(event.event, ora_core::extension_ui::EventKind::Click { .. })
            {
                let id = pilot.instance.clone();
                self.start_tool_server(&id, cx);
            }
            return;
        }
        self.pilot_request(generation, "ui.event", json!(event), Origin::UserEvent, cx);
    }

    fn pilot_request(
        &mut self,
        generation: Generation,
        method: &'static str,
        params: serde_json::Value,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        let Some(pilot) = self
            .tools
            .pilot_by_gen(generation)
            .filter(|pilot| !pilot.busy)
        else {
            return;
        };
        let Some(process) = pilot.process.clone() else {
            return;
        };
        pilot.busy = true;
        spawn_tool_job(
            cx,
            move || {
                let mut process = process.lock().unwrap_or_else(|err| err.into_inner());
                let result = call(&mut process, method, params, origin);
                (result, process.is_broken(), process.stderr_tail())
            },
            move |this, (result, broken, stderr), cx| {
                let Some(pilot) = this
                    .tools
                    .pilot_by_gen(generation)
                    .filter(|pilot| pilot.process.is_some())
                else {
                    return;
                };
                pilot.busy = false;
                match result {
                    Ok(update) => this.apply_pilot_update(generation, update, cx),
                    Err(error) if broken => {
                        let error = if stderr.is_empty() {
                            error.to_string()
                        } else {
                            format!("{error}\n{stderr}")
                        };
                        this.pilot_failed(generation, error, cx);
                    }
                    Err(error) if origin == Origin::Tick => {
                        this.pilot_failed(generation, error.to_string(), cx)
                    }
                    Err(error) => {
                        this.push_message(error.to_string());
                        this.schedule_pilot_poll(generation, Some(5000), cx);
                    }
                }
            },
        );
    }

    fn apply_pilot_update(
        &mut self,
        generation: Generation,
        update: ViewUpdate,
        cx: &mut Context<Self>,
    ) {
        let active = self.tools.active.clone();
        let Some(pilot) = self.tools.pilot_by_gen(generation) else {
            return;
        };
        let shown = active.as_deref() == Some(pilot.instance.as_str());
        if update.revision < pilot.revision {
            return;
        }
        pilot.revision = update.revision;
        let mut slots = update.slots;
        for slot in [Slot::Sidebar, Slot::Explorer, Slot::Main] {
            let tree = slots
                .iter()
                .position(|item| item.slot == slot)
                .map(|index| slots.swap_remove(index).tree);
            pilot
                .panel(slot)
                .update(cx, |panel, cx| panel.update(update.revision, tree, cx));
        }
        if shown {
            for effect in update.effects {
                match effect {
                    Effect::Copy { text } => cx.write_to_clipboard(
                        gpui_kit::ClipboardItem::new_string(text.as_str().to_owned()),
                    ),
                    Effect::Toast { text } => self.push_message(text.as_str().to_owned()),
                    Effect::OpenUrl { url } => cx.open_url(url.as_str()),
                    // The pilot uses native button confirmations.
                    Effect::Confirm { .. } => {}
                }
            }
        }
        self.schedule_pilot_poll(generation, update.poll_ms, cx);
        cx.notify();
    }

    fn schedule_pilot_poll(
        &mut self,
        generation: Generation,
        poll_ms: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let Some(pilot) = self.tools.pilot_by_gen(generation) else {
            return;
        };
        pilot.polling = poll_ms.map(|ms| {
            cx.spawn(async move |view, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(ms.into()))
                    .await;
                let result = view.update(cx, |this, cx| {
                    let active = this.tools.active.clone();
                    let Some(pilot) = this.tools.pilot_by_gen(generation) else {
                        return false;
                    };
                    if pilot.process.is_none() {
                        return false;
                    }
                    if !pilot.busy && active.as_deref() == Some(pilot.instance.as_str()) {
                        let revision = pilot.revision;
                        this.pilot_request(
                            generation,
                            "ui.tick",
                            json!(Tick { revision }),
                            Origin::Tick,
                            cx,
                        );
                        return false;
                    }
                    true
                });
                if !matches!(result, Ok(true)) {
                    break;
                }
            })
        });
    }

    fn pilot_failed(&mut self, generation: Generation, error: String, cx: &mut Context<Self>) {
        let Some(pilot) = self.tools.pilot_by_gen(generation) else {
            return;
        };
        let process = pilot.process.take();
        let id = pilot.instance.clone();
        pilot.polling = None;
        crate::error_log::append(&format!("{} extension: {error}", pilot.kind.label()));
        pilot.failure = Some(error);
        if let Some(process) = process {
            self.retire_pilot_process(id, process, cx);
        }
    }

    pub(super) fn render_tool_pilot(
        &self,
        slot: Slot,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let pilot = self.tools.active_pilot()?;
        if slot == Slot::Main {
            if let Some(error) = &pilot.failure {
                let id = pilot.instance.clone();
                return Some(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .size_full()
                        .gap(px(SPACE_3))
                        .p(px(SPACE_4))
                        .child("Connection lost")
                        .child(caption_text(error.clone(), theme))
                        .child(
                            Button::new("reconnect-tool")
                                .primary()
                                .label("Reconnect")
                                .disabled(self.tools.pilot_stopping.contains(&pilot.instance))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.close_tool_pilot(&id, cx);
                                    this.open_tool(&id, window, cx);
                                })),
                        )
                        .into_any_element(),
                );
            }
            if !pilot.reachable()
                && !pilot.stopped
                && !self.tools.pilot_stopping.contains(&pilot.instance)
            {
                return Some(
                    loading_state(&format!("Connecting to {}…", pilot.kind.label()), theme)
                        .into_any_element(),
                );
            }
        }
        Some(pilot.panel(slot).clone().into_any_element())
    }
}
