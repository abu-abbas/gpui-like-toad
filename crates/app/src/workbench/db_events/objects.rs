//! Responses to explorer object actions (View Script, Export, Import,
//! Refresh Materialized View) and to a data tab's Structure, Index and
//! DDL views.

use super::*;

impl Workbench {
    pub(super) fn on_object_script(
        &mut self,
        cid: u64,
        owner: String,
        name: String,
        ddl: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The script opens in a tab of its own connection: make that the
        // active one, as the user asked for it.
        self.activate_conn(cid, cx);
        self.new_tab_with_sql(ddl, window, cx);
        let tab = self.active_tab_mut();
        tab.title = format!("{name}.sql");
        tab.kind = TabKind::Script;
        self.set_status_for(cid, format!("Script {owner}.{name}."));
    }

    pub(super) fn on_table_exported(
        &mut self,
        cid: u64,
        (owner, name): (String, String),
        path: std::path::PathBuf,
        rows: usize,
        truncated: bool,
        clipped: usize,
    ) {
        let mut cap = if truncated {
            format!(" (limited to {} rows)", ora_core::EXPORT_MAX_ROWS)
        } else {
            String::new()
        };
        if clipped > 0 {
            cap.push_str(&format!(
                "; {clipped} cells longer than {} MiB were cut",
                ora_core::fetch::EXPORT_MAX_CELL_BYTES / (1024 * 1024)
            ));
        }
        self.push_message_for(
            cid,
            format!(
                "Exported {rows} rows from {owner}.{name} to {}{cap}",
                path.display()
            ),
        );
        self.set_status_for(cid, format!("Exported {rows} rows from {name}."));
    }

    pub(super) fn on_materialized_view_refreshed(
        &mut self,
        cid: u64,
        owner: String,
        name: String,
        cx: &mut Context<Self>,
    ) {
        self.push_message_for(cid, format!("Refreshed materialized view {owner}.{name}."));
        self.set_status_for(cid, format!("Refreshed {name}."));
        self.target_tab_for_mut(cid).results_tab = ResultsTab::Messages;
        // Show the new rows when that view is open in a data tab.
        let open_here = self.target_tab_for_mut(cid).data.as_ref().is_some_and(|d| {
            d.owner.eq_ignore_ascii_case(&owner) && d.name.eq_ignore_ascii_case(&name)
        });
        // Only the connection the user works on refetches; a background
        // one shows the new rows when its tab is next reloaded.
        if open_here && cid == self.conn.id {
            self.reload_data(cx);
        }
    }

    pub(super) fn on_csv_imported(&mut self, cid: u64, owner: String, name: String, rows: u64) {
        self.push_message_for(
            cid,
            format!("Imported {rows} rows into {owner}.{name} (uncommitted)."),
        );
        self.set_status_for(
            cid,
            format!("Imported {rows} rows into {name} (uncommitted)."),
        );
        self.target_tab_for_mut(cid).results_tab = ResultsTab::Messages;
    }

    pub(super) fn on_data_structure(
        &mut self,
        cid: u64,
        defs: Vec<ora_core::ColumnDef>,
        cx: &mut Context<Self>,
    ) {
        let result = schema::describe_to_result(&defs);
        self.set_status_for(cid, format!("Columns: {}.", result.row_count()));
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

    pub(super) fn on_data_indexes(
        &mut self,
        cid: u64,
        indexes: Vec<ora_core::IndexDef>,
        cx: &mut Context<Self>,
    ) {
        let result = schema::indexes_to_result(&indexes);
        self.set_status_for(cid, format!("{} index.", result.row_count()));
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

    pub(super) fn on_data_ddl(
        &mut self,
        cid: u64,
        ddl: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_status_for(cid, "DDL loaded.".to_string());
        if let Some(editor) = self
            .target_tab_for_mut(cid)
            .data
            .as_ref()
            .map(|d| d.ddl.clone())
        {
            editor.update(cx, |state, cx| state.set_value(ddl, window, cx));
        }
    }
}
