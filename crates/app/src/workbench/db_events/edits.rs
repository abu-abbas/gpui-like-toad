//! Responses to Apply of a data tab's staged cell edits.

use super::*;

impl Workbench {
    pub(super) fn on_cells_updated(
        &mut self,
        cid: u64,
        done: usize,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.set_conn_state(cid, |c| {
            if done > 0 {
                let tab = c.pending_tab;
                c.note_write(u32::try_from(done).unwrap_or(u32::MAX), tab);
            }
        });
        self.edits_applied(cid, done, error, cx);
    }
}
