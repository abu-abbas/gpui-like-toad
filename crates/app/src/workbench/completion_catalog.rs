use super::*;

impl Workbench {
    /// Load one schema's objects and columns into the autocomplete catalog.
    /// Deliberately outside `spawn_db`: it must not set the busy flag or
    /// supersede the operation the user is waiting on. The worker runs it
    /// in turn, after whatever is already queued.
    pub(super) fn load_catalog(&mut self, owner: String, cx: &mut Context<Self>) {
        let Some(db) = self.conn.db.clone() else {
            return;
        };
        if self.conn.session.is_none() {
            return;
        }
        let key = owner.to_ascii_uppercase();
        if !self.conn.catalog_loading.insert(key.clone()) {
            return;
        }
        let rx: Receiver<Response> = db.request(Job::Catalog { owner });
        let epoch = self.conn.catalog_epoch;
        let handle = self.window_handle;
        let conn_id = self.conn.id;
        cx.spawn(async move |view, cx| {
            let resp = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.with_conn(conn_id, cx, |this, background, cx| {
                        this.on_catalog_response(epoch, key, resp, background, window, cx)
                    })
                })
            });
        })
        .detach();
    }

    fn on_catalog_response(
        &mut self,
        epoch: u64,
        owner: String,
        resp: Result<Response, RecvError>,
        background: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.conn.catalog_epoch {
            return;
        }
        self.conn.catalog_loading.remove(&owner);
        let waiting = self.conn.catalog_retrigger.as_deref() == Some(owner.as_str());
        if waiting {
            self.conn.catalog_retrigger = None;
        }
        match resp {
            Ok(Response::Catalog(objects)) => {
                self.catalog.borrow_mut().set_owner(&owner, objects);
                // The user typed `SCHEMA.` and got nothing because it was
                // still loading: show the list now, if they are still there.
                if waiting && !background && !self.show_home {
                    let editor = self.active_tab().editor.clone();
                    if editor.read(cx).focus_handle(cx).is_focused(window) {
                        editor.update(cx, |state, cx| state.show_completions(window, cx));
                    }
                }
            }
            Ok(Response::Failed(message)) => {
                // Stored as loaded-but-empty so every later `SCHEMA.` does
                // not retry a schema the session cannot read.
                self.catalog.borrow_mut().set_owner(&owner, Vec::new());
                self.push_message(format!(
                    "Autocomplete: gagal memuat metadata {owner}: {message}"
                ));
                cx.notify();
            }
            _ => {}
        }
    }

    pub(super) fn reset_catalog(&mut self, default_owner: Option<String>) {
        self.conn.catalog_epoch += 1;
        self.conn.catalog_loading.clear();
        self.conn.catalog_retrigger = None;
        let mut catalog = self.catalog.borrow_mut();
        catalog.clear();
        catalog.set_default_owner(default_owner);
    }
}
