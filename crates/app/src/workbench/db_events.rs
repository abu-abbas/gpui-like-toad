use super::*;

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
        if let Response::Failed(message) = &resp {
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
            self.push_message(message.clone());
            self.conn.status = "Error (see Messages).".to_string();
            self.target_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        match (pending, resp) {
            (Some(PendingOp::Connect), Response::Connected(info)) => {
                self.conn.session = Some(info.clone());
                self.conn.connection_lost = false;
                self.reset_catalog(Some(info.user.clone()));
                self.conn.current_schema = None;
                self.conn.connect_error = None;
                self.connection_form.show_connection_form = false;
                self.conn.status = format!("Connected as {}.", info.user);
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
                        "Connection test succeeded: {} @ {} ({})",
                        info.user, info.database, info.version
                    );
                    self.push_message(text.clone());
                    self.connection_form.form_notice = Some((false, text));
                    self.conn.status = "Connection test succeeded.".to_string();
                }
                Err(message) => {
                    self.push_message(format!("Connection test failed: {message}"));
                    self.connection_form.form_notice =
                        Some((true, format!("Connection test failed: {message}")));
                    self.conn.status = "Connection test failed.".to_string();
                }
            },
            (Some(PendingOp::Run), Response::ScriptDone(results)) => {
                let Some(ix) = self.target_ix() else {
                    return;
                };
                for entry in &results {
                    if entry.outcome.is_ok() {
                        self.record_run(&entry.sql);
                    }
                    if let Err(err) = &entry.outcome {
                        self.push_message(err.clone());
                    }
                }
                self.tabs[ix].result_connection = self
                    .conn
                    .session
                    .as_ref()
                    .map(|info| format!("{}  ·  {}", self.conn.label(), info.database))
                    .unwrap_or_default();
                self.conn.status = format!("Queries completed: {}.", results.len());
                self.tabs[ix].statement_results = results;
                self.show_statement_result(ix, 0, cx);
            }
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
                    format!("{rows} rows in {elapsed} ms (more rows available)")
                } else if truncated {
                    format!("{rows} rows in {elapsed} ms (limited to {DEFAULT_MAX_ROWS} rows)")
                } else {
                    format!("{rows} rows in {elapsed} ms")
                };
            }
            (Some(PendingOp::Run), Response::Executed(affected)) => {
                if let Some(sql) = pending_sql {
                    self.record_run(&sql);
                }
                self.target_tab_mut().statement_results.clear();
                self.push_message(format!("Affected rows: {affected}."));
                self.conn.status = format!("Affected rows: {affected} (uncommitted).");
                self.target_tab_mut().results_tab = ResultsTab::Messages;
            }
            (Some(PendingOp::Explain), Response::PlanTree(nodes, lines)) => {
                let tab = self.target_tab_mut();
                tab.plan_expanded = plan::all_plan_ids(&nodes);
                tab.plan_nodes = Some(nodes);
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.conn.status = "Explain plan ready.".to_string();
            }
            (Some(PendingOp::Explain), Response::Plan(lines)) => {
                let tab = self.target_tab_mut();
                tab.plan_nodes = None;
                tab.plan_lines = lines;
                tab.results_tab = ResultsTab::Plan;
                self.conn.status = "Explain plan ready.".to_string();
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
                self.conn.status = format!("{schema}: loaded objects: {count}.");
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
                    format!(" (limited to {} rows)", ora_core::EXPORT_MAX_ROWS)
                } else {
                    String::new()
                };
                self.push_message(format!(
                    "Exported {rows} rows from {owner}.{name} to {}{cap}",
                    path.display()
                ));
                self.conn.status = format!("Exported {rows} rows from {name}.");
            }
            (Some(PendingOp::RefreshMaterializedView { owner, name }), Response::Executed(_)) => {
                self.push_message(format!("Refreshed materialized view {owner}.{name}."));
                self.conn.status = format!("Refreshed {name}.");
                self.target_tab_mut().results_tab = ResultsTab::Messages;
                // Show the new rows when that view is open in a data tab.
                let open_here = self.target_tab_mut().data.as_ref().is_some_and(|d| {
                    d.owner.eq_ignore_ascii_case(&owner) && d.name.eq_ignore_ascii_case(&name)
                });
                if open_here {
                    self.reload_data(cx);
                }
            }
            (Some(PendingOp::ImportCsv { owner, name }), Response::Imported(rows)) => {
                self.push_message(format!(
                    "Imported {rows} rows into {owner}.{name} (uncommitted)."
                ));
                self.conn.status = format!("Imported {rows} rows into {name} (uncommitted).");
                self.target_tab_mut().results_tab = ResultsTab::Messages;
            }
            (Some(PendingOp::DuplicateRows(rows)), Response::Described(defs)) => {
                self.stage_duplicates(rows, &defs, window, cx);
            }
            (Some(PendingOp::DataStructure), Response::Described(defs)) => {
                let result = schema::describe_to_result(&defs);
                self.conn.status = format!("Columns: {}.", result.row_count());
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
                self.conn.status = "DDL loaded.".to_string();
                if let Some(editor) = self.target_tab_mut().data.as_ref().map(|d| d.ddl.clone()) {
                    editor.update(cx, |state, cx| state.set_value(ddl, window, cx));
                }
            }
            (Some(PendingOp::ApplyEdits), Response::CellsUpdated { done, error }) => {
                self.conn.uncommitted |= done > 0;
                self.edits_applied(done, error, cx);
            }
            (Some(PendingOp::Commit), Response::Committed) => {
                self.conn.uncommitted = false;
                self.push_message("Commit succeeded.".to_string());
                self.conn.status = "Commit succeeded.".to_string();
                self.clear_edit_marks(cx);
            }
            (Some(PendingOp::Rollback), Response::RolledBack) => {
                self.conn.uncommitted = false;
                self.push_message("Rollback succeeded.".to_string());
                self.conn.status = "Rollback succeeded.".to_string();
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
}
