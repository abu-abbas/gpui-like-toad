//! Total row count of a data tab ("Rows 1–1000 of 54,321").
//!
//! A `COUNT(*)` can take long on a big table, and the connection runs one
//! statement at a time, so the count is never started on its own: the
//! footer offers a Count button. The count does not make the connection
//! busy, and its answer is dropped when the filter changed meanwhile.

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

impl Workbench {
    /// Count the rows of the active data tab under its applied filter.
    pub(super) fn count_rows(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let Some(data) = tab.data.as_ref() else {
            return;
        };
        if data.count.value == RowCount::Counting {
            return;
        }
        if self.conn.busy || tab.conn != Some(self.conn.id) {
            self.conn.status = "An operation is already running.".to_string();
            cx.notify();
            return;
        }
        let Some(db) = self.conn.db.as_ref() else {
            return;
        };
        let tab_id = tab.id;
        let conn_id = self.conn.id;
        let generation = data.count.gen;
        let (database, owner, name) =
            (data.database.clone(), data.owner.clone(), data.name.clone());
        let rx = db.request(Job::CountTable {
            database,
            owner,
            name,
            filter: data.applied.clone(),
        });
        if let Some(data) = self.tabs[self.active_tab].data.as_mut() {
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
    use super::group_digits;

    #[test]
    fn digits_are_grouped_by_three() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(54321), "54,321");
        assert_eq!(group_digits(1234567), "1,234,567");
    }
}
