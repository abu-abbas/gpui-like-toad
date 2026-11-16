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
    Sizable as _,
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
use ora_core::sql::{clean_statement, format_sql};
use ora_core::store::{self, HistoryEntry, SavedProfile, ThemePreference};
use ora_core::{
    compose_easy_connect, parse_easy_connect, ConnectionProfile, DbInfo, ObjectKind, PlanNode,
    QueryResult, SchemaObject, DEFAULT_MAX_ROWS,
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

mod data_tab;
use data_tab::DataTab;

actions!(db_explorer, [RunQuery]);

const MAX_MESSAGES: usize = 200;

/// Rows per page in a data tab: the whole capped fetch, so the grid
/// scrolls instead of paging.
const DATA_PAGE_SIZE: usize = DEFAULT_MAX_ROWS;

/// Header height of the dense data-tab grid (rows are 26px).
const DATA_HEADER_HEIGHT: f32 = 28.;

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
    Describe { owner: String, name: String },
    Ddl { owner: String, name: String },
    ExportTable { owner: String, name: String },
    ImportCsv { owner: String, name: String },
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
    next_tab_id: u64,
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
    window_handle: AnyWindowHandle,
    schemas: Vec<String>,
    current_schema: Option<String>,
    category: ObjectCategory,
    selected_object: Option<String>,
    object_scroll: UniformListScrollHandle,
    objects: Vec<SchemaObject>,
    tree_filter: String,
    messages: Vec<String>,
    status: String,
    /// Autocomplete metadata, shared with every tab's completion provider.
    catalog: Rc<RefCell<Catalog>>,
    completion: Rc<SqlCompletionProvider>,
    /// Schemas whose catalog load is in flight, so one is not asked twice.
    catalog_loading: HashSet<String>,
    /// Bumped on connect and disconnect; a catalog response from an older
    /// session is dropped.
    catalog_epoch: u64,
    /// Schema the user is waiting on after typing `SCHEMA.`: reopen the
    /// popup when its catalog arrives.
    catalog_retrigger: Option<String>,
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
                    this.catalog_retrigger = Some(schema.to_ascii_uppercase());
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
            next_tab_id: 1,
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
            window_handle: window.window_handle(),
            schemas: Vec::new(),
            current_schema: None,
            category: ObjectCategory::Tables,
            selected_object: None,
            object_scroll: UniformListScrollHandle::new(),
            objects: Vec::new(),
            tree_filter: String::new(),
            messages: Vec::new(),
            status: "Belum terhubung.".to_string(),
            catalog,
            completion,
            catalog_loading: HashSet::new(),
            catalog_epoch: 0,
            catalog_retrigger: None,
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
        match self.pending_tab {
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
        self.pending_tab = self.tabs.get(self.active_tab).map(|t| t.id);
        self.busy = true;
        cx.notify();
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            // The window is needed to create tabs (View Script) and set
            // editor text from a response.
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.on_db_response(generation, resp, window, cx)
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
        let Some(db) = self.db.clone() else {
            return;
        };
        if self.session.is_none() {
            return;
        }
        let key = owner.to_ascii_uppercase();
        if !self.catalog_loading.insert(key.clone()) {
            return;
        }
        let rx: Receiver<Response> = db.request(Job::Catalog { owner });
        let epoch = self.catalog_epoch;
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.on_catalog_response(epoch, key, resp, window, cx)
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.catalog_epoch {
            return;
        }
        self.catalog_loading.remove(&owner);
        let waiting = self.catalog_retrigger.as_deref() == Some(owner.as_str());
        if waiting {
            self.catalog_retrigger = None;
        }
        match resp {
            Ok(Response::Catalog(objects)) => {
                self.catalog.borrow_mut().set_owner(&owner, objects);
                // The user typed `SCHEMA.` and got nothing because it was
                // still loading: show the list now, if they are still there.
                if waiting && !self.show_home {
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
        self.catalog_epoch += 1;
        self.catalog_loading.clear();
        self.catalog_retrigger = None;
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

    fn connect(&mut self, demo: bool, cx: &mut Context<Self>) {
        if self.busy {
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
        if !saved.save_password || self.busy {
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
        if self.db.is_none() {
            self.db = Some(DbHandle::spawn());
        }
        self.demo_mode = demo;
        self.connect_error = None;
        self.last_connection = Some(LastConnection {
            profile: profile.clone(),
            demo,
            profile_ix,
        });
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
        let connect_string = self.form_connect_string(cx);
        if username.is_empty() || connect_string.is_empty() {
            self.form_notice = Some((true, FORM_INCOMPLETE.to_string()));
            cx.notify();
            return;
        }
        self.form_notice = None;
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
                password: self.dialog_password(cx),
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
        self.reset_catalog(None);
        self.demo_mode = false;
        self.show_connection_form = false;
        self.objects.clear();
        self.schemas.clear();
        self.current_schema = None;
        self.selected_object = None;
        self.clear_all_grids(cx);
        self.status = "Belum terhubung.".to_string();
        self.push_message("Disconnected.".to_string());
        cx.notify();
    }

    /// Reload the schema list, then the objects of the current schema
    /// (the session user's own schema on first load).
    fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.session.is_none() {
            self.push_message("Belum terhubung.".to_string());
            return;
        }
        self.status = "Memuat daftar schema...".to_string();
        self.spawn_db(Job::ListSchemas, PendingOp::ListSchemas, cx);
    }

    fn load_objects(&mut self, cx: &mut Context<Self>) {
        let Some(owner) = self.current_schema.clone() else {
            return;
        };
        self.status = format!("Memuat objek {owner}...");
        self.spawn_db(Job::ListObjects { owner }, PendingOp::ListObjects, cx);
    }

    fn select_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        if self.busy || self.current_schema.as_deref() == Some(schema.as_str()) {
            return;
        }
        self.current_schema = Some(schema);
        self.selected_object = None;
        self.objects.clear();
        self.load_objects(cx);
    }

    fn select_category(&mut self, category: ObjectCategory, cx: &mut Context<Self>) {
        self.category = category;
        self.selected_object = None;
        cx.notify();
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
        if self.active_tab().data.is_some() {
            // Data tab "Refresh": re-run the table fetch with its filter.
            self.reload_data(cx);
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
        let completion = self.completion.clone();
        let tab = QueryTab::new(id, format!("Query {id}"), "", &completion, window, cx);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_home = false;
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
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.show_home = false;
        self.tab_scroll.scroll_to_item(ix);
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
            self.connect_error = None;
            self.show_connection_form = true;
            cx.notify();
        }
    }

    fn new_profile_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_profile = None;
        self.use_tns = false;
        self.form_notice = None;
        self.connect_error = None;
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
            self.connect_error = None;
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
        if let Some(last) = &mut self.last_connection {
            last.profile_ix = match last.profile_ix {
                Some(i) if i == ix => None,
                Some(i) if i > ix => Some(i - 1),
                other => other,
            };
        }
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
        self.selected_object = Some(object.name.clone());
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
        if self.session.is_none() {
            return;
        }
        if self.busy {
            self.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        self.add_tab(window, cx);
        let view = cx.entity().downgrade();
        let tab = self.active_tab_mut();
        tab.title = format!("{owner}.{name}");
        tab.kind = TabKind::Data;
        tab.data = Some(DataTab::new(owner, name));
        tab.table_state.update(cx, |state, _| {
            *state.delegate_mut() = ResultsDelegate::empty()
                .with_page_size(DATA_PAGE_SIZE)
                .with_row_numbers()
                .with_quick_filter(Rc::new(move |condition, window, cx| {
                    let _ = view.update(cx, |this, cx| this.quick_filter(condition, window, cx));
                }));
        });
        self.add_filter_row(None, None, window, cx);
        self.reload_data(cx);
    }

    fn describe_object(&mut self, object: ObjectRef, cx: &mut Context<Self>) {
        if self.busy || self.session.is_none() || self.show_home {
            return;
        }
        self.status = format!("Describe {}.{}...", object.owner, object.name);
        self.spawn_db(
            Job::Describe {
                owner: object.owner.clone(),
                name: object.name.clone(),
            },
            PendingOp::Describe {
                owner: object.owner,
                name: object.name,
            },
            cx,
        );
    }

    /// "View Script": fetch the DDL; the response opens it in a new tab.
    fn view_script(&mut self, object: ObjectRef, cx: &mut Context<Self>) {
        if self.busy || self.session.is_none() {
            return;
        }
        self.status = format!("Mengambil DDL {}.{}...", object.owner, object.name);
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
        if self.busy || self.session.is_none() {
            return;
        }
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
                this.status = format!("Export {}.{}...", object.owner, object.name);
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
        })
        .detach();
    }

    /// "Import Data": pick a CSV file whose header names match the table's
    /// columns; rows are inserted without commit.
    fn import_table(&mut self, object: ObjectRef, cx: &mut Context<Self>) {
        if self.busy || self.session.is_none() {
            return;
        }
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
                this.status = format!("Import ke {}.{}...", object.owner, object.name);
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
        })
        .detach();
    }

    // ----- response handling -----

    fn on_db_response(
        &mut self,
        generation: u64,
        resp: Result<Response, RecvError>,
        window: &mut Window,
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
                // A direct connect with a saved password failed (wrong or
                // missing password): open the dialog so it can be fixed.
                if !self.show_connection_form {
                    if let Some(ix) = self
                        .last_connection
                        .as_ref()
                        .filter(|l| !l.demo)
                        .and_then(|l| l.profile_ix)
                    {
                        self.edit_profile(ix, window, cx);
                    }
                }
                self.connect_error = Some(message.clone());
            }
            self.push_message(message.clone());
            self.status = "Error (lihat Messages).".to_string();
            self.target_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        match (pending, resp) {
            (Some(PendingOp::Connect), Response::Connected(info)) => {
                self.session = Some(info.clone());
                self.reset_catalog(Some(info.user.clone()));
                self.current_schema = None;
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
                    let text = format!(
                        "Test koneksi berhasil: {} @ {} ({})",
                        info.user, info.database, info.version
                    );
                    self.push_message(text.clone());
                    self.form_notice = Some((false, text));
                    self.status = "Test koneksi berhasil.".to_string();
                }
                Err(message) => {
                    self.push_message(format!("Test koneksi gagal: {message}"));
                    self.form_notice = Some((true, format!("Test koneksi gagal: {message}")));
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
                let column_names: Vec<String> =
                    result.columns.iter().map(|c| c.name.clone()).collect();
                let tab = self.target_tab_mut();
                tab.elapsed_ms = Some(elapsed);
                if let Some(data) = tab.data.as_mut() {
                    data.set_columns(column_names);
                }
                self.set_grid_result(result, cx);
                self.target_tab_mut().results_tab = ResultsTab::Results;
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
                self.target_tab_mut().results_tab = ResultsTab::Messages;
            }
            (Some(PendingOp::Explain), Response::PlanTree(nodes, lines)) => {
                let tab = self.target_tab_mut();
                tab.plan_expanded = plan::all_plan_ids(&nodes);
                tab.plan_nodes = Some(nodes);
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.status = "Explain plan selesai.".to_string();
            }
            (Some(PendingOp::Explain), Response::Plan(lines)) => {
                let tab = self.target_tab_mut();
                tab.plan_nodes = None;
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.status = "Explain plan selesai.".to_string();
            }
            (Some(PendingOp::ListSchemas), Response::Schemas(schemas)) => {
                let user = self.session.as_ref().map(|s| s.user.clone());
                // Keep the current pick on refresh; otherwise start in the
                // session user's own schema, as Toad does.
                let keep = self.current_schema.clone().filter(|c| schemas.contains(c));
                self.current_schema = keep
                    .or_else(|| user.filter(|u| schemas.contains(u)))
                    .or_else(|| schemas.first().cloned());
                self.catalog.borrow_mut().set_schemas(schemas.clone());
                self.schemas = schemas;
                self.load_objects(cx);
            }
            (Some(PendingOp::ListObjects), Response::Objects(objects)) => {
                let count = objects.len();
                self.objects = objects;
                self.selected_object = None;
                let schema = self.current_schema.clone().unwrap_or_default();
                self.status = format!("{schema}: {count} objek dimuat.");
                // Autocomplete metadata follows the explorer: (re)load the
                // schema just listed, and the session user's own schema,
                // which unqualified names resolve against.
                let user = self.session.as_ref().map(|s| s.user.clone());
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
                self.status = format!("Script {owner}.{name}.");
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
                self.status = format!("Export {name} selesai: {rows} baris.");
            }
            (Some(PendingOp::ImportCsv { owner, name }), Response::Imported(rows)) => {
                self.push_message(format!(
                    "Import {owner}.{name}: {rows} baris dimasukkan (belum commit)."
                ));
                self.status = format!("{rows} baris diimpor ke {name} (belum commit).");
                self.target_tab_mut().results_tab = ResultsTab::Messages;
            }
            (Some(PendingOp::Describe { owner, name }), Response::Described(defs)) => {
                let result = schema::describe_to_result(&defs);
                let count = result.row_count();
                self.set_grid_result(result, cx);
                self.target_tab_mut().results_tab = ResultsTab::Results;
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
            return (theme.warning, "Connecting…", String::new());
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
                "lihat Messages".to_string(),
            );
        }
        (theme.muted_foreground, "Disconnected", String::new())
    }

    // ----- rendering: chrome -----

    fn render_tab_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Home stays pinned on the left and "+" on the right; the tabs in
        // between scroll sideways (mouse wheel or trackpad) when they no
        // longer fit. `flex_none` keeps the bar at its full height: as a
        // flex child it used to shrink when the content below was tall.
        let mut strip = div()
            .id("tab-strip")
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink(1.)
            .min_w(px(0.))
            .h_full()
            .gap(px(SPACE_1))
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        for (ix, tab) in self.tabs.iter().enumerate() {
            let active = !self.show_home && ix == self.active_tab;
            strip = strip.child(self.render_query_tab(ix, tab, active, theme, cx));
        }
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(TAB_BAR_HEIGHT))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(self.render_home_tab(theme, cx))
            .child(strip)
            .child(
                div()
                    .id("tab-add")
                    .flex_none()
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
            .flex_none()
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
            .flex_none()
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
                        match tab.kind {
                            TabKind::Query => IconName::Database,
                            TabKind::Data => IconName::Table,
                            TabKind::Script => IconName::FileCode,
                        },
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
                .on_click(cx.listener(|this, _, window, cx| this.open_new_connection(window, cx)))
                .child(icon(IconName::Plus, 16., theme.muted_foreground)),
        );
        panel = panel.child(header);
        panel = panel.child(self.render_session_card(theme, cx));
        // Navicat-style: saved connections are listed while disconnected
        // (click one to open its dialog); once connected the panel belongs
        // to the explorer. New/edit connection happens in a modal.
        if self.session.is_some() {
            panel = panel.child(self.render_explorer(theme, cx));
        } else {
            panel = panel.child(self.render_profile_list(theme, cx));
            panel = panel.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_3))
                    .pt(px(SPACE_3))
                    .child(
                        Button::new("side-new-connection")
                            .label("New connection")
                            .icon(IconName::Plus)
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_new_connection(window, cx)
                            })),
                    )
                    .child(
                        Button::new("demo")
                            .label("Demo")
                            .icon(IconName::Play)
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.connect(true, cx))),
                    ),
            );
        }
        panel
    }

    /// Connect again to the last connection: demo and profiles with a
    /// saved password reconnect directly; anything else opens its dialog.
    fn reconnect_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(last) = self.last_connection.clone() else {
            return;
        };
        if last.demo {
            self.connect(true, cx);
        } else if let Some(ix) = last.profile_ix.filter(|ix| *ix < self.profiles.len()) {
            self.open_profile(ix, window, cx);
        } else {
            self.edit_last_connection(window, cx);
        }
    }

    fn edit_last_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(last) = self.last_connection.clone() else {
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

    /// Minimal connection row: status dot (green connected, red not),
    /// connection name, and a ⋮ menu with Refresh, Connect/Disconnect,
    /// and Edit connection.
    fn render_session_card(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(last) = self.last_connection.clone() else {
            return div()
                .px(px(SPACE_3))
                .pt(px(SPACE_3))
                .child(caption_text(
                    "Not connected. Pick a connection or try Demo.".to_string(),
                    theme,
                ))
                .into_any_element();
        };
        let connected = self.session.is_some();
        let connecting = self.busy && matches!(self.pending_op, Some(PendingOp::Connect));
        let dot = if connecting {
            theme.warning
        } else if connected {
            theme.success
        } else {
            theme.danger
        };
        let view = cx.entity();
        let busy = self.busy;
        let demo = last.demo;
        let menu_button = Button::new("connection-actions")
            .icon(IconName::EllipsisVertical)
            .ghost()
            .small()
            .dropdown_menu(move |menu, _, _| {
                let refresh_view = view.clone();
                let toggle_view = view.clone();
                let edit_view = view.clone();
                menu.min_w(px(200.))
                    .item(
                        PopupMenuItem::new("Refresh")
                            .icon(Icon::new(IconName::RefreshCw))
                            .disabled(!connected || busy)
                            .on_click(move |_, _, cx: &mut App| {
                                refresh_view.update(cx, |this, cx| this.refresh_schema(cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new(if connected { "Disconnect" } else { "Connect" })
                            .icon(Icon::new(if connected {
                                IconName::LogOut
                            } else {
                                IconName::LogIn
                            }))
                            .disabled(busy)
                            .on_click(move |_, window, cx: &mut App| {
                                toggle_view.update(cx, |this, cx| {
                                    if this.session.is_some() {
                                        this.disconnect(cx);
                                    } else {
                                        this.reconnect_last(window, cx);
                                    }
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Edit connection")
                            .icon(Icon::new(IconName::SquarePen))
                            // Only while disconnected: editing the profile a
                            // live session uses would not change that session.
                            // Demo has no host/user to edit.
                            .disabled(connected || busy || demo)
                            .on_click(move |_, window, cx: &mut App| {
                                edit_view
                                    .update(cx, |this, cx| this.edit_last_connection(window, cx));
                            }),
                    )
            });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .mx(px(SPACE_3))
            .mt(px(SPACE_2))
            .pl(px(SPACE_2))
            .h(px(TOOLBAR_HEIGHT))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .child(status_dot(dot))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(FONT_LABEL))
                    .line_height(px(LEADING_LABEL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(last.profile.name.clone()),
            )
            .child(menu_button)
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

    /// The New/Edit connection dialog, drawn as a modal over the whole
    /// window (scrim + centered card).
    fn render_connection_dialog(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let field = |label: &str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_1))
                .child(form_label(label, theme))
                .child(Input::new(input).w_full())
        };
        let mode_button = |id: &'static str, label: &'static str, tns: bool| {
            let active = self.use_tns == tns;
            let mut b = div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .flex_1()
                .h(px(28.))
                .rounded(px(RADIUS_SM))
                .text_size(px(FONT_LABEL))
                .font_weight(FontWeight::MEDIUM)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.use_tns = tns;
                    cx.notify();
                }))
                .child(label);
            if active {
                b = b
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.foreground);
            } else {
                b = b.text_color(theme.muted_foreground);
            }
            b
        };
        let title = if self.editing_profile.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_3))
            .p(px(SPACE_4))
            .child(field("Connection name", &self.name_input))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(RADIUS_MD))
                    .bg(theme.secondary)
                    .child(mode_button("conn-mode-basic", "Basic", false))
                    .child(mode_button("conn-mode-tns", "TNS", true)),
            );
        if self.use_tns {
            body = body.child(field("TNS alias / connect descriptor", &self.tns_input));
        } else {
            body = body
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(SPACE_2))
                        .child(div().flex_1().child(field("Host", &self.host_input)))
                        .child(div().w(px(96.)).child(field("Port", &self.port_input))),
                )
                .child(field("Service name", &self.service_input));
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE_2))
                .child(div().flex_1().child(field("Username", &self.user_input)))
                .child(
                    div().flex_1().child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_1))
                            .child(form_label("Password", theme))
                            .child(Input::new(&self.password_input).mask_toggle().w_full()),
                    ),
                ),
        );
        body = body.child(
            Checkbox::new("save-password")
                .label("Save password")
                .checked(self.save_password)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.save_password = *checked;
                    cx.notify();
                })),
        );
        let notice = self
            .connect_error
            .clone()
            .map(|e| (true, e))
            .or_else(|| self.form_notice.clone());
        if let Some((is_error, text)) = notice {
            body = body.child(
                div()
                    .p(px(SPACE_2))
                    .rounded(px(RADIUS_SM))
                    .text_size(px(FONT_CAPTION))
                    .line_height(px(LEADING_CAPTION))
                    .text_color(if is_error {
                        theme.danger
                    } else {
                        theme.success
                    })
                    .border_1()
                    .border_color(if is_error {
                        theme.danger
                    } else {
                        theme.success
                    })
                    .child(text),
            );
        } else {
            let has_saved = self
                .editing_profile
                .and_then(|ix| self.profiles.get(ix))
                .is_some_and(|p| p.save_password);
            let hint = if self.save_password && has_saved {
                "Password tersimpan di keychain OS. Kosongkan untuk tetap pakai yang lama."
            } else if self.save_password {
                "Password disimpan di keychain OS, bukan di file profil."
            } else {
                "Password tidak disimpan; diisi setiap connect."
            };
            body = body.child(caption_text(hint.to_string(), theme));
        }
        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("test-connect")
                    .label("Test")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
            )
            .child(
                Button::new("save-profile")
                    .label("Save")
                    .icon(IconName::Save)
                    .on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))),
            );
        if let Some(ix) = self.editing_profile {
            footer = footer.child(
                Button::new("remove-profile")
                    .icon(IconName::Trash)
                    .danger()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_profile(ix, cx);
                        this.close_connection_dialog(cx);
                    })),
            );
        }
        footer = footer
            .child(div().flex_1())
            .child(
                Button::new("cancel-connect")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.close_connection_dialog(cx))),
            )
            .child(
                Button::new("connect")
                    .label(
                        if self.busy && matches!(self.pending_op, Some(PendingOp::Connect)) {
                            "Connecting…"
                        } else {
                            "Connect"
                        },
                    )
                    .icon(IconName::Database)
                    .primary()
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.connect(false, cx))),
            );
        let card = div()
            .id("connection-dialog")
            .flex()
            .flex_col()
            .w(px(480.))
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
                    .h(px(48.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(IconName::Database, 18., theme.primary))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(FONT_BODY))
                            .line_height(px(LEADING_BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .id("connection-dialog-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(28.))
                            .rounded(px(RADIUS_MD))
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.close_connection_dialog(cx)),
                            )
                            .child(icon(IconName::X, 16., theme.muted_foreground)),
                    ),
            )
            .child(body)
            .child(footer);
        div()
            .id("connection-dialog-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::hsla(0., 0., 0., 0.45))
            // Swallow clicks so nothing under the modal reacts.
            .occlude()
            .child(card)
    }

    /// Toad-style object browser: schema dropdown, category tabs, filter
    /// box, and a flat virtualized list of objects with a context menu.
    fn render_explorer(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let schemas = self.schemas.clone();
        let current = self.current_schema.clone();
        let schema_button = Button::new("schema-select")
            .child(select_trigger(
                current.clone().unwrap_or_else(|| "(schema)".to_string()),
                theme,
            ))
            .w_full()
            .disabled(self.schemas.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(220.));
                for schema in &schemas {
                    let pick = schema.clone();
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(schema.clone())
                            .checked(current.as_deref() == Some(schema.as_str()))
                            .on_click(move |_, _, cx: &mut App| {
                                let pick = pick.clone();
                                view.update(cx, |this, cx| this.select_schema(pick, cx));
                            }),
                    );
                }
                menu
            });

        let view = cx.entity();
        let counts: Vec<(ObjectCategory, usize)> = ObjectCategory::ALL
            .iter()
            .map(|c| (*c, schema::count_in(&self.objects, *c)))
            .collect();
        let current_category = self.category;
        let category_button = Button::new("category-select")
            .child(select_trigger(
                format!(
                    "{} ({})",
                    current_category.label(),
                    schema::count_in(&self.objects, current_category)
                ),
                theme,
            ))
            .w_full()
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(220.));
                for (category, count) in &counts {
                    let category = *category;
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{} ({count})", category.label()))
                            .checked(category == current_category)
                            .on_click(move |_, _, cx: &mut App| {
                                view.update(cx, |this, cx| this.select_category(category, cx));
                            }),
                    );
                }
                menu
            });

        let visible: Vec<ObjectRef> =
            schema::objects_in(&self.objects, self.category, &self.tree_filter)
                .into_iter()
                .map(|o| ObjectRef {
                    owner: o.owner.clone(),
                    name: o.name.clone(),
                    kind: o.kind.clone(),
                })
                .collect();
        let list = if visible.is_empty() {
            let message = if self.busy && self.objects.is_empty() {
                "Memuat objek..."
            } else if self.tree_filter.is_empty() {
                "Tidak ada objek di kategori ini."
            } else {
                "Tidak ada objek yang cocok dengan filter."
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p(px(SPACE_4))
                .child(empty_state_text(message.to_string(), theme))
                .into_any_element()
        } else {
            let count = visible.len();
            let selected = self.selected_object.clone();
            let theme = theme.clone();
            div()
                .flex_1()
                .px(px(SPACE_2))
                .child(
                    uniform_list(
                        "explorer-objects",
                        count,
                        cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .map(|ix| {
                                    let object = visible[ix].clone();
                                    let is_selected =
                                        selected.as_deref() == Some(object.name.as_str());
                                    render_object_row(ix, object, is_selected, &theme, cx)
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.object_scroll)
                    .size_full(),
                )
                .into_any_element()
        };

        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .pt(px(SPACE_3))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_3))
                    .child(div().w(px(52.)).child(form_label("Schema", theme)))
                    .child(div().flex_1().child(schema_button)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_3))
                    .child(div().w(px(52.)).child(form_label("Objects", theme)))
                    .child(div().flex_1().child(category_button)),
            )
            .child(
                div()
                    .px(px(SPACE_3))
                    .child(Input::new(&self.search_input).w_full()),
            )
            .child(list)
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
                format!(concat!(app_name!(), " {}"), env!("CARGO_PKG_VERSION")),
                theme,
            ))
            .child(caption_text(
                "Oracle database client built with GPUI + Rust.".to_string(),
                theme,
            ))
            .child(caption_text(
                "Theme preference, saved connections, and query history are stored in your user config directory. Passwords are only saved when you tick \"Save password\", and then in the OS keychain, never in these files.".to_string(),
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
        // `min_w(0)` + clip: without them a wide result grid's min-content
        // width pushes the column past the window edge, hiding the
        // toolbar's right side and the plan panel.
        let mut main = div()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .min_w(px(0.))
            .overflow_hidden()
            .bg(theme.background);
        // The tab strip lives at the top of the main column only (right of
        // the sidebar), above the toolbar — as in the design. No spacer
        // above the rail/sidebar: the design shares the top row between
        // the first rail item, the sidebar header, and the tab strip.
        main = main.child(self.render_tab_bar(theme, cx));
        main = main.child(self.render_toolbar(theme, cx));
        // The split wrappers below take `min_h(0)`: as flex items they
        // otherwise grow to their content (e.g. a long Messages list),
        // pushing the bottom panel past the window instead of scrolling.
        if self.show_home {
            main = main.child(self.render_home(theme, cx));
        } else if self.active_tab().kind != TabKind::Query {
            // Toad-style object tabs: the data grid (or the script) fills
            // the work area, with info and errors in a strip below.
            let top = if self.active_tab().kind == TabKind::Data {
                self.render_data_grid(theme, cx).into_any_element()
            } else {
                self.render_editor(cx).into_any_element()
            };
            main = main.child(
                div().flex_1().min_h(px(0.)).child(
                    v_resizable("object-split")
                        .child(resizable_panel().child(top))
                        .child(
                            resizable_panel()
                                .size(px(150.))
                                .size_range(px(80.)..px(400.))
                                .child(self.render_info_panel(theme)),
                        ),
                ),
            );
        } else {
            main = main.child(
                div().flex_1().min_h(px(0.)).child(
                    v_resizable("editor-split")
                        .child(resizable_panel().child(self.render_editor(cx)))
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
            .flex_none()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .gap(px(SPACE_2))
            .px(px(SPACE_3))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border);
        let data_tab = !on_home && self.active_tab().kind == TabKind::Data;
        bar = bar
            // Like Navicat's "New Query" / Toad's "Editor": always opens a
            // fresh SQL editor tab, from any tab including Home.
            .child(
                Button::new("new-sql")
                    .small()
                    .label("New SQL")
                    .icon(IconName::SquarePen)
                    .disabled(no_session)
                    .tooltip("Buka tab SQL editor baru")
                    .on_click(cx.listener(|this, _, window, cx| this.add_tab(window, cx))),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(theme.border))
            .child(
                Button::new("run")
                    .small()
                    .label(if data_tab { "Refresh" } else { "Run" })
                    .icon(if data_tab {
                        IconName::RefreshCw
                    } else {
                        IconName::Play
                    })
                    .primary()
                    .loading(self.busy)
                    .disabled(on_home || no_session)
                    .tooltip("Run (Cmd/Ctrl+Enter atau F5)")
                    .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
            )
            .when(!data_tab, |bar| {
                bar.child(
                    Button::new("explain")
                        .small()
                        .label("Explain")
                        .icon(IconName::ChartColumn)
                        .disabled(on_home || no_session || self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.explain(cx))),
                )
            })
            .when(!data_tab, |bar| {
                bar.child(
                    Button::new("format")
                        .small()
                        .label("Format")
                        .icon(IconName::WandSparkles)
                        .disabled(on_home || no_session)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.format_editor(window, cx)),
                        ),
                )
            })
            .child(
                Button::new("stop")
                    .small()
                    .label("Stop")
                    .icon(IconName::Square)
                    .disabled(no_session || !self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            )
            .child(
                Button::new("commit")
                    .small()
                    .label("Commit")
                    .icon(IconName::Check)
                    .ghost()
                    .disabled(no_session || self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
            )
            .child(
                Button::new("rollback")
                    .small()
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
                .h(px(24.))
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

    /// Full-area grid of a data tab, with pagination under it.
    fn render_data_grid(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let grid = if tab.last_result.is_some() {
            div()
                .flex_1()
                .child(
                    DataTable::new(&tab.table_state)
                        .stripe(true)
                        .bordered(true)
                        .xsmall()
                        .header_row_height(px(DATA_HEADER_HEIGHT)),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(empty_state_text(
                    if self.busy {
                        "Memuat data...".to_string()
                    } else {
                        "Tidak ada data. Lihat Info di bawah untuk error.".to_string()
                    },
                    theme,
                ))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .overflow_hidden()
            .child(self.render_filter_bar(theme, cx))
            .child(div().flex().flex_col().flex_1().child(grid))
            .child(self.render_data_footer(theme))
    }

    /// Info strip under a data/script tab: what the tab shows, then the
    /// latest messages with errors highlighted.
    fn render_info_panel(&self, theme: &gpui_kit::component::Theme) -> impl IntoElement {
        let tab = self.active_tab();
        let mut summary = tab.title.clone();
        if let Some(result) = &tab.last_result {
            summary.push_str(&format!(
                " · {} kolom · {} baris",
                result.columns.len(),
                result.row_count()
            ));
            if result.truncated {
                summary.push_str(&format!(" (dibatasi {DEFAULT_MAX_ROWS})"));
            }
        }
        if let Some(ms) = tab.elapsed_ms {
            summary.push_str(&format!(" · {ms} ms"));
        }
        let start = self.messages.len().saturating_sub(MAX_MESSAGES_SHOWN);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .h(px(SUB_TAB_HEIGHT))
                    .px(px(SPACE_3))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(IconName::Info, 14., theme.muted_foreground))
                    .child(
                        div()
                            .text_size(px(FONT_LABEL))
                            .line_height(px(LEADING_LABEL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Info"),
                    )
                    .child(caption_text(summary, theme)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .id("info-messages")
                    .overflow_y_scroll()
                    .px(px(SPACE_3))
                    .py(px(SPACE_2))
                    .child(div().flex().flex_col().gap(px(SPACE_1)).children(
                        self.messages[start..].iter().rev().map(|m| {
                            div()
                                .text_size(px(FONT_LABEL))
                                .line_height(px(LEADING_LABEL))
                                .text_color(if is_error_message(m) {
                                    theme.danger
                                } else {
                                    theme.foreground
                                })
                                .child(m.clone())
                        }),
                    )),
            )
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            // Tab accepts the highlighted suggestion while the popup is
            // open. The editor only does that for Enter, so Tab (its
            // indent action) is caught on the way down and replayed as
            // Enter; with the popup closed it indents as usual.
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                let open = this
                    .active_tab()
                    .editor
                    .read(cx)
                    .completion_menu_state()
                    .open;
                if open {
                    cx.stop_propagation();
                    window.dispatch_action(
                        Box::new(Enter {
                            secondary: false,
                            shift: false,
                        }),
                        cx,
                    );
                }
            }))
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
        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .overflow_hidden()
            .bg(theme.background);
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
            // `min_h(0)`: without it the wrapper grows to fit a long
            // Messages list instead of letting it scroll.
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
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
                .child(self.render_export_dropdown(cx)),
        )
    }

    /// One Export dropdown (CSV/XLSX) via the component `DropdownMenu`
    /// API. The trigger shows no chevron glyph: `Button` renders none
    /// automatically and a unicode chevron is not allowed in this UI.
    fn render_export_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_result = self.active_tab().last_result.is_some();
        let view = cx.entity();
        Button::new("export")
            .label("Export")
            .icon(IconName::Download)
            .disabled(!has_result)
            .dropdown_menu(move |menu, _, _| {
                let csv_view = view.clone();
                let xlsx_view = view.clone();
                menu.item(
                    PopupMenuItem::new("Export CSV")
                        .disabled(!has_result)
                        .on_click(move |_, _, cx: &mut App| {
                            csv_view.update(cx, |this, cx| this.export_csv(cx));
                        }),
                )
                .item(
                    PopupMenuItem::new("Export XLSX")
                        .disabled(!has_result)
                        .on_click(move |_, _, cx: &mut App| {
                            xlsx_view.update(cx, |this, cx| this.export_xlsx(cx));
                        }),
                )
            })
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
                        .min_h(px(0.))
                        .id("messages-scroll")
                        .overflow_y_scroll()
                        .track_scroll(&self.messages_scroll)
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
        let (first, last, total, current, total_pages, page_size) = {
            let state = tab.table_state.read(cx);
            let delegate = state.delegate();
            let (first, last, total) = delegate.page_range();
            (
                first,
                last,
                total,
                delegate.page(),
                delegate.total_pages(),
                delegate.page_size(),
            )
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
                    .child(caption_text(format!("{page_size} / page"), theme)),
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
                            .child(app_name!()),
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

    /// Navicat-style start page: one tile per connection, plus the demo
    /// database and a "New connection" tile.
    fn render_home_connections(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let section = div().flex().flex_col().gap(px(SPACE_2)).child(
            div()
                .text_size(px(FONT_HEADING_MD))
                .line_height(px(LEADING_HEADING_MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child("Connections"),
        );
        let tile = |id: String| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .w(px(232.))
                .p(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .hover(|this| this.border_color(theme.primary))
        };
        let tile_title = |icon_name: IconName, title: String, subtitle: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(32.))
                        .rounded(px(RADIUS_MD))
                        .bg(theme.accent)
                        .child(icon(icon_name, 18., theme.primary)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_size(px(FONT_BODY))
                                .line_height(px(LEADING_BODY))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .truncate()
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(FONT_CAPTION))
                                .line_height(px(LEADING_CAPTION))
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(subtitle),
                        ),
                )
        };
        let mut grid = div().flex().flex_row().flex_wrap().gap(px(SPACE_3));
        let demo_connected = self.demo_mode && self.session.is_some();
        grid = grid.child(
            tile("home-demo-tile".to_string())
                .child(tile_title(
                    IconName::Database,
                    "Demo database".to_string(),
                    "Built-in, no Oracle needed".to_string(),
                ))
                .child(
                    Button::new("home-demo")
                        .label(if demo_connected {
                            "Connected"
                        } else {
                            "Connect"
                        })
                        .icon(IconName::Play)
                        .primary()
                        .disabled(self.busy || demo_connected)
                        .on_click(cx.listener(|this, _, _, cx| this.connect(true, cx))),
                ),
        );
        for (ix, profile) in self.profiles.iter().enumerate() {
            let profile = profile.clone();
            grid = grid.child(
                tile(format!("home-profile-{ix}"))
                    .child(tile_title(
                        IconName::Database,
                        profile.name.clone(),
                        format!("{}@{}", profile.username, profile.connect_string),
                    ))
                    .child(
                        Button::new(format!("home-use-{ix}"))
                            .label("Open")
                            .icon(IconName::LogIn)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                // Saved password: connect now. Otherwise
                                // open the filled dialog for the password.
                                this.open_profile(ix, window, cx);
                            })),
                    ),
            );
        }
        grid = grid.child(
            tile("home-new-tile".to_string())
                .child(tile_title(
                    IconName::Plus,
                    "New connection".to_string(),
                    "Oracle (Easy Connect / TNS)".to_string(),
                ))
                .child(
                    Button::new("home-new")
                        .label("Create")
                        .icon(IconName::Plus)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_new_connection(window, cx);
                        })),
                ),
        );
        section.child(grid)
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
        let conn_text = if conn_detail.is_empty() {
            conn_label.to_string()
        } else {
            format!("{conn_label} · {conn_detail}")
        };
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
                        .child(conn_text),
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
        // Right side: activity while busy, last action while connected,
        // empty while disconnected — the connection state already shows
        // once on the left, and every status line is mirrored to Messages.
        bar.child(div().flex_1()).child(caption_text(
            if self.busy {
                "Running…".to_string()
            } else if self.session.is_some() {
                self.status.clone()
            } else {
                String::new()
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
            .relative()
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
    }
}

/// One explorer row: icon + name. Double click opens data (tables/views)
/// or the script (everything else); right click opens the Toad-style
/// object menu.
fn render_object_row(
    ix: usize,
    object: ObjectRef,
    selected: bool,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<Workbench>,
) -> impl IntoElement {
    let icon_name = match object.kind {
        ObjectKind::Table => IconName::Table,
        ObjectKind::View => IconName::Eye,
        ObjectKind::Index => IconName::ListOrdered,
        ObjectKind::Sequence => IconName::Hash,
        ObjectKind::Procedure | ObjectKind::Function => IconName::SquareFunction,
        ObjectKind::Package => IconName::Braces,
        ObjectKind::Trigger => IconName::Zap,
        ObjectKind::Synonym => IconName::Link,
        ObjectKind::Other(_) => IconName::FileText,
    };
    let click_object = object.clone();
    let mut row = div()
        .id(("explorer-row", ix))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE_2))
        .h(px(TREE_ROW_HEIGHT))
        .px(px(SPACE_2))
        .rounded(px(RADIUS_SM))
        .text_size(px(FONT_LABEL))
        .line_height(px(LEADING_LABEL))
        .on_click(cx.listener(move |this, event, window, cx| {
            this.on_object_click(click_object.clone(), event, window, cx);
        }));
    if selected {
        row = row.bg(theme.accent).text_color(theme.accent_foreground);
    } else {
        row = row
            .text_color(theme.foreground)
            .hover(|this| this.bg(theme.secondary));
    }
    let view = cx.entity();
    row.child(icon(icon_name, 14., theme.muted_foreground))
        .child(div().flex_1().truncate().child(object.name.clone()))
        .context_menu(move |menu, _, _| object_menu(menu, &view, &object))
}

/// The right-click menu for an explorer object.
fn object_menu(menu: PopupMenu, view: &Entity<Workbench>, object: &ObjectRef) -> PopupMenu {
    let has_data = schema::has_data(&object.kind);
    let is_table = object.kind == ObjectKind::Table;
    let item =
        |label: &str,
         icon_name: IconName,
         enabled: bool,
         f: fn(&mut Workbench, ObjectRef, &mut Window, &mut Context<Workbench>)| {
            let view = view.clone();
            let object = object.clone();
            PopupMenuItem::new(label.to_string())
                .icon(Icon::new(icon_name))
                .disabled(!enabled)
                .on_click(move |_, window, cx: &mut App| {
                    let object = object.clone();
                    view.update(cx, |this, cx| f(this, object, window, cx));
                })
        };
    menu.min_w(px(220.))
        .item(item(
            "Open Data",
            IconName::Table,
            has_data,
            |this, o, window, cx| this.open_table_data(&o.owner, &o.name, window, cx),
        ))
        .item(item(
            "Describe",
            IconName::Columns3,
            has_data,
            |this, o, _, cx| this.describe_object(o, cx),
        ))
        .item(item(
            "View Script",
            IconName::FileCode,
            true,
            |this, o, _, cx| this.view_script(o, cx),
        ))
        .separator()
        .item(item(
            "Export Data (CSV)",
            IconName::Download,
            has_data,
            |this, o, _, cx| this.export_table(o, ExportFormat::Csv, cx),
        ))
        .item(item(
            "Export Data (XLSX)",
            IconName::FileSpreadsheet,
            has_data,
            |this, o, _, cx| this.export_table(o, ExportFormat::Xlsx, cx),
        ))
        .item(item(
            "Import Data (CSV)",
            IconName::Upload,
            is_table,
            |this, o, _, cx| this.import_table(o, cx),
        ))
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

/// Content of a select-style dropdown button: the value left aligned and a
/// chevron on the right, like a native select (Button centers its label).
fn select_trigger(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .min_w(px(0.))
        .gap(px(SPACE_2))
        .child(div().flex_1().min_w(px(0.)).truncate().child(text))
        .child(icon(IconName::ChevronDown, 14., theme.muted_foreground))
}

/// Messages are plain strings; ORA-/driver errors and Indonesian failure
/// texts ("gagal") are shown in the danger color in the Info strip.
fn is_error_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    message.contains("ORA-")
        || message.contains("DPY-")
        || lower.contains("error")
        || lower.contains("gagal")
        || lower.starts_with("demo:")
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

    #[test]
    fn page_items_pin_first_last_with_ellipsis() {
        use PageItem::{Gap, Page};
        assert!(page_items(0, 0).is_empty());
        assert_eq!(page_items(0, 1), vec![Page(0)]);
        assert_eq!(page_items(0, 3), vec![Page(0), Page(1), Page(2)]);
        assert_eq!(
            page_items(0, 7),
            vec![
                Page(0),
                Page(1),
                Page(2),
                Page(3),
                Page(4),
                Page(5),
                Page(6)
            ]
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
