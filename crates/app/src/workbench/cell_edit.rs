//! Editing cells of a data tab. A change is first *staged* (the grid shows
//! the new value, tinted amber), then *applied* (`UPDATE ... WHERE ROWID`,
//! tinted blue), and finally leaves the tint when the user commits or rolls
//! back with the toolbar buttons, as in Toad: nothing is committed behind
//! the user's back.

use std::collections::HashMap;

use ora_core::Value;

use super::*;
use crate::results::{CellEditRequest, CellKey, CellMark, InlineCell};
use crate::worker::CellUpdate;

/// One edited cell of a data tab.
#[derive(Clone, Debug)]
pub(super) struct CellEdit {
    pub(super) rowid: String,
    pub(super) column: String,
    /// New value; `None` is NULL.
    pub(super) new: Option<String>,
    /// The UPDATE ran (uncommitted).
    pub(super) applied: bool,
    /// A re-edit of a cell that was already applied: discarding it goes
    /// back to "applied", not to unedited.
    pub(super) was_applied: bool,
}

impl CellEdit {
    fn mark(&self) -> CellMark {
        if self.applied {
            CellMark::Applied
        } else {
            CellMark::Staged
        }
    }
}

/// The in-place editor of one grid cell.
pub(super) struct InlineEdit {
    request: CellEditRequest,
    input: Entity<InputState>,
    _blur: Subscription,
}

/// The edit dialog.
pub(super) struct CellEditor {
    request: CellEditRequest,
    input: Entity<InputState>,
    _sub: Subscription,
}

impl Workbench {
    /// True (with a status message) when the active connection is Read
    /// Only, so edits are refused before anything is staged. The worker
    /// refuses the write too; this just keeps the UI from queuing it.
    pub(super) fn blocked_by_read_only(&mut self, what: &str, cx: &mut Context<Self>) -> bool {
        if !self.conn.access().read_only {
            return false;
        }
        self.conn.status = format!("This connection is read-only: {what}.");
        cx.notify();
        true
    }

    /// Any data tab of the active connection with edits that were not
    /// sent yet.
    pub(super) fn has_staged_edits(&self) -> bool {
        let conn = Some(self.conn.id);
        self.tabs
            .iter()
            .filter(|t| t.conn == conn)
            .filter_map(|t| t.data.as_ref())
            .any(|d| d.has_staged())
    }

    /// Open the dialog for one cell.
    pub(super) fn open_cell_editor(
        &mut self,
        request: CellEditRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conn.busy {
            self.conn.status = "An operation is already running.".to_string();
            cx.notify();
            return;
        }
        if self.blocked_by_read_only("cannot edit cells", cx) {
            return;
        }
        let initial = if request.value.is_null() {
            String::new()
        } else {
            request.value.display()
        };
        let input = cx.new(|cx| {
            InputState::new(&mut *window, cx)
                .placeholder("New value")
                .default_value(initial)
        });
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_cell_editor(false, window, cx);
            }
        });
        // The table takes focus when the click finishes, so focus the box
        // after that.
        let focus_input = input.clone();
        window.defer(cx, move |window, cx| {
            focus_input.update(cx, |state, cx| state.focus(window, cx));
        });
        self.cell_editor = Some(CellEditor {
            request,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    pub(super) fn close_cell_editor(&mut self, cx: &mut Context<Self>) {
        self.cell_editor = None;
        cx.notify();
    }

    /// Stage the dialog's value (or NULL) for its cell.
    fn save_cell_editor(&mut self, null: bool, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.cell_editor.take() else {
            return;
        };
        let typed = editor.input.read(cx).value().to_string();
        // Oracle stores an empty string as NULL, so an empty box is NULL.
        let new = if null || typed.is_empty() {
            None
        } else {
            Some(typed)
        };
        self.stage_edit(editor.request, new, cx);
    }

    /// Paste the clipboard into the picked cells of the active data tab
    /// as staged edits (amber), the same as typing each value in the edit
    /// dialog. One value fills every picked cell; a block of tab
    /// separated lines is laid out from the top-left picked cell.
    pub(super) fn paste_into_cells(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        let ready = tab.conn == Some(self.conn.id)
            && self.conn.session.is_some()
            && tab.data.as_ref().is_some_and(|d| d.view == DataView::Data);
        if !ready {
            return;
        }
        if self.conn.busy {
            self.conn.status = "An operation is already running.".to_string();
            cx.notify();
            return;
        }
        if self.conn.access().read_only {
            self.conn.status = "This connection is read-only: cannot edit cells.".to_string();
            cx.notify();
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let values = match ora_core::import::clipboard_rows(&text) {
            Ok(values) => values,
            Err(error) => {
                self.conn.status = error;
                cx.notify();
                return;
            }
        };
        let table = self.active_tab().table_state.clone();
        let paste = table.read(cx).delegate().cell_paste(&values);
        let count = paste.edits.len();
        for (request, new) in paste.edits {
            self.stage_edit(request, new, cx);
        }
        table.update(cx, |state, cx| {
            state.delegate_mut().pick_pasted_block(&values);
            cx.notify();
        });
        self.conn.status = match (count, paste.clipped) {
            (0, 0) => "The clipboard values match the existing cells.".to_string(),
            (n, 0) => {
                format!("Pasted cells: {n}. Click Apply to send the changes to the database.")
            }
            (n, c) => {
                format!("Pasted cells: {n}. Values outside the visible rows or columns: {c}.")
            }
        };
        cx.notify();
    }

    fn stage_edit(
        &mut self,
        request: CellEditRequest,
        new: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let unchanged = match &new {
            None => request.value.is_null(),
            Some(text) => !request.value.is_null() && request.value.display() == *text,
        };
        let tab_ix = self.active_tab;
        let Some(data) = self.tabs.get_mut(tab_ix).and_then(|t| t.data.as_mut()) else {
            return;
        };
        let existing = data
            .edits
            .iter()
            .position(|e| e.rowid == request.rowid && e.column == request.column);
        if unchanged && existing.is_none() {
            cx.notify();
            return;
        }
        match existing {
            Some(ix) => {
                let edit = &mut data.edits[ix];
                edit.new = new.clone();
                if edit.applied {
                    edit.applied = false;
                    edit.was_applied = true;
                }
            }
            None => data.edits.push(CellEdit {
                rowid: request.rowid.clone(),
                column: request.column.clone(),
                new: new.clone(),
                applied: false,
                was_applied: false,
            }),
        }
        let shown = new.map_or(Value::Null, Value::Text);
        let table = self.tabs[tab_ix].table_state.clone();
        table.update(cx, |state, _| {
            state
                .delegate_mut()
                .set_cell(&request.rowid, &request.column, shown);
        });
        self.sync_pending(tab_ix, cx);
        self.conn.status = "Changes staged. Click Apply to send them to the database.".to_string();
    }

    /// Push the tab's edit marks to its grid.
    pub(super) fn sync_pending(&mut self, tab_ix: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(tab_ix) else {
            return;
        };
        let marks: HashMap<(String, String), CellMark> = tab
            .data
            .as_ref()
            .map(|d| {
                d.edits
                    .iter()
                    .map(|e| ((e.rowid.clone(), e.column.clone()), e.mark()))
                    .collect()
            })
            .unwrap_or_default();
        tab.table_state.update(cx, |state, cx| {
            state.delegate_mut().set_pending(marks);
            cx.notify();
        });
        cx.notify();
    }

    /// Connections where edits are sent and committed right away, as in
    /// Navicat: everything but Oracle (whose transactions are explicit,
    /// as in Toad) and Production-flagged connections.
    pub(super) fn auto_save(&self) -> bool {
        self.active_kind() != DbKind::Oracle
            && self.conn.access().environment != ConnectionEnvironment::Production
    }

    /// Send what is staged when the connection saves by itself.
    pub(super) fn auto_apply(&mut self, cx: &mut Context<Self>) {
        if self.auto_save() && !self.conn.busy && self.has_staged_edits() {
            self.apply_edits(cx);
        }
    }

    /// Edit one cell where it is (double click, Enter, F2, Tab).
    pub(super) fn open_inline_editor(
        &mut self,
        request: CellEditRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.blocked_by_read_only("cannot edit cells", cx) {
            return;
        }
        // The previous cell keeps what was typed.
        self.close_inline(true, cx);
        let initial = if request.value.is_null() {
            String::new()
        } else {
            request.value.display()
        };
        let input = cx.new(|cx| {
            InputState::new(&mut *window, cx)
                .placeholder("NULL")
                .default_value(initial)
        });
        // Clicking elsewhere ends the edit and keeps the value.
        let blur = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Blur) && this.close_inline(true, cx).is_some() {
                this.auto_apply(cx);
            }
        });
        let table = self.active_tab().table_state.clone();
        let shown = InlineCell {
            rowid: request.rowid.clone(),
            column: request.column.clone(),
            input: input.clone(),
        };
        table.update(cx, |state, cx| {
            state.delegate_mut().set_inline(Some(shown));
            cx.notify();
        });
        self.inline_edit = Some(InlineEdit {
            request,
            input: input.clone(),
            _blur: blur,
        });
        // The table takes focus while the click is handled: focus the box
        // after that.
        window.defer(cx, move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
        cx.notify();
    }

    /// Close the in-place editor; `save` stages what was typed. Returns the
    /// cell it was editing.
    fn close_inline(&mut self, save: bool, cx: &mut Context<Self>) -> Option<CellEditRequest> {
        let edit = self.inline_edit.take()?;
        let typed = edit.input.read(cx).value().to_string();
        let table = self.active_tab().table_state.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().set_inline(None);
            cx.notify();
        });
        if save {
            // Oracle stores an empty string as NULL, so an empty box is NULL.
            let new = (!typed.is_empty()).then_some(typed);
            self.stage_edit(edit.request.clone(), new, cx);
        }
        Some(edit.request)
    }

    /// Tab / Shift+Tab / Enter / Escape in an insert-row field or the
    /// in-place editor.
    pub(super) fn handle_cell_key(
        &mut self,
        key: CellKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.inline_edit.as_ref().map(|e| e.request.pos) else {
            return self.insert_key(key, window, cx);
        };
        if key == CellKey::Cancel {
            self.close_inline(false, cx);
            cx.notify();
            return;
        }
        self.close_inline(true, cx);
        let table = self.active_tab().table_state.clone();
        let target = table.read(cx).delegate().neighbor_request(from, key);
        // Leaving the row sends the change when the connection saves by itself.
        if target.as_ref().is_none_or(|t| t.pos.0 != from.0) {
            self.auto_apply(cx);
        }
        let Some(target) = target else {
            cx.notify();
            return;
        };
        let at = table.update(cx, |state, cx| {
            let at = state.delegate_mut().pick_request(&target);
            if let Some((row, col)) = at {
                state.scroll_to_row(row, cx);
                state.scroll_to_col(col, cx);
            }
            at
        });
        if at.is_some() {
            self.open_inline_editor(target, window, cx);
        }
    }

    /// Enter / F2 on the picked cell: edit it in place.
    pub(super) fn edit_picked_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.inline_edit.is_some() || self.focused_insert_cell(window, cx).is_some() {
            return false;
        }
        let tab = self.active_tab();
        if tab.conn != Some(self.conn.id)
            || tab.data.as_ref().is_none_or(|d| d.view != DataView::Data)
        {
            return false;
        }
        let Some(request) = tab.table_state.read(cx).delegate().picked_edit_request() else {
            return false;
        };
        self.open_inline_editor(request, window, cx);
        true
    }

    /// Tab / Enter between the fields of the rows being added.
    fn insert_key(&mut self, key: CellKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row, name)) = self.focused_insert_cell(window, cx) else {
            return;
        };
        let Some(data) = self.data_mut() else {
            return;
        };
        let shown: Vec<usize> = data
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| !data.hidden.contains(*c))
            .map(|(ix, _)| ix)
            .collect();
        let Some(pos) = shown.iter().position(|&ix| data.columns[ix] == name) else {
            return;
        };
        let (mut r, mut c) = (row, pos as isize);
        match key {
            CellKey::Cancel => return,
            CellKey::Next | CellKey::Done => c += 1,
            CellKey::Prev | CellKey::Up => c -= 1,
        }
        if c < 0 {
            if r == 0 {
                return;
            }
            r -= 1;
            c = shown.len() as isize - 1;
        } else if c as usize >= shown.len() {
            r += 1;
            c = 0;
        }
        let Some(field) = data
            .insert_rows
            .get(r)
            .map(|fields| fields[shown[c as usize]].1.clone())
        else {
            // Past the last field of the last new row: send it.
            self.auto_apply(cx);
            return;
        };
        let table = self.active_tab().table_state.clone();
        table.update(cx, |state, cx| {
            let index = state.delegate().page_rows() + r;
            state.scroll_to_row(index, cx);
            state.scroll_to_col(1 + c as usize, cx);
        });
        window.defer(cx, move |window, cx| {
            field.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    /// Send every staged edit of the active data tab as UPDATEs. They stay
    /// uncommitted until the toolbar's Commit.
    pub(super) fn apply_edits(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        let Some(data) = self.tabs.get(self.active_tab).and_then(|t| t.data.as_ref()) else {
            return;
        };
        let staged: Vec<&CellEdit> = data.edits.iter().filter(|e| !e.applied).collect();
        let deletes = data.deletes.clone();
        let inserts: Vec<Vec<_>> = data
            .insert_rows
            .iter()
            .map(|row| {
                row.iter()
                    .filter_map(|(name, field)| {
                        let value = field.read(cx).value().to_string();
                        (!value.is_empty()).then(|| (name.clone(), value))
                    })
                    .collect()
            })
            .collect();
        if inserts.iter().any(Vec::is_empty) {
            self.conn.status = "Enter a value in at least one column of each new row.".into();
            cx.notify();
            return;
        }
        if !data.has_staged() {
            return;
        }
        let (owner, name) = (data.owner.clone(), data.name.clone());
        let updates: Vec<CellUpdate> = staged
            .iter()
            .map(|e| CellUpdate {
                rowid: e.rowid.clone(),
                column: e.column.clone(),
                value: e.new.clone(),
            })
            .collect();
        let dialect = self.active_dialect();
        for u in &updates {
            let query = ora_core::edit::update_cell_sql(
                dialect,
                &owner,
                &name,
                &u.column,
                &u.rowid,
                u.value.as_deref(),
            );
            // The statement shows its values; keep them out of errors.log.
            self.push_private_message(query.inlined());
        }
        self.conn.status = format!("Applying changes ({})…", updates.len());
        self.spawn_db(
            Job::UpdateCells {
                owner,
                name,
                edits: updates,
                deletes,
                inserts,
            },
            PendingOp::ApplyEdits,
            cx,
        );
    }

    /// The worker's answer: mark the edits that went through as applied.
    pub(super) fn edits_applied(
        &mut self,
        cid: u64,
        done: usize,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_ix) = self.target_ix_for(cid) else {
            return;
        };
        let mut changed_rows = false;
        if let Some(data) = self.tabs.get_mut(tab_ix).and_then(|t| t.data.as_mut()) {
            let mut remaining = done;
            for edit in data.edits.iter_mut().filter(|e| !e.applied).take(done) {
                edit.applied = true;
                edit.was_applied = false;
                remaining -= 1;
            }
            changed_rows = remaining > 0;
            let removed = remaining.min(data.deletes.len());
            data.deletes.drain(..removed);
            if remaining > removed {
                std::rc::Rc::make_mut(&mut data.insert_rows).drain(..remaining - removed);
            }
        }
        let auto = error.is_none() && done > 0 && cid == self.conn.id && self.auto_save();
        if changed_rows
            && error.is_none()
            && tab_ix == self.active_tab
            && cid == self.conn.id
            && self.tabs[tab_ix].conn == Some(cid)
        {
            if auto {
                self.reload_after_commit = true;
            } else {
                self.reload_data(cx);
            }
        }
        self.sync_pending(tab_ix, cx);
        match error {
            None => {
                self.push_message_for(
                    cid,
                    if auto {
                        format!("Applied changes: {done}. Committing…")
                    } else {
                        format!(
                            "Applied changes: {done}. Use Commit to keep them or Rollback to undo them."
                        )
                    },
                );
                self.set_status_for(cid, format!("Applied changes: {done} (uncommitted)."));
                if auto {
                    self.spawn_db(Job::Commit, PendingOp::Commit, cx);
                }
            }
            Some(err) => {
                self.push_message_for(
                    cid,
                    format!(
                        "Failed to apply change #{} ({err}). Changes already applied: {done}.",
                        done + 1
                    ),
                );
                self.set_status_for(cid, format!("Failed to apply changes: {err}"));
            }
        }
    }

    /// Drop the edits that were not sent and show what the database holds.
    pub(super) fn discard_edits(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy {
            return;
        }
        let tab_ix = self.active_tab;
        let Some(data) = self.tabs.get_mut(tab_ix).and_then(|t| t.data.as_mut()) else {
            return;
        };
        data.deletes.clear();
        data.insert_rows = Default::default();
        data.edits.retain(|e| e.applied || e.was_applied);
        for edit in &mut data.edits {
            if edit.was_applied {
                edit.applied = true;
                edit.was_applied = false;
            }
        }
        self.sync_pending(tab_ix, cx);
        self.reload_data(cx);
    }

    /// After Commit or Rollback: the tints go away in every data tab.
    pub(super) fn clear_edit_marks_for(
        &mut self,
        conn_id: u64,
        keep_staged: bool,
        cx: &mut Context<Self>,
    ) {
        let conn = Some(conn_id);
        for ix in 0..self.tabs.len() {
            if self.tabs[ix].conn != conn {
                continue;
            }
            if let Some(data) = self.tabs[ix].data.as_mut() {
                let before = data.edits.len();
                if keep_staged {
                    data.edits.retain(|e| !e.applied);
                    for edit in &mut data.edits {
                        edit.was_applied = false;
                    }
                } else {
                    data.deletes.clear();
                    data.insert_rows = Default::default();
                    data.edits.clear();
                }
                if before > 0 {
                    self.sync_pending(ix, cx);
                }
            }
        }
    }

    pub(super) fn render_cell_editor(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(editor) = self.cell_editor.as_ref() else {
            return div().into_any_element();
        };
        let request = &editor.request;
        let old = if request.value.is_null() {
            "NULL".to_string()
        } else {
            request.value.display()
        };
        let card = div()
            .id("cell-editor")
            .flex()
            .flex_col()
            .w(px(460.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_cell_editor(cx);
                }
            }))
            .child(
                div()
                    .px(px(SPACE_4))
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(crate::theme::ui_size(theme, FONT_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Edit {}", request.column)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .child(caption_text(format!("Current value: {old}"), theme))
                    .child(Input::new(&editor.input).w_full())
                    .child(caption_text(
                        "Leave blank for NULL. Click Apply to send the change, then Commit to save it permanently."
                            .to_string(),
                        theme,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_4))
                    .py(px(SPACE_3))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("cell-null")
                            .label("Set NULL")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_cell_editor(true, window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("cell-cancel")
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close_cell_editor(cx))),
                    )
                    .child(
                        Button::new("cell-save")
                            .label("Save")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_cell_editor(false, window, cx)
                            })),
                    ),
            );
        div()
            .id("cell-editor-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(120.))
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
            .into_any_element()
    }
}
