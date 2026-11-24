//! Total row count of a data tab ("Rows 1–1000 of 54,321").
//!
//! A `COUNT(*)` can take long on a big table, and the connection runs one
//! statement at a time, so by default it is not started on its own: the
//! footer offers a Count button. Two things make the total show up
//! without a click:
//!
//! * A page that is not full already tells the total (`offset + rows`),
//!   for free, with no extra statement.
//! * The "Auto" toggle (off by default, remembered in `settings.json`)
//!   counts right after a page loads, once per filter. The count waits in
//!   the worker's queue, so a slow count can delay the next page on that
//!   connection; that is why it is opt-in.
//!
//! The count does not make the connection busy, and its answer is dropped
//! when the filter changed meanwhile.

use super::*;

/// What a data tab knows about the number of rows under its filter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RowCount {
    #[default]
    Unknown,
    Counting,
    Known(u64),
}

/// Count state of a data tab: the value, and a generation that moves when
/// the filter changes so an answer for an older filter is ignored.
#[derive(Default)]
pub(super) struct CountState {
    pub(super) value: RowCount,
    gen: u64,
}

impl CountState {
    /// The filter changed: the old total no longer applies.
    pub(super) fn reset(&mut self) {
        self.value = RowCount::Unknown;
        self.gen += 1;
    }
}

/// `54321` -> `54,321`.
pub(super) fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut groups: Vec<&str> = digits
        .as_bytes()
        .rchunks(3)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    groups.reverse();
    groups.join(",")
}

/// Number of pages for `total` rows at `limit` rows per page (at least 1).
pub(super) fn page_count(total: u64, limit: usize) -> u64 {
    total.div_ceil(limit.max(1) as u64).max(1)
}

/// Offset of the 1-based `page`, clamped to `1..=last` when the last page
/// is known.
pub(super) fn page_offset(page: u64, limit: usize, last: Option<u64>) -> usize {
    let page = page.max(1).min(last.unwrap_or(u64::MAX));
    (page - 1)
        .saturating_mul(limit as u64)
        .min(usize::MAX as u64) as usize
}

impl Workbench {
    /// Count the rows of the active data tab under its applied filter.
    pub(super) fn count_rows(&mut self, cx: &mut Context<Self>) {
        let ix = self.active_tab;
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        if tab
            .data
            .as_ref()
            .is_some_and(|d| d.count.value == RowCount::Counting)
        {
            return;
        }
        if self.conn.busy || tab.conn != Some(self.conn.id) {
            self.conn.status = "An operation is already running.".to_string();
            cx.notify();
            return;
        }
        self.start_count(ix, cx);
    }

    /// Turn the Auto toggle on or off and remember it.
    pub(super) fn set_auto_count(&mut self, on: bool, cx: &mut Context<Self>) {
        self.auto_count = on;
        if let Err(err) = store::save_auto_count(on) {
            self.push_message(format!("Failed to save the Auto count setting: {err}"));
        }
        if on {
            // Count the page that is already on screen.
            let ix = self.active_tab;
            self.count_if_wanted(ix, cx);
        }
        cx.notify();
    }

    /// A data page of connection `cid` just loaded: a page that is not
    /// full tells the total for free; otherwise count it when Auto is on.
    pub(super) fn data_page_loaded(&mut self, cid: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.target_ix_for(cid) else {
            return;
        };
        let tab = &mut self.tabs[ix];
        let rows = tab
            .last_result
            .as_ref()
            .map(|r| (r.row_count(), r.truncated));
        let Some((rows, truncated)) = rows else {
            return;
        };
        let Some(data) = tab.data.as_mut() else {
            return;
        };
        if !truncated
            && data.count.value != RowCount::Counting
            && (rows > 0 || data.page.offset == 0)
        {
            data.count.value = RowCount::Known((data.page.offset + rows) as u64);
        } else if truncated && cid == self.conn.id {
            self.count_if_wanted(ix, cx);
        }
    }

    /// Start the automatic count of tab `ix` when Auto is on and the tab
    /// does not know its total yet.
    fn count_if_wanted(&mut self, ix: usize, cx: &mut Context<Self>) {
        let wanted = self.auto_count
            && self.tabs.get(ix).is_some_and(|t| {
                t.conn == Some(self.conn.id)
                    && t.last_result.as_ref().is_some_and(|r| r.truncated)
                    && t.data
                        .as_ref()
                        .is_some_and(|d| d.count.value == RowCount::Unknown)
            });
        if wanted {
            self.start_count(ix, cx);
        }
    }

    /// Send the count of tab `ix` to the worker.
    fn start_count(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        let Some(data) = tab.data.as_ref() else {
            return;
        };
        let Some(db) = self.conn.db.as_ref() else {
            return;
        };
        let tab_id = tab.id;
        let conn_id = self.conn.id;
        let generation = data.count.gen;
        let (owner, name) = (data.owner.clone(), data.name.clone());
        let rx = db.request(Job::CountTable {
            owner,
            name,
            filter: data.applied.clone(),
        });
        if let Some(data) = self.tabs[ix].data.as_mut() {
            data.count.value = RowCount::Counting;
        }
        cx.notify();
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = view.update(cx, |this, cx| {
                this.on_row_count(tab_id, conn_id, generation, resp, cx)
            });
        })
        .detach();
    }

    fn on_row_count(
        &mut self,
        tab_id: u64,
        conn_id: u64,
        generation: u64,
        resp: Result<Response, RecvError>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let Some(data) = self.tabs[ix].data.as_mut() else {
            return;
        };
        if data.count.gen != generation {
            return;
        }
        match resp {
            Ok(Response::RowCount(n)) => data.count.value = RowCount::Known(n),
            other => {
                data.count.value = RowCount::Unknown;
                let message = match other {
                    Ok(Response::Failed(message)) => message,
                    _ => "The database worker stopped.".to_string(),
                };
                self.push_message_for(conn_id, format!("Could not count the rows: {message}"));
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{group_digits, page_count, page_offset};

    #[test]
    fn pages_and_offsets() {
        assert_eq!(page_count(0, 1000), 1);
        assert_eq!(page_count(1000, 1000), 1);
        assert_eq!(page_count(1001, 1000), 2);
        assert_eq!(page_count(54321, 1000), 55);
        assert_eq!(page_offset(1, 1000, None), 0);
        assert_eq!(page_offset(3, 1000, None), 2000);
        assert_eq!(page_offset(0, 1000, None), 0);
        assert_eq!(page_offset(99, 1000, Some(55)), 54000);
    }

    #[test]
    fn digits_are_grouped_by_three() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(54321), "54,321");
        assert_eq!(group_digits(1234567), "1,234,567");
    }
}
