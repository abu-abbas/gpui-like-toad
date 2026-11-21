//! Right-click operations on explorer objects: New/Rename/Duplicate/Delete/
//! Empty/Truncate Table, Compile, Execute Function and Reset Sequence.
//!
//! The dialogs only collect input. The SQL comes from [`ora_core::ddl`]
//! (identifiers quoted, column types checked) and runs on the worker like
//! any write, so Read Only and the production confirmation apply there too.
//! The UI refuses early on Read Only connections, and the destructive
//! dialogs of a PROD connection ask for the object's name to be typed.

use ora_core::ddl::{self, CopyMode, NewColumn, NewTable, ObjectOp, RoutineArg, TableKind};
use ora_core::recycle;

use super::*;

/// What an object-menu entry does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObjectAction {
    Rename,
    Duplicate,
    Drop,
    Empty,
    Truncate,
    ResetSequence,
    Compile {
        debug: bool,
    },
    Execute,
    Trigger {
        enable: bool,
    },
    ConstraintState {
        enable: bool,
    },
    DropConstraint,
    /// Recycle Bin: `FLASHBACK TABLE ... TO BEFORE DROP`.
    Flashback,
    /// Recycle Bin: `PURGE TABLE|INDEX` of one object.
    PurgeObject,
    /// Recycle Bin: `PURGE RECYCLEBIN`.
    PurgeAll,
    /// Recycle Bin: `PURGE DBA_RECYCLEBIN` (needs SYSDBA).
    PurgeAllUsers,
}

impl ObjectAction {
    /// Removes data or objects: the dialog names the object and, on a
    /// PROD connection, wants its name typed.
    fn destructive(self) -> bool {
        matches!(
            self,
            Self::Drop
                | Self::Empty
                | Self::Truncate
                | Self::ResetSequence
                | Self::DropConstraint
                | Self::PurgeObject
                | Self::PurgeAll
                | Self::PurgeAllUsers
        )
    }

    fn what(self) -> &'static str {
        match self {
            Self::Rename => "cannot rename tables",
            Self::Duplicate => "cannot duplicate tables",
            Self::Drop => "cannot delete objects",
            Self::Empty => "cannot empty tables",
            Self::Truncate => "cannot truncate tables",
            Self::ResetSequence => "cannot reset sequences",
            Self::Compile { .. } => "cannot compile objects",
            Self::Execute => "cannot execute functions",
            Self::Trigger { .. } => "cannot change triggers",
            Self::ConstraintState { .. } | Self::DropConstraint => "cannot change constraints",
            Self::Flashback => "cannot flash back tables",
            Self::PurgeObject | Self::PurgeAll | Self::PurgeAllUsers => {
                "cannot purge the recycle bin"
            }
        }
    }
}

/// One column row of the New Table dialog.
struct ColumnRow {
    name: Entity<InputState>,
    data_type: Entity<InputState>,
    not_null: bool,
    primary_key: bool,
}

struct NewTableForm {
    owner: String,
    kind: TableKind,
    name: Entity<InputState>,
    directory: Entity<InputState>,
    location: Entity<InputState>,
    columns: Vec<ColumnRow>,
}

enum DialogKind {
    NewTable(NewTableForm),
    Object {
        object: ObjectRef,
        action: ObjectAction,
        /// Rename/Duplicate: the new name. Reset: the value. Execute: one
        /// input per parameter.
        inputs: Vec<Entity<InputState>>,
        args: Vec<RoutineArg>,
        copy_rows: bool,
    },
}

/// The open dialog, if any.
pub(super) struct ObjectDialog {
    kind: DialogKind,
    /// PROD connection, destructive action: type this to enable the button.
    confirm: Option<(String, Entity<InputState>)>,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

fn text_input(
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

fn column_row(
    name: &str,
    data_type: &str,
    window: &mut Window,
    cx: &mut Context<Workbench>,
) -> ColumnRow {
    ColumnRow {
        name: text_input("Column name", name, window, cx),
        data_type: text_input("Type, e.g. VARCHAR2(100)", data_type, window, cx),
        not_null: false,
        primary_key: false,
    }
}

impl Workbench {
    fn object_op_ready(&mut self, action_what: &str, cx: &mut Context<Self>) -> bool {
        if self.conn.session.is_none() {
            return false;
        }
        if self.conn.busy {
            self.conn.status = "An operation is already running.".to_string();
            cx.notify();
            return false;
        }
        !self.blocked_by_read_only(action_what, cx)
    }

    /// Explorer menu entry on `object`.
    pub(super) fn object_action(
        &mut self,
        object: ObjectRef,
        action: ObjectAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.object_op_ready(action.what(), cx) {
            return;
        }
        let name = object.name.clone();
        let mut inputs = Vec::new();
        let mut subs = Vec::new();
        match action {
            ObjectAction::Compile { debug } => {
                if ddl::compile_sql(
                    self.active_dialect(),
                    &object.owner,
                    &object.name,
                    &object.kind,
                    debug,
                )
                .is_none()
                {
                    return;
                }
                self.conn.status = format!("Compiling {}.{}…", object.owner, object.name);
                let op = ObjectOp::Compile {
                    owner: object.owner,
                    name: object.name,
                    kind: object.kind,
                    debug,
                };
                self.run_object_op(op, false, false, cx);
                return;
            }
            ObjectAction::Trigger { enable } => {
                let sql = ddl::trigger_state_sql(
                    self.active_dialect(),
                    &object.owner,
                    &object.name,
                    enable,
                );
                let word = if enable { "Enabled" } else { "Disabled" };
                self.conn.status = format!("{word} {}…", object.name);
                let summary = format!("{word} trigger {}.{}.", object.owner, object.name);
                self.run_object_op(
                    ObjectOp::Statements {
                        sql: vec![sql],
                        summary,
                    },
                    false,
                    false,
                    cx,
                );
                return;
            }
            ObjectAction::ConstraintState { enable } => {
                let Some(table) = object.parent.clone() else {
                    return;
                };
                let sql = ddl::constraint_state_sql(
                    self.active_dialect(),
                    &object.owner,
                    &table,
                    &object.name,
                    enable,
                );
                let word = if enable { "Enabled" } else { "Disabled" };
                self.conn.status = format!("{word} {}…", object.name);
                let summary = format!("{word} constraint {} on {table}.", object.name);
                self.run_object_op(
                    ObjectOp::Statements {
                        sql: vec![sql],
                        summary,
                    },
                    true,
                    false,
                    cx,
                );
                return;
            }
            ObjectAction::Execute => {
                self.conn.status = format!("Reading parameters of {}…", object.name);
                self.spawn_db(
                    Job::RoutineSignature {
                        owner: object.owner.clone(),
                        name: object.name.clone(),
                    },
                    PendingOp::RoutineSignature(object),
                    cx,
                );
                return;
            }
            ObjectAction::Rename => inputs.push(text_input("New table name", &name, window, cx)),
            ObjectAction::Duplicate => inputs.push(text_input(
                "Name of the copy",
                &format!("{name}_COPY"),
                window,
                cx,
            )),
            ObjectAction::ResetSequence => inputs.push(text_input("Restart at", "1", window, cx)),
            ObjectAction::Flashback => inputs.push(text_input(
                "Rename to (optional, if the name is taken again)",
                "",
                window,
                cx,
            )),
            ObjectAction::Drop
            | ObjectAction::Empty
            | ObjectAction::Truncate
            | ObjectAction::DropConstraint
            | ObjectAction::PurgeObject
            | ObjectAction::PurgeAll
            | ObjectAction::PurgeAllUsers => {}
        }
        for input in &inputs {
            subs.push(
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit_object_dialog(window, cx);
                    }
                }),
            );
        }
        let mut confirm = self.prod_confirm(action, &name, window, cx);
        // Emptying every user's recycle bin always asks for the keyword,
        // not only on PROD.
        if action == ObjectAction::PurgeAllUsers && confirm.is_none() {
            confirm = Some((
                name.clone(),
                text_input(&format!("Type {name}"), "", window, cx),
            ));
        }
        let first = inputs.first().cloned();
        self.object_dialog = Some(ObjectDialog {
            kind: DialogKind::Object {
                object,
                action,
                inputs,
                args: Vec::new(),
                copy_rows: true,
            },
            confirm,
            error: None,
            _subs: subs,
        });
        self.focus_dialog_input(first, window, cx);
        cx.notify();
    }

    /// Whole-bin purge from the Recycle Bin menu: there is no object to
    /// point at, so the dialog gets a stand-in named after the statement.
    pub(super) fn recycle_bin_action(
        &mut self,
        action: ObjectAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self.conn.current_schema.clone() else {
            return;
        };
        let name = if action == ObjectAction::PurgeAllUsers {
            "DBA_RECYCLEBIN"
        } else {
            "RECYCLEBIN"
        };
        let object = ObjectRef {
            owner,
            name: name.to_string(),
            kind: ObjectKind::RecycleBin,
            parent: None,
            detail: None,
        };
        self.object_action(object, action, window, cx);
    }

    /// Toolbar button: recompile every INVALID object of the schema.
    pub(super) fn compile_invalid(&mut self, cx: &mut Context<Self>) {
        if !self.object_op_ready("cannot compile objects", cx) {
            return;
        }
        let Some(owner) = self.conn.current_schema.clone() else {
            return;
        };
        self.conn.status = format!("Compiling invalid objects of {owner}…");
        self.run_object_op(ObjectOp::CompileInvalid { owner }, true, false, cx);
    }

    fn prod_confirm(
        &self,
        action: ObjectAction,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(String, Entity<InputState>)> {
        (action.destructive()
            && self.conn.access().environment == ConnectionEnvironment::Production)
            .then(|| {
                (
                    name.to_string(),
                    text_input(&format!("Type {name}"), "", window, cx),
                )
            })
    }

    fn focus_dialog_input(
        &self,
        input: Option<Entity<InputState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = input {
            window.defer(cx, move |window, cx| {
                input.update(cx, |state, cx| state.focus(window, cx));
            });
        }
    }

    /// "New Table…" in the explorer's Tables category.
    pub(super) fn open_new_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.object_op_ready("cannot create tables", cx) {
            return;
        }
        let Some(owner) = self.conn.current_schema.clone() else {
            return;
        };
        let name = text_input("Table name", "", window, cx);
        let directory = text_input("Directory object, e.g. DATA_PUMP_DIR", "", window, cx);
        let location = text_input("File name, e.g. people.csv", "", window, cx);
        let mut id = column_row("ID", "NUMBER", window, cx);
        id.primary_key = true;
        let columns = vec![id, column_row("", "VARCHAR2(100)", window, cx)];
        let first = name.clone();
        self.object_dialog = Some(ObjectDialog {
            kind: DialogKind::NewTable(NewTableForm {
                owner,
                kind: TableKind::Normal,
                name,
                directory,
                location,
                columns,
            }),
            confirm: None,
            error: None,
            _subs: Vec::new(),
        });
        self.focus_dialog_input(Some(first), window, cx);
        cx.notify();
    }

    /// The worker sent the parameters: show the Execute dialog, or say why
    /// the function cannot be run from here.
    pub(super) fn on_routine_signature(
        &mut self,
        cid: u64,
        object: ObjectRef,
        args: Vec<RoutineArg>,
        returns: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = ddl::call_blocker(&args, &returns) {
            self.push_message_for(cid, format!("{}.{}: {reason}", object.owner, object.name));
            self.set_status_for(cid, "Cannot execute here (see Messages).".to_string());
            self.target_tab_for_mut(cid).results_tab = ResultsTab::Messages;
            return;
        }
        if cid != self.conn.id {
            return;
        }
        let inputs: Vec<_> = args
            .iter()
            .map(|a| text_input(&format!("{} ({})", a.name, a.data_type), "", window, cx))
            .collect();
        let first = inputs.first().cloned();
        let subs = inputs
            .iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit_object_dialog(window, cx);
                    }
                })
            })
            .collect();
        self.object_dialog = Some(ObjectDialog {
            kind: DialogKind::Object {
                object,
                action: ObjectAction::Execute,
                inputs,
                args,
                copy_rows: false,
            },
            confirm: None,
            error: None,
            _subs: subs,
        });
        self.focus_dialog_input(first, window, cx);
        cx.notify();
    }

    pub(super) fn close_object_dialog(&mut self, cx: &mut Context<Self>) {
        self.object_dialog = None;
        cx.notify();
    }

    fn run_object_op(
        &mut self,
        op: ObjectOp,
        reload: bool,
        uncommitted: bool,
        cx: &mut Context<Self>,
    ) {
        self.spawn_db(
            Job::ObjectOp(op),
            PendingOp::ObjectOp {
                reload,
                uncommitted,
            },
            cx,
        );
    }

    /// OK in the open dialog: build the operation, or show what is wrong.
    pub(super) fn submit_object_dialog(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let sqlite = self.active_kind() == DbKind::Sqlite;
        let dialect = self.active_dialect();
        let Some(dialog) = self.object_dialog.as_mut() else {
            return;
        };
        if let Some((expected, input)) = &dialog.confirm {
            if input.read(cx).value().trim() != expected {
                dialog.error = Some(format!("Type {expected} to confirm."));
                cx.notify();
                return;
            }
        }
        let built = match &dialog.kind {
            DialogKind::NewTable(form) => build_new_table(dialect, form, cx).map(|(sql, name)| {
                let summary = format!("Created table {}.{name}.", form.owner);
                (
                    ObjectOp::Statements {
                        sql: vec![sql],
                        summary,
                    },
                    true,
                    sqlite,
                    format!("Creating {name}…"),
                )
            }),
            DialogKind::Object {
                object,
                action,
                inputs,
                args,
                copy_rows,
            } => build_object_op(
                dialect, object, *action, inputs, args, *copy_rows, sqlite, cx,
            ),
        };
        match built {
            Ok((op, reload, uncommitted, status)) => {
                self.object_dialog = None;
                self.conn.status = status;
                self.run_object_op(op, reload, uncommitted, cx);
            }
            Err(message) => {
                dialog.error = Some(message);
                cx.notify();
            }
        }
    }

    /// The worker finished an object operation.
    pub(super) fn on_object_op_done(
        &mut self,
        cid: u64,
        lines: Vec<String>,
        reload: bool,
        cx: &mut Context<Self>,
    ) {
        let first = lines.first().cloned().unwrap_or_default();
        for line in lines {
            self.push_message_for(cid, line);
        }
        self.set_status_for(cid, first);
        self.target_tab_for_mut(cid).results_tab = ResultsTab::Messages;
        if reload {
            self.load_objects_for(cid, cx);
        }
    }
}

type Built = (ObjectOp, bool, bool, String);

fn build_new_table(
    dialect: &dyn Dialect,
    form: &NewTableForm,
    cx: &App,
) -> Result<(String, String), String> {
    let name = form.name.read(cx).value().to_string();
    let table = NewTable {
        owner: form.owner.clone(),
        name: name.clone(),
        kind: form.kind,
        columns: form
            .columns
            .iter()
            .map(|row| NewColumn {
                name: row.name.read(cx).value().trim().to_string(),
                data_type: row.data_type.read(cx).value().to_string(),
                not_null: row.not_null,
                primary_key: row.primary_key,
            })
            .collect(),
        directory: form.directory.read(cx).value().to_string(),
        location: form.location.read(cx).value().to_string(),
    };
    ddl::create_table_sql(dialect, &table).map(|sql| (sql, name))
}

#[allow(clippy::too_many_arguments)]
fn build_object_op(
    dialect: &dyn Dialect,
    object: &ObjectRef,
    action: ObjectAction,
    inputs: &[Entity<InputState>],
    args: &[RoutineArg],
    copy_rows: bool,
    sqlite: bool,
    cx: &App,
) -> Result<Built, String> {
    let (owner, name) = (&object.owner, &object.name);
    let typed = |ix: usize| {
        inputs
            .get(ix)
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default()
    };
    let statements = |sql: Vec<String>, summary: String| ObjectOp::Statements { sql, summary };
    Ok(match action {
        ObjectAction::Rename => {
            let new = typed(0);
            let sql = ddl::rename_table_sql(dialect, owner, name, &new)?;
            (
                statements(vec![sql], format!("Renamed {owner}.{name} to {new}.")),
                true,
                sqlite,
                format!("Renaming {name}…"),
            )
        }
        ObjectAction::Duplicate => {
            let new = typed(0);
            let mode = if copy_rows {
                CopyMode::StructureAndData
            } else {
                CopyMode::StructureOnly
            };
            let sql = ddl::duplicate_table_sql(dialect, owner, name, &new, mode)?;
            let how = if copy_rows { "with data" } else { "structure only" };
            (
                statements(
                    vec![sql],
                    format!("Copied {owner}.{name} to {owner}.{new} ({how}). Indexes and constraints are not copied."),
                ),
                true,
                sqlite,
                format!("Copying {name}…"),
            )
        }
        ObjectAction::Drop => {
            let sql = ddl::drop_object_sql(dialect, owner, name, &object.kind)
                .ok_or_else(|| "This kind of object cannot be deleted here.".to_string())?;
            let noun = ddl::kind_noun(&object.kind);
            (
                statements(vec![sql], format!("Deleted {noun} {owner}.{name}.")),
                true,
                sqlite,
                format!("Deleting {name}…"),
            )
        }
        ObjectAction::DropConstraint => {
            let table = object.parent.clone().unwrap_or_default();
            (
                statements(
                    vec![ddl::drop_constraint_sql(dialect, owner, &table, name)],
                    format!("Deleted constraint {name} of {table}."),
                ),
                true,
                sqlite,
                format!("Deleting {name}…"),
            )
        }
        ObjectAction::Empty => (
            statements(
                vec![ddl::empty_table_sql(dialect, owner, name)],
                format!("Deleted all rows of {owner}.{name} (uncommitted: Commit to keep, Rollback to undo)."),
            ),
            false,
            true,
            format!("Emptying {name}…"),
        ),
        ObjectAction::Truncate => (
            statements(
                vec![ddl::truncate_table_sql(dialect, owner, name)],
                format!("Truncated {owner}.{name}."),
            ),
            false,
            false,
            format!("Truncating {name}…"),
        ),
        ObjectAction::ResetSequence => {
            let text = typed(0);
            let value: i128 = text
                .trim()
                .parse()
                .map_err(|_| "Enter a whole number.".to_string())?;
            (
                ObjectOp::ResetSequence {
                    owner: owner.clone(),
                    name: name.clone(),
                    value,
                },
                false,
                false,
                format!("Resetting {name}…"),
            )
        }
        ObjectAction::Execute => (
            ObjectOp::CallFunction {
                owner: owner.clone(),
                name: name.clone(),
                args: (0..args.len()).map(typed).collect(),
            },
            false,
            false,
            format!("Executing {name}…"),
        ),
        ObjectAction::Flashback => {
            let original = object.parent.clone().unwrap_or_else(|| name.clone());
            let rename = typed(0);
            let sql = recycle::flashback_table_sql(dialect, owner, name, &rename)?;
            let restored = if rename.trim().is_empty() {
                original.clone()
            } else {
                rename.trim().to_string()
            };
            (
                statements(
                    vec![sql],
                    format!("Restored {owner}.{restored} from the recycle bin."),
                ),
                true,
                false,
                format!("Restoring {original}…"),
            )
        }
        ObjectAction::PurgeObject => {
            let info = object
                .detail
                .as_deref()
                .and_then(recycle::RecycleInfo::parse)
                .ok_or_else(|| "This recycle bin entry cannot be purged.".to_string())?;
            let original = object.parent.clone().unwrap_or_else(|| name.clone());
            let sql = recycle::purge_object_sql(dialect, name, &info)?;
            (
                statements(
                    vec![sql],
                    format!("Purged {} {original} from the recycle bin.", info.kind.to_lowercase()),
                ),
                true,
                false,
                format!("Purging {original}…"),
            )
        }
        ObjectAction::PurgeAll => (
            statements(
                vec![recycle::PURGE_RECYCLEBIN_SQL.to_string()],
                "Purged the recycle bin.".to_string(),
            ),
            true,
            false,
            "Purging the recycle bin…".to_string(),
        ),
        ObjectAction::PurgeAllUsers => (
            statements(
                vec![recycle::PURGE_DBA_RECYCLEBIN_SQL.to_string()],
                "Purged the DBA recycle bin.".to_string(),
            ),
            true,
            false,
            "Purging the DBA recycle bin…".to_string(),
        ),
        ObjectAction::Compile { .. }
        | ObjectAction::Trigger { .. }
        | ObjectAction::ConstraintState { .. } => {
            return Err("Nothing to ask for.".to_string())
        }
    })
}

fn confirm_text(action: ObjectAction, object: &ObjectRef, sqlite: bool) -> String {
    let target = format!("{}.{}", object.owner, object.name);
    match action {
        ObjectAction::Drop if object.kind == ObjectKind::Table && sqlite => format!(
            "Delete table {target} with all its rows? The change is not final until you Commit."
        ),
        ObjectAction::Drop if object.kind == ObjectKind::Table => format!(
            "Delete table {target} with all its rows? Oracle commits this at once; the table may stay in the recycle bin (FLASHBACK TABLE) until purged."
        ),
        ObjectAction::Drop => format!(
            "Delete {} {target}? Anything that uses it stops working{}.",
            ddl::kind_noun(&object.kind),
            if sqlite { "; not final until you Commit" } else { ", and Oracle commits this at once" }
        ),
        ObjectAction::DropConstraint => format!(
            "Delete constraint {target} from table {}? The rule is no longer enforced.",
            object.parent.clone().unwrap_or_default()
        ),
        ObjectAction::Empty => format!(
            "Delete every row of {target}? Nothing is final until you press Commit; Rollback undoes it."
        ),
        ObjectAction::Truncate => format!(
            "Truncate {target}? Every row is removed at once and Oracle commits immediately. This cannot be rolled back."
        ),
        ObjectAction::ResetSequence => format!(
            "Restart sequence {target} so its next value is the number below. Rows that already use higher numbers can then collide with new ones."
        ),
        ObjectAction::Rename => format!("Rename table {target}. Views and code that use the old name stop working."),
        ObjectAction::Duplicate => format!("Copy table {target} into a new table in the same schema."),
        ObjectAction::Execute => format!("Run {target} with these parameter values. Leave a box empty for NULL."),
        ObjectAction::Flashback => format!(
            "Restore table {} from the recycle bin as {} (FLASHBACK TABLE … TO BEFORE DROP). Its indexes come back with system names. To avoid a clash with a table that has the same name now, give a new name below.",
            object.parent.clone().unwrap_or_else(|| object.name.clone()),
            object.owner,
        ),
        ObjectAction::PurgeObject => format!(
            "Permanently remove {} from the recycle bin. It can no longer be restored with Flashback Table, and this cannot be undone.",
            object.parent.clone().unwrap_or_else(|| object.name.clone()),
        ),
        ObjectAction::PurgeAll => format!(
            "Permanently empty the recycle bin of {}. Nothing in it can be restored afterwards, and this cannot be undone.",
            object.owner
        ),
        ObjectAction::PurgeAllUsers => "Permanently empty the recycle bin of every user (PURGE DBA_RECYCLEBIN). This needs the SYSDBA privilege, nothing in any bin can be restored afterwards, and this cannot be undone.".to_string(),
        ObjectAction::Compile { .. }
        | ObjectAction::Trigger { .. }
        | ObjectAction::ConstraintState { .. } => String::new(),
    }
}

impl Workbench {
    pub(super) fn render_object_dialog(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(dialog) = self.object_dialog.as_ref() else {
            return div().into_any_element();
        };
        let sqlite = self.active_kind() == DbKind::Sqlite;
        let prod = dialog.confirm.is_some()
            && self.conn.access().environment == ConnectionEnvironment::Production;
        let (title, body, ok_label, danger, width) = match &dialog.kind {
            DialogKind::NewTable(form) => (
                "New Table".to_string(),
                self.render_new_table_body(form, theme, cx)
                    .into_any_element(),
                "Create",
                false,
                660.,
            ),
            DialogKind::Object {
                object,
                action,
                inputs,
                args,
                ..
            } => {
                let title = match action {
                    ObjectAction::Rename => format!("Rename {}", object.name),
                    ObjectAction::Duplicate => format!("Duplicate {}", object.name),
                    ObjectAction::Drop | ObjectAction::DropConstraint => {
                        format!("Delete {}?", object.name)
                    }
                    ObjectAction::Empty => format!("Empty {}?", object.name),
                    ObjectAction::Truncate => format!("Truncate {}?", object.name),
                    ObjectAction::ResetSequence => format!("Reset {}", object.name),
                    ObjectAction::Execute => format!("Execute {}", object.name),
                    ObjectAction::Flashback => format!(
                        "Flashback {}",
                        object.parent.as_deref().unwrap_or(&object.name)
                    ),
                    ObjectAction::PurgeObject => format!(
                        "Purge {}?",
                        object.parent.as_deref().unwrap_or(&object.name)
                    ),
                    ObjectAction::PurgeAll => "Purge Recycle Bin?".to_string(),
                    ObjectAction::PurgeAllUsers => "Purge DBA Recycle Bin?".to_string(),
                    ObjectAction::Compile { .. }
                    | ObjectAction::Trigger { .. }
                    | ObjectAction::ConstraintState { .. } => String::new(),
                };
                let mut col = div().flex().flex_col().gap(px(SPACE_2));
                col = col.child(body_muted(confirm_text(*action, object, sqlite), theme));
                for (ix, input) in inputs.iter().enumerate() {
                    if let Some(arg) = args.get(ix) {
                        col = col.child(form_label(
                            &format!("{} ({})", arg.name, arg.data_type),
                            theme,
                        ));
                    } else if *action == ObjectAction::ResetSequence {
                        col = col.child(form_label("Restart at", theme));
                    } else if *action == ObjectAction::Flashback {
                        col = col.child(form_label("Rename to (optional)", theme));
                    } else {
                        col = col.child(form_label("Name", theme));
                    }
                    col = col.child(Input::new(input).w_full());
                }
                if *action == ObjectAction::Duplicate {
                    col = col.child(
                        Checkbox::new("dup-rows")
                            .small()
                            .label("Copy the rows too")
                            .checked(matches!(
                                &dialog.kind,
                                DialogKind::Object {
                                    copy_rows: true,
                                    ..
                                }
                            ))
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                if let Some(ObjectDialog {
                                    kind: DialogKind::Object { copy_rows, .. },
                                    ..
                                }) = this.object_dialog.as_mut()
                                {
                                    *copy_rows = *checked;
                                }
                                cx.notify();
                            })),
                    );
                }
                let (ok, danger) = match action {
                    ObjectAction::Drop | ObjectAction::DropConstraint => ("Delete", true),
                    ObjectAction::Empty => ("Empty", true),
                    ObjectAction::Truncate => ("Truncate", true),
                    ObjectAction::ResetSequence => ("Reset", true),
                    ObjectAction::Execute => ("Execute", false),
                    ObjectAction::Rename => ("Rename", false),
                    ObjectAction::Duplicate => ("Duplicate", false),
                    ObjectAction::Flashback => ("Flashback", false),
                    ObjectAction::PurgeObject
                    | ObjectAction::PurgeAll
                    | ObjectAction::PurgeAllUsers => ("Purge", true),
                    ObjectAction::Compile { .. }
                    | ObjectAction::Trigger { .. }
                    | ObjectAction::ConstraintState { .. } => ("OK", false),
                };
                (title, col.into_any_element(), ok, danger, 460.)
            }
        };
        let mut content = div()
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
                    .child(title),
            );
        if prod {
            content = content.child(
                div()
                    .px(px(SPACE_3))
                    .py(px(SPACE_2))
                    .rounded(px(RADIUS_SM))
                    .bg(theme.danger.opacity(0.12))
                    .text_color(theme.danger)
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                    .child("This is a PRODUCTION connection. Check the name before you continue."),
            );
        }
        content = content.child(body);
        if let Some((expected, input)) = &dialog.confirm {
            content = content
                .child(form_label(&format!("Type {expected} to confirm"), theme))
                .child(Input::new(input).w_full());
        }
        if let Some(error) = &dialog.error {
            content = content.child(
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }
        let ok_button = Button::new("object-ok").flex_1().label(ok_label);
        let ok_button = if danger {
            ok_button.danger()
        } else {
            ok_button.primary()
        };
        let card = div()
            .id("object-dialog")
            .flex()
            .flex_col()
            .w(px(width))
            .max_h(relative(0.9))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_object_dialog(cx);
                }
            }))
            .child(content)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .bg(theme.muted)
                    .child(
                        Button::new("object-cancel")
                            .flex_1()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close_object_dialog(cx))),
                    )
                    .child(ok_button.on_click(
                        cx.listener(|this, _, window, cx| this.submit_object_dialog(window, cx)),
                    )),
            );
        div()
            .id("object-dialog-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(90.))
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
            .into_any_element()
    }

    fn render_new_table_body(
        &self,
        form: &NewTableForm,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let special = self.active_kind().capabilities().special_tables;
        let mut kinds = div().flex().flex_row().gap(px(SPACE_2));
        if special {
            for (ix, kind) in TableKind::ALL.into_iter().enumerate() {
                kinds = kinds.child(
                    Button::new(("nt-kind", ix))
                        .small()
                        .label(kind.label())
                        .selected(form.kind == kind)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(ObjectDialog {
                                kind: DialogKind::NewTable(form),
                                ..
                            }) = this.object_dialog.as_mut()
                            {
                                form.kind = kind;
                            }
                            cx.notify();
                        })),
                );
            }
        }
        let external = form.kind == TableKind::External;
        let mut rows = div()
            .id("nt-rows")
            .flex()
            .flex_col()
            .gap(px(SPACE_1))
            .max_h(px(300.))
            .overflow_y_scroll();
        for (ix, row) in form.columns.iter().enumerate() {
            let mut line = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(div().w(px(190.)).child(Input::new(&row.name).w_full()))
                .child(div().flex_1().child(Input::new(&row.data_type).w_full()));
            if !external {
                line = line
                    .child(
                        Checkbox::new(("nt-nn", ix))
                            .small()
                            .label("Not null")
                            .checked(row.not_null || row.primary_key)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(ObjectDialog {
                                    kind: DialogKind::NewTable(form),
                                    ..
                                }) = this.object_dialog.as_mut()
                                {
                                    if let Some(row) = form.columns.get_mut(ix) {
                                        row.not_null = *checked;
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new(("nt-pk", ix))
                            .small()
                            .label("Key")
                            .checked(row.primary_key)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(ObjectDialog {
                                    kind: DialogKind::NewTable(form),
                                    ..
                                }) = this.object_dialog.as_mut()
                                {
                                    if let Some(row) = form.columns.get_mut(ix) {
                                        row.primary_key = *checked;
                                    }
                                }
                                cx.notify();
                            })),
                    );
            }
            line = line.child(
                Button::new(("nt-del", ix))
                    .ghost()
                    .small()
                    .icon(IconName::Trash)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ObjectDialog {
                            kind: DialogKind::NewTable(form),
                            ..
                        }) = this.object_dialog.as_mut()
                        {
                            if form.columns.len() > 1 && ix < form.columns.len() {
                                form.columns.remove(ix);
                            }
                        }
                        cx.notify();
                    })),
            );
            rows = rows.child(line);
        }
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(form_label(
                &format!("Table name (schema {})", form.owner),
                theme,
            ))
            .child(Input::new(&form.name).w_full());
        if special {
            body = body.child(form_label("Type", theme)).child(kinds);
        }
        if external {
            body = body
                .child(form_label("Directory object", theme))
                .child(Input::new(&form.directory).w_full())
                .child(form_label("File name in that directory", theme))
                .child(Input::new(&form.location).w_full());
        }
        body.child(form_label("Columns", theme))
            .child(rows)
            .child(
                Button::new("nt-add")
                    .small()
                    .icon(IconName::Plus)
                    .label("Add column")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let row = column_row("", "VARCHAR2(100)", window, cx);
                        if let Some(ObjectDialog {
                            kind: DialogKind::NewTable(form),
                            ..
                        }) = this.object_dialog.as_mut()
                        {
                            form.columns.push(row);
                        }
                        cx.notify();
                    })),
            )
            .when(form.kind == TableKind::IndexOrganized, |b| {
                b.child(body_muted(
                    "An index-organized table keeps its rows in the primary key. Tick Key on at least one column.".to_string(),
                    theme,
                ))
            })
            .when(external, |b| {
                b.child(body_muted(
                    "External tables are read-only and take no constraints. Fields are read as comma separated text.".to_string(),
                    theme,
                ))
            })
    }
}

fn body_muted(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(theme, FONT_BODY))
        .line_height(crate::theme::ui_size(theme, LEADING_BODY))
        .text_color(theme.muted_foreground)
        .child(text)
}
