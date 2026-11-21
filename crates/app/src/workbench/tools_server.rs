//! Servers Kilat starts for a tool ("Start server from Kilat"): start,
//! stop, restart, the Server log tab, and stopping them all when Kilat
//! quits. Only local hosts; a server already listening on the port is used
//! as is and never touched.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use devtools::server::{self, ServerProcess};
use ora_core::store::{ToolInstance, ToolKind};

use super::tools::spawn_tool_job;
use super::*;

/// How much of the log the Server log tab shows.
const LOG_SHOWN: u64 = 64 * 1024;

pub(super) struct ServerSlot {
    /// `None` once stopped (or while another server owns the port).
    pub process: Option<Arc<Mutex<ServerProcess>>>,
    pub starting: bool,
    pub stopping: bool,
    pub error: Option<String>,
    pub log_path: PathBuf,
    /// Tail of the log, refreshed while the Server log tab is shown.
    pub log_text: String,
}

/// The servers Kilat started, by tool id. Dropping it (the window closed,
/// which on Linux and Windows is also how Kilat quits) stops them all.
#[derive(Default)]
pub(super) struct ServerMap(HashMap<String, ServerSlot>);

impl std::ops::Deref for ServerMap {
    type Target = HashMap<String, ServerSlot>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ServerMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ServerMap {
    fn drop(&mut self) {
        self.stop_all();
    }
}

impl ServerMap {
    /// Signal every running server, give them a moment, and forget the
    /// ones that ended. Ones still saving (Redis) finish on their own; a
    /// leftover is stopped on the next start.
    fn stop_all(&mut self) {
        let processes: Vec<_> = self
            .0
            .values_mut()
            .filter_map(|s| s.process.take())
            .collect();
        if processes.is_empty() {
            return;
        }
        for p in &processes {
            p.lock().unwrap_or_else(|e| e.into_inner()).request_stop();
        }
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(1500) {
            let all_done = processes.iter().all(|p| {
                p.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .exited()
                    .is_some()
            });
            if all_done {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let dir = store::config_dir();
        for p in &processes {
            let mut p = p.lock().unwrap_or_else(|e| e.into_inner());
            if p.exited().is_some() {
                server::forget_pid(&dir, p.pid());
            }
        }
    }
}

/// Kilat may start this tool's server: asked for, and on this machine.
pub(super) fn can_start(tool: &ToolInstance) -> bool {
    tool.start_server && server::is_local(&tool.host)
}

pub(super) fn command_of(tool: &ToolInstance) -> String {
    let cmd = tool.server_command.trim();
    if cmd.is_empty() {
        server::default_command(tool.kind == ToolKind::Redis, tool.port)
    } else {
        cmd.to_string()
    }
}

pub(super) fn log_path_of(tool: &ToolInstance) -> PathBuf {
    server::log_path(&store::config_dir(), &tool.name)
}

enum Started {
    /// Something already listens on the port: not started, just used.
    AlreadyRunning,
    Started(ServerProcess),
}

impl Workbench {
    pub(super) fn server_running(&self, id: &str) -> bool {
        self.tools
            .servers
            .get(id)
            .is_some_and(|s| s.process.is_some() && !s.stopping)
    }

    pub(super) fn server_busy(&self, id: &str) -> bool {
        self.tools
            .servers
            .get(id)
            .is_some_and(|s| s.starting || s.stopping)
    }

    /// Start the tool's server (if its port is free), wait for the port,
    /// then connect the tool.
    pub(super) fn start_tool_server(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(tool) = self.tools.get(id).cloned() else {
            return;
        };
        if !can_start(&tool) || self.server_busy(id) || self.server_running(id) {
            return;
        }
        let log_path = log_path_of(&tool);
        let work_dir = server::data_dir(&store::config_dir(), &tool.name);
        let slot = self
            .tools
            .servers
            .entry(id.to_string())
            .or_insert_with(|| ServerSlot {
                process: None,
                starting: false,
                stopping: false,
                error: None,
                log_path: log_path.clone(),
                log_text: String::new(),
            });
        slot.starting = true;
        slot.error = None;
        slot.log_path = log_path.clone();
        self.set_tool_status(id, None, true);
        let command = command_of(&tool);
        let id = id.to_string();
        spawn_tool_job(
            cx,
            move || {
                let (host, port) = (tool.host.clone(), tool.port);
                // Already up (started by hand)? Use it, start nothing.
                if server::wait_port(&host, port, Duration::ZERO, || None).is_ok() {
                    return Ok(Started::AlreadyRunning);
                }
                let mut process = ServerProcess::start(&command, &log_path, &work_dir)?;
                server::record_pid(&store::config_dir(), process.pid(), &command);
                let ready =
                    server::wait_port(&host, port, Duration::from_secs(15), || process.exited());
                match ready {
                    Ok(()) => Ok(Started::Started(process)),
                    Err(err) => {
                        process.stop(Duration::from_secs(2));
                        server::forget_pid(&store::config_dir(), process.pid());
                        Err(err)
                    }
                }
            },
            move |this, result, cx| {
                let Some(slot) = this.tools.servers.get_mut(&id) else {
                    return;
                };
                slot.starting = false;
                match result {
                    Ok(Started::Started(process)) => {
                        slot.process = Some(Arc::new(Mutex::new(process)));
                    }
                    Ok(Started::AlreadyRunning) => {
                        slot.error = Some(
                            "A server already answers on this port, so Kilat did not start one. It is used as is."
                                .into(),
                        );
                    }
                    Err(err) => {
                        slot.error = Some(err.clone());
                        this.set_tool_status(&id, Some(err), false);
                        this.refresh_tool_log(&id);
                        return;
                    }
                }
                this.refresh_tool_log(&id);
                this.reconnect_tool(&id, cx);
            },
        );
        cx.notify();
    }

    /// Stop the server Kilat started; `then_start` restarts it.
    pub(super) fn stop_tool_server(&mut self, id: &str, then_start: bool, cx: &mut Context<Self>) {
        let Some(slot) = self.tools.servers.get_mut(id) else {
            return;
        };
        let Some(process) = slot.process.clone() else {
            return;
        };
        if slot.stopping {
            return;
        }
        slot.stopping = true;
        // The session is about to drop; say why instead of a socket error.
        self.drop_tool_session(id, "Server stopped.");
        let id = id.to_string();
        spawn_tool_job(
            cx,
            move || {
                let mut p = process.lock().unwrap_or_else(|e| e.into_inner());
                p.stop(Duration::from_secs(5));
                server::forget_pid(&store::config_dir(), p.pid());
            },
            move |this, (), cx| {
                if let Some(slot) = this.tools.servers.get_mut(&id) {
                    slot.stopping = false;
                    slot.process = None;
                }
                this.refresh_tool_log(&id);
                if then_start {
                    this.start_tool_server(&id, cx);
                } else {
                    this.drop_tool_session(&id, "Server stopped.");
                }
            },
        );
        cx.notify();
    }

    /// Kilat is quitting: stop every server it started.
    pub(super) fn stop_all_servers_now(&mut self) {
        self.tools.servers.stop_all();
    }

    /// Re-read the tail of the tool's server log.
    pub(super) fn refresh_tool_log(&mut self, id: &str) {
        let Some(tool) = self.tools.get(id) else {
            return;
        };
        let path = log_path_of(tool);
        let slot = self
            .tools
            .servers
            .entry(id.to_string())
            .or_insert_with(|| ServerSlot {
                process: None,
                starting: false,
                stopping: false,
                error: None,
                log_path: path.clone(),
                log_text: String::new(),
            });
        slot.log_path = path;
        let text = server::tail(&slot.log_path, LOG_SHOWN);
        if text != slot.log_text {
            // Follow new output unless the user scrolled up to read.
            let scroll = &self.tools.log_scroll;
            let (offset, max) = (scroll.offset().y, scroll.max_offset().y);
            if slot.log_text.is_empty() || (offset + max).abs() < px(24.) {
                scroll.scroll_to_bottom();
            }
            slot.log_text = text;
        }
        // A server that died on its own is no longer ours to stop.
        if let Some(p) = slot.process.clone() {
            let mut guard = p.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(how) = guard.exited() {
                server::forget_pid(&store::config_dir(), guard.pid());
                drop(guard);
                slot.process = None;
                slot.error = Some(format!("The server exited ({how})."));
            }
        }
    }

    /// Keep the Server log tab current while it is on screen.
    pub(super) fn start_log_poll(&mut self, cx: &mut Context<Self>) {
        if self.tools.log_polling {
            return;
        }
        self.tools.log_polling = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(1000))
                .await;
            let go = this
                .update(cx, |this, cx| {
                    let Some(id) = this.tools.active.clone() else {
                        this.tools.log_polling = false;
                        return false;
                    };
                    if !this.tool_log_tab_shown(&id) {
                        this.tools.log_polling = false;
                        return false;
                    }
                    this.refresh_tool_log(&id);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !go {
                break;
            }
        })
        .detach();
    }

    fn tool_log_tab_shown(&self, id: &str) -> bool {
        self.tools
            .redis
            .get(id)
            .is_some_and(|v| v.tab == super::tools_redis::RedisTab::Log)
            || self
                .tools
                .mail
                .get(id)
                .is_some_and(|v| v.tab == super::tools_mail::MailTab::Log)
    }

    /// Connect the tool again (after its server came up).
    fn reconnect_tool(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.tools.redis.get_mut(id) {
            view.session = None;
            view.keys.clear();
            view.cursor = 0;
            self.redis_connect(id, cx);
        } else if self.tools.mail.contains_key(id) {
            self.mail_connect(id, cx);
        }
    }

    /// Show `error` (or "Starting…") in place of the tool's content.
    fn set_tool_status(&mut self, id: &str, error: Option<String>, starting: bool) {
        if let Some(view) = self.tools.redis.get_mut(id) {
            view.connecting = starting;
            if error.is_some() {
                view.error = error;
            }
        } else if let Some(view) = self.tools.mail.get_mut(id) {
            view.loading = starting;
            if error.is_some() {
                view.reachable = false;
                view.error = error;
            }
        }
    }

    fn drop_tool_session(&mut self, id: &str, why: &str) {
        if let Some(view) = self.tools.redis.get_mut(id) {
            view.session = None;
            view.epoch += 1;
            view.connecting = false;
            view.error = Some(why.into());
        }
        if let Some(view) = self.tools.mail.get_mut(id) {
            view.reachable = false;
            view.messages.clear();
            view.selected = None;
            view.loading = false;
            view.error = Some(why.into());
        }
    }

    /// What the server menu needs, taken at render time.
    pub(super) fn server_flags(&self, tool: &ToolInstance) -> ServerFlags {
        ServerFlags {
            can_start: can_start(tool),
            running: self.server_running(&tool.id),
            busy: self.server_busy(&tool.id),
        }
    }

    /// Big "Start server" button for the not-connected state.
    pub(super) fn start_server_button(
        &self,
        tool: &ToolInstance,
        cx: &mut Context<Self>,
    ) -> Option<Button> {
        if !can_start(tool) || self.server_running(&tool.id) {
            return None;
        }
        let id = tool.id.clone();
        Some(
            Button::new("tool-start-server")
                .small()
                .primary()
                .icon(IconName::Play)
                .label(if self.server_busy(&tool.id) {
                    "Starting…"
                } else {
                    "Start server"
                })
                .disabled(self.server_busy(&tool.id))
                .on_click(cx.listener(move |this, _, _, cx| this.start_tool_server(&id, cx))),
        )
    }

    /// The Server log tab.
    pub(super) fn render_server_log(
        &self,
        tool: &ToolInstance,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let slot = self.tools.servers.get(&tool.id);
        let running = self.server_running(&tool.id);
        let path = slot.map_or_else(|| log_path_of(tool), |s| s.log_path.clone());
        let text = slot.map(|s| s.log_text.clone()).unwrap_or_default();
        let status = if self.server_busy(&tool.id) {
            "Working…".to_string()
        } else if running {
            let pid = slot
                .and_then(|s| s.process.as_ref())
                .map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).pid())
                .unwrap_or(0);
            format!("Running · pid {pid} · {}", command_of(tool))
        } else {
            "Not running".to_string()
        };
        let mut notes = Vec::new();
        if let Some(err) = slot.and_then(|s| s.error.clone()) {
            notes.push(err);
        }
        if tool.kind == ToolKind::Redis && command_of(tool).contains(".conf") {
            notes.push(
                "If your redis.conf sets logfile, Redis writes its log there instead of here."
                    .into(),
            );
        }
        let id_clear = tool.id.clone();
        let reveal = path.clone();
        let copy = text.clone();
        let header = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .py(px(SPACE_2))
            .child(status_dot(if running {
                theme.success
            } else {
                theme.muted_foreground
            }))
            .child(caption_text(status, theme).flex_1().truncate())
            .children(self.start_server_button(tool, cx))
            .child(
                Button::new("server-log-copy")
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .tooltip("Copy log")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy.clone()))
                    }),
            )
            .child(
                Button::new("server-log-folder")
                    .small()
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .tooltip("Open log folder")
                    .on_click(move |_, _, cx| {
                        if reveal.exists() {
                            cx.reveal_path(&reveal);
                        } else if let Some(dir) = reveal.parent() {
                            let _ = std::fs::create_dir_all(dir);
                            cx.reveal_path(dir);
                        }
                    }),
            )
            .child(
                Button::new("server-log-clear")
                    .small()
                    .ghost()
                    .icon(IconName::Eraser)
                    .tooltip("Clear log")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let path = this
                            .tools
                            .get(&id_clear)
                            .map(log_path_of)
                            .unwrap_or_default();
                        let _ = server::clear_log(&path);
                        this.refresh_tool_log(&id_clear);
                        cx.notify();
                    })),
            );
        let body = if text.trim().is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(empty_state_text(
                    if can_start(tool) {
                        "No output yet. Start the server to see its log here.".into()
                    } else {
                        "Turn on \"Start server from Kilat\" in the connection settings to run this server from Kilat and see its log here."
                            .into()
                    },
                    theme,
                ))
                .into_any_element()
        } else {
            div()
                .id("server-log-scroll")
                .track_scroll(&self.tools.log_scroll)
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .px(px(SPACE_4))
                .pb(px(SPACE_4))
                .child(
                    div()
                        .p(px(SPACE_3))
                        .rounded(px(RADIUS_MD))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.secondary.opacity(0.4))
                        .font_family(theme.mono_font_family.clone())
                        .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                        .whitespace_normal()
                        .child(text),
                )
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(header)
            .children(notes.into_iter().map(|n| {
                div()
                    .px(px(SPACE_4))
                    .pb(px(SPACE_2))
                    .child(caption_text(n, theme).text_color(theme.warning))
            }))
            .child(
                caption_text(path.display().to_string(), theme)
                    .px(px(SPACE_4))
                    .pb(px(SPACE_2)),
            )
            .child(body)
    }
}

#[derive(Clone, Copy)]
pub(super) struct ServerFlags {
    pub can_start: bool,
    pub running: bool,
    pub busy: bool,
}

/// "Start server" / "Restart server" / "Stop server" menu entries for a
/// tool whose server Kilat may start.
pub(super) fn server_menu(
    menu: PopupMenu,
    id: &str,
    flags: ServerFlags,
    entity: &Entity<Workbench>,
) -> PopupMenu {
    if !flags.can_start {
        return menu;
    }
    let ServerFlags { running, busy, .. } = flags;
    let (e1, e2, e3) = (entity.clone(), entity.clone(), entity.clone());
    let (i1, i2, i3) = (id.to_string(), id.to_string(), id.to_string());
    menu.separator()
        .item(
            PopupMenuItem::new("Start server")
                .icon(Icon::new(IconName::Play))
                .disabled(running || busy)
                .on_click(move |_, _, cx| {
                    e1.update(cx, |this, cx| this.start_tool_server(&i1, cx))
                }),
        )
        .item(
            PopupMenuItem::new("Restart server")
                .icon(Icon::new(IconName::RotateCcw))
                .disabled(!running || busy)
                .on_click(move |_, _, cx| {
                    e2.update(cx, |this, cx| this.stop_tool_server(&i2, true, cx))
                }),
        )
        .item(
            PopupMenuItem::new("Stop server")
                .icon(Icon::new(IconName::Square))
                .disabled(!running || busy)
                .on_click(move |_, _, cx| {
                    e3.update(cx, |this, cx| this.stop_tool_server(&i3, false, cx))
                }),
        )
}
