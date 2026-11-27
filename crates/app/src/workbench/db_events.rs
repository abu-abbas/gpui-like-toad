use super::*;

mod catalog;
mod connect;
mod edits;
mod objects;
mod query;
mod transaction;

/// A heartbeat tick this much later than due means the computer slept.
const WAKE_GAP: std::time::Duration = std::time::Duration::from_secs(60);

impl Workbench {
    /// Index of the tab the finished operation belongs to: the tab it was
    /// started from if it still exists, else the active tab.
    /// Index of the tab the running operation belongs to. `None` when that
    /// tab was closed while the operation ran.
    pub(super) fn target_ix(&self) -> Option<usize> {
        self.target_ix_for(self.conn.id)
    }

    /// Same for the operation running on connection `conn_id`.
    pub(super) fn target_ix_for(&self, conn_id: u64) -> Option<usize> {
        match self.conn_by_id(conn_id)?.pending_tab {
            Some(id) => self.tabs.iter().position(|t| t.id == id),
            None => (self.active_tab < self.tabs.len()).then_some(self.active_tab),
        }
    }

    pub(super) fn target_tab_mut(&mut self) -> &mut QueryTab {
        self.target_tab_for_mut(self.conn.id)
    }

    pub(super) fn target_tab_for_mut(&mut self, conn_id: u64) -> &mut QueryTab {
        match self.target_ix_for(conn_id) {
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
        self.send_db(self.conn.id, job, op, cx);
    }

    /// Like [`Workbench::spawn_db`] for a job that belongs to connection
    /// `conn_id` and no tab (explorer lists, object actions), which need not
    /// be the active one. Ops that land in a tab go through `spawn_db`.
    pub(super) fn spawn_db_for(
        &mut self,
        conn_id: u64,
        job: Job,
        op: PendingOp,
        cx: &mut Context<Self>,
    ) {
        if conn_id == self.conn.id {
            return self.spawn_db(job, op, cx);
        }
        if self.conn_by_id(conn_id).is_none_or(|c| c.db.is_none()) {
            self.push_message_for(conn_id, "Not connected.".to_string());
            return;
        }
        self.send_db(conn_id, job, op, cx);
    }

    /// Ask for the production confirmation when the connection is
    /// protected, then dispatch.
    fn send_db(&mut self, conn_id: u64, job: Job, op: PendingOp, cx: &mut Context<Self>) {
        let Some(conn) = self.conn_by_id(conn_id) else {
            return;
        };
        let access = conn.access();
        if conn.session.is_some() {
            if let Some(operation) = job.protected_operation() {
                if access.protect_production && !access.read_only {
                    self.confirm_production_job(conn_id, job, op, operation, cx);
                    return;
                }
            }
        }
        self.dispatch_db(conn_id, job, op, false, cx);
    }

    fn confirm_production_job(
        &mut self,
        conn_id: u64,
        job: Job,
        op: PendingOp,
        operation: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conn_mut(conn_id) else {
            return;
        };
        conn.busy = true;
        let generation = conn.op_generation;
        let connection = conn.label();
        let database = conn
            .session
            .as_ref()
            .map(|info| format!("{} @ {}", info.user, info.database))
            .unwrap_or_default();
        cx.notify();
        let handle = self.window_handle;
        let tab_id = self.tabs.get(self.active_tab).map(|t| t.id);
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
                if !this.op_is_current(conn_id, generation) {
                    return;
                }
                // Only the user's active connection may continue; for any
                // other one the operation is cancelled in place, without
                // making it active.
                let proceed = confirmed
                    && this.conn.id == conn_id
                    && this.tabs.get(this.active_tab).map(|t| t.id) == tab_id
                    && this.conn.session.is_some();
                if proceed {
                    this.conn.busy = false;
                    this.dispatch_db(conn_id, job, op, true, cx);
                } else if let Some(conn) = this.conn_mut(conn_id) {
                    conn.busy = false;
                    conn.pending_sql = None;
                    conn.cancel_script = None;
                    conn.status = "Production operation cancelled.".to_string();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn dispatch_db(
        &mut self,
        conn_id: u64,
        job: Job,
        op: PendingOp,
        confirmed: bool,
        cx: &mut Context<Self>,
    ) {
        let pending_tab = self.tabs.get(self.active_tab).map(|t| t.id);
        let Some(conn) = self.conn_mut(conn_id) else {
            return;
        };
        let Some(db) = conn.db.as_ref() else {
            return;
        };
        let rx: Receiver<Response> = if confirmed {
            db.request_confirmed(job)
        } else {
            db.request(job)
        };
        conn.op_generation += 1;
        let generation = conn.op_generation;
        conn.pending_op = Some(op);
        conn.pending_tab = pending_tab;
        conn.busy = true;
        cx.notify();
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let mut rx = rx;
            let resp = loop {
                let (back, resp) = cx
                    .background_executor()
                    .spawn(async move {
                        let resp = rx.recv();
                        (rx, resp)
                    })
                    .await;
                rx = back;
                // Early rows of a data page: show them, keep waiting for
                // the whole page.
                let Ok(Response::QueryPartial(part)) = resp else {
                    break resp;
                };
                let _ = view.update(cx, |this, cx| {
                    if this.op_is_current(conn_id, generation) {
                        this.on_query_partial(conn_id, part, cx);
                    }
                });
            };
            // The window is needed to create tabs (View Script) and set
            // editor text from a response.
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    // A superseded operation (after Stop, or a newer op)
                    // is dropped without touching any connection.
                    if !this.op_is_current(conn_id, generation) {
                        return;
                    }
                    this.on_db_response(conn_id, generation, resp, window, cx)
                })
            });
        })
        .detach();
    }

    pub(super) fn start_heartbeat(cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            let mut last_tick = std::time::Instant::now();
            loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(15))
                .await;
            // A tick far later than 15 s means the computer slept (or the
            // app was suspended): the sockets are most likely dead.
            let gap = last_tick.elapsed();
            last_tick = std::time::Instant::now();
            let woke = gap >= WAKE_GAP;
            if woke {
                crate::error_log::append(&format!(
                    "Heartbeat resumed after {} s (computer asleep or app suspended); checking connections.",
                    gap.as_secs()
                ));
            }
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
                    let reason = if woke {
                        format!("{reason} (found after the computer woke from sleep)")
                    } else {
                        format!("{reason} (found by the 15 s keep-alive check)")
                    };
                    let _ = view.update(cx, |this, cx| {
                        if this.op_is_current(id, generation) {
                            this.connection_was_lost(id, reason, cx);
                        }
                    });
                }
            }
            }
        })
        .detach();
    }

    fn connection_was_lost(&mut self, cid: u64, error: String, cx: &mut Context<Self>) {
        let Some(conn) = self.conn_mut(cid) else {
            return;
        };
        if conn.session.take().is_none() {
            return;
        }
        let had_writes = conn.uncommitted
            || matches!(
                conn.pending_op,
                Some(
                    PendingOp::ApplyEdits
                        | PendingOp::ImportCsv { .. }
                        | PendingOp::Commit
                        | PendingOp::Rollback
                        | PendingOp::Run
                )
            );
        let connection = conn.label();
        conn.connection_lost = true;
        conn.lost_reason = Some(error.clone());
        conn.db = None;
        conn.busy = false;
        conn.abandoned = false;
        conn.pending_op = None;
        conn.pending_tab = None;
        conn.pending_sql = None;
        conn.op_generation += 1;
        conn.clear_uncommitted();
        conn.current_schema = None;
        conn.selected_object = None;
        conn.objects.clear();
        conn.schemas.clear();
        self.reset_catalog_for(cid, None);
        if self
            .tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.conn == Some(cid))
        {
            self.cell_editor = None;
            self.inline_edit = None;
        }
        self.clear_edit_marks_for(cid, false, cx);
        self.invalidate_connection_results_for(cid, cx);
        self.set_status_for(cid, "Connection lost. Reconnect to continue.".to_string());
        let text = format!("Connection lost ({connection}): {error}");
        // Always keep the reason in errors.log: a lost connection is the
        // first thing to check when everything looks disconnected.
        crate::error_log::append(&text);
        self.push_message_with(cid, text, true);
        if had_writes {
            self.push_message_for(
                cid,
                "Check the database before retrying. The last operation may have reached Oracle."
                    .into(),
            );
        }
        self.tx_step_done(cid, false, cx);
        if !had_writes {
            self.schedule_auto_reconnect(cid, cx);
        }
        cx.notify();
    }

    /// Try to log in again by itself after an idle drop (never after a
    /// write that may have reached the server): at 2 s, 8 s and 30 s, so a
    /// Mac that just woke has time to bring its network back. Only the
    /// connection in front is retried; the others wait for their banner's
    /// Reconnect button, so no background login steals the view.
    fn schedule_auto_reconnect(&mut self, cid: u64, cx: &mut Context<Self>) {
        const DELAYS: [u64; 3] = [2, 8, 30];
        let Some(conn) = self.conn_mut(cid) else {
            return;
        };
        let Some(delay) = DELAYS.get(usize::from(conn.auto_reconnects)).copied() else {
            return;
        };
        conn.auto_reconnects += 1;
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(delay))
                .await;
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = view.update(cx, |this, cx| this.try_auto_reconnect(cid, window, cx));
            });
        })
        .detach();
    }

    fn try_auto_reconnect(&mut self, cid: u64, window: &mut Window, cx: &mut Context<Self>) {
        let ready = self.conn.id == cid
            && self.conn.connection_lost
            && !self.conn.busy
            && !self.connection_form.show_connection_form;
        if !ready {
            return;
        }
        let Some(last) = self.conn.last_connection.as_ref() else {
            return;
        };
        // Without a saved password, reconnecting would open the password
        // form; leave that to the user's click.
        let Some(profile) = last.profile_ix.and_then(|ix| self.profiles.get(ix)) else {
            return;
        };
        if profile.kind.capabilities().password && !profile.save_password {
            return;
        }
        self.push_message_for(cid, "Reconnecting automatically…".to_string());
        self.reconnect_last(window, cx);
        // If this attempt fails too, the connection stays lost: try again
        // later (the next attempt stops by itself once it is connected).
        self.schedule_auto_reconnect(cid, cx);
    }

    /// The worker finally answered an operation the user stopped waiting
    /// for: free the connection, ignore the data, but remember a write may
    /// have happened.
    fn on_abandoned_response(
        &mut self,
        cid: u64,
        resp: Result<Response, RecvError>,
        cx: &mut Context<Self>,
    ) {
        if let Ok(Response::ConnectionLost(error)) = resp {
            self.connection_was_lost(cid, error, cx);
            return;
        }
        self.set_conn_state(cid, |conn| {
            let pending = conn.pending_op.take();
            let tab = conn.pending_tab.take();
            let sql = conn.pending_sql.take();
            conn.busy = false;
            if let Ok(resp) = &resp {
                conn.note_response(pending.as_ref(), resp, sql.as_deref(), tab);
            }
            conn.status = "The stopped operation finished; its result was ignored.".to_string();
        });
        self.push_message_for(
            cid,
            "The operation you stopped has finished on the server. Its result was ignored."
                .to_string(),
        );
        cx.notify();
    }

    fn on_db_response(
        &mut self,
        cid: u64,
        generation: u64,
        resp: Result<Response, RecvError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A superseded operation (after Stop, or a newer op) drops its
        // response entirely: stale data must never overwrite fresh state.
        let Some(conn) = self.conn_mut(cid) else {
            return;
        };
        if generation != conn.op_generation {
            return;
        }
        conn.cancel_script = None;
        if std::mem::take(&mut conn.abandoned) {
            return self.on_abandoned_response(cid, resp, cx);
        }
        if let Ok(Response::ConnectionLost(error)) = resp {
            self.connection_was_lost(cid, error, cx);
            return;
        }
        let pending = conn.pending_op.take();
        let pending_sql = conn.pending_sql.take();
        conn.busy = false;
        let resp = match resp {
            Ok(resp) => resp,
            Err(_) => {
                conn.status = "Worker error".to_string();
                self.push_message_for(cid, "The database worker stopped.".to_string());
                cx.notify();
                return;
            }
        };
        let tab = conn.pending_tab;
        conn.note_response(pending.as_ref(), &resp, pending_sql.as_deref(), tab);
        if let Response::Failed(message) = resp {
            self.on_db_failed(cid, pending, message, window, cx);
            return;
        }
        match (pending, resp) {
            // Connect and Test (connect.rs).
            (Some(PendingOp::Connect), Response::Connected(info)) => {
                self.on_connected(cid, info, window, cx)
            }
            (Some(PendingOp::TestConnect), Response::Tested(outcome)) => {
                self.on_connection_tested(cid, outcome)
            }
            // SQL tabs: Run and Explain (query.rs).
            (Some(PendingOp::Run), Response::ScriptDone(results)) => {
                self.on_script_done(cid, results, cx)
            }
            (Some(PendingOp::Run), Response::ScriptRan(steps)) => {
                self.on_script_ran(cid, steps, cx)
            }
            (
                Some(PendingOp::LoadCell),
                Response::CellLoaded {
                    rowid,
                    column,
                    value,
                },
            ) => self.on_cell_loaded(cid, rowid, column, value, cx),
            (Some(PendingOp::Run), Response::QueryDone(result)) => {
                self.on_query_done(cid, result, pending_sql, cx)
            }
            (Some(PendingOp::Run), Response::Executed(affected)) => {
                self.on_statement_executed(cid, affected, pending_sql)
            }
            (Some(PendingOp::Explain), Response::PlanTree(nodes, lines)) => {
                self.on_plan_tree(cid, nodes, lines)
            }
            (Some(PendingOp::Explain), Response::Plan(lines)) => self.on_plan_text(cid, lines),
            // Explorer schema and object lists (catalog.rs).
            (Some(PendingOp::ListSchemas), Response::Schemas(schemas)) => {
                self.on_schemas(cid, schemas, cx)
            }
            (Some(PendingOp::ListObjects), Response::Objects(objects)) => {
                self.on_objects(cid, objects, cx)
            }
            // Explorer object actions and data tab views (objects.rs).
            (Some(PendingOp::Ddl { owner, name }), Response::Ddl(ddl)) => {
                self.on_object_script(cid, owner, name, ddl, window, cx)
            }
            (
                Some(PendingOp::ExportTable { owner, name }),
                Response::Exported {
                    path,
                    rows,
                    truncated,
                    clipped,
                },
            ) => self.on_table_exported(cid, (owner, name), path, rows, truncated, clipped),
            (Some(PendingOp::RefreshMaterializedView { owner, name }), Response::Executed(_)) => {
                self.on_materialized_view_refreshed(cid, owner, name, cx)
            }
            (Some(PendingOp::ObjectOp { reload, .. }), Response::ObjectOp(lines)) => {
                self.on_object_op_done(cid, lines, reload, cx)
            }
            (
                Some(PendingOp::RoutineSignature(object)),
                Response::RoutineSignature(args, returns),
            ) => self.on_routine_signature(cid, object, args, returns, window, cx),
            (Some(PendingOp::ImportCsv { owner, name }), Response::Imported(rows)) => {
                self.on_csv_imported(cid, owner, name, rows)
            }
            (Some(PendingOp::DataStructure), Response::Described(defs)) => {
                self.on_data_structure(cid, defs, cx)
            }
            (Some(PendingOp::DataIndexes), Response::Indexes(indexes)) => {
                self.on_data_indexes(cid, indexes, cx)
            }
            (Some(PendingOp::DataDdl), Response::Ddl(ddl)) => {
                self.on_data_ddl(cid, ddl, window, cx)
            }
            // Grid edits (edits.rs).
            (Some(PendingOp::DuplicateRows(rows)), Response::Described(defs)) => {
                self.stage_duplicates(cid, rows, &defs, window, cx)
            }
            (Some(PendingOp::ApplyEdits), Response::CellsUpdated { done, error }) => {
                self.on_cells_updated(cid, done, error, cx)
            }
            // Commit and Rollback (transaction.rs).
            (Some(PendingOp::Commit), Response::Committed) => self.on_committed(cid, cx),
            (Some(PendingOp::Rollback), Response::RolledBack) => self.on_rolled_back(cid, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Any operation failed: show the error. A failed direct connect also
    /// opens the profile's dialog so the password can be fixed.
    fn on_db_failed(
        &mut self,
        cid: u64,
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
                    .conn_by_id(cid)
                    .and_then(|c| c.last_connection.as_ref())
                    .filter(|l| !l.demo)
                    .and_then(|l| l.profile_ix)
                {
                    // Only for the connection the user is working on; a
                    // background one must not pop a dialog over other work.
                    if cid == self.conn.id {
                        self.edit_profile(ix, window, cx);
                    }
                }
            }
            self.set_conn_state(cid, |c| c.connect_error = Some(message.clone()));
        }
        let ended_tx = matches!(pending, Some(PendingOp::Commit | PendingOp::Rollback));
        self.push_message_for(cid, message);
        self.set_status_for(cid, "Error (see Messages).".to_string());
        self.target_tab_for_mut(cid).results_tab = ResultsTab::Messages;
        if ended_tx {
            self.tx_step_done(cid, false, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
