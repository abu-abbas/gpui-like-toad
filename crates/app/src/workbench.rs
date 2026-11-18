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

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvError};

use gpui_kit::actions;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{
    highlight_characters, Editor, EditorState, Enter, IndentInline, Input, InputEvent, InputState,
};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{
    h_resizable, resizable_panel, v_resizable, ActiveTheme as _, Disableable as _, Icon,
    Selectable as _, Sizable as _,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, uniform_list, AnyWindowHandle, App, ClickEvent, Context, Entity,
    Focusable as _, FontWeight, Hsla, IntoElement, PathPromptOptions, Render, ScrollHandle,
    Subscription, UniformListScrollHandle, Window,
};
use ora_core::completion::Catalog;
use ora_core::credentials::{PasswordAction, SecretString};
use ora_core::export::{to_csv, to_xlsx};
use ora_core::sql::{clean_statement, format_sql, statement_at};
use ora_core::store::{self, HistoryEntry, SavedProfile, ThemePreference};
use ora_core::{
    compose_easy_connect, parse_easy_connect, ConnectionProfile, DbInfo, ObjectKind, PlanNode,
    QueryResult, SchemaObject, DEFAULT_MAX_ROWS, MAX_PAGE_LIMIT,
};

use crate::completion::SqlCompletionProvider;
use crate::plan;
use crate::results::ResultsDelegate;
use crate::schema::{self, ObjectCategory};
use crate::theme::{
    self, FONT_BODY, FONT_CAPTION, FONT_HEADING_MD, FONT_HEADING_XL, FONT_LABEL, FONT_RAIL,
    GRID_ROW_HEIGHT, LEADING_BODY, LEADING_CAPTION, LEADING_HEADING_MD, LEADING_HEADING_XL,
    LEADING_LABEL, LEADING_RAIL, PAGINATION_HEIGHT, PLAN_PANEL_WIDTH, RADIUS_MD, RADIUS_SM,
    RAIL_BUTTON_HEIGHT, RAIL_WIDTH, SIDE_PANEL_WIDTH, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
    STATUS_BAR_HEIGHT, SUB_TAB_HEIGHT, TAB_BAR_HEIGHT, TOOLBAR_HEIGHT, TREE_ROW_HEIGHT,
};
use crate::worker::{DbHandle, ExportFormat, Job, Password, Response};

mod cell_edit;
mod chrome;
mod connections_ui;
mod conns;
mod data_tab;
mod explorer;
mod home;
mod query;
mod quick_open;
mod results;
mod settings;
mod sidebar;
mod tabs;
mod ui_helpers;
mod updates;
use cell_edit::CellEditor;
use data_tab::{DataTab, DataView};
use quick_open::QuickOpen;
use ui_helpers::{
    body_text, caption_text, empty_state_text, form_label, icon, select_trigger, status_dot,
};
use updates::UpdateState;

actions!(
    db_explorer,
    [
        RunQuery,
        RunCurrent,
        QuickOpenTable,
        CheckForUpdates,
        OpenSettings,
        NewConnection,
        ConnectLast,
        DisconnectSession,
        RefreshSchema,
        EditConnection
    ]
);

const MAX_MESSAGES: usize = 200;

/// Rows per page in a data tab: the whole capped fetch, so the grid
/// scrolls instead of paging.
const DATA_PAGE_SIZE: usize = MAX_PAGE_LIMIT;

/// Header height of the dense data-tab grid (rows are 26px).
const DATA_HEADER_HEIGHT: f32 = 28.;

/// A tab whose connection was disconnected while another one stays open.
const ORPHAN_TAB: &str =
    "Koneksi tab ini sudah ditutup. Pilih koneksi lewat tombol koneksi di kanan atas.";

const FORM_INCOMPLETE: &str = "Isi username dan host (atau TNS) dulu.";
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
    ListSchemas,
    ListObjects,
    /// Data tab views: the table's columns, indexes and DDL.
    DataStructure,
    DataIndexes,
    DataDdl,
    /// Send a data tab's staged cell edits as UPDATEs.
    ApplyEdits,
    Ddl {
        owner: String,
        name: String,
    },
    ExportTable {
        owner: String,
        name: String,
    },
    ImportCsv {
        owner: String,
        name: String,
    },
    Commit,
    Rollback,
}

/// What a work-area tab holds; only changes its icon and default title.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TabKind {
    Query,
    /// Opened by double-clicking a table/view in the explorer.
    Data,
    /// DDL from the explorer's "View Script".
    Script,
}

/// The connection last opened from this window, kept after disconnect so
/// the sidebar can still show it (red) and offer Connect / Edit.
#[derive(Clone)]
struct LastConnection {
    profile: ConnectionProfile,
    demo: bool,
    /// Index into the saved profiles, when it was opened from one.
    profile_ix: Option<usize>,
}

/// Everything that belongs to one open connection: its worker thread and
/// session, the operation in flight, the explorer's schema/object list and
/// the autocomplete catalog. Each connection has its own worker, so a slow
/// query on one never blocks another.
///
/// The active connection lives in [`Workbench::conn`] and every other one
/// in [`Workbench::parked`]; activating a connection swaps the two, so the
/// rest of the workbench only ever deals with `self.conn`.
struct ConnState {
    /// Stable id; tabs bind to a connection by it.
    id: u64,
    db: Option<DbHandle>,
    session: Option<DbInfo>,
    demo_mode: bool,
    busy: bool,
    pending_op: Option<PendingOp>,
    pending_sql: Option<String>,
    op_generation: u64,
    connect_error: Option<String>,
    last_connection: Option<LastConnection>,
    /// Tab (by id) that the in-flight operation writes its result into, so
    /// switching tabs mid-query never lands the result in the wrong tab.
    pending_tab: Option<u64>,
    schemas: Vec<String>,
    current_schema: Option<String>,
    selected_object: Option<String>,
    objects: Vec<SchemaObject>,
    /// Connect was started from a tab bound to another connection: open a
    /// new tab for this one once it connects.
    open_tab_on_connect: bool,
    /// DML ran since the last commit/rollback (best effort: DDL commits
    /// on its own and is not tracked). Shown in the connection switcher.
    uncommitted: bool,
    status: String,
    /// This connection's autocomplete catalog while it is parked; empty
    /// while active (the live one is `Workbench::catalog`).
    catalog: Catalog,
    /// Schemas whose catalog load is in flight, so one is not asked twice.
    catalog_loading: HashSet<String>,
    /// Bumped on connect and disconnect; a catalog response from an older
    /// session is dropped.
    catalog_epoch: u64,
    /// Schema the user is waiting on after typing `SCHEMA.`: reopen the
    /// popup when its catalog arrives.
    catalog_retrigger: Option<String>,
}

impl ConnState {
    fn new(id: u64) -> Self {
        ConnState {
            id,
            db: None,
            session: None,
            demo_mode: false,
            busy: false,
            pending_op: None,
            pending_sql: None,
            op_generation: 0,
            connect_error: None,
            last_connection: None,
            pending_tab: None,
            schemas: Vec::new(),
            current_schema: None,
            selected_object: None,
            objects: Vec::new(),
            open_tab_on_connect: false,
            uncommitted: false,
            status: "Belum terhubung.".to_string(),
            catalog: Catalog::new(),
            catalog_loading: HashSet::new(),
            catalog_epoch: 0,
            catalog_retrigger: None,
        }
    }

    /// Connected, or still connecting: worth keeping when parked.
    fn is_open(&self) -> bool {
        self.session.is_some() || (self.busy && matches!(self.pending_op, Some(PendingOp::Connect)))
    }

    /// Short name for tab labels and the switcher: the profile name.
    fn label(&self) -> String {
        match (&self.last_connection, &self.session) {
            (Some(last), _) => last.profile.name.clone(),
            (None, Some(s)) => format!("{}@{}", s.user, s.database),
            (None, None) => "—".to_string(),
        }
    }
}

/// The explorer object a context-menu action applies to.
#[derive(Clone)]
struct ObjectRef {
    owner: String,
    name: String,
    kind: ObjectKind,
}

/// One query tab: its own editor, grid, last result, and plan.
struct QueryTab {
    id: u64,
    title: String,
    kind: TabKind,
    editor: Entity<EditorState>,
    table_state: Entity<TableState<ResultsDelegate>>,
    last_result: Option<QueryResult>,
    elapsed_ms: Option<u128>,
    plan_nodes: Option<Vec<PlanNode>>,
    plan_lines: Vec<String>,
    plan_expanded: HashSet<i64>,
    results_tab: ResultsTab,
    /// Set for data tabs: the table shown and its filter bar.
    data: Option<DataTab>,
    /// The connection (by [`ConnState::id`]) this tab runs on. `None`
    /// until the tab first runs something: it then binds to the active
    /// connection. A tab whose connection was closed keeps the old id and
    /// refuses to run until a connection is picked for it.
    conn: Option<u64>,
}

impl QueryTab {
    fn new(
        id: u64,
        title: String,
        default_sql: &str,
        completion: &Rc<SqlCompletionProvider>,
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
        editor.update(cx, |state, cx| {
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(completion.clone());
            // Matches are fuzzy, so paint the matched characters rather
            // than a prefix.
            lsp.set_completion_highlighter(Rc::new(highlight_characters));
            lsp.completion_menu.max_width = px(460.);
            cx.notify();
        });
        let table_state = cx.new(|cx| TableState::new(ResultsDelegate::empty(), &mut *window, cx));
        QueryTab {
            id,
            title,
            kind: TabKind::Query,
            editor,
            table_state,
            last_result: None,
            elapsed_ms: None,
            plan_nodes: None,
            plan_lines: Vec::new(),
            plan_expanded: HashSet::new(),
            results_tab: ResultsTab::Results,
            data: None,
            conn: None,
        }
    }
}

pub struct Workbench {
    name_input: Entity<InputState>,
    user_input: Entity<InputState>,
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    service_input: Entity<InputState>,
    tns_input: Entity<InputState>,
    /// Connection dialog mode: separate host/port/service (Basic) or one
    /// TNS alias / connect descriptor.
    use_tns: bool,
    /// Feedback shown inside the connection dialog: (is_error, text).
    form_notice: Option<(bool, String)>,
    password_input: Entity<InputState>,
    /// "Save password" checkbox in the connection dialog. Off by default;
    /// follows the profile when editing one.
    save_password: bool,
    search_input: Entity<InputState>,
    rail: RailPanel,
    profiles: Vec<SavedProfile>,
    editing_profile: Option<usize>,
    /// Whether the connection dialog (modal) is open. Closing it never
    /// clears the inputs: they live on `Workbench` as entities.
    show_connection_form: bool,
    history: Vec<HistoryEntry>,
    theme_pref: ThemePreference,
    tabs: Vec<QueryTab>,
    active_tab: usize,
    /// Stand-in used while every tab is closed (only Home is left), so
    /// `active_tab()` never indexes an empty `tabs`. Never shown.
    scratch_tab: QueryTab,
    /// Horizontal scroll of the tab strip, so tabs past the right edge
    /// stay reachable and a newly opened/activated tab scrolls into view.
    tab_scroll: ScrollHandle,
    /// Messages tab scroll; follows the newest message.
    messages_scroll: ScrollHandle,
    show_home: bool,
    /// Demo database is offered in the UI only with ORA_DEMO set (kept for
    /// screenshots and tests; real use connects to Oracle).
    show_demo: bool,
    next_tab_id: u64,
    /// The active connection: the one the explorer, toolbar and status bar
    /// show, and the one the active tab runs on.
    conn: ConnState,
    /// Every other open connection, parked until it is activated again
    /// (see [`Workbench::activate_conn`]).
    parked: Vec<ConnState>,
    next_conn_id: u64,
    window_handle: AnyWindowHandle,
    category: ObjectCategory,
    object_scroll: UniformListScrollHandle,
    /// The Quick Open palette, while it is showing.
    quick_open: Option<QuickOpen>,
    /// The cell edit dialog, while it is showing.
    cell_editor: Option<CellEditor>,
    /// "Save scripts before disconnect?" dialog, while it is showing.
    disconnect_prompt: Option<conns::DisconnectPrompt>,
    /// The Info panel under data / script tabs is folded to its header.
    info_collapsed: bool,
    /// Home page: connections as a zebra list instead of cards, and
    /// whether each section is folded.
    home_list: bool,
    side_conn_folded: bool,
    home_conn_folded: bool,
    home_recent_folded: bool,
    /// Keeps shortcuts (Quick Open, Run) working when no input has focus.
    focus_handle: gpui_kit::FocusHandle,
    tree_filter: String,
    messages: Vec<String>,
    /// Autocomplete metadata of the active connection, shared with every
    /// tab's completion provider. A parked connection keeps its own in
    /// [`ConnState::catalog`]; the two are swapped on activation.
    catalog: Rc<RefCell<Catalog>>,
    completion: Rc<SqlCompletionProvider>,
    /// GitHub Releases update check state (see `updates.rs`).
    updates: UpdateState,
    _subscriptions: Vec<Subscription>,
}

impl Workbench {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Nama koneksi (opsional)"));
        let user_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Username, mis. HR"));
        let host_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("dbhost.example.com"));
        let port_input = cx.new(|cx| {
            InputState::new(&mut *window, cx)
                .placeholder("1521")
                .default_value("1521")
        });
        let service_input = cx.new(|cx| InputState::new(&mut *window, cx).placeholder("orclpdb"));
        let tns_input = cx
            .new(|cx| InputState::new(&mut *window, cx).placeholder("ORCL atau (DESCRIPTION=...)"));
        let password_input = cx.new(|cx| InputState::new(&mut *window, cx).masked(true));
        let search_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Filter objects..."));
        let catalog = Rc::new(RefCell::new(Catalog::new()));
        let workbench = cx.weak_entity();
        let completion = Rc::new(SqlCompletionProvider::new(
            catalog.clone(),
            Rc::new(move |schema, cx| {
                let _ = workbench.update(cx, |this, cx| {
                    this.conn.catalog_retrigger = Some(schema.to_ascii_uppercase());
                    this.load_catalog(schema, cx);
                });
            }),
        ));
        let first_tab = QueryTab::new(
            1,
            "Query 1".to_string(),
            WELCOME_SQL,
            &completion,
            window,
            cx,
        );
        let scratch_tab = QueryTab::new(0, String::new(), "", &completion, window, cx);

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
            port_input,
            service_input,
            tns_input,
            use_tns: false,
            form_notice: None,
            password_input,
            save_password: false,
            search_input,
            rail: RailPanel::Connections,
            profiles: store::load_profiles(),
            editing_profile: None,
            show_connection_form: false,
            history: store::load_history(),
            theme_pref: theme::preference(cx),
            tabs: vec![first_tab],
            active_tab: 0,
            scratch_tab,
            tab_scroll: ScrollHandle::new(),
            messages_scroll: ScrollHandle::new(),
            // Start on the Navicat-style Home page.
            show_home: true,
            show_demo: std::env::var_os("ORA_DEMO").is_some(),
            next_tab_id: 1,
            conn: ConnState::new(1),
            parked: Vec::new(),
            next_conn_id: 1,
            window_handle: window.window_handle(),
            category: ObjectCategory::Tables,
            object_scroll: UniformListScrollHandle::new(),
            quick_open: None,
            cell_editor: None,
            disconnect_prompt: None,
            info_collapsed: false,
            home_list: false,
            side_conn_folded: false,
            home_conn_folded: false,
            home_recent_folded: false,
            focus_handle: cx.focus_handle(),
            tree_filter: String::new(),
            messages: Vec::new(),
            catalog,
            completion,
            updates: UpdateState::load(),
            _subscriptions: vec![search_sub, appearance_sub],
        }
    }

    // ----- helpers -----

    pub(crate) fn push_message(&mut self, message: String) {
        // With several connections open, say which one a message is from.
        let message = if self.open_conn_count() > 1 {
            format!("[{}] {message}", self.conn.label())
        } else {
            message
        };
        self.messages.push(message);
        if self.messages.len() > MAX_MESSAGES {
            let excess = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(..excess);
        }
        self.messages_scroll.scroll_to_bottom();
    }

    fn input_text(entity: &Entity<InputState>, cx: &App) -> String {
        entity.read(cx).value().trim().to_string()
    }

    /// The typed password, untrimmed, wrapped so it is redacted in
    /// `Debug` and zeroed on drop.
    fn typed_password(&self, cx: &App) -> SecretString {
        SecretString::from(self.password_input.read(cx).value().to_string())
    }

    /// Password for a connect/test from the dialog: what was typed, or the
    /// keychain entry when the field is blank and the profile being edited
    /// has a saved password.
    fn dialog_password(&self, cx: &App) -> Password {
        let typed = self.typed_password(cx);
        if ora_core::credentials::ExposeSecret::expose_secret(&typed).is_empty() {
            if let Some(profile) = self
                .editing_profile
                .and_then(|ix| self.profiles.get(ix))
                .filter(|p| p.save_password && self.save_password)
            {
                return Password::Saved {
                    profile_id: profile.id.clone(),
                };
            }
        }
        Password::Typed(typed)
    }

    /// Run a keychain write/delete off the UI thread; report failures in
    /// Messages (and the dialog, if open).
    fn apply_keychain(
        &mut self,
        profile_id: String,
        action: PasswordAction,
        cx: &mut Context<Self>,
    ) {
        let storing = matches!(action, PasswordAction::Store(_));
        let id = profile_id.clone();
        let task = cx
            .background_executor()
            .spawn(async move { action.apply(crate::keychain::store(), &id) });
        cx.spawn(async move |view, cx| {
            if let Err(err) = task.await {
                let _ = view.update(cx, |this, cx| {
                    // Nothing was saved: untick the profile so Connect asks
                    // for the password instead of failing on a missing entry.
                    if storing {
                        if let Some(ix) = this.profiles.iter().position(|p| p.id == profile_id) {
                            this.profiles[ix].save_password = false;
                            this.persist_profiles();
                        }
                        this.save_password = false;
                    }
                    let text = format!("Gagal menyimpan password ke keychain: {err}");
                    this.push_message(text.clone());
                    if this.show_connection_form {
                        this.form_notice = Some((true, text));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
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
        self.tabs.get(self.active_tab).unwrap_or(&self.scratch_tab)
    }

    fn active_tab_mut(&mut self) -> &mut QueryTab {
        if self.active_tab < self.tabs.len() {
            &mut self.tabs[self.active_tab]
        } else {
            &mut self.scratch_tab
        }
    }

    fn editor_text(&self, cx: &App) -> String {
        self.active_tab().editor.read(cx).value().to_string()
    }

    /// Index of the tab the finished operation belongs to: the tab it was
    /// started from if it still exists, else the active tab.
    /// Index of the tab the running operation belongs to. `None` when that
    /// tab was closed while the operation ran.
    fn target_ix(&self) -> Option<usize> {
        match self.conn.pending_tab {
            Some(id) => self.tabs.iter().position(|t| t.id == id),
            None => (self.active_tab < self.tabs.len()).then_some(self.active_tab),
        }
    }

    fn target_tab_mut(&mut self) -> &mut QueryTab {
        match self.target_ix() {
            Some(ix) => &mut self.tabs[ix],
            None => &mut self.scratch_tab,
        }
    }

    fn set_grid_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        let tab = self.target_tab_mut();
        tab.last_result = Some(result.clone());
        tab.table_state.update(cx, |state, cx| {
            state.delegate_mut().set_result(result);
            state.refresh(cx);
        });
    }

    /// Send a job to the worker and apply the response when it arrives. The
    /// blocking receive runs on the background executor, never the UI
    /// thread. The generation tag lets [`Workbench::stop`] drop the
    /// response of a superseded operation.
    fn spawn_db(&mut self, job: Job, op: PendingOp, cx: &mut Context<Self>) {
        let Some(db) = self.conn.db.clone() else {
            self.push_message("Belum terhubung.".to_string());
            return;
        };
        // Work that lands in the active tab must run on that tab's own
        // connection; a tab whose connection was closed has none.
        let tab_op = matches!(
            op,
            PendingOp::Run
                | PendingOp::Explain
                | PendingOp::DataStructure
                | PendingOp::DataIndexes
                | PendingOp::DataDdl
                | PendingOp::ApplyEdits
        );
        let tab_conn = self.tabs.get(self.active_tab).and_then(|t| t.conn);
        if tab_op && !self.show_home {
            // Safety net: the tab shows work of another open connection
            // but that one is not active (some tab switch did not follow
            // it). Run on the tab's own connection, never the active one.
            if let Some(id) = tab_conn.filter(|id| *id != self.conn.id) {
                if self.conn_by_id(id).is_some_and(|c| c.is_open() && !c.busy) {
                    self.activate_conn(id, cx);
                    return self.spawn_db(job, op, cx);
                }
            }
            if tab_conn.is_some_and(|id| id != self.conn.id) {
                let text = if self.active_tab_orphaned() {
                    ORPHAN_TAB
                } else {
                    "Tab ini milik koneksi lain."
                };
                self.push_message(text.to_string());
                self.conn.status = text.to_string();
                self.active_tab_mut().results_tab = ResultsTab::Messages;
                cx.notify();
                return;
            }
            let id = self.conn.id;
            let tab = self.active_tab_mut();
            if tab.conn.is_none() {
                tab.conn = Some(id);
            }
        }
        let rx: Receiver<Response> = db.request(job);
        self.conn.op_generation += 1;
        let generation = self.conn.op_generation;
        self.conn.pending_op = Some(op);
        self.conn.pending_tab = self.tabs.get(self.active_tab).map(|t| t.id);
        self.conn.busy = true;
        cx.notify();
        let handle = self.window_handle;
        let conn_id = self.conn.id;
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            // The window is needed to create tabs (View Script) and set
            // editor text from a response.
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.with_conn(conn_id, cx, |this, background, cx| {
                        this.on_db_response(generation, resp, background, window, cx)
                    })
                })
            });
        })
        .detach();
    }

    /// Load one schema's objects and columns into the autocomplete catalog.
    /// Deliberately outside `spawn_db`: it must not set the busy flag or
    /// supersede the operation the user is waiting on. The worker runs it
    /// in turn, after whatever is already queued.
    fn load_catalog(&mut self, owner: String, cx: &mut Context<Self>) {
        let Some(db) = self.conn.db.clone() else {
            return;
        };
        if self.conn.session.is_none() {
            return;
        }
        let key = owner.to_ascii_uppercase();
        if !self.conn.catalog_loading.insert(key.clone()) {
            return;
        }
        let rx: Receiver<Response> = db.request(Job::Catalog { owner });
        let epoch = self.conn.catalog_epoch;
        let handle = self.window_handle;
        let conn_id = self.conn.id;
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.with_conn(conn_id, cx, |this, background, cx| {
                        this.on_catalog_response(epoch, key, resp, background, window, cx)
                    })
                })
            });
        })
        .detach();
    }

    fn on_catalog_response(
        &mut self,
        epoch: u64,
        owner: String,
        resp: Result<Response, RecvError>,
        background: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.conn.catalog_epoch {
            return;
        }
        self.conn.catalog_loading.remove(&owner);
        let waiting = self.conn.catalog_retrigger.as_deref() == Some(owner.as_str());
        if waiting {
            self.conn.catalog_retrigger = None;
        }
        match resp {
            Ok(Response::Catalog(objects)) => {
                self.catalog.borrow_mut().set_owner(&owner, objects);
                // The user typed `SCHEMA.` and got nothing because it was
                // still loading: show the list now, if they are still there.
                if waiting && !background && !self.show_home {
                    let editor = self.active_tab().editor.clone();
                    if editor.read(cx).focus_handle(cx).is_focused(window) {
                        editor.update(cx, |state, cx| state.show_completions(window, cx));
                    }
                }
            }
            Ok(Response::Failed(message)) => {
                // Stored as loaded-but-empty so every later `SCHEMA.` does
                // not retry a schema the session cannot read.
                self.catalog.borrow_mut().set_owner(&owner, Vec::new());
                self.push_message(format!(
                    "Autocomplete: gagal memuat metadata {owner}: {message}"
                ));
                cx.notify();
            }
            _ => {}
        }
    }

    fn reset_catalog(&mut self, default_owner: Option<String>) {
        self.conn.catalog_epoch += 1;
        self.conn.catalog_loading.clear();
        self.conn.catalog_retrigger = None;
        let mut catalog = self.catalog.borrow_mut();
        catalog.clear();
        catalog.set_default_owner(default_owner);
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

    fn connect(&mut self, demo: bool, window: &mut Window, cx: &mut Context<Self>) {
        if demo {
            if let Some(id) = self.find_open_conn(None, true) {
                self.switch_conn(id, window, cx);
                return;
            }
        }
        if self.conn.busy {
            return;
        }
        let (profile, password) = if demo {
            (
                ConnectionProfile {
                    name: "Demo database".to_string(),
                    username: "DEMO".to_string(),
                    connect_string: "demo".to_string(),
                },
                Password::Typed(SecretString::from(String::new())),
            )
        } else {
            let username = Self::input_text(&self.user_input, cx);
            let connect_string = self.form_connect_string(cx);
            if username.is_empty() || connect_string.is_empty() {
                self.form_notice = Some((true, FORM_INCOMPLETE.to_string()));
                cx.notify();
                return;
            }
            // "Save password" ticked, or unticked on a profile that had
            // one: Connect also saves the profile so the keychain matches
            // the checkbox.
            let had_saved = self
                .editing_profile
                .and_then(|ix| self.profiles.get(ix))
                .is_some_and(|p| p.save_password);
            let password = self.dialog_password(cx);
            if self.save_password || had_saved {
                self.save_profile_quiet(cx);
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
                password,
            )
        };
        self.start_connect(
            profile,
            password,
            demo,
            if demo { None } else { self.editing_profile },
            cx,
        );
    }

    /// Connect straight to a saved profile when its password is in the
    /// keychain; otherwise open its dialog so the user types one.
    fn open_profile(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = self.profiles.get(ix).cloned() else {
            return;
        };
        // Already open: switch to it rather than logging in twice.
        if let Some(id) = self.find_open_conn(Some(ix), false) {
            self.switch_conn(id, window, cx);
            return;
        }
        if !saved.save_password || self.conn.busy {
            self.edit_profile(ix, window, cx);
            return;
        }
        let profile = ConnectionProfile {
            name: saved.name.clone(),
            username: saved.username.clone(),
            connect_string: saved.connect_string.clone(),
        };
        self.start_connect(
            profile,
            Password::Saved {
                profile_id: saved.id.clone(),
            },
            false,
            Some(ix),
            cx,
        );
    }

    fn start_connect(
        &mut self,
        profile: ConnectionProfile,
        password: Password,
        demo: bool,
        profile_ix: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.prepare_new_conn();
        if self.conn.db.is_none() {
            self.conn.db = Some(DbHandle::spawn());
        }
        self.conn.demo_mode = demo;
        self.conn.connect_error = None;
        self.conn.last_connection = Some(LastConnection {
            profile: profile.clone(),
            demo,
            profile_ix,
        });
        self.conn.status = "Menghubungkan...".to_string();
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
        if self.conn.busy {
            return;
        }
        let username = Self::input_text(&self.user_input, cx);
        let connect_string = self.form_connect_string(cx);
        if username.is_empty() || connect_string.is_empty() {
            self.form_notice = Some((true, FORM_INCOMPLETE.to_string()));
            cx.notify();
            return;
        }
        self.form_notice = None;
        if self.conn.db.is_none() {
            self.conn.db = Some(DbHandle::spawn());
        }
        let name = Self::input_text(&self.name_input, cx);
        self.conn.status = "Mengetes koneksi...".to_string();
        self.spawn_db(
            Job::TestConnect {
                profile: ConnectionProfile {
                    name,
                    username,
                    connect_string,
                },
                password: self.dialog_password(cx),
            },
            PendingOp::TestConnect,
            cx,
        );
    }

    /// Close the active connection. Other open connections stay open; the
    /// next one becomes active.
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.push_message("Disconnected.".to_string());
        if let Some(db) = self.conn.db.take() {
            // Fire and forget: the worker closes the session on its own.
            let _ = db.request(Job::Disconnect);
        }
        self.conn.session = None;
        self.reset_catalog(None);
        self.conn.demo_mode = false;
        self.conn.uncommitted = false;
        self.show_connection_form = false;
        self.conn.objects.clear();
        self.conn.schemas.clear();
        self.conn.current_schema = None;
        self.conn.selected_object = None;
        self.conn.status = "Belum terhubung.".to_string();
        self.after_disconnect(cx);
        cx.notify();
    }

    /// Reload the schema list, then the objects of the current schema
    /// (the session user's own schema on first load).
    fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy {
            return;
        }
        if self.conn.session.is_none() {
            self.push_message("Belum terhubung.".to_string());
            return;
        }
        self.conn.status = "Memuat daftar schema...".to_string();
        self.spawn_db(Job::ListSchemas, PendingOp::ListSchemas, cx);
    }

    fn load_objects(&mut self, cx: &mut Context<Self>) {
        let Some(owner) = self.conn.current_schema.clone() else {
            return;
        };
        self.conn.status = format!("Memuat objek {owner}...");
        self.spawn_db(Job::ListObjects { owner }, PendingOp::ListObjects, cx);
    }

    fn select_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.current_schema.as_deref() == Some(schema.as_str()) {
            return;
        }
        self.conn.current_schema = Some(schema);
        self.conn.selected_object = None;
        self.conn.objects.clear();
        self.load_objects(cx);
    }

    fn select_category(&mut self, category: ObjectCategory, cx: &mut Context<Self>) {
        self.category = category;
        self.conn.selected_object = None;
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_home {
            return;
        }
        if self.conn.session.is_none() {
            self.push_message("Belum terhubung: Connect atau Demo dulu.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        if self.active_tab().data.is_some() {
            // Data tab "Refresh": reload whichever view is showing.
            self.load_data_view(cx);
            return;
        }
        let sql = self.editor_text(cx);
        self.run_sql(sql, cx);
    }

    /// "Run Current": the selection if there is one, else the statement
    /// the cursor sits in (statements end at `;`).
    fn run_current(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_home || self.conn.session.is_none() {
            return self.run(cx);
        }
        if self.active_tab().data.is_some() {
            return self.run(cx);
        }
        let editor = self.active_tab().editor.clone();
        let sql = {
            let state = editor.read(cx);
            let range = state.selected_range();
            let text = state.value().to_string();
            if range.start < range.end && range.end <= text.len() {
                text[range].to_string()
            } else {
                statement_at(&text, state.cursor())
            }
        };
        self.run_sql(sql, cx);
    }

    fn run_sql(&mut self, sql: String, cx: &mut Context<Self>) {
        if clean_statement(&sql).is_empty() {
            self.push_message("Editor kosong.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.conn.status = "Menjalankan...".to_string();
        self.conn.pending_sql = Some(sql.clone());
        self.spawn_db(Job::Run { sql }, PendingOp::Run, cx);
    }

    fn explain(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_home {
            return;
        }
        if self.conn.session.is_none() {
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
        self.conn.status = "Menjelaskan plan...".to_string();
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
        if !self.conn.busy {
            return;
        }
        self.conn.op_generation += 1;
        self.conn.pending_op = None;
        self.conn.pending_sql = None;
        self.conn.busy = false;
        self.conn.status = "Dihentikan (respons dibuang).".to_string();
        self.push_message(
            "Stop: operasi database yang sedang berjalan tidak dapat dibatalkan; responsnya akan diabaikan saat tiba.".to_string(),
        );
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        if self.has_staged_edits() {
            self.conn.status =
                "Ada perubahan sel yang belum dikirim: klik Apply (atau Discard) dulu.".to_string();
            cx.notify();
            return;
        }
        self.spawn_db(Job::Commit, PendingOp::Commit, cx);
    }

    fn rollback(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
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
                self.conn.status = format!("Exported {filename}");
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
                        self.conn.status = format!("Exported {filename}");
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
        let completion = self.completion.clone();
        let mut tab = QueryTab::new(id, format!("Query {id}"), "", &completion, window, cx);
        tab.conn = self.conn.is_open().then_some(self.conn.id);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_home = false;
        self.leave_whats_new();
        self.tab_scroll.scroll_to_item(self.active_tab);
        cx.notify();
    }

    /// Every tab can be closed; only Home stays. Closing the last one
    /// goes back to Home.
    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.tabs.is_empty() {
            self.active_tab = 0;
            self.show_home = true;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if ix < self.active_tab {
            self.active_tab -= 1;
        }
        // The tab now showing may run on another connection.
        self.follow_tab_conn(cx);
        cx.notify();
    }

    /// Close every tab for which `close(ix, tab)` is true. The active tab
    /// stays active when it survives; otherwise the nearest survivor is
    /// shown (or Home when none is left).
    fn close_tabs_where(
        &mut self,
        close: impl Fn(usize, &QueryTab, &App) -> bool,
        cx: &mut Context<Self>,
    ) {
        let active_id = self.tabs.get(self.active_tab).map(|t| t.id);
        let old_active = self.active_tab;
        let doomed: Vec<bool> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, t)| close(ix, t, cx))
            .collect();
        let mut ix = 0;
        self.tabs.retain(|_| {
            let keep = !doomed[ix];
            ix += 1;
            keep
        });
        if self.tabs.is_empty() {
            self.active_tab = 0;
            self.show_home = true;
        } else if let Some(pos) = self.tabs.iter().position(|t| Some(t.id) == active_id) {
            self.active_tab = pos;
        } else {
            self.active_tab = old_active.min(self.tabs.len() - 1);
        }
        self.follow_tab_conn(cx);
        cx.notify();
    }

    /// Whether a tab holds nothing that would be lost: a data tab with
    /// no staged or uncommitted cell edits, or an empty SQL editor.
    fn tab_is_saved(tab: &QueryTab, cx: &App) -> bool {
        match &tab.data {
            Some(d) => d.edits.is_empty(),
            None => tab.editor.read(cx).value().trim().is_empty(),
        }
    }

    fn activate_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.show_home = false;
        self.leave_whats_new();
        self.tab_scroll.scroll_to_item(ix);
        self.follow_tab_conn(cx);
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

    /// The connect string the dialog describes: the TNS field as typed,
    /// or `host:port/service` built from the Basic fields. Empty when the
    /// required field (TNS text or host) is blank.
    fn form_connect_string(&self, cx: &App) -> String {
        if self.use_tns {
            return Self::input_text(&self.tns_input, cx);
        }
        let host = Self::input_text(&self.host_input, cx);
        if host.is_empty() {
            return String::new();
        }
        compose_easy_connect(
            &host,
            &Self::input_text(&self.port_input, cx),
            &Self::input_text(&self.service_input, cx),
        )
    }

    fn fill_form(&mut self, profile: &SavedProfile, window: &mut Window, cx: &mut Context<Self>) {
        Self::set_input(&self.name_input.clone(), profile.name.clone(), window, cx);
        Self::set_input(
            &self.user_input.clone(),
            profile.username.clone(),
            window,
            cx,
        );
        // Easy Connect strings split into the Basic fields; descriptors and
        // TNS aliases open in TNS mode unchanged.
        match parse_easy_connect(&profile.connect_string) {
            Some((host, port, service)) => {
                self.use_tns = false;
                Self::set_input(&self.host_input.clone(), host, window, cx);
                Self::set_input(&self.port_input.clone(), port, window, cx);
                Self::set_input(&self.service_input.clone(), service, window, cx);
                Self::set_input(&self.tns_input.clone(), String::new(), window, cx);
            }
            None => {
                self.use_tns = true;
                Self::set_input(&self.host_input.clone(), String::new(), window, cx);
                Self::set_input(&self.port_input.clone(), "1521".into(), window, cx);
                Self::set_input(&self.service_input.clone(), String::new(), window, cx);
                Self::set_input(
                    &self.tns_input.clone(),
                    profile.connect_string.clone(),
                    window,
                    cx,
                );
            }
        }
        // A saved password is never loaded into the field: blank means
        // "use the one in the keychain" (see `dialog_password`).
        Self::set_input(&self.password_input.clone(), String::new(), window, cx);
        self.save_password = profile.save_password;
    }

    /// Open the connection dialog for a saved profile.
    fn edit_profile(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(profile) = self.profiles.get(ix).cloned() {
            self.editing_profile = Some(ix);
            self.fill_form(&profile, window, cx);
            self.form_notice = None;
            self.conn.connect_error = None;
            self.show_connection_form = true;
            cx.notify();
        }
    }

    fn new_profile_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_profile = None;
        self.use_tns = false;
        self.form_notice = None;
        self.conn.connect_error = None;
        Self::set_input(&self.name_input.clone(), String::new(), window, cx);
        Self::set_input(&self.user_input.clone(), String::new(), window, cx);
        Self::set_input(&self.host_input.clone(), String::new(), window, cx);
        Self::set_input(&self.port_input.clone(), "1521".into(), window, cx);
        Self::set_input(&self.service_input.clone(), String::new(), window, cx);
        Self::set_input(&self.tns_input.clone(), String::new(), window, cx);
        Self::set_input(&self.password_input.clone(), String::new(), window, cx);
        self.save_password = false;
        cx.notify();
    }

    /// "+" / "New connection": open an empty connection dialog.
    fn open_new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_profile_form(window, cx);
        self.show_connection_form = true;
        cx.notify();
    }

    fn close_connection_dialog(&mut self, cx: &mut Context<Self>) {
        self.show_connection_form = false;
        cx.notify();
    }

    fn save_profile(&mut self, cx: &mut Context<Self>) {
        if self.save_profile_quiet(cx) {
            // A stale connect error would hide the "saved" notice.
            self.conn.connect_error = None;
            let text = if self.save_password {
                "Profil koneksi tersimpan, password di keychain."
            } else {
                "Profil koneksi tersimpan."
            };
            self.push_message(text.to_string());
            self.form_notice = Some((false, text.to_string()));
            cx.notify();
        }
    }

    /// Save the dialog into its profile (new or edited) and bring the
    /// keychain entry in line with "Save password". Returns false when
    /// the form is incomplete.
    fn save_profile_quiet(&mut self, cx: &mut Context<Self>) -> bool {
        let username = Self::input_text(&self.user_input, cx);
        let connect_string = self.form_connect_string(cx);
        if username.is_empty() || connect_string.is_empty() {
            self.form_notice = Some((true, FORM_INCOMPLETE.to_string()));
            cx.notify();
            return false;
        }
        let name = Self::input_text(&self.name_input, cx);
        let existing = self
            .editing_profile
            .and_then(|ix| self.profiles.get(ix))
            .cloned();
        let profile = SavedProfile {
            id: existing
                .as_ref()
                .map(|p| p.id.clone())
                .unwrap_or_else(store::new_profile_id),
            name: if name.is_empty() {
                format!("{username}@{connect_string}")
            } else {
                name
            },
            username,
            connect_string,
            save_password: self.save_password,
        };
        let action = PasswordAction::decide(self.save_password, self.typed_password(cx));
        // Skip the keychain entirely for a profile that never had a
        // saved password and still does not want one.
        let touch_keychain =
            self.save_password || existing.as_ref().is_some_and(|p| p.save_password);
        let profile_id = profile.id.clone();
        match self.editing_profile {
            Some(ix) if ix < self.profiles.len() => self.profiles[ix] = profile,
            _ => {
                self.profiles.push(profile);
                self.editing_profile = Some(self.profiles.len() - 1);
            }
        }
        self.persist_profiles();
        if touch_keychain && !matches!(action, PasswordAction::Keep) {
            self.apply_keychain(profile_id, action, cx);
        }
        true
    }

    fn remove_profile(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.profiles.len() {
            return;
        }
        let removed = self.profiles.remove(ix);
        if removed.save_password {
            self.apply_keychain(removed.id.clone(), PasswordAction::Delete, cx);
        }
        self.shift_profile_ix(ix);
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

    /// Pick the color theme for light (`dark == false`) or dark mode;
    /// `None` is the app's own palette.
    fn set_theme_name(
        &mut self,
        dark: bool,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut names = theme::theme_names(cx);
        if dark {
            names.dark = name;
        } else {
            names.light = name;
        }
        theme::set_theme_names(names.clone(), cx);
        theme::apply_preference(self.theme_pref, window, cx);
        if let Err(err) = store::save_theme_names(&names) {
            self.push_message(format!("Gagal menyimpan pilihan tema: {err}"));
        }
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

    // ----- explorer (Toad-style object browser) -----

    fn apply_tree_filter(&mut self, cx: &mut Context<Self>) {
        self.tree_filter = Self::input_text(&self.search_input, cx);
        cx.notify();
    }

    /// Single click selects; double click on a table/view opens its data
    /// in a new tab, like Toad. The platform reports the tap count on the
    /// mouse-down event, so a double click arrives with `click_count >= 2`.
    fn on_object_click(
        &mut self,
        object: ObjectRef,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.conn.selected_object = Some(object.name.clone());
        let double_click = matches!(event, ClickEvent::Mouse(m) if m.down.click_count >= 2);
        if double_click {
            if schema::has_data(&object.kind) {
                self.open_table_data(&object.owner, &object.name, window, cx);
            } else {
                self.view_script(object, cx);
            }
        }
        cx.notify();
    }

    /// Open `owner.name` in a new data tab titled after the table and run a
    /// capped `SELECT *` there.
    fn open_table_data(
        &mut self,
        owner: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_table_data_in(owner, name, DataView::Data, window, cx);
    }

    /// Like [`Self::open_table_data`], starting on `view` (Describe opens
    /// the Structure view; the rows load when Data is first shown).
    fn open_table_data_in(
        &mut self,
        owner: &str,
        name: &str,
        start_view: DataView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conn.session.is_none() {
            return;
        }
        if self.conn.busy {
            self.conn.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        let kind = self
            .conn
            .objects
            .iter()
            .find(|o| o.owner == owner && o.name == name)
            .map_or(ObjectKind::Table, |o| o.kind.clone());
        self.add_tab(window, cx);
        let view = cx.entity().downgrade();
        let editable = kind == ObjectKind::Table;
        let data = DataTab::new(owner, name, kind, window, cx);
        let table_state = self.active_tab().table_state.clone();
        let observe = cx.observe(&table_state, |_, _, cx| cx.notify());
        let mut data = data;
        data._grid_observe = Some(observe);
        let tab = self.active_tab_mut();
        tab.title = format!("{owner}.{name}");
        tab.kind = TabKind::Data;
        tab.data = Some(data);
        let edit_view = view.clone();
        let table_name = format!(
            "{}.{}",
            ora_core::import::quote_ident(owner),
            ora_core::import::quote_ident(name)
        );
        tab.table_state.update(cx, |state, _| {
            let mut delegate = ResultsDelegate::empty()
                .with_page_size(DATA_PAGE_SIZE)
                .with_row_numbers()
                .with_quick_filter(Rc::new(move |condition, window, cx| {
                    let _ = view.update(cx, |this, cx| this.quick_filter(condition, window, cx));
                }));
            // Only tables have a stable ROWID to find a row by.
            if editable {
                delegate = delegate.with_cell_edit(Rc::new(move |request, window, cx| {
                    let _ =
                        edit_view.update(cx, |this, cx| this.open_cell_editor(request, window, cx));
                }));
            }
            delegate.set_table_name(Some(table_name));
            *state.delegate_mut() = delegate;
        });
        self.add_filter_row(None, None, window, cx);
        if start_view == DataView::Data {
            self.reload_data(cx);
        } else {
            if let Some(data) = self.data_mut() {
                data.view = start_view;
            }
            self.load_data_view(cx);
        }
    }

    /// "View Script": fetch the DDL; the response opens it in a new tab.
    fn view_script(&mut self, object: ObjectRef, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        self.conn.status = format!("Mengambil DDL {}.{}...", object.owner, object.name);
        self.spawn_db(
            Job::Ddl {
                owner: object.owner.clone(),
                name: object.name.clone(),
                kind: object.kind,
            },
            PendingOp::Ddl {
                owner: object.owner,
                name: object.name,
            },
            cx,
        );
    }

    /// "Export Data": ask where to save (native save dialog), then let the
    /// worker fetch and write the whole table. If the platform has no file
    /// dialog, fall back to a timestamped file in the working directory.
    fn export_table(&mut self, object: ObjectRef, format: ExportFormat, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        let conn_id = self.conn.id;
        let suggested = format!("{}.{}", object.name, format.extension());
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |view, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => Some(path),
                Ok(Ok(None)) => None, // cancelled
                _ => Some(dir.join(format!(
                    "{}-{}.{}",
                    object.name,
                    unix_stamp(),
                    format.extension()
                ))),
            };
            let Some(path) = path else {
                return;
            };
            let _ = view.update(cx, |this, cx| {
                this.with_conn(conn_id, cx, |this, _, cx| {
                    this.conn.status = format!("Export {}.{}...", object.owner, object.name);
                    this.spawn_db(
                        Job::ExportTable {
                            owner: object.owner.clone(),
                            name: object.name.clone(),
                            path,
                            format,
                        },
                        PendingOp::ExportTable {
                            owner: object.owner,
                            name: object.name,
                        },
                        cx,
                    );
                });
            });
        })
        .detach();
    }

    /// "Import Data": pick a CSV file whose header names match the table's
    /// columns; rows are inserted without commit.
    fn import_table(&mut self, object: ObjectRef, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        let conn_id = self.conn.id;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import CSV".into()),
        });
        cx.spawn(async move |view, cx| {
            let picked = prompt.await;
            let _ = view.update(cx, |this, cx| {
                let path = match picked {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) => None,
                    _ => {
                        this.push_message(
                            "Dialog pilih file tidak tersedia di sistem ini.".to_string(),
                        );
                        cx.notify();
                        None
                    }
                };
                let Some(path) = path else {
                    return;
                };
                this.with_conn(conn_id, cx, |this, _, cx| {
                    this.conn.status = format!("Import ke {}.{}...", object.owner, object.name);
                    this.spawn_db(
                        Job::ImportCsv {
                            owner: object.owner.clone(),
                            name: object.name.clone(),
                            path,
                        },
                        PendingOp::ImportCsv {
                            owner: object.owner,
                            name: object.name,
                        },
                        cx,
                    );
                });
            });
        })
        .detach();
    }

    // ----- response handling -----

    fn on_db_response(
        &mut self,
        generation: u64,
        resp: Result<Response, RecvError>,
        background: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A superseded operation (after Stop, or a newer op) drops its
        // response entirely: stale data must never overwrite fresh state.
        if generation != self.conn.op_generation {
            return;
        }
        let pending = self.conn.pending_op.take();
        let pending_sql = self.conn.pending_sql.take();
        self.conn.busy = false;
        let resp = match resp {
            Ok(resp) => resp,
            Err(_) => {
                self.push_message("DB worker berhenti.".to_string());
                self.conn.status = "Worker error".to_string();
                cx.notify();
                return;
            }
        };
        if matches!(
            (&pending, &resp),
            (Some(PendingOp::Run), Response::Executed(_))
                | (Some(PendingOp::ImportCsv { .. }), Response::Imported(_))
                | (Some(PendingOp::ApplyEdits), Response::CellsUpdated { .. })
        ) {
            self.conn.uncommitted = true;
        }
        if let Response::Failed(message) = &resp {
            if matches!(pending, Some(PendingOp::Connect)) {
                self.conn.open_tab_on_connect = false;
                // A direct connect with a saved password failed (wrong or
                // missing password): open the dialog so it can be fixed.
                if !self.show_connection_form {
                    if let Some(ix) = self
                        .conn
                        .last_connection
                        .as_ref()
                        .filter(|l| !l.demo)
                        .and_then(|l| l.profile_ix)
                    {
                        self.edit_profile(ix, window, cx);
                    }
                }
                self.conn.connect_error = Some(message.clone());
            }
            self.push_message(message.clone());
            self.conn.status = "Error (lihat Messages).".to_string();
            self.target_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        match (pending, resp) {
            (Some(PendingOp::Connect), Response::Connected(info)) => {
                self.conn.session = Some(info.clone());
                self.reset_catalog(Some(info.user.clone()));
                self.conn.current_schema = None;
                self.conn.connect_error = None;
                self.show_connection_form = false;
                self.conn.status = format!("Connected sebagai {}.", info.user);
                self.push_message(format!(
                    "Connected: {} @ {} ({})",
                    info.user, info.database, info.version
                ));
                // Connected from a tab of another connection: give this
                // one a tab of its own to work in.
                if std::mem::take(&mut self.conn.open_tab_on_connect)
                    && !background
                    && self.show_home
                {
                    self.add_tab(window, cx);
                }
                cx.notify();
                self.refresh_schema(cx);
            }
            (Some(PendingOp::TestConnect), Response::Tested(outcome)) => match outcome {
                Ok(info) => {
                    let text = format!(
                        "Test koneksi berhasil: {} @ {} ({})",
                        info.user, info.database, info.version
                    );
                    self.push_message(text.clone());
                    self.form_notice = Some((false, text));
                    self.conn.status = "Test koneksi berhasil.".to_string();
                }
                Err(message) => {
                    self.push_message(format!("Test koneksi gagal: {message}"));
                    self.form_notice = Some((true, format!("Test koneksi gagal: {message}")));
                    self.conn.status = "Test koneksi gagal.".to_string();
                }
            },
            (Some(PendingOp::Run), Response::QueryDone(result)) => {
                let rows = result.row_count();
                let elapsed = result.elapsed.as_millis();
                let truncated = result.truncated;
                if let Some(sql) = pending_sql {
                    self.record_run(&sql);
                }
                let column_names: Vec<String> =
                    result.columns.iter().map(|c| c.name.clone()).collect();
                let tab = self.target_tab_mut();
                tab.elapsed_ms = Some(elapsed);
                if let Some(data) = tab.data.as_mut() {
                    data.set_columns(column_names);
                }
                self.set_grid_result(result, cx);
                self.target_tab_mut().results_tab = ResultsTab::Results;
                let is_data = self.target_tab_mut().data.is_some();
                self.conn.status = if truncated && is_data {
                    format!("{rows} baris dalam {elapsed}ms (masih ada baris berikutnya)")
                } else if truncated {
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
                self.conn.status = format!("{affected} baris terpengaruh (belum commit).");
                self.target_tab_mut().results_tab = ResultsTab::Messages;
            }
            (Some(PendingOp::Explain), Response::PlanTree(nodes, lines)) => {
                let tab = self.target_tab_mut();
                tab.plan_expanded = plan::all_plan_ids(&nodes);
                tab.plan_nodes = Some(nodes);
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.conn.status = "Explain plan selesai.".to_string();
            }
            (Some(PendingOp::Explain), Response::Plan(lines)) => {
                let tab = self.target_tab_mut();
                tab.plan_nodes = None;
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.conn.status = "Explain plan selesai.".to_string();
            }
            (Some(PendingOp::ListSchemas), Response::Schemas(schemas)) => {
                let user = self.conn.session.as_ref().map(|s| s.user.clone());
                // Keep the current pick on refresh; otherwise start in the
                // session user's own schema, as Toad does.
                let keep = self
                    .conn
                    .current_schema
                    .clone()
                    .filter(|c| schemas.contains(c));
                self.conn.current_schema = keep
                    .or_else(|| user.filter(|u| schemas.contains(u)))
                    .or_else(|| schemas.first().cloned());
                self.catalog.borrow_mut().set_schemas(schemas.clone());
                self.conn.schemas = schemas;
                self.load_objects(cx);
            }
            (Some(PendingOp::ListObjects), Response::Objects(objects)) => {
                let count = objects.len();
                self.conn.objects = objects;
                self.conn.selected_object = None;
                let schema = self.conn.current_schema.clone().unwrap_or_default();
                self.conn.status = format!("{schema}: {count} objek dimuat.");
                // Autocomplete metadata follows the explorer: (re)load the
                // schema just listed, and the session user's own schema,
                // which unqualified names resolve against.
                let user = self.conn.session.as_ref().map(|s| s.user.clone());
                if !schema.is_empty() {
                    self.load_catalog(schema, cx);
                }
                if let Some(user) = user {
                    if !self.catalog.borrow().has_owner(&user) {
                        self.load_catalog(user, cx);
                    }
                }
            }
            (Some(PendingOp::Ddl { owner, name }), Response::Ddl(ddl)) => {
                self.new_tab_with_sql(ddl, window, cx);
                let tab = self.active_tab_mut();
                tab.title = format!("{name}.sql");
                tab.kind = TabKind::Script;
                self.conn.status = format!("Script {owner}.{name}.");
            }
            (
                Some(PendingOp::ExportTable { owner, name }),
                Response::Exported {
                    path,
                    rows,
                    truncated,
                },
            ) => {
                let cap = if truncated {
                    format!(" (dibatasi {} baris)", ora_core::EXPORT_MAX_ROWS)
                } else {
                    String::new()
                };
                self.push_message(format!(
                    "Export {owner}.{name}: {rows} baris ke {}{cap}",
                    path.display()
                ));
                self.conn.status = format!("Export {name} selesai: {rows} baris.");
            }
            (Some(PendingOp::ImportCsv { owner, name }), Response::Imported(rows)) => {
                self.push_message(format!(
                    "Import {owner}.{name}: {rows} baris dimasukkan (belum commit)."
                ));
                self.conn.status = format!("{rows} baris diimpor ke {name} (belum commit).");
                self.target_tab_mut().results_tab = ResultsTab::Messages;
            }
            (Some(PendingOp::DataStructure), Response::Described(defs)) => {
                let result = schema::describe_to_result(&defs);
                self.conn.status = format!("{} kolom.", result.row_count());
                if let Some(state) = self
                    .target_tab_mut()
                    .data
                    .as_ref()
                    .map(|d| d.structure.clone())
                {
                    state.update(cx, |state, cx| {
                        state.delegate_mut().set_result(result);
                        state.refresh(cx);
                    });
                }
            }
            (Some(PendingOp::DataIndexes), Response::Indexes(indexes)) => {
                let result = schema::indexes_to_result(&indexes);
                self.conn.status = format!("{} index.", result.row_count());
                if let Some(state) = self
                    .target_tab_mut()
                    .data
                    .as_ref()
                    .map(|d| d.indexes.clone())
                {
                    state.update(cx, |state, cx| {
                        state.delegate_mut().set_result(result);
                        state.refresh(cx);
                    });
                }
            }
            (Some(PendingOp::DataDdl), Response::Ddl(ddl)) => {
                self.conn.status = "DDL dimuat.".to_string();
                if let Some(editor) = self.target_tab_mut().data.as_ref().map(|d| d.ddl.clone()) {
                    editor.update(cx, |state, cx| state.set_value(ddl, window, cx));
                }
            }
            (Some(PendingOp::ApplyEdits), Response::CellsUpdated { done, error }) => {
                self.edits_applied(done, error, cx);
            }
            (Some(PendingOp::Commit), Response::Committed) => {
                self.conn.uncommitted = false;
                self.push_message("Commit berhasil.".to_string());
                self.conn.status = "Commit berhasil.".to_string();
                self.clear_edit_marks(cx);
            }
            (Some(PendingOp::Rollback), Response::RolledBack) => {
                self.conn.uncommitted = false;
                self.push_message("Rollback berhasil.".to_string());
                self.conn.status = "Rollback berhasil.".to_string();
                self.clear_edit_marks(cx);
                // The grid still shows the rolled-back values.
                if !background
                    && self
                        .tabs
                        .get(self.active_tab)
                        .is_some_and(|t| t.data.is_some())
                {
                    self.reload_data(cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Connect again to the last connection: demo and profiles with a
    /// saved password reconnect directly; anything else opens its dialog.
    fn reconnect_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(last) = self.conn.last_connection.clone() else {
            return;
        };
        if last.demo {
            self.connect(true, window, cx);
        } else if let Some(ix) = last.profile_ix.filter(|ix| *ix < self.profiles.len()) {
            self.open_profile(ix, window, cx);
        } else {
            self.edit_last_connection(window, cx);
        }
    }

    fn edit_last_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(last) = self.conn.last_connection.clone() else {
            return;
        };
        if last.demo {
            return;
        }
        match last.profile_ix.filter(|ix| *ix < self.profiles.len()) {
            Some(ix) => self.edit_profile(ix, window, cx),
            None => {
                let profile = SavedProfile {
                    id: String::new(),
                    name: last.profile.name.clone(),
                    username: last.profile.username.clone(),
                    connect_string: last.profile.connect_string.clone(),
                    save_password: false,
                };
                self.editing_profile = None;
                self.fill_form(&profile, window, cx);
                self.form_notice = None;
                self.show_connection_form = true;
                cx.notify();
            }
        }
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The Root sets rem to the theme font size (13px) every frame; the
        // layout was built for the default 16px rem.
        window.set_rem_size(px(16.));
        let theme = cx.theme().clone();
        // Nothing focused (fresh start, closed palette): take focus so
        // window-level shortcuts reach this view.
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("Workbench")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _action: &RunQuery, _, cx| this.run(cx)))
            .on_action(cx.listener(|this, _: &RunCurrent, _, cx| this.run_current(cx)))
            .on_action(cx.listener(|this, _action: &OpenSettings, _, cx| {
                this.rail = RailPanel::Settings;
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _action: &CheckForUpdates, _, cx| this.check_updates(true, cx)),
            )
            .on_action(cx.listener(|this, _: &NewConnection, window, cx| {
                this.rail = RailPanel::Connections;
                this.open_new_connection(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ConnectLast, window, cx| {
                if this.conn.session.is_none() && !this.conn.busy {
                    this.reconnect_last(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DisconnectSession, window, cx| {
                if this.conn.session.is_some() && !this.conn.busy {
                    let id = this.conn.id;
                    this.request_disconnect(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RefreshSchema, _, cx| {
                if this.conn.session.is_some() && !this.conn.busy {
                    this.refresh_schema(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &EditConnection, window, cx| {
                if this.conn.session.is_none() && !this.conn.busy && !this.conn.demo_mode {
                    this.edit_last_connection(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _action: &QuickOpenTable, window, cx| {
                this.toggle_quick_open(window, cx)
            }))
            .relative()
            .children(self.render_update_banner(&theme, cx))
            .child(
                // The rail sits outside the resizable group: as a fixed
                // panel it was padded up to the group's minimum panel
                // width, leaving an empty strip next to it.
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .min_h(px(0.))
                    .child(self.render_rail(&theme, cx))
                    .child(
                        div().flex_1().min_w(px(0.)).child(
                            h_resizable("main-split")
                                .child(
                                    resizable_panel()
                                        .size(px(SIDE_PANEL_WIDTH))
                                        .size_range(px(220.)..px(440.))
                                        .child(self.render_side_panel(&theme, cx)),
                                )
                                .child(resizable_panel().child(self.render_main(&theme, cx))),
                        ),
                    ),
            )
            .child(self.render_status_bar(&theme))
            .when(self.show_connection_form, |this| {
                this.child(self.render_connection_dialog(&theme, cx))
            })
            .when(self.quick_open.is_some(), |this| {
                this.child(self.render_quick_open(&theme, cx))
            })
            .when(self.cell_editor.is_some(), |this| {
                this.child(self.render_cell_editor(&theme, cx))
            })
            .when(self.disconnect_prompt.is_some(), |this| {
                this.child(self.render_disconnect_prompt(&theme, cx))
            })
    }
}

fn unix_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// CSV export filename for a unix timestamp. Split out for tests.
fn export_filename(stamp: u64) -> String {
    format!("Ora-The-Explorer-export-{stamp}.csv")
}

/// XLSX export filename for a unix timestamp. Split out for tests.
fn export_xlsx_filename(stamp: u64) -> String {
    format!("Ora-The-Explorer-export-{stamp}.xlsx")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_filename_is_stable_and_suffixed() {
        assert_eq!(export_filename(0), "Ora-The-Explorer-export-0.csv");
        assert_eq!(
            export_filename(1_700_000_000),
            "Ora-The-Explorer-export-1700000000.csv"
        );
        assert_eq!(
            export_xlsx_filename(1_700_000_000),
            "Ora-The-Explorer-export-1700000000.xlsx"
        );
    }
}
