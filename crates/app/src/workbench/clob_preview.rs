//! Lazy CLOB previews of a table's data tab.
//!
//! A page of a table comes back with CLOB cells as lengths only. The start
//! of each CLOB is then read in small batches, only for the rows on screen
//! plus a buffer, and again as the grid scrolls. A batch is one query by
//! ROWID. It does not make the connection busy; the grid keeps working
//! while it runs and a result that no longer matches the grid is dropped.

use std::ops::Range;
use std::time::Duration;

use super::*;

/// Wait after the last scroll before reading, so a fast scroll reads only
/// where it stops.
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);
/// Retry delay while the connection runs another operation.
const PREVIEW_RETRY: Duration = Duration::from_millis(400);
const PREVIEW_MAX_RETRIES: u32 = 25;
/// Rows read before and after the rows on screen.
const PREVIEW_BUFFER_ROWS: usize = 30;
/// Rows per query. Small, so a query the user queues behind it waits
/// one round trip, not a whole page.
const PREVIEW_BATCH_ROWS: usize = 60;
/// Rows assumed on screen before the grid reports its visible rows.
const FIRST_VISIBLE_ROWS: usize = 40;

/// Preview state of a data tab.
pub(super) struct PreviewState {
    /// Table rows (page-relative) last reported on screen.
    pub(super) visible: Range<usize>,
    /// Bumped on every scroll; a debounce tick for an older one is dropped.
    epoch: u64,
    /// Bumped when the grid gets a new result; a response read for an older
    /// one is dropped.
    result_gen: u64,
    inflight: bool,
    /// An error was already reported for this result.
    reported: bool,
}

impl PreviewState {
    pub(super) fn new() -> Self {
        PreviewState {
            visible: 0..FIRST_VISIBLE_ROWS,
            epoch: 0,
            result_gen: 0,
            inflight: false,
            reported: false,
        }
    }
}

impl Workbench {
    fn tab_ix_by_id(&self, tab_id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == tab_id)
    }

    fn preview_state_mut(&mut self, tab_id: u64) -> Option<&mut PreviewState> {
        let ix = self.tab_ix_by_id(tab_id)?;
        Some(&mut self.tabs[ix].data.as_mut()?.preview)
    }

    /// The grid of data tab `tab_id` shows other rows (`visible` is `None`
    /// when only the sorted or paged rows changed): read their CLOB starts
    /// once the scrolling settles.
    pub(super) fn clob_rows_changed(
        &mut self,
        tab_id: u64,
        visible: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.preview_state_mut(tab_id) else {
            return;
        };
        state.epoch += 1;
        let epoch = state.epoch;
        if let Some(visible) = visible {
            state.visible = visible;
        }
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            let _ = view.update(cx, |this, cx| {
                let current = this
                    .preview_state_mut(tab_id)
                    .is_some_and(|state| state.epoch == epoch);
                if current {
                    this.pump_clob_previews(tab_id, 0, cx);
                }
            });
        })
        .detach();
    }

    /// A new result is in the grid of the data tab `tab_id`.
    pub(super) fn clob_result_changed(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let Some(state) = self.preview_state_mut(tab_id) else {
            return;
        };
        state.result_gen += 1;
        state.reported = false;
        self.clob_rows_changed(tab_id, None, cx);
    }

    /// Read the next batch of previews for the rows now on screen, if any
    /// cells are still unread.
    fn pump_clob_previews(&mut self, tab_id: u64, attempt: u32, cx: &mut Context<Self>) {
        let Some(ix) = self.tab_ix_by_id(tab_id) else {
            return;
        };
        let tab = &self.tabs[ix];
        let Some(data) = tab.data.as_ref() else {
            return;
        };
        // Only the tab's own, open connection reads for it.
        if data.preview.inflight
            || data.view != DataView::Data
            || !data.page.rowid
            || tab.conn != Some(self.conn.id)
            || self.conn.db.is_none()
        {
            return;
        }
        if self.conn.busy {
            // Another operation runs: try again shortly instead of
            // queueing behind it.
            if attempt < PREVIEW_MAX_RETRIES {
                cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(PREVIEW_RETRY).await;
                    let _ = view.update(cx, |this, cx| {
                        this.pump_clob_previews(tab_id, attempt + 1, cx)
                    });
                })
                .detach();
            }
            return;
        }
        let (owner, name) = (data.owner.clone(), data.name.clone());
        let visible = data.preview.visible.clone();
        let result_gen = data.preview.result_gen;
        let Some(batch) = tab.table_state.update(cx, |state, _| {
            state.delegate_mut().take_preview_batch(
                visible,
                PREVIEW_BUFFER_ROWS,
                PREVIEW_BATCH_ROWS,
            )
        }) else {
            return;
        };
        let conn_id = self.conn.id;
        let Some(db) = self.conn.db.as_ref() else {
            return;
        };
        let rx = db.request(Job::ClobPreviews {
            owner,
            name,
            columns: batch.columns,
            rowids: batch.rowids,
        });
        if let Some(state) = self.preview_state_mut(tab_id) {
            state.inflight = true;
        }
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = view.update(cx, |this, cx| {
                this.on_clob_previews(tab_id, conn_id, result_gen, resp, cx)
            });
        })
        .detach();
    }

    fn on_clob_previews(
        &mut self,
        tab_id: u64,
        conn_id: u64,
        result_gen: u64,
        resp: Result<Response, RecvError>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.tab_ix_by_id(tab_id) else {
            return;
        };
        let Some(state) = self.preview_state_mut(tab_id) else {
            return;
        };
        state.inflight = false;
        // The grid got a new result (reload, filter, page) while this read:
        // its rows are not the ones asked for.
        if state.result_gen != result_gen {
            self.pump_clob_previews(tab_id, 0, cx);
            return;
        }
        match resp {
            Ok(Response::ClobPreviews(previews)) => {
                let room = super::results::result_room(self.open_result_bytes(cx));
                self.tabs[ix].table_state.update(cx, |state, cx| {
                    if state.delegate_mut().apply_previews(previews, room) > 0 {
                        state.refresh(cx);
                    }
                });
                cx.notify();
            }
            Ok(Response::Failed(message)) => self.report_clob_failure(tab_id, conn_id, &message),
            Ok(_) => {}
            Err(_) => self.report_clob_failure(tab_id, conn_id, "The database worker stopped."),
        }
        self.pump_clob_previews(tab_id, 0, cx);
    }

    /// Say once per result that the CLOB starts could not be read; the
    /// cells keep showing their length.
    fn report_clob_failure(&mut self, tab_id: u64, conn_id: u64, message: &str) {
        let first = self
            .preview_state_mut(tab_id)
            .is_some_and(|state| !std::mem::replace(&mut state.reported, true));
        if first {
            self.push_message_for(
                conn_id,
                format!("Could not read the start of some CLOB values: {message}"),
            );
        }
    }
}
