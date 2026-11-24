//! Responses to Commit and Rollback.

use super::*;

impl Workbench {
    pub(in crate::workbench) fn on_committed(&mut self, cid: u64, cx: &mut Context<Self>) {
        self.set_conn_state(cid, |c| {
            c.clear_uncommitted();
            c.status = "Commit succeeded.".to_string();
        });
        self.push_message_for(cid, "Commit succeeded.".to_string());
        self.tx_step_done(cid, true, cx);
        // Auto-saved edits are committed while the next cell may already
        // be staged: keep what was not sent yet.
        self.clear_edit_marks_for(cid, true, cx);
        if std::mem::take(&mut self.reload_after_commit)
            && cid == self.conn.id
            && self.inline_edit.is_none()
            && self
                .tabs
                .get(self.active_tab)
                .is_some_and(|t| t.data.as_ref().is_some_and(|d| !d.has_staged()))
        {
            self.reload_data(cx);
        } else if self.has_staged_edits() && self.auto_save() {
            self.auto_apply(cx);
        }
    }

    pub(super) fn on_rolled_back(&mut self, cid: u64, cx: &mut Context<Self>) {
        self.set_conn_state(cid, |c| {
            c.clear_uncommitted();
            c.status = "Rollback succeeded.".to_string();
        });
        self.push_message_for(cid, "Rollback succeeded.".to_string());
        self.tx_step_done(cid, true, cx);
        self.clear_edit_marks_for(cid, false, cx);
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
