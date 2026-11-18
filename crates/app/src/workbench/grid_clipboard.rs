use super::*;
use std::rc::Rc;

impl Workbench {
    pub(super) fn can_insert_rows(&self) -> bool {
        let tab = self.active_tab();
        !self.conn.busy
            && self.conn.session.is_some()
            && !self.conn.access().read_only
            && tab.conn == Some(self.conn.id)
            && tab.last_result.is_some()
            && tab.data.as_ref().is_some_and(|data| {
                data.view == DataView::Data
                    && data.kind == ObjectKind::Table
                    && data.columns.iter().any(|c| !data.hidden.contains(c))
            })
    }

    pub(super) fn paste_grid_rows(
        &mut self,
        anchor: Option<(usize, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_insert_rows() {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        match ora_core::import::clipboard_rows(&text) {
            Ok(rows) if !rows.is_empty() => self.stage_grid_rows(rows, anchor, window, cx),
            Err(error) => {
                self.conn.status = error;
                cx.notify();
            }
            _ => {}
        }
    }

    pub(super) fn stage_grid_rows(
        &mut self,
        values: Vec<Vec<String>>,
        anchor: Option<(usize, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_insert_rows() {
            return;
        }
        let data = self.data_mut().unwrap();
        let columns: Vec<_> = data
            .columns
            .iter()
            .enumerate()
            .filter(|(_, name)| !data.hidden.contains(*name))
            .map(|(ix, _)| ix)
            .collect();
        let (start, col) = if let Some((row, name)) = anchor {
            let Some(col) = columns.iter().position(|&ix| data.columns[ix] == name) else {
                return;
            };
            (row, col)
        } else {
            let mut start = data.insert_rows.len();
            if values.first().is_some_and(|r| !r.is_empty())
                && data
                    .insert_rows
                    .last()
                    .is_some_and(|r| r.iter().all(|(_, f)| f.read(cx).value().is_empty()))
            {
                start -= 1;
            }
            (start, 0)
        };
        if values.iter().any(|row| row.len() > columns.len() - col) {
            self.conn.status = "Clipboard has more columns than fit in the visible grid.".into();
            cx.notify();
            return;
        }
        let drafts = Rc::make_mut(&mut data.insert_rows);
        for (offset, row) in values.into_iter().enumerate() {
            let ix = start + offset;
            if ix == drafts.len() {
                drafts.push(
                    data.columns
                        .iter()
                        .map(|name| (name.clone(), cx.new(|cx| InputState::new(window, cx))))
                        .collect(),
                );
            }
            for (&index, value) in columns[col..].iter().zip(row) {
                Self::set_input(&drafts[ix][index].1, value, window, cx);
            }
        }
        let focus = drafts[start][columns[col]].1.clone();
        let rows = data.insert_rows.clone();
        self.active_tab().table_state.update(cx, |state, cx| {
            state.delegate_mut().insert_rows = rows;
            let index = state.delegate().page_rows() + start;
            state.set_selected_row(index, cx);
            state.scroll_to_row(index, cx);
            state.scroll_to_col(1 + col, cx);
        });
        window.defer(cx, move |win, cx| {
            focus.update(cx, |state, cx| state.focus(win, cx))
        });
        cx.notify();
    }

    pub(super) fn focused_insert_cell(&self, window: &Window, cx: &App) -> Option<(usize, String)> {
        self.active_tab()
            .data
            .as_ref()?
            .insert_rows
            .iter()
            .enumerate()
            .find_map(|(ix, row)| {
                row.iter()
                    .find(|(_, field)| field.read(cx).focus_handle(cx).is_focused(window))
                    .map(|(name, _)| (ix, name.clone()))
            })
    }
}
