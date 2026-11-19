use super::*;

mod catalog;
mod connect;
mod edits;
mod objects;
mod query;
mod transaction;

impl Workbench {
    /// Index of the tab the finished operation belongs to: the tab it was
    /// started from if it still exists, else the active tab.
    /// Index of the tab the running operation belongs to. `None` when that
    /// tab was closed while the operation ran.
    pub(super) fn target_ix(&self) -> Option<usize> {
        match self.conn.pending_tab {
            Some(id) => self.tabs.iter().position(|t| t.id == id),
            None => (self.active_tab < self.tabs.len()).then_some(self.active_tab),
        }
    }

    pub(super) fn target_tab_mut(&mut self) -> &mut QueryTab {
        match self.target_ix() {
            Some(ix) => &mut self.tabs[ix],
            None => &mut self.scratch_tab,
        }
    }

    /// Send a job to the worker and apply the response when it arrives. The
    /// blocking receive runs on the background executor, never the UI
    /// thread. The generation tag lets [`Workbench::stop`] drop the
    /// response of a superseded operation.
    pub(super) fn spawn_db(&mut self, job: Job, op: PendingOp, cx: &mut Context<Self>) {
        if self.conn.db.is_none() {
            self.push_message("Not connected.".to_string());
            return;
        }
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
                | PendingOp::LoadCell
                | PendingOp::DuplicateRows(_)
        );
        let tab_conn = self.tabs.get(self.active_tab).and_then(|t| t.conn);
        if tab_op && !self.show_empty_workspace {
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
                    "This tab belongs to another connection."
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
        let access = self.conn.access();
        if self.conn.session.is_some() {
            if let Some(operation) = job.protected_operation() {
                if access.protect_production && !access.read_only {
                    self.confirm_production_job(job, op, operation, cx);
                    return;
                }
            }
        }
        self.dispatch_db(job, op, false, cx);
    }

    fn confirm_production_job(
        &mut self,
        job: Job,
        op: PendingOp,
        operation: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.conn.busy = true;
        cx.notify();
        let handle = self.window_handle;
        let conn_id = self.conn.id;
        let generation = self.conn.op_generation;
        let tab_id = self.tabs.get(self.active_tab).map(|t| t.id);
        let connection = self.conn.label();
        let database = self
            .conn
            .session
            .as_ref()
            .map(|info| format!("{} @ {}", info.user, info.database))
            .unwrap_or_default();
        cx.spawn(async move |view, cx| {
            let prompt = cx.update_window(handle, |_, window, cx| {
                window.prompt(
                    gpui_kit::PromptLevel::Warning,
                    "Confirm production operation",
                    Some(&format!("{operation} on {connection} ({database})?")),
                    &["Cancel", "Continue"],
                    cx,
                )
            });
            let confirmed = match prompt {
                Ok(prompt) => prompt.await == Ok(1),
                Err(_) => false,
            };
            let _ = view.update(cx, |this, cx| {
                this.with_conn(conn_id, cx, |this, background, cx| {
                    if this.conn.op_generation != generation {
                        return;
                    }
                    this.conn.busy = false;
                    if confirmed
                        && !background
                        && this.tabs.get(this.active_tab).map(|t| t.id) == tab_id
                        && this.conn.session.is_some()
                    {
                        this.dispatch_db(job, op, true, cx);
                    } else {
                        this.conn.pending_sql = None;
                        this.conn.cancel_script = None;
                        this.conn.status = "Production operation cancelled.".to_string();
                        cx.notify();
                    }
                });
            });
        })
        .detach();
    }

    fn dispatch_db(&mut self, job: Job, op: PendingOp, confirmed: bool, cx: &mut Context<Self>) {
        let Some(db) = self.conn.db.as_ref() else {
            return;
        };
        let rx: Receiver<Response> = if confirmed {
            db.request_confirmed(job)
        } else {
            db.request(job)
        };
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

    pub(super) fn start_heartbeat(cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(15))
                .await;
            let Ok(requests) = view.update(cx, |this, _| {
                std::iter::once(&this.conn)
                    .chain(&this.parked)
                    .filter(|conn| !conn.busy && !conn.demo_mode && conn.session.is_some())
                    .filter_map(|conn| {
                        Some((
                            conn.id,
                            conn.op_generation,
                            conn.db.as_ref()?.request(Job::Ping),
                        ))
                    })
                    .collect::<Vec<_>>()
            }) else {
                break;
            };
            for (id, generation, rx) in requests {
                let result = cx
                    .background_executor()
                    .spawn(async move { rx.recv() })
                    .await;
                if let Ok(Response::ConnectionLost(reason)) = result {
                    let _ = view.update(cx, |this, cx| {
                        this.with_conn(id, cx, |this, _, cx| {
                            if this.conn.op_generation == generation {
                                this.connection_was_lost(reason, cx);
                            }
                        })
                    });
                }
            }
        })
        .detach();
    }

    fn connection_was_lost(&mut self, error: String, cx: &mut Context<Self>) {
        if self.conn.session.take().is_none() {
            return;
        }
        let had_writes = self.conn.uncommitted
            || matches!(
                self.conn.pending_op,
                Some(
                    PendingOp::ApplyEdits
                        | PendingOp::ImportCsv { .. }
                        | PendingOp::Commit
                        | PendingOp::Rollback
                        | PendingOp::Run
                )
            );
        let connection = self.conn.label();
        self.conn.connection_lost = true;
        self.conn.db = None;
        self.conn.busy = false;
        self.conn.pending_op = None;
        self.conn.pending_tab = None;
        self.conn.pending_sql = None;
        self.conn.op_generation += 1;
        self.conn.uncommitted = false;
        self.conn.current_schema = None;
        self.conn.selected_object = None;
        self.conn.objects.clear();
        self.conn.schemas.clear();
        self.reset_catalog(None);
        if self
            .tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.conn == Some(self.conn.id))
        {
            self.cell_editor = None;
        }
        self.clear_edit_marks(cx);
        self.invalidate_connection_results(cx);
        self.conn.status = "Connection lost. Reconnect to continue.".to_string();
        self.push_message(format!("Connection lost ({connection}): {error}"));
        if had_writes {
            self.push_message(
                "Check the database before retrying. The last operation may have reached Oracle."
                    .into(),
            );
        }
        cx.notify();
    }

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
        self.conn.cancel_script = None;
        if let Ok(Response::ConnectionLost(error)) = resp {
            self.connection_was_lost(error, cx);
            return;
        }
        let pending = self.conn.pending_op.take();
        let pending_sql = self.conn.pending_sql.take();
        self.conn.busy = false;
        let resp = match resp {
            Ok(resp) => resp,
            Err(_) => {
                self.push_message("The database worker stopped.".to_string());
                self.conn.status = "Worker error".to_string();
                cx.notify();
                return;
            }
        };
        if matches!(
            (&pending, &resp),
            (Some(PendingOp::Run), Response::Executed(_))
                | (Some(PendingOp::ImportCsv { .. }), Response::Imported(_))
        ) {
            self.conn.uncommitted = true;
        }
        if let Response::Failed(message) = resp {
            self.on_db_failed(pending, message, window, cx);
            return;
        }
        match (pending, resp) {
            // Connect and Test (connect.rs).
            (Some(PendingOp::Connect), Response::Connected(info)) => {
                self.on_connected(info, window, cx)
            }
            (Some(PendingOp::TestConnect), Response::Tested(outcome)) => {
                self.on_connection_tested(outcome)
            }
            // SQL tabs: Run and Explain (query.rs).
            (Some(PendingOp::Run), Response::ScriptDone(results)) => {
                self.on_script_done(results, cx)
            }
            (
                Some(PendingOp::LoadCell),
                Response::CellLoaded {
                    rowid,
                    column,
                    value,
                },
            ) => self.on_cell_loaded(rowid, column, value, cx),
            (Some(PendingOp::Run), Response::QueryDone(result)) => {
                self.on_query_done(result, pending_sql, cx)
            }
            (Some(PendingOp::Run), Response::Executed(affected)) => {
                self.on_statement_executed(affected, pending_sql)
            }
            (Some(PendingOp::Explain), Response::PlanTree(nodes, lines)) => {
                self.on_plan_tree(nodes, lines)
            }
            (Some(PendingOp::Explain), Response::Plan(lines)) => self.on_plan_text(lines),
            // Explorer schema and object lists (catalog.rs).
            (Some(PendingOp::ListSchemas), Response::Schemas(schemas)) => {
                self.on_schemas(schemas, cx)
            }
            (Some(PendingOp::ListObjects), Response::Objects(objects)) => {
                self.on_objects(objects, cx)
            }
            // Explorer object actions and data tab views (objects.rs).
            (Some(PendingOp::Ddl { owner, name }), Response::Ddl(ddl)) => {
                self.on_object_script(owner, name, ddl, window, cx)
            }
            (
                Some(PendingOp::ExportTable { owner, name }),
                Response::Exported {
                    path,
                    rows,
                    truncated,
                    clipped,
                },
            ) => self.on_table_exported(owner, name, path, rows, truncated, clipped),
            (Some(PendingOp::RefreshMaterializedView { owner, name }), Response::Executed(_)) => {
                self.on_materialized_view_refreshed(owner, name, cx)
            }
            (Some(PendingOp::ImportCsv { owner, name }), Response::Imported(rows)) => {
                self.on_csv_imported(owner, name, rows)
            }
            (Some(PendingOp::DataStructure), Response::Described(defs)) => {
                self.on_data_structure(defs, cx)
            }
            (Some(PendingOp::DataIndexes), Response::Indexes(indexes)) => {
                self.on_data_indexes(indexes, cx)
            }
            (Some(PendingOp::DataDdl), Response::Ddl(ddl)) => self.on_data_ddl(ddl, window, cx),
            // Grid edits (edits.rs).
            (Some(PendingOp::DuplicateRows(rows)), Response::Described(defs)) => {
                self.stage_duplicates(rows, &defs, window, cx)
            }
            (Some(PendingOp::ApplyEdits), Response::CellsUpdated { done, error }) => {
                self.on_cells_updated(done, error, cx)
            }
            // Commit and Rollback (transaction.rs).
            (Some(PendingOp::Commit), Response::Committed) => self.on_committed(cx),
            (Some(PendingOp::Rollback), Response::RolledBack) => {
                self.on_rolled_back(background, cx)
            }
            _ => {}
        }
        cx.notify();
    }

    /// Any operation failed: show the error. A failed direct connect also
    /// opens the profile's dialog so the password can be fixed.
    fn on_db_failed(
        &mut self,
        pending: Option<PendingOp>,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(pending, Some(PendingOp::Connect)) {
            // A direct connect with a saved password failed (wrong or
            // missing password): open the dialog so it can be fixed.
            if !self.connection_form.show_connection_form {
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
        self.push_message(message);
        self.conn.status = "Error (see Messages).".to_string();
        self.target_tab_mut().results_tab = ResultsTab::Messages;
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
