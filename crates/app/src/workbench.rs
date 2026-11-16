//! The workbench: connection form, schema tree, SQL editor, result grid,
//! explain-plan view, and message log in one window.
//!
//! All database work runs on the worker thread ([`crate::worker`]); this view
//! only sends jobs and applies responses. One operation runs at a time
//! (`busy` + `pending_op`) so responses can never interleave.

use std::sync::mpsc::{Receiver, RecvError};

use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::tree::{tree, TreeItem, TreeState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{Button, ListItem};
use gpui_kit::{
    div, h_flex, px, v_flex, App, Context, Entity, IntoElement, Render, SharedString, Window,
};
use ora_core::export::to_csv;
use ora_core::sql::clean_statement;
use ora_core::{ConnectionProfile, DbInfo, QueryResult, SchemaObject, DEFAULT_MAX_ROWS};

use crate::results::ResultsDelegate;
use crate::schema;
use crate::worker::{DbHandle, Job, Response};

const MAX_MESSAGES: usize = 200;
const MAX_MESSAGES_SHOWN: usize = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultsTab {
    Results,
    Plan,
    Messages,
}

#[derive(Clone)]
enum PendingOp {
    Connect,
    Run,
    Explain,
    ListObjects,
    Describe { owner: String, name: String },
    Commit,
    Rollback,
}

pub struct Workbench {
    name_input: Entity<InputState>,
    user_input: Entity<InputState>,
    host_input: Entity<InputState>,
    password_input: Entity<InputState>,
    db: Option<DbHandle>,
    session: Option<DbInfo>,
    demo_mode: bool,
    busy: bool,
    pending_op: Option<PendingOp>,
    tree_state: Entity<TreeState>,
    objects: Vec<SchemaObject>,
    editor: Entity<EditorState>,
    table_state: Entity<TableState<ResultsDelegate>>,
    last_result: Option<QueryResult>,
    plan_lines: Vec<String>,
    messages: Vec<String>,
    tab: ResultsTab,
    status: String,
}

impl Workbench {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Nama koneksi (opsional)"));
        let user_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Username, mis. HR"));
        let host_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("host:1521/service"));
        let password_input = cx.new(|cx| InputState::new(&mut *window, cx).masked(true));
        let editor = cx.new(|cx| {
            EditorState::new(&mut *window, cx)
                .language("sql")
                .line_number(true)
                .folding(true)
                .default_value(
                    "-- Klik Demo di kiri untuk coba tanpa database,\n\
                     -- atau isi koneksi lalu Connect.\n\
                     SELECT * FROM employees",
                )
        });
        let tree_state = cx.new(|cx| TreeState::new(cx).items(Vec::<TreeItem>::new()));
        let table_state = cx.new(|cx| TableState::new(ResultsDelegate::empty(), &mut *window, cx));
        Workbench {
            name_input,
            user_input,
            host_input,
            password_input,
            db: None,
            session: None,
            demo_mode: false,
            busy: false,
            pending_op: None,
            tree_state,
            objects: Vec::new(),
            editor,
            table_state,
            last_result: None,
            plan_lines: Vec::new(),
            messages: Vec::new(),
            tab: ResultsTab::Results,
            status: "Belum terhubung.".to_string(),
        }
    }

    // ----- helpers -----

    fn push_message(&mut self, message: String) {
        self.messages.push(message);
        if self.messages.len() > MAX_MESSAGES {
            let excess = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(..excess);
        }
    }

    fn input_text(entity: &Entity<InputState>, cx: &App) -> String {
        entity.read(cx).value().trim().to_string()
    }

    fn editor_text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    fn set_grid_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        self.table_state.update(cx, |state, cx| {
            state.delegate_mut().set_result(result);
            state.refresh(cx);
        });
    }

    fn clear_grid(&mut self, cx: &mut Context<Self>) {
        self.set_grid_result(QueryResult::default(), cx);
        self.last_result = None;
    }

    /// Send a job to the worker and apply the response when it arrives. The
    /// blocking receive runs on the background executor, never the UI thread.
    fn spawn_db(&mut self, job: Job, op: PendingOp, cx: &mut Context<Self>) {
        let Some(db) = self.db.clone() else {
            self.push_message("Belum terhubung.".to_string());
            return;
        };
        let rx: Receiver<Response> = db.request(job);
        self.pending_op = Some(op);
        self.busy = true;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = view.update(cx, |this, cx| this.on_db_response(resp, cx));
        })
        .detach();
    }

    // ----- actions -----

    fn connect(&mut self, demo: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (profile, password) = if demo {
            (
                ConnectionProfile {
                    name: "Demo".to_string(),
                    username: "DEMO".to_string(),
                    connect_string: "demo".to_string(),
                },
                String::new(),
            )
        } else {
            let username = Self::input_text(&self.user_input, cx);
            let connect_string = Self::input_text(&self.host_input, cx);
            if username.is_empty() || connect_string.is_empty() {
                self.push_message("Isi username dan connect string dulu.".to_string());
                self.tab = ResultsTab::Messages;
                cx.notify();
                return;
            }
            let name = Self::input_text(&self.name_input, cx);
            (
                ConnectionProfile {
                    name: if name.is_empty() {
                        format!("{username}@{connect_string}")
                    } else {
                        name
                    },
                    username,
                    connect_string,
                },
                Self::input_text(&self.password_input, cx),
            )
        };
        if self.db.is_none() {
            self.db = Some(DbHandle::spawn());
        }
        self.demo_mode = demo;
        self.status = "Menghubungkan...".to_string();
        self.spawn_db(
            Job::Connect {
                profile,
                password,
                demo,
            },
            PendingOp::Connect,
            cx,
        );
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.db.take() {
            // Fire and forget: the worker closes the session on its own.
            let _ = db.request(Job::Disconnect);
        }
        self.session = None;
        self.demo_mode = false;
        self.objects.clear();
        self.tree_state.update(cx, |state, cx| {
            state.set_items(Vec::<TreeItem>::new(), cx);
        });
        self.clear_grid(cx);
        self.plan_lines.clear();
        self.status = "Belum terhubung.".to_string();
        self.push_message("Disconnected.".to_string());
        cx.notify();
    }

    fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.push_message("Belum terhubung.".to_string());
            return;
        };
        self.status = "Memuat schema...".to_string();
        self.spawn_db(
            Job::ListObjects {
                owner: session.user,
            },
            PendingOp::ListObjects,
            cx,
        );
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.session.is_none() {
            self.push_message("Belum terhubung: Connect atau Demo dulu.".to_string());
            self.tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        let sql = self.editor_text(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("Editor kosong.".to_string());
            self.tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.status = "Menjalankan...".to_string();
        self.spawn_db(Job::Run { sql }, PendingOp::Run, cx);
    }

    fn explain(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.session.is_none() {
            self.push_message("Belum terhubung: Connect atau Demo dulu.".to_string());
            self.tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        let sql = self.editor_text(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("Editor kosong.".to_string());
            self.tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.status = "Menjelaskan plan...".to_string();
        self.spawn_db(Job::Explain { sql }, PendingOp::Explain, cx);
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.session.is_none() {
            return;
        }
        self.spawn_db(Job::Commit, PendingOp::Commit, cx);
    }

    fn rollback(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.session.is_none() {
            return;
        }
        self.spawn_db(Job::Rollback, PendingOp::Rollback, cx);
    }

    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let Some(result) = self.last_result.clone() else {
            self.push_message("Belum ada hasil query untuk diekspor.".to_string());
            self.tab = ResultsTab::Messages;
            cx.notify();
            return;
        };
        let csv = to_csv(&result);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filename = format!("gpui-like-toad-export-{stamp}.csv");
        let path = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(&filename);
        match std::fs::write(&path, csv) {
            Ok(()) => {
                self.push_message(format!(
                    "CSV tersimpan: {} ({} baris)",
                    path.display(),
                    result.row_count()
                ));
                self.status = format!("Exported {filename}");
            }
            Err(err) => {
                self.push_message(format!("Gagal menulis CSV: {err}"));
            }
        }
        self.tab = ResultsTab::Messages;
        cx.notify();
    }

    fn on_tree_click(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some((owner, name, group)) = schema::parse_object_id(&id) else {
            return; // group header: expand/collapse is handled by the tree
        };
        if !schema::is_describable_group(group) {
            self.push_message(format!(
                "Describe untuk {group} belum didukung di MVP ({owner}.{name})."
            ));
            self.tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        if self.busy || self.session.is_none() {
            return;
        }
        self.status = format!("Describe {owner}.{name}...");
        self.spawn_db(
            Job::Describe {
                owner: owner.to_string(),
                name: name.to_string(),
            },
            PendingOp::Describe {
                owner: owner.to_string(),
                name: name.to_string(),
            },
            cx,
        );
    }

    // ----- response handling -----

    fn on_db_response(&mut self, resp: Result<Response, RecvError>, cx: &mut Context<Self>) {
        let pending = self.pending_op.take();
        self.busy = false;
        let resp = match resp {
            Ok(resp) => resp,
            Err(_) => {
                self.push_message("DB worker berhenti.".to_string());
                self.status = "Worker error".to_string();
                cx.notify();
                return;
            }
        };
        if let Response::Failed(message) = &resp {
            self.push_message(message.clone());
            self.status = "Error (lihat Messages).".to_string();
            self.tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        match (pending, resp) {
            (Some(PendingOp::Connect), Response::Connected(info)) => {
                self.session = Some(info.clone());
                self.status = format!("Connected sebagai {}.", info.user);
                self.push_message(format!(
                    "Connected: {} @ {} ({})",
                    info.user, info.database, info.version
                ));
                cx.notify();
                self.refresh_schema(cx);
            }
            (Some(PendingOp::Run), Response::QueryDone(result)) => {
                let rows = result.row_count();
                let elapsed = result.elapsed.as_millis();
                let truncated = result.truncated;
                self.set_grid_result(result.clone(), cx);
                self.last_result = Some(result);
                self.tab = ResultsTab::Results;
                self.status = if truncated {
                    format!("{rows} baris dalam {elapsed}ms (dibatasi {DEFAULT_MAX_ROWS} baris)")
                } else {
                    format!("{rows} baris dalam {elapsed}ms")
                };
            }
            (Some(PendingOp::Run), Response::Executed(affected)) => {
                self.push_message(format!("{affected} baris terpengaruh."));
                self.status = format!("{affected} baris terpengaruh (belum commit).");
                self.tab = ResultsTab::Messages;
            }
            (Some(PendingOp::Explain), Response::Plan(lines)) => {
                self.plan_lines = lines;
                self.tab = ResultsTab::Plan;
                self.status = "Explain plan selesai.".to_string();
            }
            (Some(PendingOp::ListObjects), Response::Objects(objects)) => {
                let count = objects.len();
                self.objects = objects;
                let items = schema::build_tree_items(&self.objects);
                self.tree_state.update(cx, |state, cx| {
                    state.set_items(items, cx);
                });
                self.status = format!("{count} objek dimuat.");
            }
            (Some(PendingOp::Describe { owner, name }), Response::Described(defs)) => {
                let result = schema::describe_to_result(&defs);
                let count = result.row_count();
                self.set_grid_result(result.clone(), cx);
                self.last_result = Some(result);
                self.tab = ResultsTab::Results;
                self.status = format!("{owner}.{name}: {count} kolom.");
            }
            (Some(PendingOp::Commit), Response::Committed) => {
                self.push_message("Commit berhasil.".to_string());
                self.status = "Commit berhasil.".to_string();
            }
            (Some(PendingOp::Rollback), Response::RolledBack) => {
                self.push_message("Rollback berhasil.".to_string());
                self.status = "Rollback berhasil.".to_string();
            }
            _ => {}
        }
        cx.notify();
    }

    // ----- rendering -----

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sidebar = v_flex().w(px(280.)).p_2().gap_2();
        if let Some(session) = &self.session {
            let mode = if self.demo_mode { " (demo)" } else { "" };
            sidebar = sidebar
                .child(format!("Connected: {}{mode}", session.user))
                .child(format!("{} {}", session.database, session.version))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("disconnect")
                                .label("Disconnect")
                                .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                        )
                        .child(
                            Button::new("refresh-schema")
                                .label("Refresh")
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_schema(cx))),
                        ),
                );
        } else {
            sidebar = sidebar
                .child("Nama")
                .child(Input::new(&self.name_input).w_full())
                .child("Username")
                .child(Input::new(&self.user_input).w_full())
                .child("Connect string")
                .child(Input::new(&self.host_input).w_full())
                .child("Password")
                .child(Input::new(&self.password_input).mask_toggle().w_full())
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("connect")
                                .label("Connect")
                                .on_click(cx.listener(|this, _, _, cx| this.connect(false, cx))),
                        )
                        .child(
                            Button::new("demo")
                                .label("Demo")
                                .on_click(cx.listener(|this, _, _, cx| this.connect(true, cx))),
                        ),
                );
        }
        sidebar = sidebar.child("Schema").child(self.render_tree(cx));
        sidebar
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        tree(&self.tree_state, {
            let view = cx.entity();
            move |ix, entry, selected, _window, cx| {
                view.update(cx, |_, cx| {
                    let item = entry.item();
                    let item_id = item.id.clone();
                    ListItem::new(ix)
                        .selected(selected)
                        .pl(px(12. + 16. * entry.depth() as f32))
                        .child(item.label.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.on_tree_click(item_id.clone(), cx);
                        }))
                })
            }
        })
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        match self.tab {
            ResultsTab::Results => div()
                .flex_1()
                .child(
                    DataTable::new(&self.table_state)
                        .stripe(true)
                        .bordered(true),
                )
                .into_any_element(),
            ResultsTab::Plan => {
                if self.plan_lines.is_empty() {
                    div()
                        .p_2()
                        .child("Jalankan Explain untuk melihat execution plan.")
                        .into_any_element()
                } else {
                    v_flex()
                        .flex_1()
                        .p_2()
                        .font_family(cx.theme().mono_font_family.clone())
                        .children(self.plan_lines.iter().map(|line| div().child(line.clone())))
                        .into_any_element()
                }
            }
            ResultsTab::Messages => {
                let start = self.messages.len().saturating_sub(MAX_MESSAGES_SHOWN);
                v_flex()
                    .flex_1()
                    .p_2()
                    .gap_1()
                    .children(
                        self.messages[start..]
                            .iter()
                            .map(|m| div().child(m.clone())),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for Workbench {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self
            .last_result
            .as_ref()
            .map(|r| r.row_count())
            .unwrap_or(0);
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .gap_2()
                    .p_2()
                    .child(
                        Button::new("run")
                            .label("Run")
                            .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                    )
                    .child(
                        Button::new("explain")
                            .label("Explain")
                            .on_click(cx.listener(|this, _, _, cx| this.explain(cx))),
                    )
                    .child(
                        Button::new("commit")
                            .label("Commit")
                            .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
                    )
                    .child(
                        Button::new("rollback")
                            .label("Rollback")
                            .on_click(cx.listener(|this, _, _, cx| this.rollback(cx))),
                    )
                    .child(
                        Button::new("export")
                            .label("Export CSV")
                            .on_click(cx.listener(|this, _, _, cx| this.export_csv(cx))),
                    )
                    .child(div().flex_1())
                    .child(self.status.clone()),
            )
            .child(
                h_flex().flex_1().child(self.render_sidebar(cx)).child(
                    v_flex()
                        .flex_1()
                        .child(Editor::new(&self.editor).flex_1())
                        .child(
                            h_flex()
                                .gap_2()
                                .p_2()
                                .child(
                                    Button::new("tab-results")
                                        .label(format!("Results ({rows})"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.tab = ResultsTab::Results;
                                            cx.notify();
                                        })),
                                )
                                .child(Button::new("tab-plan").label("Explain Plan").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.tab = ResultsTab::Plan;
                                        cx.notify();
                                    }),
                                ))
                                .child(
                                    Button::new("tab-messages")
                                        .label(format!("Messages ({})", self.messages.len()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.tab = ResultsTab::Messages;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(self.render_results(cx)),
                ),
            )
    }
}
