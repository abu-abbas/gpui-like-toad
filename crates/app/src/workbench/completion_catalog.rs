use super::*;

impl Workbench {
    /// Load one schema's objects and columns into the autocomplete catalog.
    /// Deliberately outside `spawn_db`: it must not set the busy flag or
    /// supersede the operation the user is waiting on. The worker runs it
    /// in turn, after whatever is already queued.
    pub(super) fn load_catalog(&mut self, owner: String, cx: &mut Context<Self>) {
        self.load_catalog_for(self.conn.id, owner, cx);
    }

    pub(super) fn load_catalog_for(&mut self, conn_id: u64, owner: String, cx: &mut Context<Self>) {
        let Some(conn) = self.conn_mut(conn_id) else {
            return;
        };
        let Some(db) = conn.db.clone() else {
            return;
        };
        if conn.session.is_none() {
            return;
        }
        let key = owner.to_ascii_uppercase();
        if !conn.catalog_loading.insert(key.clone()) {
            return;
        }
        let rx: Receiver<Response> = db.request(Job::Catalog { owner });
        let epoch = conn.catalog_epoch;
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.on_catalog_response(conn_id, epoch, key, resp, window, cx)
                })
            });
        })
        .detach();
    }

    fn on_catalog_response(
        &mut self,
        cid: u64,
        epoch: u64,
        owner: String,
        resp: Result<Response, RecvError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conn_mut(cid) else {
            return;
        };
        if epoch != conn.catalog_epoch {
            return;
        }
        conn.catalog_loading.remove(&owner);
        let waiting = conn.catalog_retrigger.as_deref() == Some(owner.as_str());
        if waiting {
            conn.catalog_retrigger = None;
        }
        match resp {
            Ok(Response::Catalog(objects)) => {
                self.with_catalog_for(cid, |catalog| catalog.set_owner(&owner, objects));
                // The user typed `SCHEMA.` and got nothing because it was
                // still loading: show the list now, if they are still there.
                if waiting && cid == self.conn.id && !self.show_empty_workspace {
                    let editor = self.active_tab().editor.clone();
                    if editor.read(cx).focus_handle(cx).is_focused(window) {
                        editor.update(cx, |state, cx| state.show_completions(window, cx));
                    }
                }
            }
            Ok(Response::Failed(message)) => {
                // Stored as loaded-but-empty so every later `SCHEMA.` does
                // not retry a schema the session cannot read.
                self.with_catalog_for(cid, |catalog| catalog.set_owner(&owner, Vec::new()));
                self.push_message_for(
                    cid,
                    format!("Autocomplete failed to load metadata for {owner}: {message}"),
                );
                cx.notify();
            }
            _ => {}
        }
    }

    pub(super) fn reset_catalog(&mut self, default_owner: Option<String>) {
        self.reset_catalog_for(self.conn.id, default_owner);
    }

    pub(super) fn reset_catalog_for(&mut self, conn_id: u64, default_owner: Option<String>) {
        let Some(conn) = self.conn_mut(conn_id) else {
            return;
        };
        conn.catalog_epoch += 1;
        conn.catalog_loading.clear();
        conn.catalog_retrigger = None;
        self.with_catalog_for(conn_id, |catalog| {
            catalog.clear();
            catalog.set_default_owner(default_owner);
        });
    }
}
