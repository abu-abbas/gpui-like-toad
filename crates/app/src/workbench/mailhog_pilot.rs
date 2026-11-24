//! Workbench entry point for the optional declarative MailHog Inbox.

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

pub(super) const ID: &str = "mailhog";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const TIMEOUT: Duration = Duration::from_secs(20);

type Process = Arc<Mutex<ExtProcess>>;

pub(super) struct MailhogPilot {
    pub instance: String,
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

impl MailhogPilot {
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

fn start(program: PathBuf, tool: ToolInstance, typed: Option<String>) -> Result<Connected, String> {
    let password = resolve_password(&tool, typed);
    let target = MailHogTarget {
        host: tool.host.clone(),
        port: tool.port,
        https: tool.tls,
        username: tool.username.clone(),
        password,
    };
    let mut process = ExtProcess::start(&program, ID, env!("CARGO_PKG_VERSION"), START_TIMEOUT)
        .map_err(|err| err.to_string())?;
    let payload = json!({
        "target": target_to_value(&target),
        "read_only": tool.read_only(),
        "start_server": tool.start_server,
        "server_command": tool.server_command,
        "instance_id": tool.id,
    });
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
    pub(super) fn mailhog_extension(&self, tool: &ToolInstance) -> Option<store::RemoteInstall> {
        if tool.kind == ToolKind::MailHog && self.extensions.tool_enabled(tool.kind) {
            store::remote_install(ID)
        } else {
            None
        }
    }

    pub(super) fn open_mailhog_pilot(
        &mut self,
        tool: ToolInstance,
        program: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let previous = self.tools.pilot.take().and_then(|pilot| pilot.process);
        let start_requested = super::tools_server::can_start(&tool);
        let read_only = tool.read_only();
        let prod = tool.environment == ConnectionEnvironment::Production;
        let sidebar = cx.new(|_| ExtensionPanel::new(Slot::Sidebar, read_only, prod));
        let explorer = cx.new(|_| ExtensionPanel::new(Slot::Explorer, read_only, prod).mailhog());
        let main = cx.new(|_| ExtensionPanel::new(Slot::Main, read_only, prod).mailhog());
        let generation = explorer.entity_id();
        let subscriptions = [&sidebar, &explorer, &main]
            .into_iter()
            .map(|panel| {
                cx.subscribe(panel, move |this, _, event: &Event, cx| {
                    this.mailhog_event(generation, event, cx);
                })
            })
            .collect();
        let typed = self.tools.session_passwords.get(&tool.id).cloned();
        self.tools.pilot = Some(MailhogPilot {
            instance: tool.id.clone(),
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
        });
        spawn_tool_job(
            cx,
            move || {
                if let Some(previous) = previous {
                    disconnect(previous);
                }
                start(program, tool, typed)
            },
            move |this, result, cx| {
                let Some(pilot) = this
                    .tools
                    .pilot
                    .as_mut()
                    .filter(|pilot| pilot.explorer.entity_id() == generation)
                else {
                    return;
                };
                pilot.busy = false;
                match result {
                    Ok(connected) => {
                        pilot.process = Some(connected.process);
                        pilot.server = connected.server;
                        this.apply_mailhog_update(connected.update, cx);
                        if start_requested && connected.server == ServerOwnership::External {
                            this.push_message(
                                "MailHog was already running. Connected to the existing server."
                                    .into(),
                            );
                        }
                    }
                    Err(error) => this.mailhog_failed(error, cx),
                }
            },
        );
        cx.notify();
    }

    pub(super) fn stop_mailhog_server(&mut self, id: &str, restart: bool, cx: &mut Context<Self>) {
        let Some(pilot) = self
            .tools
            .pilot
            .as_mut()
            .filter(|pilot| pilot.instance == id && pilot.manages_server())
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
        self.tools.mailhog_stopping.insert(id.clone());
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
                this.tools.mailhog_stopping.remove(&id);
                if let Some(pilot) = this
                    .tools
                    .pilot
                    .as_mut()
                    .filter(|pilot| pilot.explorer.entity_id() == generation)
                {
                    pilot.busy = false;
                    match result {
                        Ok(update) => {
                            pilot.stopped = true;
                            this.apply_mailhog_update(update, cx);
                            if restart {
                                if let Some(tool) = this.tools.get(&id).cloned() {
                                    if let Some(install) = this.mailhog_extension(&tool) {
                                        this.open_mailhog_pilot(tool, install.path, cx);
                                    }
                                }
                            }
                        }
                        Err(error) => this.mailhog_failed(error, cx),
                    }
                }
            },
        );
        cx.notify();
    }

    pub(super) fn close_mailhog_pilot(&mut self, cx: &mut Context<Self>) {
        if let Some(pilot) = self.tools.pilot.take() {
            if let Some(process) = pilot.process {
                self.retire_mailhog_process(pilot.instance, process, cx);
            }
        }
    }

    fn retire_mailhog_process(&mut self, id: String, process: Process, cx: &mut Context<Self>) {
        self.tools.mailhog_stopping.insert(id.clone());
        spawn_tool_job(
            cx,
            move || disconnect(process),
            move |this, (), _| {
                this.tools.mailhog_stopping.remove(&id);
            },
        );
    }

    pub(in crate::workbench) fn stop_mailhog_before_uninstall(&mut self) -> bool {
        if !self.tools.mailhog_stopping.is_empty()
            || self.tools.pilot.as_ref().is_some_and(|pilot| pilot.busy)
        {
            return false;
        }
        if let Some(pilot) = self.tools.pilot.take() {
            if self.tools.active.as_deref() == Some(pilot.instance.as_str()) {
                self.tools.active = None;
            }
        }
        true
    }

    fn mailhog_event(
        &mut self,
        generation: gpui_kit::EntityId,
        event: &Event,
        cx: &mut Context<Self>,
    ) {
        let active = self.tools.active.as_deref();
        if self.tools.pilot.as_ref().is_none_or(|pilot| {
            active != Some(pilot.instance.as_str())
                || pilot.explorer.entity_id() != generation
                || pilot.revision != event.revision
        }) {
            return;
        }
        if self.tools.pilot.as_ref().is_some_and(|pilot| pilot.busy) {
            if !matches!(
                event.event,
                ora_core::extension_ui::EventKind::Change { .. }
            ) {
                self.push_message("MailHog is still loading. Try again when it finishes.".into());
                cx.notify();
            }
            return;
        }
        if let Some(pilot) = self.tools.pilot.as_ref().filter(|pilot| pilot.stopped) {
            if event.slot == Slot::Explorer
                && event.node.as_str() == "start-server"
                && matches!(event.event, ora_core::extension_ui::EventKind::Click { .. })
            {
                let id = pilot.instance.clone();
                self.start_tool_server(&id, cx);
            }
            return;
        }
        self.mailhog_request("ui.event", json!(event), Origin::UserEvent, cx);
    }

    fn mailhog_request(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        let Some(pilot) = self.tools.pilot.as_mut().filter(|pilot| !pilot.busy) else {
            return;
        };
        let Some(process) = pilot.process.clone() else {
            return;
        };
        pilot.busy = true;
        let generation = pilot.explorer.entity_id();
        spawn_tool_job(
            cx,
            move || {
                let mut process = process.lock().unwrap_or_else(|err| err.into_inner());
                let result = call(&mut process, method, params, origin);
                (result, process.is_broken(), process.stderr_tail())
            },
            move |this, (result, broken, stderr), cx| {
                let Some(pilot) = this.tools.pilot.as_mut().filter(|pilot| {
                    pilot.explorer.entity_id() == generation && pilot.process.is_some()
                }) else {
                    return;
                };
                pilot.busy = false;
                match result {
                    Ok(update) => this.apply_mailhog_update(update, cx),
                    Err(error) if broken => {
                        let error = if stderr.is_empty() {
                            error.to_string()
                        } else {
                            format!("{error}\n{stderr}")
                        };
                        this.mailhog_failed(error, cx);
                    }
                    Err(error) if origin == Origin::Tick => {
                        this.mailhog_failed(error.to_string(), cx)
                    }
                    Err(error) => {
                        this.push_message(error.to_string());
                        this.schedule_mailhog_poll(Some(5000), cx);
                    }
                }
            },
        );
    }

    fn apply_mailhog_update(&mut self, update: ViewUpdate, cx: &mut Context<Self>) {
        let Some(pilot) = self.tools.pilot.as_mut() else {
            return;
        };
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
        if self.tools.active.as_deref() == Some(pilot.instance.as_str()) {
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
        self.schedule_mailhog_poll(update.poll_ms, cx);
        cx.notify();
    }

    fn schedule_mailhog_poll(&mut self, poll_ms: Option<u32>, cx: &mut Context<Self>) {
        let Some(pilot) = self.tools.pilot.as_mut() else {
            return;
        };
        let generation = pilot.explorer.entity_id();
        pilot.polling = poll_ms.map(|ms| {
            cx.spawn(async move |view, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(ms.into()))
                    .await;
                let result = view.update(cx, |this, cx| {
                    let Some(pilot) = this
                        .tools
                        .pilot
                        .as_ref()
                        .filter(|pilot| pilot.explorer.entity_id() == generation)
                    else {
                        return false;
                    };
                    if pilot.process.is_none() {
                        return false;
                    }
                    if !pilot.busy && this.tools.active.as_deref() == Some(pilot.instance.as_str())
                    {
                        let revision = pilot.revision;
                        this.mailhog_request("ui.tick", json!(Tick { revision }), Origin::Tick, cx);
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

    fn mailhog_failed(&mut self, error: String, cx: &mut Context<Self>) {
        let Some(pilot) = self.tools.pilot.as_mut() else {
            return;
        };
        let process = pilot.process.take();
        let id = pilot.instance.clone();
        pilot.polling = None;
        crate::error_log::append(&format!("MailHog extension: {error}"));
        pilot.failure = Some(error);
        if let Some(process) = process {
            self.retire_mailhog_process(id, process, cx);
        }
    }

    pub(super) fn render_mailhog_pilot(
        &self,
        slot: Slot,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let pilot = self.tools.pilot.as_ref()?;
        if self.tools.active.as_deref() != Some(pilot.instance.as_str()) {
            return None;
        }
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
                            Button::new("reconnect-mailhog")
                                .primary()
                                .label("Reconnect")
                                .disabled(self.tools.mailhog_stopping.contains(&pilot.instance))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.close_mailhog_pilot(cx);
                                    this.open_tool(&id, window, cx);
                                })),
                        )
                        .into_any_element(),
                );
            }
            if !pilot.reachable()
                && !pilot.stopped
                && !self.tools.mailhog_stopping.contains(&pilot.instance)
            {
                return Some(loading_state("Connecting to MailHog…", theme).into_any_element());
            }
        }
        Some(pilot.panel(slot).clone().into_any_element())
    }
}
