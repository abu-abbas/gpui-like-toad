//! Responses to Connect and Test Connection.

use super::*;

impl Workbench {
    pub(super) fn on_connected(
        &mut self,
        info: DbInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.conn.session = Some(info.clone());
        self.conn.connection_lost = false;
        self.reset_catalog(Some(info.user.clone()));
        self.conn.current_schema = None;
        self.conn.connect_error = None;
        self.connection_form.show_connection_form = false;
        self.scrub_password_input(window, cx);
        self.conn.status = format!("Connected as {}.", info.user);
        self.push_message(format!(
            "Connected: {} @ {} ({})",
            info.user, info.database, info.version
        ));
        cx.notify();
        self.refresh_schema(cx);
    }

    pub(super) fn on_connection_tested(&mut self, outcome: Result<DbInfo, String>) {
        match outcome {
            Ok(info) => {
                let text = format!(
                    "Connection test succeeded: {} @ {} ({})",
                    info.user, info.database, info.version
                );
                self.push_message(text.clone());
                self.connection_form.form_notice = Some((false, text));
                self.conn.status = "Connection test succeeded.".to_string();
            }
            Err(message) => {
                self.push_message(format!("Connection test failed: {message}"));
                self.connection_form.form_notice =
                    Some((true, format!("Connection test failed: {message}")));
                self.conn.status = "Connection test failed.".to_string();
            }
        }
    }
}
