//! Responses to Commit and Rollback.

use super::*;

impl Workbench {
    pub(super) fn on_committed(&mut self, cx: &mut Context<Self>) {
        self.conn.uncommitted = false;
        self.push_message("Commit succeeded.".to_string());
        self.conn.status = "Commit succeeded.".to_string();
        self.clear_edit_marks(cx);
    }

    pub(super) fn on_rolled_back(&mut self, background: bool, cx: &mut Context<Self>) {
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
}
