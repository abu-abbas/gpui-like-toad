//! Editing cells of a data tab. A change is first *staged* (the grid shows
//! the new value, tinted amber), then *applied* (`UPDATE ... WHERE ROWID`,
//! tinted blue), and finally leaves the tint when the user commits or rolls
//! back with the toolbar buttons, as in Toad: nothing is committed behind
//! the user's back.

use std::collections::HashMap;

use ora_core::Value;

use super::*;
use crate::results::{CellEditRequest, CellMark};
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

/// The edit dialog.
pub(super) struct CellEditor {
    request: CellEditRequest,
    input: Entity<InputState>,
    _sub: Subscription,
}

impl Workbench {
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
            self.conn.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        let initial = if request.value.is_null() {
            String::new()
        } else {
            request.value.display()
        };
        let input = cx.new(|cx| {
            InputState::new(&mut *window, cx)
                .placeholder("Nilai baru")
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
        self.conn.status = "Perubahan ditandai. Klik Apply untuk mengirim ke database.".to_string();
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
            self.conn.status = "Isi minimal satu kolom untuk setiap baris baru.".into();
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
        for u in &updates {
            let value = u.value.as_deref().map_or("NULL".to_string(), |v| {
                format!("'{}'", v.replace('\'', "''"))
            });
            self.push_message(format!(
                "UPDATE {owner}.{name} SET {} = {value} WHERE ROWID = '{}'",
                u.column, u.rowid
            ));
        }
        self.conn.status = format!("Mengirim {} perubahan...", updates.len());
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
        done: usize,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_ix) = self.target_ix() else {
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
        if changed_rows
            && error.is_none()
            && tab_ix == self.active_tab
            && self.tabs[tab_ix].conn == Some(self.conn.id)
        {
            self.reload_data(cx);
        }
        self.sync_pending(tab_ix, cx);
        match error {
            None => {
                self.push_message(format!(
                    "{done} perubahan dikirim. Belum di-commit: pakai Commit atau Rollback."
                ));
                self.conn.status = format!("{done} perubahan dikirim, belum di-commit.");
            }
            Some(err) => {
                self.push_message(format!(
                    "Gagal pada perubahan ke-{} ({err}). {done} sebelumnya sudah dikirim.",
                    done + 1
                ));
                self.conn.status = format!("Perubahan gagal: {err}");
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
    pub(super) fn clear_edit_marks(&mut self, cx: &mut Context<Self>) {
        let conn = Some(self.conn.id);
        for ix in 0..self.tabs.len() {
            if self.tabs[ix].conn != conn {
                continue;
            }
            if let Some(data) = self.tabs[ix].data.as_mut() {
                data.deletes.clear();
                data.insert_rows = Default::default();
                if !data.edits.is_empty() {
                    data.edits.clear();
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
                    .text_size(crate::theme::ui_size(&theme, FONT_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Edit {}", request.column)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .child(caption_text(format!("Nilai sekarang: {old}"), theme))
                    .child(Input::new(&editor.input).w_full())
                    .child(caption_text(
                        "Kosong = NULL. Perubahan dikirim lewat Apply dan baru permanen setelah Commit."
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
                            .label("Batal")
                            .on_click(cx.listener(|this, _, _, cx| this.close_cell_editor(cx))),
                    )
                    .child(
                        Button::new("cell-save")
                            .label("Simpan")
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
            .bg(gpui_kit::hsla(0., 0., 0., 0.35))
            .occlude()
            .child(card)
            .into_any_element()
    }
}
