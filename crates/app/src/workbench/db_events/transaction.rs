//! Responses to Commit and Rollback.

use super::*;

impl Workbench {
    pub(super) fn on_committed(&mut self, cid: u64, cx: &mut Context<Self>) {
        self.set_conn_state(cid, |c| {
            c.uncommitted = false;
            c.status = "Commit succeeded.".to_string();
        });
        self.push_message_for(cid, "Commit succeeded.".to_string());
        self.clear_edit_marks_for(cid, cx);
    }

    pub(super) fn on_rolled_back(&mut self, cid: u64, cx: &mut Context<Self>) {
        self.set_conn_state(cid, |c| {
            c.uncommitted = false;
            c.status = "Rollback succeeded.".to_string();
        });
        self.push_message_for(cid, "Rollback succeeded.".to_string());
        self.clear_edit_marks_for(cid, cx);
        // The grid still shows the rolled-back values. Only the tab the
        // user is looking at reloads; others refresh when opened.
        if cid == self.conn.id
            && self
                .tabs
                .get(self.active_tab)
                .is_some_and(|t| t.data.is_some())
        {
            self.reload_data(cx);
        }
    }
}
