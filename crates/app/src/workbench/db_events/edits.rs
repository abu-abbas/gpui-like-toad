//! Responses to Apply of a data tab's staged cell edits.

use super::*;

impl Workbench {
    pub(super) fn on_cells_updated(
        &mut self,
        done: usize,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.conn.uncommitted |= done > 0;
        self.edits_applied(done, error, cx);
    }
}
