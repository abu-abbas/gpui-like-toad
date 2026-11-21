//! Data Transfer wizard: state and actions. Rendering is in
//! `transfer_ui.rs`; the statements, paging and safety rules are in
//! `ora_core::transfer`, which this file only drives.
//!
//! Every database call goes to a connection's own worker, so a transfer
//! never blocks the UI or other connections, and the worker's Read Only
//! and production guards apply to it as to any write. The wizard refuses a
//! Read Only target up front, asks for confirmation (typed schema name on
//! PROD) before dropping or emptying target tables, and Stop ends the run
//! between batches.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};

use ora_core::transfer::profile::{load_profiles_in, save_profile_in};
use ora_core::transfer::ranges::{self, RangeKind};
use ora_core::transfer::run::ScriptIo;
use ora_core::transfer::{
    run_transfer, Event, FieldMap, Fields, LogLevel, Rows, TableSpec, TransferProfile,
    TransferSpec, TransferSummary, TransferTarget,
};
use ora_core::{ColumnDef, ConnectionEnvironment};

use super::*;
use crate::transfer_io::WorkerIo;

const MAX_LOG_LINES: usize = 2000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Endpoints,
    Objects,
    Run,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TargetMode {
    Connection,
    File,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    None,
    Options,
    Fields,
    Generator,
    SaveProfile,
    LoadProfile,
    Confirm,
}

pub(super) struct PickedField {
    pub source: String,
    pub data_type: String,
    pub on: bool,
    pub target: Entity<InputState>,
}

/// Advanced-mode choices of one table (Auto mode needs none).
pub(super) struct TableConfig {
    pub advanced: bool,
    pub target: Entity<InputState>,
    pub custom_fields: bool,
    pub fields: Option<Vec<PickedField>>,
    pub fields_loading: bool,
    pub fields_error: Option<String>,
    /// From a loaded profile, applied when the columns arrive.
    pub pending_fields: Option<Vec<FieldMap>>,
    pub custom_rows: bool,
    pub batch: Entity<InputState>,
    pub recordsets: Vec<Entity<InputState>>,
}

pub(super) struct Generator {
    pub field: Option<String>,
    pub kind: RangeKind,
    pub start: Entity<InputState>,
    pub end: Entity<InputState>,
    pub count: Entity<InputState>,
    pub error: Option<String>,
}

pub(super) enum TablesState {
    Idle,
    Loading,
    Ready,
    Failed(String),
}

enum RunMsg {
    Event(Event),
    Done(TransferSummary),
}

pub(super) struct RunState {
    pub log: Vec<(LogLevel, String)>,
    pub table: Option<(usize, usize, String)>,
    pub rows: u64,
    pub total_rows: u64,
    pub skipped: u64,
    pub failed: usize,
    pub objects: usize,
    pub done_tables: usize,
    pub started: std::time::Instant,
    pub elapsed: Option<std::time::Duration>,
    pub running: bool,
    pub summary: Option<TransferSummary>,
    pub cancel: Arc<AtomicBool>,
}

pub(super) struct TransferWizard {
    pub step: Step,
    pub panel: Panel,
    pub source: Option<u64>,
    pub source_schema: Option<String>,
    pub source_schemas: Vec<String>,
    pub target_mode: TargetMode,
    pub target: Option<u64>,
    pub target_schema: Option<String>,
    pub target_schemas: Vec<String>,
    pub file_input: Entity<InputState>,
    pub file_schema: Entity<InputState>,
    pub file_from_dialog: bool,
    pub script_kind: DbKind,
    /// File target: one CSV per table in a folder instead of an SQL script.
    pub file_csv: bool,
    pub tables: Vec<String>,
    /// Names in `tables` that are views (transferred as new tables).
    pub views: BTreeSet<String>,
    pub tables_state: TablesState,
    pub selected: BTreeSet<String>,
    pub search: Entity<InputState>,
    pub focus: Option<String>,
    pub configs: HashMap<String, TableConfig>,
    pub options: ora_core::transfer::TransferOptions,
    pub generator: Option<Generator>,
    pub profile_name: Entity<InputState>,
    pub profiles: Vec<TransferProfile>,
    pub confirm_input: Entity<InputState>,
    pub error: Option<String>,
    pub run: Option<RunState>,
    /// Bumped when the source or target changes; late replies for an
    /// older choice are dropped.
    pub generation: u64,
}

/// What the endpoint pickers and the info panels show about one open
/// connection.
pub(super) struct OpenConn {
    pub id: u64,
    pub label: String,
    pub kind: DbKind,
    pub access: ConnectionAccess,
    pub info: Vec<(&'static str, String)>,
}

fn input(
    placeholder: &str,
    value: &str,
    window: &mut Window,
    cx: &mut Context<Workbench>,
) -> Entity<InputState> {
    let (placeholder, value) = (placeholder.to_string(), value.to_string());
    cx.new(|cx| {
        InputState::new(&mut *window, cx)
            .placeholder(placeholder)
            .default_value(value)
    })
}

fn set_text(
    entity: &Entity<InputState>,
    text: &str,
    window: &mut Window,
    cx: &mut Context<Workbench>,
) {
    let text = text.to_string();
    entity.update(cx, |state, cx| state.set_value(text, window, cx));
}

impl Workbench {
    pub(super) fn open_conns(&self) -> Vec<OpenConn> {
        std::iter::once(&self.conn)
            .chain(&self.parked)
            .filter(|c| c.session.is_some() && c.db.is_some())
            .map(|c| {
                let profile = c.last_connection.as_ref().map(|l| &l.profile);
                let kind = profile.map_or(DbKind::Oracle, |p| p.kind);
                let mut info = vec![
                    ("Name", c.label()),
                    ("Database Type", kind.label().to_string()),
                ];
                if let Some(p) = profile {
                    match kind {
                        DbKind::Oracle => match parse_easy_connect(&p.connect_string) {
                            Some((host, port, service)) => {
                                info.push(("Host", host));
                                info.push(("Port", port));
                                info.push(("Service", service));
                            }
                            None => info.push(("Connect string", p.connect_string.clone())),
                        },
                        DbKind::Sqlite => info.push(("File", p.connect_string.clone())),
                    }
                }
                if let Some(s) = &c.session {
                    if !s.user.is_empty() {
                        info.push(("User", s.user.clone()));
                    }
                    if !s.version.is_empty() {
                        info.push(("Server Version", s.version.clone()));
                    }
                }
                OpenConn {
                    id: c.id,
                    label: c.label(),
                    kind,
                    access: c.access(),
                    info,
                }
            })
            .collect()
    }

    pub(super) fn open_conn(&self, id: u64) -> Option<OpenConn> {
        self.open_conns().into_iter().find(|c| c.id == id)
    }

    /// Run `job` on connection `conn_id`'s worker without touching its
    /// busy state (metadata reads for the wizard only).
    fn transfer_job(
        &mut self,
        conn_id: u64,
        job: Job,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<Response, String>, &mut Window, &mut Context<Self>)
            + 'static,
    ) {
        let Some(db) = self.conn_by_id(conn_id).and_then(|c| c.db.clone()) else {
            return;
        };
        let rx = db.request(job);
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let reply = match reply {
                Ok(Response::Failed(m)) | Ok(Response::ConnectionLost(m)) => Err(m),
                Ok(other) => Ok(other),
                Err(_) => Err("The database worker stopped.".to_string()),
            };
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| then(this, reply, window, cx))
            });
        })
        .detach();
    }

    /// Open the wizard. `table`, from a table's context menu, preselects
    /// the active connection, its schema and that table.
    pub(super) fn open_data_transfer(
        &mut self,
        table: Option<ObjectRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.open_conns();
        if open.is_empty() {
            self.conn.status = "Connect to a database first to use Data Transfer.".to_string();
            cx.notify();
            return;
        }
        let source = open
            .iter()
            .find(|c| c.id == self.conn.id)
            .map_or(open[0].id, |c| c.id);
        let source_kind = open.iter().find(|c| c.id == source).map(|c| c.kind);
        let schema = table.as_ref().map(|t| t.owner.clone()).or_else(|| {
            self.conn_by_id(source)
                .and_then(|c| c.current_schema.clone())
        });
        let wizard = TransferWizard {
            step: Step::Endpoints,
            panel: Panel::None,
            source: Some(source),
            source_schema: schema,
            source_schemas: Vec::new(),
            target_mode: TargetMode::Connection,
            target: open.iter().map(|c| c.id).find(|id| *id != source),
            target_schema: None,
            target_schemas: Vec::new(),
            file_input: input("Path of the .sql file", "", window, cx),
            file_schema: input("Schema name used in the script", "", window, cx),
            file_from_dialog: false,
            script_kind: source_kind.unwrap_or(DbKind::Oracle),
            file_csv: false,
            tables: Vec::new(),
            views: BTreeSet::new(),
            tables_state: TablesState::Idle,
            selected: table.iter().map(|t| t.name.clone()).collect(),
            search: input("Search tables", "", window, cx),
            focus: table.as_ref().map(|t| t.name.clone()),
            configs: HashMap::new(),
            options: Default::default(),
            generator: None,
            profile_name: input("Profile name", "", window, cx),
            profiles: Vec::new(),
            confirm_input: input("", "", window, cx),
            error: None,
            run: None,
            generation: 0,
        };
        self.transfer = Some(wizard);
        self.load_transfer_schemas(true, cx);
        if self.transfer.as_ref().and_then(|w| w.target).is_some() {
            self.load_transfer_schemas(false, cx);
        }
        cx.notify();
    }

    pub(super) fn close_data_transfer(&mut self, cx: &mut Context<Self>) {
        if self
            .transfer
            .as_ref()
            .and_then(|w| w.run.as_ref())
            .is_some_and(|r| r.running)
        {
            return;
        }
        self.transfer = None;
        cx.notify();
    }

    fn wizard(&mut self) -> Option<&mut TransferWizard> {
        self.transfer.as_mut()
    }

    // ---- endpoints -------------------------------------------------

    /// Fetch the schemas of the source or target connection.
    fn load_transfer_schemas(&mut self, source_side: bool, cx: &mut Context<Self>) {
        let Some(w) = self.transfer.as_ref() else {
            return;
        };
        let id = if source_side { w.source } else { w.target };
        let Some(id) = id else {
            return;
        };
        let token = w.generation;
        self.transfer_job(id, Job::ListSchemas, cx, move |this, reply, _, cx| {
            let default = this.conn_by_id(id).and_then(|c| c.current_schema.clone());
            let user = this
                .conn_by_id(id)
                .and_then(|c| c.session.as_ref())
                .map(|s| s.user.to_uppercase());
            let Some(w) = this.wizard() else {
                return;
            };
            if w.generation != token {
                return;
            }
            let (list, chosen) = if source_side {
                (&mut w.source_schemas, &mut w.source_schema)
            } else {
                (&mut w.target_schemas, &mut w.target_schema)
            };
            match reply {
                Ok(Response::Schemas(schemas)) => {
                    *list = schemas;
                    let valid = chosen.as_ref().is_some_and(|s| list.contains(s));
                    if !valid {
                        *chosen = default
                            .filter(|d| list.contains(d))
                            .or_else(|| user.filter(|u| list.contains(u)))
                            .or_else(|| list.first().cloned());
                    }
                }
                Ok(_) => {}
                Err(e) => w.error = Some(format!("Could not list schemas: {e}")),
            }
            if source_side {
                this.load_transfer_tables(cx);
            }
            cx.notify();
        });
    }

    fn load_transfer_tables(&mut self, cx: &mut Context<Self>) {
        let Some(w) = self.wizard() else {
            return;
        };
        let (Some(id), Some(schema)) = (w.source, w.source_schema.clone()) else {
            return;
        };
        w.tables_state = TablesState::Loading;
        w.tables.clear();
        let token = w.generation;
        self.transfer_job(
            id,
            Job::ListObjects { owner: schema },
            cx,
            move |this, reply, _, cx| {
                let Some(w) = this.wizard() else {
                    return;
                };
                if w.generation != token {
                    return;
                }
                match reply {
                    Ok(Response::Objects(objects)) => {
                        let views: BTreeSet<String> = objects
                            .iter()
                            .filter(|o| o.kind == ObjectKind::View)
                            .map(|o| o.name.clone())
                            .collect();
                        let mut tables: Vec<String> = objects
                            .into_iter()
                            .filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View))
                            .map(|o| o.name)
                            .collect();
                        tables.sort();
                        w.views = views;
                        w.selected.retain(|t| tables.contains(t));
                        w.configs.retain(|t, _| tables.contains(t));
                        w.tables = tables;
                        w.tables_state = TablesState::Ready;
                    }
                    Ok(_) => w.tables_state = TablesState::Idle,
                    Err(e) => w.tables_state = TablesState::Failed(e),
                }
                cx.notify();
            },
        );
    }

    fn reset_selection(w: &mut TransferWizard) {
        w.generation += 1;
        w.selected.clear();
        w.configs.clear();
        w.focus = None;
        w.tables.clear();
        w.views.clear();
        w.tables_state = TablesState::Idle;
        w.error = None;
    }

    pub(super) fn transfer_set_source(&mut self, id: u64, cx: &mut Context<Self>) {
        let kind = self.open_conn(id).map(|c| c.kind);
        let Some(w) = self.wizard() else {
            return;
        };
        w.source = Some(id);
        w.source_schema = None;
        w.source_schemas.clear();
        if let Some(kind) = kind {
            w.script_kind = kind;
        }
        Self::reset_selection(w);
        self.load_transfer_schemas(true, cx);
        cx.notify();
    }

    pub(super) fn transfer_set_source_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        let Some(w) = self.wizard() else {
            return;
        };
        w.source_schema = Some(schema);
        Self::reset_selection(w);
        self.load_transfer_tables(cx);
        cx.notify();
    }

    pub(super) fn transfer_set_target(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(w) = self.wizard() else {
            return;
        };
        w.target = Some(id);
        w.target_schema = None;
        w.target_schemas.clear();
        w.error = None;
        self.load_transfer_schemas(false, cx);
        cx.notify();
    }

    pub(super) fn transfer_set_target_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard() {
            w.target_schema = Some(schema);
            w.error = None;
        }
        cx.notify();
    }

    pub(super) fn transfer_set_mode(&mut self, mode: TargetMode, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard() {
            w.target_mode = mode;
            w.error = None;
        }
        cx.notify();
    }

    pub(super) fn transfer_swap(&mut self, cx: &mut Context<Self>) {
        let Some(w) = self.wizard() else {
            return;
        };
        let (Some(s), Some(t)) = (w.source, w.target) else {
            return;
        };
        let (ss, ts) = (w.source_schema.take(), w.target_schema.take());
        w.source = Some(t);
        w.target = Some(s);
        w.source_schema = ts;
        w.target_schema = ss;
        std::mem::swap(&mut w.source_schemas, &mut w.target_schemas);
        Self::reset_selection(w);
        self.load_transfer_tables(cx);
        cx.notify();
    }

    pub(super) fn transfer_browse_file(&mut self, cx: &mut Context<Self>) {
        let dir = store::config_dir();
        let csv = self.transfer.as_ref().is_some_and(|w| w.file_csv);
        let folder = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let file = cx.prompt_for_new_path(&dir, Some("transfer.sql"));
        let prompt = async move {
            if csv {
                folder
                    .await
                    .map(|r| r.map(|p| p.and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)))))
            } else {
                file.await
            }
        };
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    if let Some(w) = this.wizard() {
                        set_text(&w.file_input.clone(), &path.to_string_lossy(), window, cx);
                        if let Some(w) = this.wizard() {
                            w.file_from_dialog = true;
                        }
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }

    // ---- steps -------------------------------------------------------

    fn endpoints_error(&self, cx: &App) -> Option<String> {
        let w = self.transfer.as_ref()?;
        let (Some(source), Some(_)) = (w.source, w.source_schema.as_ref()) else {
            return Some("Choose the source connection and schema.".to_string());
        };
        match w.target_mode {
            TargetMode::Connection => {
                let (Some(target), Some(schema)) = (w.target, w.target_schema.as_ref()) else {
                    return Some("Choose the target connection and schema.".to_string());
                };
                if target == source && Some(schema) == w.source_schema.as_ref() {
                    return Some("Source and target are the same schema.".to_string());
                }
                if self.open_conn(target)?.access.read_only {
                    return Some(
                        "The target connection is Read Only; a transfer would write to it."
                            .to_string(),
                    );
                }
                None
            }
            TargetMode::File => {
                let path = w.file_input.read(cx).value().trim().to_string();
                if w.file_csv {
                    if path.is_empty() {
                        return Some("Enter the folder that receives the CSV files.".to_string());
                    }
                    if !std::path::Path::new(&path).is_dir() {
                        return Some("That folder does not exist.".to_string());
                    }
                    return None;
                }
                if path.is_empty() {
                    return Some("Enter the path of the SQL file to write.".to_string());
                }
                if std::path::Path::new(&path).exists() && !w.file_from_dialog {
                    return Some(
                        "That file already exists. Use Browse to confirm overwriting it."
                            .to_string(),
                    );
                }
                None
            }
        }
    }

    pub(super) fn transfer_next(&mut self, cx: &mut Context<Self>) {
        let step = self.transfer.as_ref().map(|w| w.step);
        match step {
            Some(Step::Endpoints) => {
                let error = self.endpoints_error(cx);
                let Some(w) = self.wizard() else {
                    return;
                };
                w.error = error;
                if w.error.is_none() {
                    w.step = Step::Objects;
                }
            }
            Some(Step::Objects) => {
                let spec = self.build_transfer_spec(cx);
                let Some(w) = self.wizard() else {
                    return;
                };
                match spec {
                    Ok(_) => {
                        w.error = None;
                        w.step = Step::Run;
                    }
                    Err(e) => w.error = Some(e),
                }
            }
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn transfer_back(&mut self, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard() {
            if w.run.as_ref().is_some_and(|r| r.running) {
                return;
            }
            w.error = None;
            w.panel = Panel::None;
            w.step = match w.step {
                Step::Run => {
                    w.run = None;
                    Step::Objects
                }
                _ => Step::Endpoints,
            };
        }
        cx.notify();
    }

    pub(super) fn transfer_set_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard() {
            w.panel = panel;
        }
        cx.notify();
    }

    // ---- objects ------------------------------------------------------

    pub(super) fn transfer_toggle_table(&mut self, table: &str, on: bool, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard() {
            if on {
                w.selected.insert(table.to_string());
            } else {
                w.selected.remove(table);
            }
        }
        cx.notify();
    }

    pub(super) fn transfer_toggle_all(
        &mut self,
        tables: Vec<String>,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(w) = self.wizard() {
            for t in tables {
                if on {
                    w.selected.insert(t);
                } else {
                    w.selected.remove(&t);
                }
            }
        }
        cx.notify();
    }

    fn ensure_config(&mut self, table: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .transfer
            .as_ref()
            .is_none_or(|w| w.configs.contains_key(table))
        {
            return;
        }
        let config = TableConfig {
            advanced: false,
            target: input("Target table name", table, window, cx),
            custom_fields: false,
            fields: None,
            fields_loading: false,
            fields_error: None,
            pending_fields: None,
            custom_rows: false,
            batch: input("Rows per batch", "1000", window, cx),
            recordsets: Vec::new(),
        };
        if let Some(w) = self.wizard() {
            w.configs.insert(table.to_string(), config);
        }
    }

    pub(super) fn transfer_focus_table(
        &mut self,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_config(table, window, cx);
        if let Some(w) = self.wizard() {
            w.focus = Some(table.to_string());
            w.panel = Panel::None;
        }
        cx.notify();
    }

    pub(super) fn transfer_set_advanced(
        &mut self,
        table: &str,
        advanced: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_config(table, window, cx);
        if let Some(c) = self.wizard().and_then(|w| w.configs.get_mut(table)) {
            c.advanced = advanced;
        }
        if advanced {
            self.load_table_fields(table, cx);
        }
        cx.notify();
    }

    /// Read the source columns of `table` for the field picker.
    pub(super) fn load_table_fields(&mut self, table: &str, cx: &mut Context<Self>) {
        let Some(w) = self.wizard() else {
            return;
        };
        let (Some(id), Some(schema)) = (w.source, w.source_schema.clone()) else {
            return;
        };
        let token = w.generation;
        let Some(c) = w.configs.get_mut(table) else {
            return;
        };
        if c.fields.is_some() || c.fields_loading {
            return;
        }
        c.fields_loading = true;
        c.fields_error = None;
        let name = table.to_string();
        self.transfer_job(
            id,
            Job::Describe {
                owner: schema,
                name: name.clone(),
            },
            cx,
            move |this, reply, window, cx| {
                let defs: Result<Vec<ColumnDef>, String> = match reply {
                    Ok(Response::Described(defs)) => Ok(defs),
                    Ok(_) => Err("unexpected reply".to_string()),
                    Err(e) => Err(e),
                };
                let built = defs.as_ref().ok().map(|defs| {
                    defs.iter()
                        .map(|d| PickedField {
                            source: d.name.clone(),
                            data_type: d.data_type.clone(),
                            on: true,
                            target: input("Target field", &d.name, window, cx),
                        })
                        .collect::<Vec<_>>()
                });
                let Some(w) = this.wizard() else {
                    return;
                };
                if w.generation != token {
                    return;
                }
                if let Some(c) = w.configs.get_mut(&name) {
                    c.fields_loading = false;
                    match (built, defs) {
                        (Some(mut picked), _) => {
                            if let Some(pending) = c.pending_fields.take() {
                                for p in picked.iter_mut() {
                                    p.on = pending.iter().any(|m| m.source == p.source);
                                }
                                let renames: Vec<(Entity<InputState>, String)> = picked
                                    .iter()
                                    .filter_map(|p| {
                                        pending
                                            .iter()
                                            .find(|m| m.source == p.source)
                                            .map(|m| (p.target.clone(), m.target.clone()))
                                    })
                                    .collect();
                                c.fields = Some(picked);
                                for (entity, text) in renames {
                                    set_text(&entity, &text, window, cx);
                                }
                            } else {
                                c.fields = Some(picked);
                            }
                        }
                        (None, Err(e)) => c.fields_error = Some(e),
                        _ => {}
                    }
                }
                cx.notify();
            },
        );
    }

    pub(super) fn transfer_set_custom_fields(
        &mut self,
        table: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(c) = self.wizard().and_then(|w| w.configs.get_mut(table)) {
            c.custom_fields = on;
        }
        if on {
            self.load_table_fields(table, cx);
        }
        cx.notify();
    }

    pub(super) fn transfer_toggle_field(
        &mut self,
        table: &str,
        ix: usize,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(f) = self
            .wizard()
            .and_then(|w| w.configs.get_mut(table))
            .and_then(|c| c.fields.as_mut())
            .and_then(|f| f.get_mut(ix))
        {
            f.on = on;
        }
        cx.notify();
    }

    pub(super) fn transfer_all_fields(&mut self, table: &str, on: bool, cx: &mut Context<Self>) {
        if let Some(fields) = self
            .wizard()
            .and_then(|w| w.configs.get_mut(table))
            .and_then(|c| c.fields.as_mut())
        {
            for f in fields {
                f.on = on;
            }
        }
        cx.notify();
    }

    pub(super) fn transfer_set_custom_rows(
        &mut self,
        table: &str,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let empty = self
            .transfer
            .as_ref()
            .and_then(|w| w.configs.get(table))
            .is_some_and(|c| c.recordsets.is_empty());
        let first = empty.then(|| input("e.g. \"ID\" >= 1 AND \"ID\" < 1000", "", window, cx));
        if let Some(c) = self.wizard().and_then(|w| w.configs.get_mut(table)) {
            c.custom_rows = on;
            if let Some(first) = first {
                c.recordsets.push(first);
            }
        }
        if on {
            self.load_table_fields(table, cx);
        }
        cx.notify();
    }

    pub(super) fn transfer_add_recordset(
        &mut self,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = input("e.g. \"ID\" >= 1 AND \"ID\" < 1000", "", window, cx);
        if let Some(c) = self.wizard().and_then(|w| w.configs.get_mut(table)) {
            c.recordsets.push(entity);
        }
        cx.notify();
    }

    pub(super) fn transfer_remove_recordset(
        &mut self,
        table: &str,
        ix: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(c) = self.wizard().and_then(|w| w.configs.get_mut(table)) {
            if ix < c.recordsets.len() {
                c.recordsets.remove(ix);
            }
        }
        cx.notify();
    }

    pub(super) fn transfer_open_generator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generator = Generator {
            field: None,
            kind: RangeKind::Integer,
            start: input("Start value", "", window, cx),
            end: input("End value", "", window, cx),
            count: input("Number of recordsets", "10", window, cx),
            error: None,
        };
        if let Some(w) = self.wizard() {
            w.generator = Some(generator);
            w.panel = Panel::Generator;
        }
        cx.notify();
    }

    /// Recordset Generator: append the generated conditions to `table`.
    pub(super) fn transfer_generate(
        &mut self,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source_kind = self
            .transfer
            .as_ref()
            .and_then(|w| w.source)
            .and_then(|id| self.open_conn(id))
            .map_or(DbKind::Oracle, |c| c.kind);
        let Some(w) = self.transfer.as_ref() else {
            return;
        };
        let Some(g) = w.generator.as_ref() else {
            return;
        };
        let result = (|| {
            let field = g.field.clone().ok_or("Choose the field to split on.")?;
            let count = g
                .count
                .read(cx)
                .value()
                .trim()
                .parse::<usize>()
                .map_err(|_| "The number of recordsets must be a whole number.".to_string())?;
            ranges::generate(
                source_kind,
                &field,
                g.kind,
                g.start.read(cx).value().trim(),
                g.end.read(cx).value().trim(),
                count,
            )
            .map_err(|e| e.to_string())
        })();
        match result {
            Ok(conditions) => {
                let inputs: Vec<_> = conditions
                    .iter()
                    .map(|c| input("", c, window, cx))
                    .collect();
                if let Some(c) = self.wizard().and_then(|w| w.configs.get_mut(table)) {
                    // A lone empty row is replaced; typed rows are kept.
                    c.recordsets
                        .retain(|r| !r.read(cx).value().trim().is_empty());
                    c.recordsets.extend(inputs);
                    c.custom_rows = true;
                }
                if let Some(w) = self.wizard() {
                    w.panel = Panel::None;
                    w.generator = None;
                }
            }
            Err(e) => {
                if let Some(g) = self.wizard().and_then(|w| w.generator.as_mut()) {
                    g.error = Some(e);
                }
            }
        }
        cx.notify();
    }

    // ---- spec / run ------------------------------------------------------

    pub(super) fn build_transfer_spec(&self, cx: &App) -> Result<TransferSpec, String> {
        let w = self.transfer.as_ref().ok_or("The wizard is closed.")?;
        let source_schema = w.source_schema.clone().ok_or("Choose the source schema.")?;
        let target_schema = match w.target_mode {
            TargetMode::Connection => w.target_schema.clone().ok_or("Choose the target schema.")?,
            TargetMode::File => {
                let typed = w.file_schema.read(cx).value().trim().to_string();
                if typed.is_empty() {
                    source_schema.clone()
                } else {
                    typed
                }
            }
        };
        let mut tables = Vec::new();
        for name in &w.selected {
            let mut spec = TableSpec::auto(name.clone());
            if let Some(c) = w.configs.get(name).filter(|c| c.advanced) {
                let target = c.target.read(cx).value().trim().to_string();
                if !target.is_empty() && target != *name {
                    spec.target = Some(target);
                }
                if c.custom_fields {
                    let picked = c
                        .fields
                        .as_ref()
                        .ok_or_else(|| format!("{name}: the fields are still loading."))?;
                    spec.fields = Fields::Custom(
                        picked
                            .iter()
                            .filter(|f| f.on)
                            .map(|f| FieldMap {
                                source: f.source.clone(),
                                target: f.target.read(cx).value().trim().to_string(),
                            })
                            .collect(),
                    );
                }
                let batch = {
                    let text = c.batch.read(cx).value().trim().to_string();
                    if text.is_empty() {
                        ora_core::transfer::spec::DEFAULT_BATCH
                    } else {
                        text.parse::<usize>()
                            .ok()
                            .filter(|n| *n >= 1)
                            .ok_or_else(|| {
                                format!("{name}: rows per batch must be a whole number.")
                            })?
                    }
                };
                spec.rows = if c.custom_rows {
                    Rows::Custom {
                        batch,
                        conditions: c
                            .recordsets
                            .iter()
                            .map(|r| r.read(cx).value().trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect(),
                    }
                } else {
                    Rows::All { batch }
                };
            }
            tables.push(spec);
        }
        let spec = TransferSpec {
            source_schema,
            target_schema,
            tables,
            options: w.options.clone(),
        };
        spec.validate()?;
        Ok(spec)
    }

    /// The target counts as production: the flag or the PROD environment.
    pub(super) fn target_is_production(&self) -> bool {
        self.transfer
            .as_ref()
            .filter(|w| w.target_mode == TargetMode::Connection)
            .and_then(|w| w.target)
            .and_then(|id| self.open_conn(id))
            .is_some_and(|c| {
                c.access.environment == ConnectionEnvironment::Production
                    || c.access.protect_production
            })
    }

    /// Start button: ask for confirmation first when the run drops or
    /// empties target tables or writes to a production target.
    pub(super) fn transfer_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = match self.build_transfer_spec(cx) {
            Ok(s) => s,
            Err(e) => {
                if let Some(w) = self.wizard() {
                    w.error = Some(e);
                }
                cx.notify();
                return;
            }
        };
        let to_database = self
            .transfer
            .as_ref()
            .is_some_and(|w| w.target_mode == TargetMode::Connection);
        let needs_confirm =
            to_database && (spec.options.destroys_target_data() || self.target_is_production());
        let confirmed = self
            .transfer
            .as_ref()
            .is_some_and(|w| w.panel == Panel::Confirm);
        if needs_confirm && !confirmed {
            if let Some(w) = self.wizard() {
                w.panel = Panel::Confirm;
                w.error = None;
            }
            cx.notify();
            return;
        }
        if needs_confirm && spec.options.destroys_target_data() && self.target_is_production() {
            let expected = spec.target_schema.clone();
            let typed = self
                .transfer
                .as_ref()
                .map(|w| w.confirm_input.read(cx).value().trim().to_string())
                .unwrap_or_default();
            if typed != expected {
                if let Some(w) = self.wizard() {
                    w.error = Some(format!("Type {expected} to confirm."));
                }
                cx.notify();
                return;
            }
        }
        self.run_transfer_thread(spec, needs_confirm, window, cx);
    }

    fn run_transfer_thread(
        &mut self,
        spec: TransferSpec,
        production_confirmed: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.transfer.as_ref() else {
            return;
        };
        let source = w.source.and_then(|id| {
            let kind = self.open_conn(id)?.kind;
            Some(WorkerIo::new(self.conn_by_id(id)?.db.clone()?, kind, false))
        });
        let Some(source) = source else {
            return self.transfer_fail("The source connection is no longer open.", cx);
        };
        enum Sink {
            Db(WorkerIo, u64),
            File(PathBuf, DbKind),
            Csv(PathBuf, bool),
        }
        let sink = match w.target_mode {
            TargetMode::Connection => {
                let t = w.target.and_then(|id| {
                    let open = self.open_conn(id)?;
                    if open.access.read_only {
                        return None;
                    }
                    Some(Sink::Db(
                        WorkerIo::new(
                            self.conn_by_id(id)?.db.clone()?,
                            open.kind,
                            production_confirmed,
                        ),
                        id,
                    ))
                });
                match t {
                    Some(t) => t,
                    None => {
                        return self
                            .transfer_fail("The target connection is closed or Read Only.", cx)
                    }
                }
            }
            TargetMode::File => {
                let path = PathBuf::from(w.file_input.read(cx).value().trim());
                if w.file_csv {
                    Sink::Csv(path, spec.options.drop_target && spec.options.create_tables)
                } else {
                    Sink::File(path, w.script_kind)
                }
            }
        };
        let objects = spec.tables.len();
        let (tx, rx) = mpsc::channel::<RunMsg>();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let target_conn = match &sink {
            Sink::Db(_, id) => Some(*id),
            Sink::File(..) | Sink::Csv(..) => None,
        };
        let target_schema = spec.target_schema.clone();
        std::thread::spawn(move || {
            let mut source = source;
            let events = tx.clone();
            let mut emit = move |e: Event| {
                let _ = events.send(RunMsg::Event(e));
            };
            let summary = match sink {
                Sink::Db(mut dst, _) => {
                    run_transfer(&mut source, &mut dst, &spec, &thread_cancel, &mut emit)
                }
                Sink::Csv(dir, overwrite) => {
                    let mut dst = ora_core::transfer::CsvIo::new(dir, overwrite);
                    let summary =
                        run_transfer(&mut source, &mut dst, &spec, &thread_cancel, &mut emit);
                    if let Err(e) = dst.finish() {
                        emit(Event::Log(LogLevel::Error, e.to_string()));
                    }
                    summary
                }
                Sink::File(path, kind) => match std::fs::File::create(&path) {
                    Ok(file) => {
                        let mut dst = ScriptIo::new(kind, std::io::BufWriter::new(file));
                        let summary =
                            run_transfer(&mut source, &mut dst, &spec, &thread_cancel, &mut emit);
                        use std::io::Write as _;
                        if let Err(e) = dst.into_inner().flush() {
                            emit(Event::Log(
                                LogLevel::Error,
                                format!("Writing the script failed: {e}"),
                            ));
                        }
                        summary
                    }
                    Err(e) => {
                        emit(Event::Log(
                            LogLevel::Error,
                            format!("Cannot create the file: {e}"),
                        ));
                        TransferSummary {
                            invalid: Some(format!("Cannot create the file: {e}")),
                            ..Default::default()
                        }
                    }
                },
            };
            let _ = tx.send(RunMsg::Done(summary));
        });
        if let Some(w) = self.wizard() {
            w.panel = Panel::None;
            w.error = None;
            w.run = Some(RunState {
                log: Vec::new(),
                table: None,
                rows: 0,
                total_rows: 0,
                skipped: 0,
                failed: 0,
                objects,
                done_tables: 0,
                started: std::time::Instant::now(),
                elapsed: None,
                running: true,
                summary: None,
                cancel,
            });
        }
        cx.notify();
        let rx = Arc::new(Mutex::new(rx));
        cx.spawn(async move |view, cx| loop {
            let rx = rx.clone();
            let msg = cx
                .background_executor()
                .spawn(async move { rx.lock().ok().and_then(|rx| rx.recv().ok()) })
                .await;
            let Some(msg) = msg else {
                break;
            };
            let done = matches!(msg, RunMsg::Done(_));
            let alive = view.update(cx, |this, cx| {
                this.apply_run_msg(msg, target_conn, &target_schema, cx);
            });
            if alive.is_err() || done {
                break;
            }
        })
        .detach();
    }

    fn transfer_fail(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard() {
            w.error = Some(message.to_string());
        }
        cx.notify();
    }

    fn apply_run_msg(
        &mut self,
        msg: RunMsg,
        target_conn: Option<u64>,
        target_schema: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.transfer.as_mut().and_then(|w| w.run.as_mut()) else {
            return;
        };
        match msg {
            RunMsg::Event(Event::Log(level, text)) => {
                if run.log.len() < MAX_LOG_LINES {
                    run.log.push((level, text));
                }
            }
            RunMsg::Event(Event::TableStart {
                index,
                total,
                source,
            }) => {
                run.table = Some((index + 1, total, source));
            }
            RunMsg::Event(Event::Rows { done, .. }) => run.rows = done,
            RunMsg::Event(Event::TableEnd { outcome, .. }) => {
                run.total_rows += outcome.rows;
                run.skipped += outcome.skipped;
                run.failed += usize::from(outcome.error.is_some());
                run.done_tables += 1;
                run.rows = 0;
            }
            RunMsg::Done(summary) => {
                run.running = false;
                run.elapsed = Some(run.started.elapsed());
                run.summary = Some(summary);
                run.table = None;
                if let Some(id) = target_conn {
                    let same_schema = self
                        .conn_by_id(id)
                        .is_some_and(|c| c.current_schema.as_deref() == Some(target_schema));
                    if same_schema {
                        self.load_objects_for(id, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    pub(super) fn transfer_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = self.wizard().and_then(|w| w.run.as_mut()) {
            run.cancel.store(true, Ordering::Relaxed);
            run.log.push((
                LogLevel::Warn,
                "Stop requested: finishing the current batch…".into(),
            ));
        }
        cx.notify();
    }

    // ---- profiles ---------------------------------------------------------

    pub(super) fn transfer_save_profile(&mut self, cx: &mut Context<Self>) {
        let spec = self.build_transfer_spec(cx);
        let Some(w) = self.transfer.as_ref() else {
            return;
        };
        let name = w.profile_name.read(cx).value().trim().to_string();
        let result = (|| {
            if name.is_empty() {
                return Err("Enter a profile name.".to_string());
            }
            let source = w
                .source
                .and_then(|id| self.open_conn(id))
                .ok_or("The source connection is closed.")?;
            let target = match w.target_mode {
                TargetMode::Connection => TransferTarget::Connection(
                    w.target
                        .and_then(|id| self.open_conn(id))
                        .ok_or("Choose the target connection.")?
                        .label,
                ),
                TargetMode::File => {
                    let path = w.file_input.read(cx).value().trim().to_string();
                    if w.file_csv {
                        TransferTarget::CsvDir(path)
                    } else {
                        TransferTarget::SqlFile(path)
                    }
                }
            };
            let profile = TransferProfile {
                name: name.clone(),
                source_connection: source.label,
                target,
                spec: spec?,
            };
            save_profile_in(&store::config_dir(), &profile)
                .map_err(|e| format!("Could not save the profile: {e}"))
        })();
        if let Some(w) = self.wizard() {
            match result {
                Ok(()) => {
                    w.error = None;
                    w.panel = Panel::None;
                }
                Err(e) => w.error = Some(e),
            }
        }
        cx.notify();
    }

    pub(super) fn transfer_open_load_profile(&mut self, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard() {
            w.profiles = load_profiles_in(&store::config_dir());
            w.panel = Panel::LoadProfile;
        }
        cx.notify();
    }

    pub(super) fn transfer_load_profile(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .transfer
            .as_ref()
            .and_then(|w| w.profiles.get(ix).cloned())
        else {
            return;
        };
        let open = self.open_conns();
        let find = |name: &str| open.iter().find(|c| c.label == name).map(|c| c.id);
        let Some(source) = find(&profile.source_connection) else {
            return self.transfer_fail(
                &format!(
                    "Connect to {} first (the profile's source).",
                    profile.source_connection
                ),
                cx,
            );
        };
        let target = match &profile.target {
            TransferTarget::Connection(name) => match find(name) {
                Some(id) => Some(id),
                None => {
                    return self.transfer_fail(
                        &format!("Connect to {name} first (the profile's target)."),
                        cx,
                    )
                }
            },
            TransferTarget::SqlFile(_) | TransferTarget::CsvDir(_) => None,
        };
        let spec = profile.spec.clone();
        let Some(w) = self.wizard() else {
            return;
        };
        Self::reset_selection(w);
        w.source = Some(source);
        w.source_schema = Some(spec.source_schema.clone());
        w.source_schemas.clear();
        w.options = spec.options.clone();
        match &profile.target {
            TransferTarget::Connection(_) => {
                w.target_mode = TargetMode::Connection;
                w.target = target;
                w.target_schema = Some(spec.target_schema.clone());
                w.target_schemas.clear();
            }
            TransferTarget::SqlFile(path) | TransferTarget::CsvDir(path) => {
                w.target_mode = TargetMode::File;
                w.file_csv = matches!(profile.target, TransferTarget::CsvDir(_));
                let (file, schema) = (w.file_input.clone(), w.file_schema.clone());
                w.file_from_dialog = false;
                set_text(&file, path, window, cx);
                set_text(&schema, &spec.target_schema, window, cx);
            }
        }
        w.panel = Panel::None;
        let names: Vec<String> = spec.tables.iter().map(|t| t.source.clone()).collect();
        w.selected = names.iter().cloned().collect();
        for t in &spec.tables {
            let advanced = t.target.is_some()
                || t.fields != Fields::All
                || !matches!(t.rows, Rows::All { batch } if batch == ora_core::transfer::spec::DEFAULT_BATCH);
            if !advanced {
                continue;
            }
            self.ensure_config(&t.source, window, cx);
            let (target, batch) = (t.target.clone(), t.rows.batch().to_string());
            let recordsets: Vec<Entity<InputState>> = match &t.rows {
                Rows::Custom { conditions, .. } => conditions
                    .iter()
                    .map(|c| input("", c, window, cx))
                    .collect(),
                Rows::All { .. } => Vec::new(),
            };
            let Some(c) = self.wizard().and_then(|w| w.configs.get_mut(&t.source)) else {
                continue;
            };
            c.advanced = true;
            let (target_input, batch_input) = (c.target.clone(), c.batch.clone());
            if let Fields::Custom(map) = &t.fields {
                c.custom_fields = true;
                c.pending_fields = Some(map.clone());
            }
            if let Rows::Custom { .. } = &t.rows {
                c.custom_rows = true;
                c.recordsets = recordsets;
            }
            if let Some(target) = target {
                set_text(&target_input, &target, window, cx);
            }
            set_text(&batch_input, &batch, window, cx);
            self.load_table_fields(&t.source, cx);
        }
        self.load_transfer_schemas(true, cx);
        if target.is_some() {
            self.load_transfer_schemas(false, cx);
        }
        // The loaded schema lists must not replace the profile's choice.
        if let Some(w) = self.wizard() {
            w.focus = names.first().cloned();
            w.step = Step::Endpoints;
        }
        cx.notify();
    }
}
