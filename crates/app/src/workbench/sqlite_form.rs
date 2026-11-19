//! The SQLite side of the connection dialog: picking the connection type,
//! the database file, and which fields a profile of each kind needs.

use super::*;

/// The "Connection type" choices: Oracle by Basic fields or TNS, or a
/// SQLite database file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnectionType {
    OracleBasic,
    OracleTns,
    SqliteFile,
}

impl ConnectionType {
    pub(super) const ALL: [Self; 3] = [Self::OracleBasic, Self::OracleTns, Self::SqliteFile];

    pub(super) fn of(kind: DbKind, use_tns: bool) -> Self {
        match kind {
            DbKind::Sqlite => Self::SqliteFile,
            DbKind::Oracle if use_tns => Self::OracleTns,
            DbKind::Oracle => Self::OracleBasic,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::OracleBasic => "Oracle · Basic",
            Self::OracleTns => "Oracle · TNS",
            Self::SqliteFile => "SQLite",
        }
    }

    fn kind(self) -> DbKind {
        match self {
            Self::SqliteFile => DbKind::Sqlite,
            Self::OracleBasic | Self::OracleTns => DbKind::Oracle,
        }
    }
}

impl Workbench {
    pub(super) fn set_connection_type(&mut self, choice: ConnectionType, cx: &mut Context<Self>) {
        let form = &mut self.connection_form;
        form.kind = choice.kind();
        form.use_tns = choice == ConnectionType::OracleTns;
        let caps = form.kind.capabilities();
        // A SQLite file has no password to keep in the keychain, and no
        // SYSDBA.
        if !caps.password {
            form.save_password = false;
        }
        if !caps.sysdba {
            form.access.sysdba = false;
        }
        form.form_notice = None;
        cx.notify();
    }

    /// "Browse…": pick an existing database file into the file field.
    pub(super) fn browse_sqlite_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open SQLite database".into()),
        });
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, cx| {
                    let text = path.to_string_lossy().into_owned();
                    Self::set_input(&this.connection_form.file_input.clone(), text, window, cx);
                    cx.notify();
                })
            });
        })
        .detach();
    }

    /// The dialog lacks what this kind of connection needs: a connect
    /// string (or file), and a username where the database has logins.
    pub(super) fn form_incomplete(&self, username: &str, connect_string: &str) -> bool {
        connect_string.is_empty()
            || (self.connection_form.kind.capabilities().password && username.is_empty())
    }
}
