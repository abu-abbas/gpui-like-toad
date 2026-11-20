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
        let tab = self.target_tab_for_mut(cid);
        tab.elapsed_ms = Some(elapsed);
        if let Some(data) = tab.data.as_mut() {
            data.set_columns(column_names);
        }
        self.set_grid_result_for(cid, result, cx);
        self.target_tab_for_mut(cid).results_tab = ResultsTab::Results;
        let is_data = self.target_tab_for_mut(cid).data.is_some();
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
