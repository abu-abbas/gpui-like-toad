//! Developer Tools (Redis, MailHog): saved instances in their own sidebar
//! section under the connections, the "+" picker, the connection form,
//! and the confirm dialog. An open tool takes over the explorer column
//! (key list / inbox) and the work area (key / message detail); see
//! `tools_redis.rs` and `tools_mail.rs`.
//!
//! Every network call runs on the background executor; the UI thread only
//! applies results.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use devtools::mailhog::{MailHogClient, MailHogTarget};
use ora_core::credentials::{CredentialStore as _, ExposeSecret as _};
use ora_core::store::{ToolInstance, ToolKind};

use super::tool_pilot::ServerOwnership;
use super::tools_mail::MailView;
use super::*;

/// Brand-like tint for a tool's icon: Redis red, MailHog amber.
pub(super) fn tool_icon(kind: ToolKind, live: bool, theme: &gpui_kit::component::Theme) -> Icon {
    let dark = theme.background.l < 0.5;
    let color = if !live {
        theme.muted_foreground
    } else {
        match (kind, dark) {
            (ToolKind::Redis, false) => gpui_kit::hsla(4. / 360., 0.74, 0.48, 1.),
            (ToolKind::Redis, true) => gpui_kit::hsla(4. / 360., 0.85, 0.65, 1.),
            (ToolKind::MailHog, false) => gpui_kit::hsla(32. / 360., 0.85, 0.42, 1.),
            (ToolKind::MailHog, true) => gpui_kit::hsla(36. / 360., 0.9, 0.6, 1.),
        }
    };
    let svg: &'static [u8] = match kind {
        ToolKind::Redis => include_bytes!("../../assets/db/redis.svg"),
        ToolKind::MailHog => include_bytes!("../../assets/db/mailhog.svg"),
    };
    Icon::default().data(svg).size(px(16.)).text_color(color)
}

/// Small colored tag for DEV / STAGING / PROD.
pub(super) fn env_tag(
    env: ConnectionEnvironment,
    theme: &gpui_kit::component::Theme,
) -> Option<gpui_kit::Div> {
    ui_helpers::environment_badge(
        ConnectionAccess {
            environment: env,
            ..Default::default()
        },
        theme,
    )
}

#[derive(Clone)]
pub(super) enum ConfirmAction {
    RemoveTool(String),
    DeleteAllMail(String),
}

pub(super) struct ToolConfirm {
    pub title: String,
    pub text: String,
    pub button: &'static str,
    pub action: ConfirmAction,
}

/// The New / Edit tool dialog.
pub(super) struct ToolForm {
    pub editing: Option<String>,
    pub kind: ToolKind,
    pub name: Entity<InputState>,
    pub host: Entity<InputState>,
    pub port: Entity<InputState>,
    pub username: Entity<InputState>,
    pub password: Entity<InputState>,
    pub database: Entity<InputState>,
    pub save_password: bool,
    pub had_saved_password: bool,
    pub tls: bool,
    pub allow_dangerous: bool,
    pub environment: ConnectionEnvironment,
    pub start_server: bool,
    pub server_command: Entity<InputState>,
    /// `(is_error, text)` under the fields.
    pub notice: Option<(bool, String)>,
    pub testing: bool,
}

#[derive(Default)]
pub(super) struct ToolsState {
    pub instances: Vec<ToolInstance>,
    /// The tool shown in the explorer column and work area.
    pub active: Option<String>,
    pub picker: bool,
    pub form: Option<ToolForm>,
    pub confirm: Option<ToolConfirm>,
    pub mail: HashMap<String, MailView>,
    /// One open extension session per tool, so switching tools keeps
    /// the others connected.
    pub pilots: HashMap<String, super::tool_pilot::ToolPilot>,
    /// Stops still in flight, including after the user switches to another tool.
    pub pilot_stopping: HashSet<String>,
    pub folded: bool,
    /// Passwords typed without "Save password": kept for this run only.
    pub session_passwords: HashMap<String, String>,
    /// Servers Kilat started, by tool id.
    pub servers: super::tools_server::ServerMap,
    /// The Server log tab is being refreshed.
    pub log_polling: bool,
    /// Scroll of the Server log tab (one is shown at a time).
    pub log_scroll: ScrollHandle,
}

impl ToolsState {
    pub fn load() -> Self {
        ToolsState {
            instances: store::load_tools(),
            ..Default::default()
        }
    }

    pub fn get(&self, id: &str) -> Option<&ToolInstance> {
        self.instances.iter().find(|t| t.id == id)
    }

    fn is_open(&self, id: &str) -> bool {
        self.pilots.contains_key(id) || self.mail.contains_key(id)
    }

    pub fn is_live(&self, id: &str) -> bool {
        self.mail.get(id).is_some_and(|v| v.reachable)
            || self.pilots.get(id).is_some_and(|v| v.reachable())
    }

    /// The active tool, if it still exists.
    pub fn active_tool(&self) -> Option<&ToolInstance> {
        self.active.as_deref().and_then(|id| self.get(id))
    }
}

/// Values the form was filled with, ready to connect with.
#[derive(Clone)]
pub(super) struct ToolDraft {
    pub tool: ToolInstance,
    pub password: Option<String>,
}

fn mail_target(t: &ToolInstance, password: Option<String>) -> MailHogTarget {
    MailHogTarget {
        host: t.host.clone(),
        port: t.port,
        https: t.tls,
        username: t.username.clone(),
        password,
    }
}

pub(super) fn mail_client(t: &ToolInstance, password: Option<String>) -> Arc<MailHogClient> {
    Arc::new(MailHogClient::new(mail_target(t, password)))
}

/// The password to connect with: typed this run, else the keychain
/// (blocking; run off the UI thread).
pub(super) fn resolve_password(t: &ToolInstance, typed: Option<String>) -> Option<String> {
    if typed.is_some() {
        return typed;
    }
    if t.save_password {
        if let Ok(Some(pw)) = crate::keychain::store().get(&t.id) {
            return Some(pw.expose_secret().to_string());
        }
    }
    None
}

impl Workbench {
    // -----------------------------------------------------------------
    // Opening and closing
    // -----------------------------------------------------------------

    /// Show tool `id` in the explorer column and work area, connecting
    /// first if it is not open.
    pub(super) fn open_tool(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tool) = self.tools.get(id).cloned() else {
            return;
        };
        if self.tools.pilot_stopping.contains(id) {
            return;
        }
        let extension = self.tool_extension(&tool);
        // Redis only exists as an extension.
        if tool.kind == ToolKind::Redis && extension.is_none() {
            self.show_extension_needed(tool.kind.extension_id(), window, cx);
            return;
        }
        self.tools.active = Some(id.to_string());
        self.rail = RailPanel::Connections;
        if let Some(install) = extension {
            if self
                .tools
                .pilots
                .get(id)
                .is_none_or(|p| p.needs_reopen(&install.path))
            {
                self.open_tool_pilot(tool, install.path, cx);
            }
            cx.notify();
            return;
        }
        if !self.tools.mail.contains_key(id) {
            let view = MailView::new(&tool, window, cx);
            self.tools.mail.insert(id.to_string(), view);
            self.connect_or_start(&tool, cx);
        }
        cx.notify();
    }

    /// Connect a freshly opened tool. With "Start server from Kilat" the
    /// server is started first (or used, when something already listens
    /// on the port); the tool connects once it is up.
    fn connect_or_start(&mut self, tool: &ToolInstance, cx: &mut Context<Self>) {
        let id = tool.id.as_str();
        if self.server_busy(id) {
            // Starting or restarting: it connects when done.
            if let Some(v) = self.tools.mail.get_mut(id) {
                v.loading = true;
            }
        } else if super::tools_server::can_start(tool) && !self.server_running(id) {
            self.start_tool_server(id, cx);
        } else {
            self.mail_connect(id, cx);
        }
    }

    /// Close tool `id`'s session; the instance stays in the list.
    pub(super) fn close_tool(&mut self, id: &str, cx: &mut Context<Self>) {
        self.tools.mail.remove(id);
        self.close_tool_pilot(id, cx);
        if self.tools.active.as_deref() == Some(id) {
            self.tools.active = None;
        }
        cx.notify();
    }

    /// Back to the database explorer (a connection row was clicked).
    pub(super) fn leave_tools(&mut self) {
        self.tools.active = None;
    }

    // -----------------------------------------------------------------
    // The form
    // -----------------------------------------------------------------

    pub(super) fn open_tool_picker(&mut self, cx: &mut Context<Self>) {
        self.tools.picker = true;
        cx.notify();
    }

    pub(super) fn open_tool_form(
        &mut self,
        kind: ToolKind,
        editing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tool = editing
            .as_deref()
            .and_then(|id| self.tools.get(id).cloned())
            .unwrap_or_else(|| ToolInstance::new(kind));
        let kind = tool.kind;
        let input =
            |placeholder: &str, value: String, window: &mut Window, cx: &mut Context<Self>| {
                let placeholder = placeholder.to_string();
                cx.new(|cx| {
                    InputState::new(&mut *window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                })
            };
        let name = input(
            match kind {
                ToolKind::Redis => "Local Redis",
                ToolKind::MailHog => "Local MailHog",
            },
            tool.name.clone(),
            window,
            cx,
        );
        let host = input("localhost", tool.host.clone(), window, cx);
        let port = input(
            &kind.default_port().to_string(),
            tool.port.to_string(),
            window,
            cx,
        );
        let username = input(
            match kind {
                ToolKind::Redis => "default (optional)",
                ToolKind::MailHog => "Basic auth user (optional)",
            },
            tool.username.clone(),
            window,
            cx,
        );
        let password = cx.new(|cx| InputState::new(&mut *window, cx).masked(true));
        let database = input("0", tool.database.to_string(), window, cx);
        let server_command = input(
            &devtools::server::default_command(kind == ToolKind::Redis, tool.port),
            tool.server_command.clone(),
            window,
            cx,
        );
        self.tools.picker = false;
        self.tools.form = Some(ToolForm {
            editing,
            kind,
            name,
            host,
            port,
            username,
            password,
            database,
            save_password: tool.save_password,
            had_saved_password: tool.save_password,
            tls: tool.tls,
            allow_dangerous: tool.allow_dangerous,
            environment: tool.environment,
            start_server: tool.start_server,
            server_command,
            notice: None,
            testing: false,
        });
        cx.notify();
    }

    /// The form's values, or why they cannot be used.
    fn tool_draft(&self, cx: &App) -> Result<ToolDraft, String> {
        let Some(form) = &self.tools.form else {
            return Err("No form.".into());
        };
        let mut tool = form
            .editing
            .as_deref()
            .and_then(|id| self.tools.get(id).cloned())
            .unwrap_or_else(|| ToolInstance::new(form.kind));
        let host = Self::input_text(&form.host, cx);
        if host.is_empty() {
            return Err("Enter a host.".into());
        }
        let port_text = Self::input_text(&form.port, cx);
        let port = if port_text.is_empty() {
            form.kind.default_port()
        } else {
            port_text
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or("The port must be a number from 1 to 65535.")?
        };
        let db_text = Self::input_text(&form.database, cx);
        tool.database = if db_text.is_empty() {
            0
        } else {
            db_text
                .parse()
                .map_err(|_| "The database must be a number, like 0.".to_string())?
        };
        tool.host = host;
        tool.port = port;
        let name = Self::input_text(&form.name, cx);
        tool.name = if name.is_empty() {
            format!("{} {}", form.kind.label(), tool.host)
        } else {
            name
        };
        tool.username = Self::input_text(&form.username, cx);
        tool.save_password = form.save_password;
        tool.tls = form.tls;
        tool.allow_dangerous = form.kind == ToolKind::Redis && form.allow_dangerous;
        tool.environment = form.environment;
        tool.start_server = form.start_server;
        tool.server_command = Self::input_text(&form.server_command, cx);
        if tool.start_server && !devtools::server::is_local(&tool.host) {
            return Err(
                "Kilat can only start a server on this machine. Use localhost or 127.0.0.1 as the host, or turn off Start server from Kilat."
                    .into(),
            );
        }
        let typed = form.password.read(cx).value().to_string();
        let password = (!typed.is_empty())
            .then_some(typed)
            .or_else(|| self.tools.session_passwords.get(&tool.id).cloned());
        Ok(ToolDraft { tool, password })
    }

    fn set_form_notice(&mut self, notice: Option<(bool, String)>, cx: &mut Context<Self>) {
        if let Some(form) = self.tools.form.as_mut() {
            form.notice = notice;
            form.testing = false;
        }
        cx.notify();
    }

    pub(super) fn test_tool(&mut self, cx: &mut Context<Self>) {
        let draft = match self.tool_draft(cx) {
            Ok(d) => d,
            Err(e) => return self.set_form_notice(Some((true, e)), cx),
        };
        if let Some(form) = self.tools.form.as_mut() {
            form.testing = true;
            form.notice = Some((false, "Testing…".into()));
        }
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let tool = draft.tool;
                    let password = resolve_password(&tool, draft.password);
                    match tool.kind {
                        ToolKind::Redis => super::tool_pilot::test_connection(&tool, password),
                        ToolKind::MailHog => mail_client(&tool, password)
                            .list(0)
                            .map(|page| format!("MailHog answered: {} message(s).", page.total)),
                    }
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                let notice = match result {
                    Ok(text) => (false, text),
                    Err(text) => (true, text),
                };
                this.set_form_notice(Some(notice), cx);
            });
        })
        .detach();
    }

    pub(super) fn save_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = match self.tool_draft(cx) {
            Ok(d) => d,
            Err(e) => return self.set_form_notice(Some((true, e)), cx),
        };
        let Some(form) = self.tools.form.as_ref() else {
            return;
        };
        let typed = form.password.read(cx).value().to_string();
        let tool = draft.tool;
        let before = self.tools.get(&tool.id).cloned();
        let mut instances = self.tools.instances.clone();
        match instances.iter_mut().find(|t| t.id == tool.id) {
            Some(slot) => *slot = tool.clone(),
            None => instances.push(tool.clone()),
        }
        if let Err(err) = store::save_tools(&instances) {
            return self.set_form_notice(Some((true, format!("Could not save: {err}"))), cx);
        }
        self.tools.instances = instances;
        // The password: keychain with "Save password", else this run only.
        if !typed.is_empty() {
            self.tools
                .session_passwords
                .insert(tool.id.clone(), typed.clone());
        }
        let id = tool.id.clone();
        let save = tool.save_password;
        if save && !typed.is_empty() || !save {
            cx.background_executor()
                .spawn(async move {
                    let store = crate::keychain::store();
                    let result = if save {
                        store.set(&id, &SecretString::from(typed))
                    } else {
                        store.delete(&id)
                    };
                    if let Err(err) = result {
                        crate::error_log::append(&format!("Developer Tools keychain: {}", err.0));
                    }
                })
                .detach();
        }
        self.tools.form = None;
        // A server Kilat runs whose command or port changed restarts; one
        // no longer wanted stops.
        if self.server_running(&tool.id) {
            let server_changed = before.as_ref().is_none_or(|b| {
                super::tools_server::command_of(b) != super::tools_server::command_of(&tool)
                    || b.port != tool.port
                    || b.host != tool.host
            });
            let wanted = super::tools_server::can_start(&tool);
            if server_changed || !wanted {
                self.stop_tool_server(&tool.id, wanted, cx);
            }
        }
        // Changed settings take effect on a fresh session.
        self.close_tool(&tool.id, cx);
        self.open_tool(&tool.id, window, cx);
    }

    pub(super) fn request_remove_tool(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(tool) = self.tools.get(id) else {
            return;
        };
        if !self.can_change_tool_profile(tool) {
            return;
        }
        self.tools.confirm = Some(ToolConfirm {
            title: format!("Remove {} \"{}\"?", tool.kind.label(), tool.name),
            text: "The saved settings and password are removed from Kilat. Nothing on the server changes."
                .into(),
            button: "Remove",
            action: ConfirmAction::RemoveTool(id.to_string()),
        });
        cx.notify();
    }

    fn remove_tool(&mut self, id: &str, cx: &mut Context<Self>) {
        if self
            .tools
            .get(id)
            .is_none_or(|tool| !self.can_change_tool_profile(tool))
        {
            return;
        }
        let instances: Vec<ToolInstance> = self
            .tools
            .instances
            .iter()
            .filter(|t| t.id != id)
            .cloned()
            .collect();
        if let Err(err) = store::save_tools(&instances) {
            self.push_message(format!("Could not save Developer Tools: {err}"));
            return;
        }
        self.tools.instances = instances;
        self.tools.session_passwords.remove(id);
        self.close_tool(id, cx);
        let id = id.to_string();
        cx.background_executor()
            .spawn(async move {
                let _ = crate::keychain::store().delete(&id);
            })
            .detach();
    }

    pub(super) fn run_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.tools.confirm.take() else {
            return;
        };
        match confirm.action {
            ConfirmAction::RemoveTool(id) => self.remove_tool(&id, cx),
            ConfirmAction::DeleteAllMail(tool) => self.mail_delete(&tool, None, cx),
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Sidebar section
    // -----------------------------------------------------------------

    fn can_change_tool_profile(&self, tool: &ToolInstance) -> bool {
        let flags = self.server_flags(tool);
        !self.tools.is_open(&tool.id) && !flags.busy && !flags.running
    }

    pub(super) fn render_tools_section(
        &self,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let folded = self.tools.folded;
        let heading = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(2.))
            .mt(px(SPACE_2))
            .child(
                div().flex_1().child(
                    sidebar::tree_button(Button::new("tools-heading"), keyboard, cx)
                        .small()
                        .w_full()
                        .h(px(30.))
                        .tooltip("Developer Tools")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tools.folded = !this.tools.folded;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .w_full()
                                .gap(px(6.))
                                .child(icon(IconName::Wrench, 16., theme.muted_foreground))
                                .child(div().flex_1().text_left().child("Developer Tools"))
                                .child(caption_text(self.tools.instances.len().to_string(), theme))
                                .child(icon(
                                    if folded {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::ChevronDown
                                    },
                                    14.,
                                    theme.muted_foreground,
                                )),
                        ),
                ),
            )
            .child(
                sidebar::tree_button(Button::new("tool-new"), keyboard, cx)
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("Add developer tool")
                    .on_click(cx.listener(|this, _, _, cx| this.open_tool_picker(cx))),
            );
        let mut section = div()
            .flex()
            .flex_col()
            .flex_none()
            .mb(px(SPACE_2))
            .child(heading);
        if folded {
            return section;
        }
        let visible = |tool: &ToolInstance| self.extensions.tool_enabled(tool.kind);
        if !self.tools.instances.iter().any(visible) {
            return section.child(div().px(px(SPACE_3)).py(px(SPACE_1)).child(caption_text(
                "No developer tools yet. Add one with +.".into(),
                theme,
            )));
        }
        for (ix, tool) in self.tools.instances.iter().enumerate() {
            if !visible(tool) {
                continue;
            }
            section = section.child(self.render_tool_row(ix, tool, keyboard, theme, cx));
        }
        section
    }

    fn render_tool_row(
        &self,
        ix: usize,
        tool: &ToolInstance,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let live = self.tools.is_live(&tool.id);
        let pilot = self.tools.pilots.get(&tool.id);
        let open = self.tools.is_open(&tool.id);
        let active = self.tools.active.as_deref() == Some(tool.id.as_str());
        let id = tool.id.clone();
        let kind = tool.kind;
        let view = cx.entity();
        let menu_id = tool.id.clone();
        let flags = self.server_flags(tool);
        let profile_editable = !open && !flags.busy && !flags.running;
        let server_ownership = pilot
            .filter(|_| flags.can_start)
            .and_then(|p| p.server_ownership());
        let newer_extension = server_ownership == Some(ServerOwnership::Unreported)
            && pilot
                .zip(self.tool_extension(tool))
                .is_some_and(|(p, release)| p.needs_reopen(&release.path));
        div()
            .id(("tool-row", ix))
            .flex()
            .items_center()
            .flex_none()
            .rounded(px(RADIUS_MD))
            .when(active, |row| row.bg(theme.accent))
            .context_menu(move |menu, _, _| {
                let (v1, v2, v3, v4) = (view.clone(), view.clone(), view.clone(), view.clone());
                let (i1, i2, i3, i4) = (
                    menu_id.clone(),
                    menu_id.clone(),
                    menu_id.clone(),
                    menu_id.clone(),
                );
                let menu = if open {
                    menu.item(
                        PopupMenuItem::new("Disconnect")
                            .icon(Icon::new(IconName::LogOut))
                            .disabled(flags.busy)
                            .on_click(move |_, _, cx| {
                                v2.update(cx, |this, cx| {
                                    if this
                                        .tools
                                        .get(&i2)
                                        .is_some_and(|t| !this.server_flags(t).busy)
                                    {
                                        this.close_tool(&i2, cx);
                                    }
                                })
                            }),
                    )
                } else {
                    menu.item(
                        PopupMenuItem::new("Open")
                            .icon(Icon::new(IconName::LogIn))
                            .disabled(flags.busy)
                            .on_click(move |_, window, cx| {
                                v1.update(cx, |this, cx| this.open_tool(&i1, window, cx))
                            }),
                    )
                };
                let menu = match server_ownership {
                    Some(ServerOwnership::Unreported) if newer_extension => {
                        let entity = view.clone();
                        let id = menu_id.clone();
                        menu.separator().item(
                            PopupMenuItem::new(format!(
                                "Reopen with updated {} extension",
                                kind.label()
                            ))
                            .icon(Icon::new(IconName::RefreshCw))
                            .on_click(move |_, window, cx| {
                                entity.update(cx, |this, cx| this.open_tool(&id, window, cx))
                            }),
                        )
                    }
                    Some(ServerOwnership::Unreported) => {
                        let entity = view.clone();
                        menu.separator().item(
                            PopupMenuItem::new(format!(
                                "Update {} extension for server controls…",
                                kind.label()
                            ))
                            .on_click(move |_, window, cx| {
                                entity
                                    .update(cx, |this, cx| this.open_extension_updates(window, cx));
                            }),
                        )
                    }
                    Some(ServerOwnership::External) => menu.separator().item(
                        PopupMenuItem::new("Server running outside this session").disabled(true),
                    ),
                    _ => super::tools_server::server_menu(menu, &i4, flags, &v4),
                };
                menu.separator()
                    .item(
                        PopupMenuItem::new("Edit…")
                            .icon(Icon::new(IconName::SquarePen))
                            .disabled(!profile_editable)
                            .on_click(move |_, window, cx| {
                                v3.update(cx, |this, cx| {
                                    if this
                                        .tools
                                        .get(&i3)
                                        .is_some_and(|t| this.can_change_tool_profile(t))
                                    {
                                        this.open_tool_form(kind, Some(i3.clone()), window, cx);
                                    }
                                })
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Remove…")
                            .icon(Icon::new(IconName::Trash))
                            .disabled(!profile_editable)
                            .on_click(move |_, _, cx| {
                                v4.update(cx, |this, cx| this.request_remove_tool(&i4, cx))
                            }),
                    )
            })
            .child(
                sidebar::tree_button(Button::new(("tool", ix)), keyboard, cx)
                    .small()
                    .w_full()
                    .tooltip(format!("{} · {}", tool.name, tool.address()))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_tool(&id, window, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .w_full()
                            .min_w(px(0.))
                            .gap(px(8.))
                            .pl(px(SPACE_2 + 13.)) // icon starts under the "Developer Tools" text
                            .child(tool_icon(tool.kind, live, theme))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .text_left()
                                    .truncate()
                                    .when(active, |t| t.font_weight(FontWeight::SEMIBOLD))
                                    .child(tool.name.clone()),
                            )
                            .when(flags.running, |row| {
                                row.child(icon(IconName::Play, 10., theme.success))
                            })
                            .when(tool.environment != ConnectionEnvironment::None, |row| {
                                let env = tool.environment;
                                let color = match env {
                                    ConnectionEnvironment::Development => theme.success,
                                    ConnectionEnvironment::Staging => theme.warning,
                                    _ => theme.danger,
                                };
                                row.child(
                                    div()
                                        .flex_none()
                                        .text_size(theme::ui_size(theme, FONT_CAPTION))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(if live {
                                            color
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(env.label()),
                                )
                            })
                            .when(
                                live && pilot.is_none()
                                    && tool.environment == ConnectionEnvironment::None
                                    && !flags.running,
                                |row| row.child(status_dot(theme.success)),
                            )
                            .children(pilot.filter(|_| active).and_then(|_| {
                                self.render_tool_pilot(
                                    ora_core::extension_ui::Slot::Sidebar,
                                    theme,
                                    cx,
                                )
                            })),
                    ),
            )
    }

    // -----------------------------------------------------------------
    // Work area dispatch
    // -----------------------------------------------------------------

    /// Explorer column for the active tool, if one is shown.
    pub(super) fn render_tool_explorer(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        if let Some(panel) =
            self.render_tool_pilot(ora_core::extension_ui::Slot::Explorer, theme, cx)
        {
            return Some(panel);
        }
        let tool = self.tools.active_tool()?.clone();
        Some(match tool.kind {
            ToolKind::Redis => div().size_full().into_any_element(),
            ToolKind::MailHog => self
                .render_mail_explorer(&tool, theme, cx)
                .into_any_element(),
        })
    }

    pub(super) fn render_tool_main(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        if let Some(panel) = self.render_tool_pilot(ora_core::extension_ui::Slot::Main, theme, cx) {
            return Some(panel);
        }
        let tool = self.tools.active_tool()?.clone();
        Some(match tool.kind {
            ToolKind::Redis => div().size_full().into_any_element(),
            ToolKind::MailHog => self.render_mail_main(&tool, theme, cx).into_any_element(),
        })
    }

    // -----------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------

    pub(super) fn render_tool_dialogs(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        if self.tools.confirm.is_some() {
            return Some(self.render_tool_confirm(theme, cx).into_any_element());
        }
        if self.tools.form.is_some() {
            return Some(self.render_tool_form(theme, cx).into_any_element());
        }
        if self.tools.picker {
            return Some(self.render_tool_picker(theme, cx).into_any_element());
        }
        None
    }

    fn render_tool_picker(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let choice = |kind: ToolKind, text: &str, cx: &mut Context<Self>| {
            div()
                .id(("tool-choice", kind as usize))
                .flex()
                .items_center()
                .gap(px(SPACE_3))
                .p(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|s| {
                    s.bg(theme.secondary)
                        .border_color(theme.primary.opacity(0.5))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_tool_form(kind, None, window, cx)
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(36.))
                        .rounded(px(RADIUS_MD))
                        .bg(theme.secondary)
                        .child(tool_icon(kind, true, theme).size(px(20.))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .child(form_label(kind.label(), theme))
                        .child(caption_text(text.to_string(), theme)),
                )
                .child(icon(IconName::ChevronRight, 14., theme.muted_foreground))
        };
        let card = dialog_card("tool-picker", 420., theme)
            .child(dialog_title(
                "Add developer tool",
                Some("Tools live under Developer Tools in the sidebar, apart from your database connections."),
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_4))
                    .pb(px(SPACE_4))
                    .children(
                        (!self.extension_ready(ToolKind::Redis.extension_id())
                            && !self.extensions.tool_enabled(ToolKind::MailHog))
                        .then(|| caption_text("No developer tool is installed yet.".into(), theme)),
                    )
                    .children(self.extension_ready(ToolKind::Redis.extension_id()).then(|| {
                        choice(
                            ToolKind::Redis,
                            "Browse keys, read values, run commands.",
                            cx,
                        )
                    }))
                    .children(self.extensions.tool_enabled(ToolKind::MailHog).then(|| {
                        choice(
                            ToolKind::MailHog,
                            "Read the emails your app sends in development.",
                            cx,
                        )
                    })),
            )
            .child(
                dialog_footer(theme)
                    .child(
                        Button::new("tool-picker-more")
                            .ghost()
                            .label("Get more in Settings > Extensions")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.tools.picker = false;
                                this.open_extensions_page(window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                    Button::new("tool-picker-cancel")
                        .label("Cancel")
                        .min_w(px(88.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tools.picker = false;
                            cx.notify();
                        })),
                ),
            );
        scrim("tool-picker-scrim", card, theme)
    }

    fn render_tool_form(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let form = self.tools.form.as_ref().expect("form");
        let field = |label: &str| {
            div()
                .flex()
                .flex_col()
                .min_w(px(0.))
                .gap(px(SPACE_1))
                .child(form_label(label, theme))
        };
        let input_field = |label: &str, input: Input| {
            field(label).flex_1().child(
                input
                    .small()
                    .w_full()
                    .border_color(theme.input.opacity(0.45)),
            )
        };
        let row = || div().flex().items_start().gap(px(SPACE_3));
        let redis = form.kind == ToolKind::Redis;
        let view = cx.entity();
        let current_env = form.environment;
        let environment = Button::new("tool-environment")
            .small()
            .ghost()
            .bg(theme.secondary)
            .w_full()
            .child(select_trigger(current_env.label().into(), theme))
            .dropdown_menu(move |menu, _, _| {
                ConnectionEnvironment::ALL
                    .into_iter()
                    .fold(menu, |menu, option| {
                        let view = view.clone();
                        menu.item(
                            PopupMenuItem::new(option.label())
                                .checked(option == current_env)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        if let Some(form) = this.tools.form.as_mut() {
                                            form.environment = option;
                                        }
                                        cx.notify();
                                    })
                                }),
                        )
                    })
            });
        let mut body = div()
            .id("tool-form-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(SPACE_3))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .child(
                row()
                    .child(input_field("Name", Input::new(&form.name)))
                    .child(
                        field("Environment")
                            .w(px(130.))
                            .flex_none()
                            .child(environment),
                    ),
            )
            .child(
                row()
                    .child(input_field("Host", Input::new(&form.host)))
                    .child(
                        div()
                            .flex()
                            .w(px(84.))
                            .flex_none()
                            .child(input_field("Port", Input::new(&form.port))),
                    )
                    .when(redis, |row| {
                        row.child(
                            div()
                                .flex()
                                .w(px(84.))
                                .flex_none()
                                .child(input_field("Database", Input::new(&form.database))),
                        )
                    }),
            )
            .child(
                row()
                    .child(input_field("Username", Input::new(&form.username)))
                    .child(input_field(
                        "Password",
                        Input::new(&form.password).mask_toggle(),
                    )),
            );
        let check = |id: &'static str, label: &'static str, on: bool| {
            Checkbox::new(id).small().label(label).checked(on)
        };
        let mut checks = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_x(px(SPACE_4))
            .gap_y(px(SPACE_2))
            .child(
                check("tool-save-password", "Save password", form.save_password).on_click(
                    cx.listener(|this, on: &bool, _, cx| {
                        if let Some(f) = this.tools.form.as_mut() {
                            f.save_password = *on;
                        }
                        cx.notify();
                    }),
                ),
            )
            .child(
                check(
                    "tool-tls",
                    if redis { "Use TLS" } else { "Use HTTPS" },
                    form.tls,
                )
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    if let Some(f) = this.tools.form.as_mut() {
                        f.tls = *on;
                    }
                    cx.notify();
                })),
            );
        if redis {
            checks = checks.child(
                check(
                    "tool-dangerous",
                    "Allow dangerous commands",
                    form.allow_dangerous,
                )
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    if let Some(f) = this.tools.form.as_mut() {
                        f.allow_dangerous = *on;
                    }
                    cx.notify();
                })),
            );
        }
        body = body.child(checks);
        let local = devtools::server::is_local(&Self::input_text(&form.host, cx));
        let mut server = div().flex().flex_col().gap(px(SPACE_2)).child(
            check(
                "tool-start-server",
                "Start server from Kilat",
                form.start_server,
            )
            .disabled(!local && !form.start_server)
            .on_click(cx.listener(|this, on: &bool, _, cx| {
                if let Some(f) = this.tools.form.as_mut() {
                    f.start_server = *on;
                }
                cx.notify();
            })),
        );
        if form.start_server {
            server = server
                .child(input_field("Command", Input::new(&form.server_command)))
                .child(caption_text(
                    format!(
                        "Kilat runs this when you open the {} and stops it when Kilat quits. Leave it blank for the default. Its output goes to the Server log tab.",
                        if redis { "instance" } else { "inbox" }
                    ),
                    theme,
                ));
        } else if !local {
            server = server.child(caption_text(
                "Only for a server on this machine (localhost).".into(),
                theme,
            ));
        }
        body = body.child(server);
        let hint = match (&form.notice, redis) {
            (Some(_), _) => None,
            (None, true) if form.environment == ConnectionEnvironment::Production => Some(
                "PROD is read only: the console only runs read commands and keys cannot be deleted.",
            ),
            (None, true) if form.allow_dangerous => {
                Some("The console may run FLUSHDB, FLUSHALL, KEYS, CONFIG and the like.")
            }
            (None, true) => Some(
                "Keys are listed with SCAN, never KEYS. FLUSHDB, KEYS, CONFIG and the like are blocked in the console.",
            ),
            (None, false) => Some(
                "The MailHog web address, usually localhost:8025. If 127.0.0.1 does not answer, try localhost.",
            ),
        };
        let password_hint = if form.save_password && form.had_saved_password {
            "The password is saved in your system keychain. Leave the field blank to keep it."
        } else if form.save_password {
            "The password will be stored in your system keychain."
        } else {
            "Without Save password, the password is kept until Kilat quits."
        };
        body = body.children(hint.map(|h| caption_text(h.into(), theme)));
        if form.notice.is_none() {
            body = body.child(caption_text(password_hint.into(), theme));
        }
        if let Some((is_error, text)) = &form.notice {
            body = body.child(
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                    .line_height(crate::theme::ui_size(theme, LEADING_CAPTION))
                    .text_color(if *is_error {
                        theme.danger
                    } else {
                        theme.success
                    })
                    .child(text.clone()),
            );
        }
        let action = |id: &'static str| Button::new(id).small().h(px(32.)).flex_none();
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .child(
                action("tool-test")
                    .secondary()
                    .min_w(px(72.))
                    .label(if form.testing { "Testing…" } else { "Test" })
                    .disabled(form.testing)
                    .on_click(cx.listener(|this, _, _, cx| this.test_tool(cx))),
            )
            .child(div().flex_1())
            .child(
                action("tool-cancel")
                    .ghost()
                    .min_w(px(72.))
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tools.form = None;
                        cx.notify();
                    })),
            )
            .child(
                action("tool-save")
                    .primary()
                    .min_w(px(96.))
                    .label("Save")
                    .on_click(cx.listener(|this, _, window, cx| this.save_tool(window, cx))),
            );
        let title = format!(
            "{} {} connection",
            if form.editing.is_some() {
                "Edit"
            } else {
                "New"
            },
            form.kind.label()
        );
        let card = div()
            .id("tool-form")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(548.))
            .max_h(relative(1.))
            .overflow_hidden()
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_4))
                    .h(px(44.))
                    .flex_none()
                    .child(tool_icon(form.kind, true, theme).size(px(18.)))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .id("tool-form-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(28.))
                            .rounded(px(RADIUS_MD))
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.tools.form = None;
                                cx.notify();
                            }))
                            .child(icon(IconName::X, 16., theme.muted_foreground)),
                    ),
            )
            .child(body)
            .child(footer);
        div()
            .id("tool-form-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(SPACE_4))
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
    }

    fn render_tool_confirm(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let confirm = self.tools.confirm.as_ref().expect("confirm");
        let card = dialog_card("tool-confirm", 440., theme)
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.tools.confirm = None;
                    cx.notify();
                }
            }))
            .child(dialog_title(&confirm.title, Some(&confirm.text), theme))
            .child(
                dialog_footer(theme)
                    .child(
                        Button::new("tool-confirm-cancel")
                            .flex_1()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.tools.confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("tool-confirm-ok")
                            .flex_1()
                            .label(confirm.button)
                            .danger()
                            .on_click(cx.listener(|this, _, _, cx| this.run_confirm(cx))),
                    ),
            );
        scrim("tool-confirm-scrim", card, theme)
    }
}

// ---------------------------------------------------------------------
// Dialog pieces (same look as the folder delete dialog)
// ---------------------------------------------------------------------

pub(super) fn dialog_card(
    id: &'static str,
    width: f32,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .w(px(width))
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .shadow_lg()
        .overflow_hidden()
}

pub(super) fn dialog_title(
    title: &str,
    text: Option<&str>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_2))
        .p(px(SPACE_4))
        .child(
            div()
                .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                .line_height(crate::theme::ui_size(theme, LEADING_HEADING_MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(title.to_string()),
        )
        .children(text.map(|text| {
            div()
                .text_size(crate::theme::ui_size(theme, FONT_BODY))
                .line_height(crate::theme::ui_size(theme, LEADING_BODY))
                .text_color(theme.muted_foreground)
                .child(text.to_string())
        }))
}

pub(super) fn dialog_footer(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .flex()
        .flex_row()
        .gap(px(SPACE_2))
        .p(px(SPACE_4))
        .bg(theme.muted)
}

pub(super) fn scrim(
    id: &'static str,
    card: impl IntoElement,
    theme: &gpui_kit::component::Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(140.))
        .bg(crate::theme::modal_scrim(theme))
        .occlude()
        .child(card)
}

/// Run blocking `job` on the background executor, then `done` on the UI
/// thread with its result (dropped if the workbench is gone).
pub(super) fn spawn_tool_job<T: Send + 'static>(
    cx: &mut Context<Workbench>,
    job: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut Workbench, T, &mut Context<Workbench>) + 'static,
) {
    cx.spawn(async move |view, cx| {
        let result = cx.background_executor().spawn(async move { job() }).await;
        let _ = view.update(cx, |this, cx| {
            done(this, result, cx);
            cx.notify();
        });
    })
    .detach();
}
