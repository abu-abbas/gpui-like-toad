//! Responses to Run and Explain in SQL tabs.

use super::*;
use crate::workbench::results::result_room;

impl Workbench {
    pub(super) fn on_script_done(
        &mut self,
        cid: u64,
        results: Vec<crate::worker::StatementResult>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.target_ix_for(cid) else {
            return;
        };
        for entry in &results {
            if entry.outcome.is_ok() {
                self.record_run_for(cid, &entry.sql);
            }
            if let Err(err) = &entry.outcome {
                self.push_message_for(cid, err.clone());
            }
        }
        self.tabs[ix].result_connection = self
            .conn_by_id(cid)
            .and_then(|c| {
                c.session
                    .as_ref()
                    .map(|info| format!("{}  ·  {}", c.label(), info.database))
            })
            .unwrap_or_default();
        self.set_status_for(cid, format!("Queries completed: {}.", results.len()));
        self.tabs[ix].statement_results = results;
        self.show_statement_result(ix, 0, cx);
        self.enforce_result_budget(cx);
    }

    /// A mixed script (F5): rows go to the result list like a SELECT
    /// script, everything else to Messages (one line per statement).
    pub(super) fn on_script_ran(
        &mut self,
        cid: u64,
        steps: Vec<crate::worker::ScriptStep>,
        cx: &mut Context<Self>,
    ) {
        use crate::worker::{StatementResult, StepOk};
        let Some(ix) = self.target_ix_for(cid) else {
            return;
        };
        let total = steps.len();
        let mut results = Vec::new();
        let mut failed = false;
        for (n, step) in steps.into_iter().enumerate() {
            let head = step.sql.lines().next().unwrap_or("").trim().to_string();
            let head: String = head.chars().take(60).collect();
            match step.outcome {
                Ok(ok) => {
                    self.record_run_for(cid, &step.sql);
                    match ok {
                        StepOk::Rows(result) => {
                            self.push_message_for(
                                cid,
                                format!("[{}] {head}: {} rows.", n + 1, result.row_count()),
                            );
                            results.push(StatementResult {
                                sql: step.sql,
                                started: step.started,
                                outcome: Ok(result),
                            });
                        }
                        StepOk::Affected(affected) => self.push_message_for(
                            cid,
                            format!("[{}] {head}: affected rows {affected}.", n + 1),
                        ),
                    }
                }
                Err(err) => {
                    failed = true;
                    self.push_message_for(cid, format!("[{}] {head}: {err}", n + 1));
                    results.push(StatementResult {
                        sql: step.sql,
                        started: step.started,
                        outcome: Err(err),
                    });
                }
            }
        }
        self.tabs[ix].result_connection = self
            .conn_by_id(cid)
            .and_then(|c| {
                c.session
                    .as_ref()
                    .map(|info| format!("{}  ·  {}", c.label(), info.database))
            })
            .unwrap_or_default();
        self.set_status_for(
            cid,
            if failed {
                format!("Script stopped at statement {total} (earlier changes are uncommitted).")
            } else {
                format!("Script completed: {total} statements (uncommitted).")
            },
        );
        let has_rows = !results.is_empty();
        self.tabs[ix].statement_results = results;
        if has_rows {
            self.show_statement_result(ix, 0, cx);
        } else {
            self.tabs[ix].results_tab = ResultsTab::Messages;
        }
        self.enforce_result_budget(cx);
    }

    /// The first rows of a data page while the rest is still on its way:
    /// shown at once, the connection stays busy until the whole page is in
    /// ([`Workbench::on_query_done`] then grows the grid in place).
    pub(crate) fn on_query_partial(
        &mut self,
        cid: u64,
        result: QueryResult,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conn_by_id(cid) else {
            return;
        };
        if conn.abandoned || !matches!(conn.pending_op, Some(PendingOp::Run)) {
            return;
        }
        let Some(ix) = self.target_ix_for(cid) else {
            return;
        };
        let rows = result.row_count();
        let column_names: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
        let Some(data) = self.tabs[ix].data.as_mut() else {
            return;
        };
        data.set_columns(column_names);
        data.partial_rows = rows;
        self.set_grid_result_for(cid, result, cx);
        self.tabs[ix].results_tab = ResultsTab::Results;
        self.set_status_for(
            cid,
            format!("First {rows} rows shown, loading the rest of the page…"),
        );
        cx.notify();
    }

    pub(super) fn on_query_done(
        &mut self,
        cid: u64,
        result: QueryResult,
        pending_sql: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let rows = result.row_count();
        let elapsed = result.elapsed.as_millis();
        let truncated = result.truncated;
        let limit_note = super::super::results::limit_note(&result);
        if let Some(sql) = pending_sql {
            self.record_run_for(cid, &sql);
        }
        let column_names: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
        let order_by = result.order_by.clone();
        let tab = self.target_tab_for_mut(cid);
        tab.elapsed_ms = Some(elapsed);
        // The first load is logged before the key order is known; once the
        // result names it, show the statement that actually ran.
        let mut relog = None;
        if let Some(data) = tab.data.as_mut() {
            if data.order_by.is_empty() && !order_by.is_empty() {
                relog = Some((
                    data.database.clone(),
                    data.owner.clone(),
                    data.name.clone(),
                    data.applied.clone(),
                    data.page,
                    order_by.clone(),
                ));
            }
            data.set_columns(column_names);
            data.order_by = order_by;
        }
        if let (Some((database, owner, name, filter, page, order)), true) =
            (relog, cid == self.conn.id)
        {
            let old = self.data_load_log(database.as_deref(), &owner, &name, &filter, page, &[]);
            let new = self.data_load_log(database.as_deref(), &owner, &name, &filter, page, &order);
            if old != new {
                self.replace_message(&old, new);
            }
        }
        // The page's first rows are on screen already: keep the grid's
        // scroll, selection and sort while the rest joins them.
        let grow = self
            .target_tab_for_mut(cid)
            .data
            .as_mut()
            .is_some_and(|d| std::mem::take(&mut d.partial_rows) > 0);
        if grow {
            self.grow_grid_result_for(cid, result, cx);
        } else {
            self.set_grid_result_for(cid, result, cx);
        }
        self.target_tab_for_mut(cid).results_tab = ResultsTab::Results;
        let is_data = self.target_tab_for_mut(cid).data.is_some();
        if is_data {
            let tab_id = self.target_tab_for_mut(cid).id;
            self.clob_result_changed(tab_id, cx);
            self.data_page_loaded(cid, cx);
        }
        self.set_status_for(
            cid,
            if truncated && is_data && limit_note.is_empty() {
                format!("{rows} rows in {elapsed} ms (more rows available)")
            } else if truncated && limit_note.is_empty() {
                format!("{rows} rows in {elapsed} ms (limited to {DEFAULT_MAX_ROWS} rows)")
            } else {
                format!("{rows} rows in {elapsed} ms{limit_note}")
            },
        );
        if !limit_note.is_empty() {
            self.push_message_for(cid, format!("{rows} rows{limit_note}"));
        }
    }

    pub(super) fn on_statement_executed(
        &mut self,
        cid: u64,
        affected: u64,
        pending_sql: Option<String>,
    ) {
        if let Some(sql) = pending_sql {
            self.record_run_for(cid, &sql);
        }
        self.target_tab_for_mut(cid).statement_results.clear();
        self.push_message_for(cid, format!("Affected rows: {affected}."));
        self.set_status_for(cid, format!("Affected rows: {affected} (uncommitted)."));
        self.target_tab_for_mut(cid).results_tab = ResultsTab::Messages;
    }

    pub(super) fn on_plan_tree(&mut self, cid: u64, nodes: Vec<PlanNode>, lines: Vec<String>) {
        let tab = self.target_tab_for_mut(cid);
        tab.plan_expanded = plan::all_plan_ids(&nodes);
        tab.plan_nodes = Some(nodes);
        tab.plan_lines = lines;
        tab.results_tab = ResultsTab::Plan;
        self.set_status_for(cid, "Explain plan ready.".to_string());
    }

    pub(super) fn on_plan_text(&mut self, cid: u64, lines: Vec<String>) {
        let tab = self.target_tab_for_mut(cid);
        tab.plan_nodes = None;
        tab.plan_lines = lines;
        tab.results_tab = ResultsTab::Plan;
        self.set_status_for(cid, "Explain plan ready.".to_string());
    }

    pub(super) fn on_cell_loaded(
        &mut self,
        cid: u64,
        rowid: String,
        column: String,
        value: ora_core::Value,
        cx: &mut Context<Self>,
    ) {
        let size = value.heap_bytes();
        // The value was read against the room left when it was asked for;
        // other results may have grown while it loaded.
        let room = result_room(self.open_result_bytes(cx));
        if size > room {
            self.set_status_for(cid, format!(
                "Cannot show the full value of {column}: it needs {} but only {} are left in the open results.",
                ora_core::fetch::human_bytes(size),
                ora_core::fetch::human_bytes(room)
            ));
            return;
        }
        let shown = self
            .target_tab_for_mut(cid)
            .table_state
            .update(cx, |state, cx| {
                let shown = state
                    .delegate_mut()
                    .set_cell(&rowid, &column, value)
                    .is_some();
                state.refresh(cx);
                shown
            });
        self.enforce_result_budget(cx);
        self.set_status_for(
            cid,
            if shown {
                format!("Loaded the full value of {column} ({size} bytes).")
            } else {
                "The row is no longer in the grid.".to_string()
            },
        );
    }
}
