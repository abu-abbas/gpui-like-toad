//! Responses to Run and Explain in SQL tabs.

use super::*;

impl Workbench {
    pub(super) fn on_script_done(
        &mut self,
        results: Vec<crate::worker::StatementResult>,
        cx: &mut Context<Self>,
    ) {
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
        self.enforce_result_budget(cx);
    }

    pub(super) fn on_query_done(
        &mut self,
        result: QueryResult,
        pending_sql: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let rows = result.row_count();
        let elapsed = result.elapsed.as_millis();
        let truncated = result.truncated;
        let limit_note = super::super::results::limit_note(&result);
        if let Some(sql) = pending_sql {
            self.record_run(&sql);
        }
        let column_names: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
        let tab = self.target_tab_mut();
        tab.elapsed_ms = Some(elapsed);
        if let Some(data) = tab.data.as_mut() {
            data.set_columns(column_names);
        }
        self.set_grid_result(result, cx);
        self.target_tab_mut().results_tab = ResultsTab::Results;
        let is_data = self.target_tab_mut().data.is_some();
        self.conn.status = if truncated && is_data && limit_note.is_empty() {
            format!("{rows} rows in {elapsed} ms (more rows available)")
        } else if truncated && limit_note.is_empty() {
            format!("{rows} rows in {elapsed} ms (limited to {DEFAULT_MAX_ROWS} rows)")
        } else {
            format!("{rows} rows in {elapsed} ms{limit_note}")
        };
        if !limit_note.is_empty() {
            self.push_message(format!("{rows} rows{limit_note}"));
        }
    }

    pub(super) fn on_statement_executed(&mut self, affected: u64, pending_sql: Option<String>) {
        if let Some(sql) = pending_sql {
            self.record_run(&sql);
        }
        self.target_tab_mut().statement_results.clear();
        self.push_message(format!("Affected rows: {affected}."));
        self.conn.status = format!("Affected rows: {affected} (uncommitted).");
        self.target_tab_mut().results_tab = ResultsTab::Messages;
    }

    pub(super) fn on_plan_tree(&mut self, nodes: Vec<PlanNode>, lines: Vec<String>) {
        let tab = self.target_tab_mut();
        tab.plan_expanded = plan::all_plan_ids(&nodes);
        tab.plan_nodes = Some(nodes);
        tab.plan_lines = lines;
        tab.results_tab = ResultsTab::Plan;
        self.conn.status = "Explain plan ready.".to_string();
    }

    pub(super) fn on_plan_text(&mut self, lines: Vec<String>) {
        let tab = self.target_tab_mut();
        tab.plan_nodes = None;
        tab.plan_lines = lines;
        tab.results_tab = ResultsTab::Plan;
        self.conn.status = "Explain plan ready.".to_string();
    }

    pub(super) fn on_cell_loaded(
        &mut self,
        rowid: String,
        column: String,
        value: ora_core::Value,
        cx: &mut Context<Self>,
    ) {
        let size = value.heap_bytes();
        let shown = self.target_tab_mut().table_state.update(cx, |state, cx| {
            let shown = state
                .delegate_mut()
                .set_cell(&rowid, &column, value)
                .is_some();
            state.refresh(cx);
            shown
        });
        self.conn.status = if shown {
            format!("Loaded the full value of {column} ({size} bytes).")
        } else {
            "The row is no longer in the grid.".to_string()
        };
    }
}
