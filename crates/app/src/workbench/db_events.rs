use super::*;

impl Workbench {
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
                self.connection_form.show_connection_form = false;
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
                    self.connection_form.form_notice = Some((false, text));
                    self.conn.status = "Test koneksi berhasil.".to_string();
                }
                Err(message) => {
                    self.push_message(format!("Test koneksi gagal: {message}"));
                    self.connection_form.form_notice =
                        Some((true, format!("Test koneksi gagal: {message}")));
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
}
