//! The SQLite side of the connection dialog: picking the connection type,
//! the database file, and which fields a profile of each kind needs.

use super::*;

/// The "Connection type" choices: a server (host, port, database) of any
/// engine, Oracle by TNS alias, or a database file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnectionType {
    /// Host, port and database name.
    Basic(DbKind),
    OracleTns,
    /// A file (SQLite).
    File(DbKind),
}

impl ConnectionType {
    fn all() -> Vec<Self> {
        let mut all = Vec::new();
        for kind in DbKind::ALL {
            if kind.capabilities().network {
                all.push(Self::Basic(kind));
                if kind == DbKind::Oracle {
                    all.push(Self::OracleTns);
                }
            } else {
                all.push(Self::File(kind));
            }
        }
        all
    }

    /// The choices of installed extensions, plus `current` so an existing
    /// profile of an uninstalled kind still shows what it is.
    pub(super) fn available(ext: &store::Extensions, current: Self) -> Vec<Self> {
        Self::all()
            .into_iter()
            .filter(|c| c == &current || ext.db_enabled(c.kind()))
            .collect()
    }

    pub(super) fn of(kind: DbKind, use_tns: bool) -> Self {
        if !kind.capabilities().network {
            Self::File(kind)
        } else if kind == DbKind::Oracle && use_tns {
            Self::OracleTns
        } else {
            Self::Basic(kind)
        }
    }

    pub(super) fn label(self) -> String {
        match self {
            Self::Basic(DbKind::Oracle) => "Oracle · Basic".to_string(),
            Self::OracleTns => "Oracle · TNS".to_string(),
            Self::Basic(kind) | Self::File(kind) => kind.label().to_string(),
        }
    }

    fn kind(self) -> DbKind {
        match self {
            Self::Basic(kind) | Self::File(kind) => kind,
            Self::OracleTns => DbKind::Oracle,
        }
    }
}

impl Workbench {
    pub(super) fn set_connection_type(
        &mut self,
        choice: ConnectionType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The port field follows the engine while it still holds an
        // engine's default (or nothing).
        if let Some(port) = choice.kind().dialect().default_port() {
            let current = Self::input_text(&self.connection_form.port_input, cx);
            let is_default = current.is_empty()
                || DbKind::ALL
                    .iter()
                    .filter_map(|k| k.dialect().default_port())
                    .any(|p| p.to_string() == current);
            if is_default {
                Self::set_input(
                    &self.connection_form.port_input.clone(),
                    port.to_string(),
                    window,
                    cx,
                );
            }
        }
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
