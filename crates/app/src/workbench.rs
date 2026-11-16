//! The workbench: DB Explorer layout — icon rail, connections/schema panel,
//! query tabs, toolbar, SQL editor, result grid, explain-plan panel, and
//! status bar. Density (heights, spacing, radius, font sizes) follows the
//! design-system spec via [`crate::theme`] constants.
//!
//! All database work runs on the worker thread ([`crate::worker`]); this
//! view only sends jobs and applies responses. One operation runs at a
//! time (`busy` + `pending_op`); Stop uses a generation counter so late
//! responses from a superseded operation are dropped. That is a
//! soft-cancel: the worker thread stays blocked until Oracle returns (see
//! [`Workbench::stop`]).

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, RecvError};

use gpui_kit::actions;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::tree::{tree, TreeItem, TreeState};
use gpui_kit::component::{
    h_resizable, resizable_panel, v_resizable, ActiveTheme as _, Disableable as _, Icon,
    Sizable as _,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, App, ClickEvent, Context, Entity, FontWeight, Hsla, IntoElement, Render,
    SharedString, Subscription, Window,
};
use ora_core::export::{to_csv, to_xlsx};
use ora_core::sql::{clean_statement, format_sql};
use ora_core::store::{self, HistoryEntry, SavedProfile, ThemePreference};
use ora_core::{ConnectionProfile, DbInfo, PlanNode, QueryResult, SchemaObject, DEFAULT_MAX_ROWS};

use crate::plan;
use crate::results::ResultsDelegate;
use crate::schema;
use crate::theme::{
    self, FONT_BODY, FONT_CAPTION, FONT_HEADING_MD, FONT_HEADING_XL, FONT_LABEL, FONT_RAIL,
    GRID_ROW_HEIGHT, LEADING_BODY, LEADING_CAPTION, LEADING_HEADING_MD, LEADING_HEADING_XL,
    LEADING_LABEL, LEADING_RAIL, PAGINATION_HEIGHT, PLAN_PANEL_WIDTH, RADIUS_MD, RADIUS_SM,
    RAIL_BUTTON_HEIGHT, RAIL_WIDTH, SIDE_PANEL_WIDTH, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
    STATUS_BAR_HEIGHT, SUB_TAB_HEIGHT, TAB_BAR_HEIGHT, TOOLBAR_HEIGHT, TREE_ROW_HEIGHT,
};
use crate::worker::{DbHandle, Job, Response};

actions!(db_explorer, [RunQuery]);

const MAX_MESSAGES: usize = 200;
const MAX_MESSAGES_SHOWN: usize = 30;

const WELCOME_SQL: &str = "-- Klik Demo di kiri untuk coba tanpa database,\n\
     -- atau isi koneksi lalu Connect.\n\
     SELECT * FROM employees";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultsTab {
    Results,
    Plan,
    Messages,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RailPanel {
    Connections,
    History,
    Favorites,
    Settings,
}

#[derive(Clone)]
enum PendingOp {
    Connect,
    TestConnect,
    Run,
    Explain,
    ListObjects,
    Describe { owner: String, name: String },
    Commit,
    Rollback,
}

/// One query tab: its own editor, grid, last result, and plan.
struct QueryTab {
    id: u64,
    title: String,
    editor: Entity<EditorState>,
    table_state: Entity<TableState<ResultsDelegate>>,
    last_result: Option<QueryResult>,
    elapsed_ms: Option<u128>,
    plan_nodes: Option<Vec<PlanNode>>,
    plan_lines: Vec<String>,
    plan_expanded: HashSet<i64>,
    results_tab: ResultsTab,
}

impl QueryTab {
    fn new(
        id: u64,
        title: String,
        default_sql: &str,
        window: &mut Window,
        cx: &mut Context<Workbench>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(&mut *window, cx)
                .language("sql")
                .line_number(true)
                .folding(true)
                .default_value(default_sql)
        });
        let table_state = cx.new(|cx| TableState::new(ResultsDelegate::empty(), &mut *window, cx));
        QueryTab {
            id,
            title,
            editor,
            table_state,
            last_result: None,
            elapsed_ms: None,
            plan_nodes: None,
            plan_lines: Vec::new(),
            plan_expanded: HashSet::new(),
            results_tab: ResultsTab::Results,
        }
    }
}

pub struct Workbench {
    name_input: Entity<InputState>,
    user_input: Entity<InputState>,
    host_input: Entity<InputState>,
    password_input: Entity<InputState>,
    search_input: Entity<InputState>,
    rail: RailPanel,
    profiles: Vec<SavedProfile>,
    editing_profile: Option<usize>,
    /// Whether the new-connection form is shown while connected. The form
    /// is always shown while disconnected; once connected it collapses
    /// until "+" toggles it back. Collapsing never clears the inputs:
    /// they live on `Workbench` as entities and survive re-render.
    show_connection_form: bool,
    history: Vec<HistoryEntry>,
    theme_pref: ThemePreference,
    tabs: Vec<QueryTab>,
    active_tab: usize,
    show_home: bool,
    next_tab_id: u64,
    db: Option<DbHandle>,
    session: Option<DbInfo>,
    demo_mode: bool,
    busy: bool,
    pending_op: Option<PendingOp>,
    pending_sql: Option<String>,
    op_generation: u64,
    connect_error: Option<String>,
    tree_state: Entity<TreeState>,
    objects: Vec<SchemaObject>,
    tree_filter: String,
    messages: Vec<String>,
    status: String,
    _subscriptions: Vec<Subscription>,
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
        let search_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Search schemas..."));
        let tree_state = cx.new(|cx| TreeState::new(cx).items(Vec::<TreeItem>::new()));
        let first_tab = QueryTab::new(1, "Query 1".to_string(), WELCOME_SQL, window, cx);

        // Rebuild the tree on every keystroke in the search box.
        let search_sub = cx.subscribe(&search_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.apply_tree_filter(cx);
            }
        });
        // Follow OS appearance changes while the preference is System.
        let appearance_sub = window
            .observe_window_appearance(|window, cx| theme::sync_system_appearance(window, cx));

        Workbench {
            name_input,
            user_input,
            host_input,
            password_input,
            search_input,
            rail: RailPanel::Connections,
            profiles: store::load_profiles(),
            editing_profile: None,
            show_connection_form: true,
            history: store::load_history(),
            theme_pref: theme::preference(cx),
            tabs: vec![first_tab],
            active_tab: 0,
            show_home: false,
            next_tab_id: 1,
            db: None,
            session: None,
            demo_mode: false,
            busy: false,
            pending_op: None,
            pending_sql: None,
            op_generation: 0,
            connect_error: None,
            tree_state,
            objects: Vec::new(),
            tree_filter: String::new(),
            messages: Vec::new(),
            status: "Belum terhubung.".to_string(),
            _subscriptions: vec![search_sub, appearance_sub],
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

    fn set_input(
        entity: &Entity<InputState>,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        entity.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    fn active_tab(&self) -> &QueryTab {
        &self.tabs[self.active_tab]
    }

    fn active_tab_mut(&mut self) -> &mut QueryTab {
        &mut self.tabs[self.active_tab]
    }

    fn editor_text(&self, cx: &App) -> String {
        self.active_tab().editor.read(cx).value().to_string()
    }

    fn set_grid_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.last_result = Some(result.clone());
        tab.table_state.update(cx, |state, cx| {
            state.delegate_mut().set_result(result);
            state.refresh(cx);
        });
    }

    fn clear_all_grids(&mut self, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            tab.last_result = None;
            tab.elapsed_ms = None;
            tab.plan_nodes = None;
            tab.plan_lines.clear();
            tab.table_state.update(cx, |state, cx| {
                state.delegate_mut().set_result(QueryResult::default());
                state.refresh(cx);
            });
        }
    }

    /// Send a job to the worker and apply the response when it arrives. The
    /// blocking receive runs on the background executor, never the UI
    /// thread. The generation tag lets [`Workbench::stop`] drop the
    /// response of a superseded operation.
    fn spawn_db(&mut self, job: Job, op: PendingOp, cx: &mut Context<Self>) {
        let Some(db) = self.db.clone() else {
            self.push_message("Belum terhubung.".to_string());
            return;
        };
        let rx: Receiver<Response> = db.request(job);
        self.op_generation += 1;
        let generation = self.op_generation;
        self.pending_op = Some(op);
        self.busy = true;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = view.update(cx, |this, cx| this.on_db_response(generation, resp, cx));
        })
        .detach();
    }

    fn record_run(&mut self, sql: &str) {
        store::record_history(&mut self.history, sql, store::now_unix_secs());
        if let Err(err) = store::save_history(&self.history) {
            self.push_message(format!("Gagal menyimpan history: {err}"));
        }
    }

    fn persist_profiles(&mut self) {
        if let Err(err) = store::save_profiles(&self.profiles) {
            self.push_message(format!("Gagal menyimpan koneksi: {err}"));
        }
    }

    fn persist_history(&mut self) {
        if let Err(err) = store::save_history(&self.history) {
            self.push_message(format!("Gagal menyimpan history: {err}"));
        }
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
                self.active_tab_mut().results_tab = ResultsTab::Messages;
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
        self.connect_error = None;
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

    fn test_connection(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let username = Self::input_text(&self.user_input, cx);
        let connect_string = Self::input_text(&self.host_input, cx);
        if username.is_empty() || connect_string.is_empty() {
            self.push_message("Isi username dan connect string dulu.".to_string());
            cx.notify();
            return;
        }
        if self.db.is_none() {
            self.db = Some(DbHandle::spawn());
        }
        let name = Self::input_text(&self.name_input, cx);
        self.status = "Mengetes koneksi...".to_string();
        self.spawn_db(
            Job::TestConnect {
                profile: ConnectionProfile {
                    name,
                    username,
                    connect_string,
                },
                password: Self::input_text(&self.password_input, cx),
            },
            PendingOp::TestConnect,
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
        self.show_connection_form = true;
        self.objects.clear();
        self.tree_state.update(cx, |state, cx| {
            state.set_items(Vec::<TreeItem>::new(), cx);
        });
        self.clear_all_grids(cx);
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
        if self.busy || self.show_home {
            return;
        }
        if self.session.is_none() {
            self.push_message("Belum terhubung: Connect atau Demo dulu.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        let sql = self.editor_text(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("Editor kosong.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.status = "Menjalankan...".to_string();
        self.pending_sql = Some(sql.clone());
        self.spawn_db(Job::Run { sql }, PendingOp::Run, cx);
    }

    fn explain(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.show_home {
            return;
        }
        if self.session.is_none() {
            self.push_message("Belum terhubung: Connect atau Demo dulu.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        let sql = self.editor_text(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("Editor kosong.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.status = "Menjelaskan plan...".to_string();
        self.spawn_db(Job::Explain { sql }, PendingOp::Explain, cx);
    }

    fn format_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_home {
            return;
        }
        let sql = self.editor_text(cx);
        let formatted = format_sql(&sql);
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |state, cx| state.set_value(formatted, window, cx));
        cx.notify();
    }

    /// Soft-cancel only: bump the generation so the in-flight response is
    /// dropped when it arrives, and clear the busy state immediately. The
    /// worker thread itself stays blocked until Oracle returns, and a job
    /// sent afterwards queues behind it in the worker channel. This is NOT
    /// true cancellation — Oracle offers no cancel handle through the thin
    /// driver call in use — and the UI must never claim otherwise.
    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            return;
        }
        self.op_generation += 1;
        self.pending_op = None;
        self.pending_sql = None;
        self.busy = false;
        self.status = "Dihentikan (respons dibuang).".to_string();
        self.push_message(
            "Stop: operasi database yang sedang berjalan tidak dapat dibatalkan; responsnya akan diabaikan saat tiba.".to_string(),
        );
        cx.notify();
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
        let Some(result) = self.active_tab().last_result.clone() else {
            self.push_message("Belum ada hasil query untuk diekspor.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        };
        let csv = to_csv(&result);
        let filename = export_filename(unix_stamp());
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
        self.active_tab_mut().results_tab = ResultsTab::Messages;
        cx.notify();
    }

    fn export_xlsx(&mut self, cx: &mut Context<Self>) {
        let Some(result) = self.active_tab().last_result.clone() else {
            self.push_message("Belum ada hasil query untuk diekspor.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        };
        match to_xlsx(&result) {
            Ok(bytes) => {
                let filename = export_xlsx_filename(unix_stamp());
                let path = std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .join(&filename);
                match std::fs::write(&path, bytes) {
                    Ok(()) => {
                        self.push_message(format!(
                            "XLSX tersimpan: {} ({} baris)",
                            path.display(),
                            result.row_count()
                        ));
                        self.status = format!("Exported {filename}");
                    }
                    Err(err) => {
                        self.push_message(format!("Gagal menulis XLSX: {err}"));
                    }
                }
            }
            Err(err) => {
                self.push_message(format!("Gagal membuat XLSX: {err}"));
            }
        }
        self.active_tab_mut().results_tab = ResultsTab::Messages;
        cx.notify();
    }

    // ----- tabs -----

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.next_tab_id += 1;
        let id = self.next_tab_id;
        let tab = QueryTab::new(id, format!("Query {id}"), "", window, cx);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_home = false;
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 || ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if ix < self.active_tab {
            self.active_tab -= 1;
        }
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.show_home = false;
        cx.notify();
    }

    fn new_tab_with_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab(window, cx);
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |state, cx| state.set_value(sql, window, cx));
    }

    fn load_sql_into_active_tab(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.show_home {
            self.new_tab_with_sql(sql, window, cx);
            return;
        }
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |state, cx| state.set_value(sql, window, cx));
        cx.notify();
    }

    // ----- connection manager -----

    fn fill_form(&mut self, profile: &SavedProfile, window: &mut Window, cx: &mut Context<Self>) {
        Self::set_input(&self.name_input.clone(), profile.name.clone(), window, cx);
        Self::set_input(
            &self.user_input.clone(),
            profile.username.clone(),
            window,
            cx,
        );
        Self::set_input(
            &self.host_input.clone(),
            profile.connect_string.clone(),
            window,
            cx,
        );
        // Passwords are never stored: always start blank.
        Self::set_input(&self.password_input.clone(), String::new(), window, cx);
    }

    fn edit_profile(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(profile) = self.profiles.get(ix).cloned() {
            self.editing_profile = Some(ix);
            self.fill_form(&profile, window, cx);
            cx.notify();
        }
    }

    fn new_profile_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_profile = None;
        Self::set_input(&self.name_input.clone(), String::new(), window, cx);
        Self::set_input(&self.user_input.clone(), String::new(), window, cx);
        Self::set_input(&self.host_input.clone(), String::new(), window, cx);
        Self::set_input(&self.password_input.clone(), String::new(), window, cx);
        cx.notify();
    }

    /// "+" in the Connections header: while connected it toggles the
    /// new-connection form (hidden by default once connected); while
    /// disconnected the form is always visible, so "+" clears it for a
    /// fresh profile instead. Toggling never clears typed text.
    fn toggle_connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_some() {
            self.show_connection_form = !self.show_connection_form;
            cx.notify();
        } else {
            self.show_connection_form = true;
            self.new_profile_form(window, cx);
        }
    }

    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let username = Self::input_text(&self.user_input, cx);
        let connect_string = Self::input_text(&self.host_input, cx);
        if username.is_empty() || connect_string.is_empty() {
            self.push_message("Isi username dan connect string dulu.".to_string());
            cx.notify();
            return;
        }
        let name = Self::input_text(&self.name_input, cx);
        let profile = SavedProfile {
            name: if name.is_empty() {
                format!("{username}@{connect_string}")
            } else {
                name
            },
            username,
            connect_string,
        };
        match self.editing_profile {
            Some(ix) if ix < self.profiles.len() => self.profiles[ix] = profile,
            _ => {
                self.profiles.push(profile);
                self.editing_profile = Some(self.profiles.len() - 1);
            }
        }
        self.persist_profiles();
        self.push_message("Profil koneksi tersimpan.".to_string());
        cx.notify();
    }

    fn remove_profile(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.profiles.len() {
            return;
        }
        let removed = self.profiles.remove(ix);
        if self.editing_profile == Some(ix) {
            self.editing_profile = None;
        } else if let Some(editing) = self.editing_profile {
            if ix < editing {
                self.editing_profile = Some(editing - 1);
            }
        }
        self.persist_profiles();
        self.push_message(format!("Profil '{}' dihapus.", removed.name));
        cx.notify();
    }

    // ----- history / favorites / theme -----

    fn toggle_favorite(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(entry) = self.history.get_mut(ix) {
            entry.favorite = !entry.favorite;
            self.persist_history();
            cx.notify();
        }
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.history.clear();
        self.persist_history();
        cx.notify();
    }

    fn set_theme(&mut self, pref: ThemePreference, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_pref = pref;
        theme::apply_preference(pref, window, cx);
        if let Err(err) = store::save_theme_preference(pref) {
            self.push_message(format!("Gagal menyimpan preferensi tema: {err}"));
        }
        cx.notify();
    }

    pub(crate) fn toggle_plan_node(&mut self, id: i64) {
        let expanded = &mut self.active_tab_mut().plan_expanded;
        if !expanded.insert(id) {
            expanded.remove(&id);
        }
    }

    fn goto_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.table_state.update(cx, |state, cx| {
            state.delegate_mut().set_page(page);
            state.refresh(cx);
        });
        cx.notify();
    }

    // ----- schema tree -----

    fn apply_tree_filter(&mut self, cx: &mut Context<Self>) {
        self.tree_filter = Self::input_text(&self.search_input, cx);
        let filtered = schema::filter_objects(&self.objects, &self.tree_filter);
        let items = schema::build_tree_items(filtered);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
    }

    fn on_tree_click(
        &mut self,
        id: SharedString,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((owner, name, group)) = schema::parse_object_id(&id) else {
            return; // group header: expand/collapse is handled by the tree
        };
        // No dedicated double-click API: the platform reports the tap count
        // on the mouse-down event, so a double-click arrives as a second
        // click with `click_count >= 2`. The first click still runs the
        // single-click describe below; the data query supersedes it.
        let double_click =
            matches!(event, ClickEvent::Mouse(m) if m.down.click_count >= 2);
        if double_click && schema::is_describable_group(group) {
            self.open_table_data(owner, name, window, cx);
            return;
        }
        if !schema::is_describable_group(group) {
            self.push_message(format!(
                "Describe untuk {group} belum didukung di MVP ({owner}.{name})."
            ));
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        if self.busy || self.session.is_none() {
            return;
        }
        self.show_home = false;
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

    /// Double-click on a table/view row: fill the editor with a capped
    /// `SELECT *` and run it, like Toad/Navicat. A describe fired by the
    /// first click of the double-click is superseded with the same
    /// soft-cancel as Stop (its late response is dropped by the generation
    /// check); a real in-flight query is never cancelled.
    fn open_table_data(
        &mut self,
        owner: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.is_none() {
            return;
        }
        if self.busy && matches!(self.pending_op, Some(PendingOp::Describe { .. })) {
            self.op_generation += 1;
            self.pending_op = None;
            self.pending_sql = None;
            self.busy = false;
        }
        if self.busy {
            return;
        }
        self.show_home = false;
        let sql = schema::select_all_sql(owner, name);
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |state, cx| state.set_value(sql, window, cx));
        self.run(cx);
    }

    // ----- response handling -----

    fn on_db_response(
        &mut self,
        generation: u64,
        resp: Result<Response, RecvError>,
        cx: &mut Context<Self>,
    ) {
        // A superseded operation (after Stop, or a newer op) drops its
        // response entirely: stale data must never overwrite fresh state.
        if generation != self.op_generation {
            return;
        }
        let pending = self.pending_op.take();
        let pending_sql = self.pending_sql.take();
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
            if matches!(pending, Some(PendingOp::Connect)) {
                self.connect_error = Some(message.clone());
            }
            self.push_message(message.clone());
            self.status = "Error (lihat Messages).".to_string();
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        match (pending, resp) {
            (Some(PendingOp::Connect), Response::Connected(info)) => {
                self.session = Some(info.clone());
                self.connect_error = None;
                self.show_connection_form = false;
                self.status = format!("Connected sebagai {}.", info.user);
                self.push_message(format!(
                    "Connected: {} @ {} ({})",
                    info.user, info.database, info.version
                ));
                cx.notify();
                self.refresh_schema(cx);
            }
            (Some(PendingOp::TestConnect), Response::Tested(outcome)) => match outcome {
                Ok(info) => {
                    self.push_message(format!(
                        "Test koneksi berhasil: {} @ {} ({})",
                        info.user, info.database, info.version
                    ));
                    self.status = "Test koneksi berhasil.".to_string();
                }
                Err(message) => {
                    self.push_message(format!("Test koneksi gagal: {message}"));
                    self.status = "Test koneksi gagal.".to_string();
                }
            },
            (Some(PendingOp::Run), Response::QueryDone(result)) => {
                let rows = result.row_count();
                let elapsed = result.elapsed.as_millis();
                let truncated = result.truncated;
                if let Some(sql) = pending_sql {
                    self.record_run(&sql);
                }
                self.active_tab_mut().elapsed_ms = Some(elapsed);
                self.set_grid_result(result, cx);
                self.active_tab_mut().results_tab = ResultsTab::Results;
                self.status = if truncated {
                    format!("{rows} baris dalam {elapsed}ms (dibatasi {DEFAULT_MAX_ROWS} baris)")
                } else {
                    format!("{rows} baris dalam {elapsed}ms")
                };
            }
            (Some(PendingOp::Run), Response::Executed(affected)) => {
                if let Some(sql) = pending_sql {
                    self.record_run(&sql);
                }
                self.push_message(format!("{affected} baris terpengaruh."));
                self.status = format!("{affected} baris terpengaruh (belum commit).");
                self.active_tab_mut().results_tab = ResultsTab::Messages;
            }
            (Some(PendingOp::Explain), Response::PlanTree(nodes, lines)) => {
                let tab = self.active_tab_mut();
                tab.plan_expanded = plan::all_plan_ids(&nodes);
                tab.plan_nodes = Some(nodes);
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.status = "Explain plan selesai.".to_string();
            }
            (Some(PendingOp::Explain), Response::Plan(lines)) => {
                let tab = self.active_tab_mut();
                tab.plan_nodes = None;
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.status = "Explain plan selesai.".to_string();
            }
            (Some(PendingOp::ListObjects), Response::Objects(objects)) => {
                let count = objects.len();
                self.objects = objects;
                self.apply_tree_filter(cx);
                self.status = format!("{count} objek dimuat.");
            }
            (Some(PendingOp::Describe { owner, name }), Response::Described(defs)) => {
                let result = schema::describe_to_result(&defs);
                let count = result.row_count();
                self.set_grid_result(result, cx);
                self.active_tab_mut().results_tab = ResultsTab::Results;
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

    fn conn_status(&self, theme: &gpui_kit::component::Theme) -> (Hsla, &'static str, String) {
        if self.busy && matches!(self.pending_op, Some(PendingOp::Connect)) {
            return (theme.warning, "Connecting…", "Menghubungkan...".to_string());
        }
        if let Some(session) = &self.session {
            let mode = if self.demo_mode { "Demo" } else { "Connected" };
            return (
                theme.success,
                mode,
                format!("{} @ {}", session.user, session.database),
            );
        }
        if self.connect_error.is_some() {
            return (
                theme.danger,
                "Connection failed",
                "Koneksi gagal (lihat Messages).".to_string(),
            );
        }
        (
            theme.muted_foreground,
            "Disconnected",
            "Belum terhubung.".to_string(),
        )
    }

    // ----- rendering: chrome -----

    fn render_tab_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(TAB_BAR_HEIGHT))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border);
        bar = bar.child(self.render_home_tab(theme, cx));
        for (ix, tab) in self.tabs.iter().enumerate() {
            let active = !self.show_home && ix == self.active_tab;
            bar = bar.child(self.render_query_tab(ix, tab, active, theme, cx));
        }
        bar.child(
            div()
                .id("tab-add")
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .h(px(28.))
                .w(px(28.))
                .rounded(px(RADIUS_MD))
                .hover(|this| this.bg(theme.secondary))
                .on_click(cx.listener(|this, _, window, cx| this.add_tab(window, cx)))
                .child(icon(IconName::Plus, 16., theme.muted_foreground)),
        )
    }

    fn render_home_tab(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.show_home;
        let mut tab = div()
            .id("tab-home")
            .flex()
            .flex_row()
            .items_center()
            .h(px(28.))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .rounded(px(RADIUS_MD))
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_home = true;
                cx.notify();
            }));
        if active {
            tab = tab
                .bg(theme.tab_active)
                .border_1()
                .border_color(theme.border)
                .text_color(theme.tab_active_foreground);
        } else {
            tab = tab
                .text_color(theme.tab_foreground)
                .hover(|this| this.bg(theme.secondary));
        }
        tab.child(icon(
            IconName::House,
            14.,
            if active {
                theme.primary
            } else {
                theme.muted_foreground
            },
        ))
        .child(
            div()
                .text_size(px(FONT_LABEL))
                .line_height(px(LEADING_LABEL))
                .font_weight(FontWeight::MEDIUM)
                .child("Home"),
        )
    }

    fn render_query_tab(
        &self,
        ix: usize,
        tab: &QueryTab,
        active: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The label area and the close button are siblings, not nested
        // click targets, so closing a tab never also activates it.
        let mut container = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(28.))
            .rounded(px(RADIUS_MD));
        if active {
            container = container
                .bg(theme.tab_active)
                .border_1()
                .border_color(theme.border);
        } else {
            container = container.hover(|this| this.bg(theme.secondary));
        }
        let label_color = if active {
            theme.tab_active_foreground
        } else {
            theme.tab_foreground
        };
        container
            .child(
                div()
                    .id(format!("query-tab-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_1()
                    .h(relative(1.0))
                    .gap(px(SPACE_1))
                    .pl(px(SPACE_2))
                    .pr(px(SPACE_1))
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_tab(ix, cx)))
                    .child(icon(
                        IconName::FileCode,
                        14.,
                        if active {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(FONT_LABEL))
                            .line_height(px(LEADING_LABEL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(label_color)
                            .child(tab.title.clone()),
                    ),
            )
            .child(
                div()
                    .id(format!("query-tab-close-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .h(px(20.))
                    .w(px(20.))
                    .mr(px(SPACE_1))
                    .rounded(px(RADIUS_SM))
                    .hover(|this| this.bg(theme.danger).text_color(theme.danger_foreground))
                    .on_click(cx.listener(move |this, _, _, cx| this.close_tab(ix, cx)))
                    .child(icon(IconName::X, 12., theme.muted_foreground)),
            )
    }

    fn render_rail(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let items = [
            (RailPanel::Connections, IconName::Database, "Connections"),
            (RailPanel::History, IconName::Clock, "History"),
            (RailPanel::Favorites, IconName::Star, "Favorites"),
            (RailPanel::Settings, IconName::Settings, "Settings"),
        ];
        let mut rail = div()
            .flex()
            .flex_col()
            .items_center()
            .w(px(RAIL_WIDTH))
            .py(px(SPACE_2))
            .gap(px(SPACE_1))
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border);
        for (panel, icon_name, label) in items {
            let active = self.rail == panel;
            let mut button = div()
                .id(format!("rail-{label}"))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .w(px(RAIL_WIDTH - 6.))
                .h(px(RAIL_BUTTON_HEIGHT))
                .gap(px(SPACE_1))
                .rounded(px(RADIUS_MD))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.rail = panel;
                    cx.notify();
                }));
            if active {
                button = button.bg(theme.accent);
            } else {
                button = button.hover(|this| this.bg(theme.secondary));
            }
            let color = if active {
                theme.primary
            } else {
                theme.muted_foreground
            };
            rail = rail.child(
                button.child(icon(icon_name, 20., color)).child(
                    div()
                        .text_size(px(FONT_RAIL))
                        .line_height(px(LEADING_RAIL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .child(label),
                ),
            );
        }
        rail
    }

    // ----- rendering: side panels -----

    fn render_side_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.rail {
            RailPanel::Connections => self.render_connections_panel(theme, cx).into_any_element(),
            RailPanel::History => self
                .render_history_panel(false, theme, cx)
                .into_any_element(),
            RailPanel::Favorites => self
                .render_history_panel(true, theme, cx)
                .into_any_element(),
            RailPanel::Settings => self.render_settings_panel(theme, cx).into_any_element(),
        }
    }

    fn render_panel_header(
        &self,
        title: &str,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(SPACE_3))
            .gap(px(SPACE_2))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_size(px(FONT_BODY))
                    .line_height(px(LEADING_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title.to_string()),
            )
    }

    fn render_connections_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut panel = div().flex().flex_col().size_full().bg(theme.background);
        let mut header = self.render_panel_header("Connections", theme);
        header = header.child(div().flex_1()).child(
            div()
                .id("profile-new")
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .h(px(28.))
                .w(px(28.))
                .rounded(px(RADIUS_MD))
                .hover(|this| this.bg(theme.secondary))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_connection_form(window, cx)))
                .child(icon(IconName::Plus, 16., theme.muted_foreground)),
        );
        panel = panel.child(header);
        panel = panel.child(self.render_session_card(theme, cx));
        panel = panel.child(self.render_profile_list(theme, cx));
        if self.session.is_none() || self.show_connection_form {
            panel = panel.child(self.render_connection_form(theme, cx));
        }
        panel = panel.child(
            div()
                .px(px(SPACE_3))
                .pb(px(SPACE_2))
                .child(Input::new(&self.search_input).w_full()),
        );
        panel = panel.child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .px(px(SPACE_2))
                .child(self.render_tree_area(theme, cx)),
        );
        panel
    }

    fn render_session_card(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(session) = &self.session else {
            return div()
                .px(px(SPACE_3))
                .pt(px(SPACE_3))
                .child(caption_text(
                    "Not connected. Fill in the form or try Demo mode.".to_string(),
                    theme,
                ))
                .into_any_element();
        };
        let mode = if self.demo_mode { "Demo" } else { "Oracle" };
        div()
            .px(px(SPACE_3))
            .pt(px(SPACE_3))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1))
                    .p(px(SPACE_3))
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(SPACE_2))
                            .child(status_dot(theme.success))
                            .child(
                                div()
                                    .text_size(px(FONT_LABEL))
                                    .line_height(px(LEADING_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(format!("{} · {}", session.user, mode)),
                            ),
                    )
                    .child(caption_text(
                        format!("{} {}", session.database, session.version),
                        theme,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(SPACE_2))
                            .pt(px(SPACE_1))
                            .child(
                                Button::new("disconnect")
                                    .label("Disconnect")
                                    .icon(IconName::LogOut)
                                    .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                            )
                            .child(
                                Button::new("refresh-schema")
                                    .label("Refresh")
                                    .icon(IconName::RefreshCw)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.refresh_schema(cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_profile_list(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.profiles.is_empty() {
            return div().into_any_element();
        }
        let mut list = div()
            .flex()
            .flex_col()
            .px(px(SPACE_3))
            .pt(px(SPACE_3))
            .gap(px(SPACE_1));
        for (ix, profile) in self.profiles.iter().enumerate() {
            let selected = self.editing_profile == Some(ix);
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .h(px(TOOLBAR_HEIGHT))
                .rounded(px(RADIUS_MD));
            if selected {
                row = row.bg(theme.accent);
            } else {
                row = row.hover(|this| this.bg(theme.secondary));
            }
            list = list.child(
                row.child(
                    div()
                        .id(format!("profile-{ix}"))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .flex_1()
                        .h(relative(1.0))
                        .px(px(SPACE_2))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.edit_profile(ix, window, cx);
                        }))
                        .child(
                            div()
                                .text_size(px(FONT_LABEL))
                                .line_height(px(LEADING_LABEL))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .truncate()
                                .child(profile.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(FONT_CAPTION))
                                .line_height(px(LEADING_CAPTION))
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(format!("{}@{}", profile.username, profile.connect_string)),
                        ),
                )
                .child(
                    div()
                        .id(format!("profile-remove-{ix}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .h(px(28.))
                        .w(px(28.))
                        .mr(px(SPACE_1))
                        .rounded(px(RADIUS_MD))
                        .hover(|this| this.bg(theme.danger))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_profile(ix, cx)))
                        .child(icon(IconName::Trash, 14., theme.muted_foreground)),
                ),
            );
        }
        list.into_any_element()
    }

    fn render_connection_form(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .p(px(SPACE_3))
            .child(form_label(
                if self.editing_profile.is_some() {
                    "Edit connection"
                } else {
                    "New connection"
                },
                theme,
            ))
            .child(Input::new(&self.name_input).w_full())
            .child(Input::new(&self.user_input).w_full())
            .child(Input::new(&self.host_input).w_full())
            .child(Input::new(&self.password_input).mask_toggle().w_full())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(
                        Button::new("connect")
                            .label("Connect")
                            .icon(IconName::Database)
                            .primary()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.connect(false, cx))),
                    )
                    .child(
                        Button::new("test-connect")
                            .label("Test")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
                    )
                    .child(
                        Button::new("demo")
                            .label("Demo")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.connect(true, cx))),
                    ),
            )
            .child(
                div().flex().flex_row().gap(px(SPACE_2)).child(
                    Button::new("save-profile")
                        .label("Save profile")
                        .icon(IconName::Save)
                        .on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))),
                ),
            );
        if self.editing_profile.is_some() {
            let ix = self.editing_profile.unwrap();
            form = form.child(
                div().flex().flex_row().child(
                    Button::new("remove-profile")
                        .label("Remove profile")
                        .icon(IconName::Trash)
                        .danger()
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_profile(ix, cx))),
                ),
            );
        }
        form.child(caption_text(
            "Passwords are never saved; they are entered per connect.".to_string(),
            theme,
        ))
    }

    fn render_tree_area(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.objects.is_empty() {
            let message = if self.session.is_none() {
                "Connect to load schema objects."
            } else if self.tree_filter.is_empty() {
                "No objects found in this schema."
            } else {
                "No objects match the filter."
            };
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p(px(SPACE_4))
                .child(empty_state_text(message.to_string(), theme))
                .into_any_element();
        }
        div()
            .flex_1()
            .child(self.render_tree(cx))
            .into_any_element()
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
                        .h(px(TREE_ROW_HEIGHT))
                        .text_size(px(FONT_LABEL))
                        .pl(px(12. + 16. * entry.depth() as f32))
                        .child(item.label.clone())
                        .on_click(cx.listener(move |this, event, window, cx| {
                            this.on_tree_click(item_id.clone(), event, window, cx);
                        }))
                })
            }
        })
    }

    fn render_history_panel(
        &self,
        favorites_only: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = if favorites_only {
            "Favorites"
        } else {
            "History"
        };
        let mut panel = div().flex().flex_col().size_full().bg(theme.background);
        let mut header = self.render_panel_header(title, theme);
        if !favorites_only {
            header = header.child(div().flex_1()).child(
                div()
                    .id("history-clear")
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .h(px(28.))
                    .w(px(28.))
                    .rounded(px(RADIUS_MD))
                    .hover(|this| this.bg(theme.secondary))
                    .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx)))
                    .child(icon(IconName::Trash, 14., theme.muted_foreground)),
            );
        }
        panel = panel.child(header);
        let entries: Vec<(usize, &HistoryEntry)> = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, e)| !favorites_only || e.favorite)
            .collect();
        if entries.is_empty() {
            panel = panel.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .p(px(SPACE_4))
                    .child(empty_state_text(
                        if favorites_only {
                            "No favorites yet. Star a query in History to pin it here."
                        } else {
                            "No history yet. Run a query to see it here."
                        }
                        .to_string(),
                        theme,
                    )),
            );
            return panel;
        }
        let mut list = div()
            .flex()
            .flex_col()
            .flex_1()
            .id(if favorites_only {
                "favorites-scroll"
            } else {
                "history-scroll"
            })
            .overflow_y_scroll()
            .p(px(SPACE_2))
            .gap(px(SPACE_1));
        for (ix, entry) in entries {
            let preview = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let sql = entry.sql.clone();
            let favorite = entry.favorite;
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded(px(RADIUS_MD))
                    .hover(|this| this.bg(theme.secondary))
                    .child(
                        div()
                            .id(format!("history-{ix}"))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .flex_1()
                            .py(px(SPACE_1))
                            .px(px(SPACE_2))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.load_sql_into_active_tab(sql.clone(), window, cx);
                            }))
                            .child(
                                div()
                                    .text_size(px(FONT_LABEL))
                                    .line_height(px(LEADING_LABEL))
                                    .text_color(theme.foreground)
                                    .truncate()
                                    .child(preview),
                            ),
                    )
                    .child(
                        div()
                            .id(format!("history-star-{ix}"))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .h(px(28.))
                            .w(px(28.))
                            .mr(px(SPACE_1))
                            .rounded(px(RADIUS_MD))
                            .hover(|this| this.bg(theme.accent))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_favorite(ix, cx)),
                            )
                            .child(icon(
                                IconName::Star,
                                14.,
                                if favorite {
                                    theme.primary
                                } else {
                                    theme.muted_foreground
                                },
                            )),
                    ),
            );
        }
        panel.child(list)
    }

    fn render_settings_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut panel = div().flex().flex_col().size_full().bg(theme.background);
        panel = panel.child(self.render_panel_header("Settings", theme));
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .p(px(SPACE_3))
            .child(form_label("Appearance", theme));
        for (pref, icon_name, label) in [
            (ThemePreference::Light, IconName::Sun, "Light"),
            (ThemePreference::Dark, IconName::Moon, "Dark"),
            (ThemePreference::System, IconName::Monitor, "System"),
        ] {
            let selected = self.theme_pref == pref;
            let mut row = div()
                .id(format!("theme-{label}"))
                .flex()
                .flex_row()
                .items_center()
                .h(px(TOOLBAR_HEIGHT))
                .gap(px(SPACE_2))
                .px(px(SPACE_2))
                .rounded(px(RADIUS_MD))
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(pref, window, cx)));
            if selected {
                row = row.bg(theme.accent);
            } else {
                row = row.hover(|this| this.bg(theme.secondary));
            }
            let color = if selected {
                theme.accent_foreground
            } else {
                theme.foreground
            };
            row = row
                .child(icon(
                    icon_name,
                    16.,
                    if selected {
                        theme.primary
                    } else {
                        theme.muted_foreground
                    },
                ))
                .child(
                    div()
                        .text_size(px(FONT_LABEL))
                        .line_height(px(LEADING_LABEL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .child(label),
                )
                .child(div().flex_1());
            if selected {
                row = row.child(icon(IconName::Check, 14., theme.primary));
            }
            body = body.child(row);
        }
        body = body
            .child(div().h(px(SPACE_4)))
            .child(form_label("About", theme))
            .child(body_text(
                format!("DB Explorer {}", env!("CARGO_PKG_VERSION")),
                theme,
            ))
            .child(caption_text(
                "Oracle database client built with GPUI + Rust.".to_string(),
                theme,
            ))
            .child(caption_text(
                "Theme preference, saved connections, and query history are stored in your user config directory. Passwords are never saved.".to_string(),
                theme,
            ));
        panel.child(body)
    }

    // ----- rendering: main column -----

    fn render_main(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut main = div().flex().flex_col().flex_1().bg(theme.background);
        // The tab strip lives at the top of the main column only (right of
        // the sidebar), above the toolbar — as in the design. No spacer
        // above the rail/sidebar: the design shares the top row between
        // the first rail item, the sidebar header, and the tab strip.
        main = main.child(self.render_tab_bar(theme, cx));
        main = main.child(self.render_toolbar(theme, cx));
        if self.show_home {
            main = main.child(self.render_home(theme, cx));
        } else {
            main = main.child(
                div().flex_1().child(
                    v_resizable("editor-split")
                        .child(resizable_panel().child(self.render_editor()))
                        .child(
                            resizable_panel()
                                .size(px(320.))
                                .size_range(px(180.)..px(560.))
                                .child(self.render_bottom_split(theme, cx)),
                        ),
                ),
            );
        }
        main
    }

    fn render_toolbar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let no_session = self.session.is_none();
        let on_home = self.show_home;
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .gap(px(SPACE_2))
            .px(px(SPACE_3))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border);
        bar = bar
            .child(
                Button::new("run")
                    .label("Run")
                    .icon(IconName::Play)
                    .primary()
                    .loading(self.busy)
                    .disabled(on_home || no_session)
                    .tooltip("Run (Cmd/Ctrl+Enter atau F5)")
                    .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
            )
            .child(
                Button::new("stop")
                    .label("Stop")
                    .icon(IconName::Square)
                    .disabled(!self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            )
            .child(
                Button::new("explain")
                    .label("Explain")
                    .icon(IconName::ChartColumn)
                    .disabled(on_home || no_session || self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.explain(cx))),
            )
            .child(
                Button::new("format")
                    .label("Format")
                    .icon(IconName::WandSparkles)
                    .disabled(on_home)
                    .on_click(cx.listener(|this, _, window, cx| this.format_editor(window, cx))),
            )
            .child(
                Button::new("commit")
                    .label("Commit")
                    .icon(IconName::Check)
                    .ghost()
                    .disabled(no_session || self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
            )
            .child(
                Button::new("rollback")
                    .label("Rollback")
                    .icon(IconName::Undo2)
                    .ghost()
                    .disabled(no_session || self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.rollback(cx))),
            )
            .child(div().flex_1());
        let (dot_color, _, conn_detail) = self.conn_status(theme);
        let pill_label = if self.session.is_some() {
            conn_detail
        } else {
            "Not connected".to_string()
        };
        bar.child(
            div()
                .id("conn-pill")
                .flex()
                .flex_row()
                .items_center()
                .h(px(28.))
                .gap(px(SPACE_2))
                .px(px(SPACE_2))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(theme.border)
                .hover(|this| this.bg(theme.secondary))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.rail = RailPanel::Connections;
                    cx.notify();
                }))
                .child(status_dot(dot_color))
                .child(
                    div()
                        .text_size(px(FONT_LABEL))
                        .line_height(px(LEADING_LABEL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(pill_label),
                )
                .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
        )
    }

    fn render_editor(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(Editor::new(&self.active_tab().editor).flex_1())
    }

    fn render_bottom_split(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_resizable("bottom-split")
            .child(resizable_panel().child(self.render_results_column(theme, cx)))
            .child(
                resizable_panel()
                    .size(px(PLAN_PANEL_WIDTH))
                    .size_range(px(240.)..px(560.))
                    .child(self.render_plan_panel(theme, cx)),
            )
    }

    fn render_results_column(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut col = div().flex().flex_col().size_full().bg(theme.background);
        col = col.child(self.render_sub_tab_bar(theme, cx));
        // The content wrapper must be a flex container: the results content
        // is a `flex_1` div, and as a plain block child its flex sizing is
        // ignored, leaving it content-height. `DataTable` sizes itself with
        // `size_full`, which resolves against that indefinite height as
        // auto, so the virtualized row list collapses to 0px and no rows
        // paint even though the delegate holds data (only the header, with
        // its explicit height, showed). As a flex item the content gets a
        // definite flex-computed height and the rows render.
        col = col.child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .child(self.render_results_content(theme, cx)),
        );
        if self.active_tab().results_tab == ResultsTab::Results {
            col = col.child(self.render_pagination(theme, cx));
        }
        col
    }

    fn render_sub_tab_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let rows = tab.last_result.as_ref().map(|r| r.row_count()).unwrap_or(0);
        let current = tab.results_tab;
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(SUB_TAB_HEIGHT))
            .px(px(SPACE_2))
            .gap(px(SPACE_1))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border);
        for (which, label) in [
            (ResultsTab::Results, format!("Results ({rows})")),
            (ResultsTab::Plan, "Explain Plan".to_string()),
            (
                ResultsTab::Messages,
                format!("Messages ({})", self.messages.len()),
            ),
        ] {
            let active = current == which;
            let mut item = div()
                .id(format!("subtab-{label}"))
                .flex()
                .flex_row()
                .items_center()
                .h(relative(1.0))
                .px(px(SPACE_2))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.active_tab_mut().results_tab = which;
                    cx.notify();
                }));
            if active {
                item = item
                    .border_b_2()
                    .border_color(theme.primary)
                    .text_color(theme.primary);
            } else {
                item = item
                    .text_color(theme.muted_foreground)
                    .hover(|this| this.text_color(theme.foreground));
            }
            bar = bar.child(
                item.child(
                    div()
                        .text_size(px(FONT_LABEL))
                        .line_height(px(LEADING_LABEL))
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                ),
            );
        }
        bar = bar.child(div().flex_1());
        bar.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(self.render_export_button("CSV", theme, cx, true))
                .child(self.render_export_button("XLSX", theme, cx, false)),
        )
    }

    fn render_export_button(
        &self,
        label: &str,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
        is_csv: bool,
    ) -> impl IntoElement {
        let has_result = self.active_tab().last_result.is_some();
        div()
            .id(format!("export-{label}"))
            .flex()
            .flex_row()
            .items_center()
            .h(px(28.))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .hover(|this| this.bg(theme.secondary))
            .on_click(cx.listener(move |this, _, _, cx| {
                if is_csv {
                    this.export_csv(cx);
                } else {
                    this.export_xlsx(cx);
                }
            }))
            .child(icon(
                IconName::Download,
                14.,
                if has_result {
                    theme.foreground
                } else {
                    theme.muted_foreground
                },
            ))
            .child(
                div()
                    .text_size(px(FONT_LABEL))
                    .line_height(px(LEADING_LABEL))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if has_result {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .child(label.to_string()),
            )
    }

    fn render_results_content(
        &self,
        theme: &gpui_kit::component::Theme,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        match tab.results_tab {
            ResultsTab::Results => {
                if tab.last_result.is_none() {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(empty_state_text(
                            if self.session.is_none() {
                                "Connect first, then run a query to see results here."
                            } else {
                                "Run a query to see results here. Cmd/Ctrl+Enter or F5 runs the active tab."
                            }
                            .to_string(),
                            theme,
                        ))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .child(
                            DataTable::new(&tab.table_state)
                                .stripe(true)
                                .bordered(true)
                                .small()
                                .header_row_height(px(GRID_ROW_HEIGHT)),
                        )
                        .into_any_element()
                }
            }
            ResultsTab::Plan => {
                if tab.plan_lines.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(empty_state_text(
                            "Run Explain to see the DBMS_XPLAN text output. The plan tree is in the right panel.".to_string(),
                            theme,
                        ))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .id("plan-text-scroll")
                        .overflow_y_scroll()
                        .p(px(SPACE_3))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .font_family(crate::MONO_FONT_FAMILY)
                                .children(tab.plan_lines.iter().map(|line| {
                                    div()
                                        .text_size(px(FONT_LABEL))
                                        .line_height(px(LEADING_LABEL))
                                        .text_color(theme.foreground)
                                        .child(line.clone())
                                })),
                        )
                        .into_any_element()
                }
            }
            ResultsTab::Messages => {
                if self.messages.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(empty_state_text("No messages yet.".to_string(), theme))
                        .into_any_element()
                } else {
                    let start = self.messages.len().saturating_sub(MAX_MESSAGES_SHOWN);
                    div()
                        .flex_1()
                        .id("messages-scroll")
                        .overflow_y_scroll()
                        .p(px(SPACE_3))
                        .child(div().flex().flex_col().gap(px(SPACE_1)).children(
                            self.messages[start..].iter().map(|m| {
                                div()
                                    .text_size(px(FONT_LABEL))
                                    .line_height(px(LEADING_LABEL))
                                    .text_color(theme.foreground)
                                    .child(m.clone())
                            }),
                        ))
                        .into_any_element()
                }
            }
        }
    }

    fn render_pagination(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let (first, last, total, current, total_pages) = {
            let state = tab.table_state.read(cx);
            let delegate = state.delegate();
            let (first, last, total) = delegate.page_range();
            (first, last, total, delegate.page(), delegate.total_pages())
        };
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(PAGINATION_HEIGHT))
            .gap(px(SPACE_1))
            .px(px(SPACE_3))
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border);
        row = row.child(self.render_page_nav(
            "first",
            IconName::ChevronsLeft,
            0,
            current > 0,
            theme,
            cx,
        ));
        row = row.child(self.render_page_nav(
            "prev",
            IconName::ChevronLeft,
            current.saturating_sub(1),
            current > 0,
            theme,
            cx,
        ));
        for item in page_items(current, total_pages) {
            match item {
                PageItem::Gap => {
                    row = row.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .h(px(24.))
                            .w(px(24.))
                            .child(caption_text("…".to_string(), theme)),
                    );
                }
                PageItem::Page(page) => {
                    let active = page == current;
                    let mut button = div()
                        .id(format!("page-{page}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .h(px(24.))
                        .w(px(24.))
                        .rounded(px(RADIUS_SM))
                        .on_click(cx.listener(move |this, _, _, cx| this.goto_page(page, cx)));
                    if active {
                        button = button.bg(theme.primary);
                    } else {
                        button = button.hover(|this| this.bg(theme.secondary));
                    }
                    row = row.child(
                        button.child(
                            div()
                                .text_size(px(FONT_LABEL))
                                .line_height(px(LEADING_LABEL))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if active {
                                    theme.primary_foreground
                                } else {
                                    theme.foreground
                                })
                                .child(format!("{}", page + 1)),
                        ),
                    );
                }
            }
        }
        row = row.child(self.render_page_nav(
            "next",
            IconName::ChevronRight,
            current + 1,
            current + 1 < total_pages,
            theme,
            cx,
        ));
        row = row.child(self.render_page_nav(
            "last",
            IconName::ChevronsRight,
            total_pages.saturating_sub(1),
            current + 1 < total_pages,
            theme,
            cx,
        ));
        row.child(div().flex_1())
            .child(caption_text(format!("{first}–{last} of {total}"), theme))
            .child(
                div()
                    .pl(px(SPACE_4))
                    .child(caption_text("10 / page".to_string(), theme)),
            )
    }

    fn render_page_nav(
        &self,
        name: &str,
        icon_name: IconName,
        target: usize,
        enabled: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(format!("page-nav-{name}"))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .h(px(24.))
            .w(px(24.))
            .rounded(px(RADIUS_SM))
            .hover(|this| this.bg(theme.secondary))
            .on_click(cx.listener(move |this, _, _, cx| {
                if enabled {
                    this.goto_page(target, cx);
                }
            }))
            .child(icon(
                icon_name,
                14.,
                if enabled {
                    theme.foreground
                } else {
                    theme.muted_foreground
                },
            ))
    }

    fn render_plan_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let mut panel = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border);
        panel = panel
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(SUB_TAB_HEIGHT))
                    .px(px(SPACE_3))
                    .child(
                        div()
                            .text_size(px(FONT_CAPTION))
                            .line_height(px(LEADING_CAPTION))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("EXPLAIN PLAN"),
                    ),
            )
            .child(div().px(px(SPACE_3)).pb(px(SPACE_2)).child(caption_text(
                "Estimated plan (EXPLAIN PLAN), not runtime statistics.".to_string(),
                theme,
            )));
        if let Some(nodes) = &tab.plan_nodes {
            let view = cx.entity();
            let expanded = tab.plan_expanded.clone();
            panel = panel.child(
                div()
                    .flex_1()
                    .id("plan-tree-scroll")
                    .overflow_y_scroll()
                    .p(px(SPACE_2))
                    .child(plan::render_plan_tree(nodes, &expanded, view, cx)),
            );
        } else if !tab.plan_lines.is_empty() {
            panel =
                panel.child(
                    div()
                        .flex_1()
                        .id("plan-text-scroll-panel")
                        .overflow_y_scroll()
                        .p(px(SPACE_3))
                        .child(div().flex().flex_col().children(tab.plan_lines.iter().map(
                            |line| {
                                div()
                                    .text_size(px(FONT_LABEL))
                                    .line_height(px(LEADING_LABEL))
                                    .text_color(theme.foreground)
                                    .child(line.clone())
                            },
                        ))),
                );
        } else {
            panel = panel.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .p(px(SPACE_4))
                    .child(empty_state_text(
                        "Run Explain to see the execution plan tree here.".to_string(),
                        theme,
                    )),
            );
        }
        panel
    }

    // ----- rendering: home -----

    fn render_home(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_6))
            .p(px(SPACE_6))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1))
                    .child(
                        div()
                            .text_size(px(FONT_HEADING_XL))
                            .line_height(px(LEADING_HEADING_XL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("DB Explorer"),
                    )
                    .child(caption_text(
                        "Desktop Database Client · Built with GPUI + Rust".to_string(),
                        theme,
                    )),
            );
        content = content.child(self.render_home_connections(theme, cx));
        content = content.child(self.render_home_recent(theme, cx));
        div()
            .flex_1()
            .id("home-scroll")
            .overflow_y_scroll()
            .child(content)
    }

    fn render_home_connections(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut section = div().flex().flex_col().gap(px(SPACE_2)).child(
            div()
                .text_size(px(FONT_HEADING_MD))
                .line_height(px(LEADING_HEADING_MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child("Saved connections"),
        );
        section = section.child(
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_1))
                .p(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .child(
                    div()
                        .text_size(px(FONT_BODY))
                        .line_height(px(LEADING_BODY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("Demo mode"),
                )
                .child(caption_text(
                    "Try the full UI with a built-in fake database. No Oracle needed.".to_string(),
                    theme,
                ))
                .child(
                    div().pt(px(SPACE_1)).child(
                        Button::new("home-demo")
                            .label("Connect to demo")
                            .icon(IconName::Play)
                            .primary()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.connect(true, cx))),
                    ),
                ),
        );
        for (ix, profile) in self.profiles.iter().enumerate() {
            let profile = profile.clone();
            section = section.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_3))
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .child(icon(IconName::Database, 18., theme.primary))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(FONT_BODY))
                                    .line_height(px(LEADING_BODY))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(profile.name.clone()),
                            )
                            .child(caption_text(
                                format!("{}@{}", profile.username, profile.connect_string),
                                theme,
                            )),
                    )
                    .child(
                        Button::new(format!("home-use-{ix}"))
                            .label("Use connection")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.rail = RailPanel::Connections;
                                this.editing_profile = Some(ix);
                                this.fill_form(&profile, window, cx);
                                cx.notify();
                            })),
                    ),
            );
        }
        if self.profiles.is_empty() {
            section = section.child(caption_text(
                "No saved connections yet. Add one from the Connections panel.".to_string(),
                theme,
            ));
        }
        section
    }

    fn render_home_recent(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut section = div().flex().flex_col().gap(px(SPACE_2)).child(
            div()
                .text_size(px(FONT_HEADING_MD))
                .line_height(px(LEADING_HEADING_MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child("Recent queries"),
        );
        if self.history.is_empty() {
            return section
                .child(caption_text(
                    "No recent queries yet. Run a query to see it here.".to_string(),
                    theme,
                ))
                .into_any_element();
        }
        for (ix, entry) in self.history.iter().take(8).enumerate() {
            let preview = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let sql = entry.sql.clone();
            section = section.child(
                div()
                    .id(format!("home-recent-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .h(px(TOOLBAR_HEIGHT))
                    .px(px(SPACE_2))
                    .rounded(px(RADIUS_MD))
                    .hover(|this| this.bg(theme.secondary))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.new_tab_with_sql(sql.clone(), window, cx);
                    }))
                    .child(icon(IconName::Clock, 14., theme.muted_foreground))
                    .child(
                        div()
                            .text_size(px(FONT_LABEL))
                            .line_height(px(LEADING_LABEL))
                            .text_color(theme.foreground)
                            .truncate()
                            .child(preview),
                    ),
            );
        }
        section.into_any_element()
    }

    // ----- rendering: status bar -----

    fn render_status_bar(&self, theme: &gpui_kit::component::Theme) -> impl IntoElement {
        let (dot_color, conn_label, conn_detail) = self.conn_status(theme);
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(STATUS_BAR_HEIGHT))
            .gap(px(SPACE_4))
            .px(px(SPACE_3))
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border);
        bar = bar.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(status_dot(dot_color))
                .child(
                    div()
                        .text_size(px(FONT_CAPTION))
                        .line_height(px(LEADING_CAPTION))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(format!("{conn_label} · {conn_detail}")),
                ),
        );
        if let Some(elapsed) = self.active_tab().elapsed_ms {
            bar = bar.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_1))
                    .child(icon(IconName::Clock, 12., theme.muted_foreground))
                    .child(caption_text(format!("{elapsed} ms"), theme)),
            );
        }
        if let Some(result) = &self.active_tab().last_result {
            bar = bar.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_1))
                    .child(icon(IconName::Table, 12., theme.muted_foreground))
                    .child(caption_text(format!("{} rows", result.row_count()), theme)),
            );
        }
        bar.child(div().flex_1()).child(caption_text(
            if self.busy {
                "Running…".to_string()
            } else {
                self.status.clone()
            },
            theme,
        ))
    }
}

impl Render for Workbench {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("Workbench")
            .on_action(cx.listener(|this, _action: &RunQuery, _, cx| this.run(cx)))
            .child(
                div().flex_1().child(
                    h_resizable("main-split")
                        .child(
                            resizable_panel()
                                .fixed()
                                .size(px(RAIL_WIDTH))
                                .child(self.render_rail(&theme, cx)),
                        )
                        .child(
                            resizable_panel()
                                .size(px(SIDE_PANEL_WIDTH))
                                .size_range(px(220.)..px(440.))
                                .child(self.render_side_panel(&theme, cx)),
                        )
                        .child(resizable_panel().child(self.render_main(&theme, cx))),
                ),
            )
            .child(self.render_status_bar(&theme))
    }
}

// ----- small shared pieces -----

fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

fn status_dot(color: Hsla) -> gpui_kit::Div {
    div().size(px(8.)).rounded_full().bg(color).flex_none()
}

fn caption_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(px(FONT_CAPTION))
        .line_height(px(LEADING_CAPTION))
        .text_color(theme.muted_foreground)
        .child(text)
}

fn body_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(px(FONT_BODY))
        .line_height(px(LEADING_BODY))
        .text_color(theme.foreground)
        .child(text)
}

fn form_label(text: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(px(FONT_LABEL))
        .line_height(px(LEADING_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child(text.to_string())
}

fn empty_state_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(px(FONT_LABEL))
        .line_height(px(LEADING_LABEL))
        .text_color(theme.muted_foreground)
        .child(text)
}

fn unix_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// CSV export filename for a unix timestamp. Split out for tests.
fn export_filename(stamp: u64) -> String {
    format!("gpui-like-toad-export-{stamp}.csv")
}

/// XLSX export filename for a unix timestamp. Split out for tests.
fn export_xlsx_filename(stamp: u64) -> String {
    format!("gpui-like-toad-export-{stamp}.xlsx")
}

/// One slot in the numbered pagination row: a clickable page or an
/// ellipsis gap between the pinned first/last pages and the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageItem {
    Page(usize),
    Gap,
}

/// Numbered pagination with pinned first/last pages and ellipsis gaps:
/// at most 5 window pages, centered on the current page when possible.
/// Pages are 0-based.
fn page_items(current: usize, total: usize) -> Vec<PageItem> {
    const WINDOW: usize = 5;
    if total == 0 {
        return Vec::new();
    }
    if total <= WINDOW + 2 {
        return (0..total).map(PageItem::Page).collect();
    }
    let start = current.saturating_sub(2).min(total - WINDOW);
    let end = start + WINDOW;
    let mut items = Vec::new();
    if start > 0 {
        items.push(PageItem::Page(0));
        if start > 1 {
            items.push(PageItem::Gap);
        }
    }
    for page in start..end {
        items.push(PageItem::Page(page));
    }
    if end < total {
        if end < total - 1 {
            items.push(PageItem::Gap);
        }
        items.push(PageItem::Page(total - 1));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_filename_is_stable_and_suffixed() {
        assert_eq!(export_filename(0), "gpui-like-toad-export-0.csv");
        assert_eq!(
            export_filename(1_700_000_000),
            "gpui-like-toad-export-1700000000.csv"
        );
        assert_eq!(
            export_xlsx_filename(1_700_000_000),
            "gpui-like-toad-export-1700000000.xlsx"
        );
    }

    #[test]
    fn page_items_pin_first_last_with_ellipsis() {
        use PageItem::{Gap, Page};
        assert!(page_items(0, 0).is_empty());
        assert_eq!(page_items(0, 1), vec![Page(0)]);
        assert_eq!(page_items(0, 3), vec![Page(0), Page(1), Page(2)]);
        assert_eq!(
            page_items(0, 7),
            vec![Page(0), Page(1), Page(2), Page(3), Page(4), Page(5), Page(6)]
        );
        assert_eq!(
            page_items(0, 9),
            vec![Page(0), Page(1), Page(2), Page(3), Page(4), Gap, Page(8)]
        );
        assert_eq!(
            page_items(4, 9),
            vec![
                Page(0),
                Gap,
                Page(2),
                Page(3),
                Page(4),
                Page(5),
                Page(6),
                Gap,
                Page(8)
            ]
        );
        assert_eq!(
            page_items(8, 9),
            vec![Page(0), Gap, Page(4), Page(5), Page(6), Page(7), Page(8)]
        );
        assert_eq!(page_items(99, 9), page_items(8, 9));
        assert_eq!(
            page_items(3, 8),
            vec![
                Page(0),
                Page(1),
                Page(2),
                Page(3),
                Page(4),
                Page(5),
                Gap,
                Page(7)
            ]
        );
    }
}
