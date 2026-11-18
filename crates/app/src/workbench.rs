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
    self, CONNECTIONS_WIDTH, EXPLORER_WIDTH, FONT_BODY, FONT_CAPTION, FONT_HEADING_MD,
    FONT_HEADING_XL, FONT_LABEL, GRID_ROW_HEIGHT, LEADING_BODY, LEADING_CAPTION,
    LEADING_HEADING_MD, LEADING_HEADING_XL, LEADING_LABEL, PAGINATION_HEIGHT, PLAN_PANEL_WIDTH,
    RADIUS_MD, RADIUS_SM, RAIL_WIDTH, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
    STATUS_BAR_HEIGHT, SUB_TAB_HEIGHT, TAB_BAR_HEIGHT, TOOLBAR_HEIGHT, TREE_ROW_HEIGHT,
};
use crate::worker::{DbHandle, ExportFormat, Job, Password, Response};

mod cell_edit;
mod chrome;
mod completion_catalog;
mod connections_ui;
mod conns;
mod data_tab;
mod db_events;
mod explorer;
mod home;
mod profiles;
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
use home::HomeViewState;
use profiles::{ConnectionForm, FORM_INCOMPLETE};
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
    connection_form: ConnectionForm,
    search_input: Entity<InputState>,
    rail: RailPanel,
    sidebar_compact: bool,
    collapsed_connection_folders: HashSet<String>,
    settings: settings::SettingsState,
    profiles: Vec<SavedProfile>,
    history: Vec<HistoryEntry>,
    tabs: Vec<QueryTab>,
    active_tab: usize,
    /// Stand-in used while every tab is closed (only Home is left), so
    /// `active_tab()` never indexes an empty `tabs`. Never shown.
    scratch_tab: QueryTab,
    /// Horizontal scroll of the tab strip, so tabs past the right edge
    /// stay reachable and a newly opened/activated tab scrolls into view.
    tab_scroll: ScrollHandle,
    tab_overflow: bool,
    plan_collapsed: bool,
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
    home_view: HomeViewState,
    side_conn_folded: bool,
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
        let connection_form = ConnectionForm::new(window, cx);
        let search_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Filter…"));
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

        let settings = settings::SettingsState::new(window, cx);
        let settings_search_sub = cx.subscribe(&settings.search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });

        Workbench {
            connection_form,
            search_input,
            rail: RailPanel::Connections,
            sidebar_compact: false,
            collapsed_connection_folders: HashSet::new(),
            settings,
            profiles: store::load_profiles(),
            history: store::load_history(),
            tabs: vec![first_tab],
            active_tab: 0,
            scratch_tab,
            tab_scroll: ScrollHandle::new(),
            tab_overflow: false,
            plan_collapsed: false,
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
            home_view: HomeViewState::default(),
            side_conn_folded: false,
            focus_handle: cx.focus_handle(),
            tree_filter: String::new(),
            messages: Vec::new(),
            catalog,
            completion,
            updates: UpdateState::load(),
            _subscriptions: vec![search_sub, appearance_sub, settings_search_sub],
        }
    }

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
            .text_size(theme.font_size)
            .key_context("Workbench")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &RunQuery, _, cx| {
                if !this.settings.open {
                    this.run(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RunCurrent, _, cx| {
                if !this.settings.open {
                    this.run_current(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
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
            .child(self.render_title_bar(&theme, cx))
            .children(self.render_update_banner(&theme, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_navigation(&theme, cx))
                    .child(div().flex().flex_1().min_w(px(0.)).child(
                        if self.conn.session.is_some() {
                            h_resizable("workbench-explorer")
                                .child(
                                    resizable_panel()
                                        .size(px(EXPLORER_WIDTH))
                                        .size_range(px(190.)..px(440.))
                                        .child(self.render_connection_explorer(&theme, cx)),
                                )
                                .child(resizable_panel().child(self.render_main(&theme, cx)))
                                .into_any_element()
                        } else {
                            self.render_disconnected(&theme, cx).into_any_element()
                        },
                    )),
            )
            .child(self.render_status_bar(&theme))
            .when(self.connection_form.show_connection_form, |this| {
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
            .when(self.settings.open, |this| {
                this.child(self.render_settings_dialog(&theme, cx))
            })
    }
}

fn unix_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
