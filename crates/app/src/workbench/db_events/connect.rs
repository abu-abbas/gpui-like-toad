//! Responses to Connect and Test Connection.

use super::*;

impl Workbench {
    pub(super) fn on_connected(
        &mut self,
        cid: u64,
        info: DbInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_conn_state(cid, |c| {
            c.session = Some(info.clone());
            c.connection_lost = false;
            c.lost_reason = None;
            c.auto_reconnects = 0;
            c.current_schema = None;
            c.connect_error = None;
            c.status = format!("Connected as {}.", info.user);
        });
        self.reset_catalog_for(cid, Some(info.user.clone()));
        self.connection_form.show_connection_form = false;
        self.scrub_password_input(window, cx);
        self.push_message_for(
            cid,
            format!(
                "Connected: {} @ {} ({})",
                info.user, info.database, info.version
            ),
        );
        cx.notify();
        self.refresh_schema_for(cid, cx);
    }

    pub(super) fn on_connection_tested(&mut self, cid: u64, outcome: Result<DbInfo, String>) {
        match outcome {
            Ok(info) => {
                let text = format!(
                    "Connection test succeeded: {} @ {} ({})",
                    info.user, info.database, info.version
                );
                self.push_message_for(cid, text.clone());
                self.connection_form.form_notice = Some((false, text));
                self.set_conn_state(cid, |c| c.status = "Connection test succeeded.".to_string());
            }
            Err(message) => {
                self.push_message_for(cid, format!("Connection test failed: {message}"));
                self.connection_form.form_notice =
                    Some((true, format!("Connection test failed: {message}")));
                self.set_conn_state(cid, |c| c.status = "Connection test failed.".to_string());
            }
        }
    }
}
