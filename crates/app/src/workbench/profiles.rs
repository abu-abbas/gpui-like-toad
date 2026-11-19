use super::*;

pub(super) struct ConnectionForm {
    pub(super) name_input: Entity<InputState>,
    pub(super) folder_input: Entity<InputState>,
    pub(super) user_input: Entity<InputState>,
    pub(super) host_input: Entity<InputState>,
    pub(super) port_input: Entity<InputState>,
    pub(super) service_input: Entity<InputState>,
    pub(super) tns_input: Entity<InputState>,
    /// Connection dialog mode: separate host/port/service (Basic) or one
    /// TNS alias / connect descriptor.
    pub(super) use_tns: bool,
    pub(super) show_advanced: bool,
    /// Feedback shown inside the connection dialog: (is_error, text).
    pub(super) form_notice: Option<(bool, String)>,
    pub(super) password_input: Entity<InputState>,
    /// "Save password" checkbox in the connection dialog. Off by default;
    /// follows the profile when editing one.
    pub(super) save_password: bool,
    pub(super) access: ConnectionAccess,
    pub(super) editing_profile: Option<usize>,
    /// Whether the connection dialog (modal) is open. Closing it never
    /// clears the inputs: the form keeps its input entities.
    pub(super) show_connection_form: bool,
}

impl ConnectionForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workbench>) -> Self {
        let name_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Nama koneksi (opsional)"));
        let folder_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Folder (optional)"));
        let user_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Username, mis. HR"));
        let host_input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("dbhost.example.com"));
        let port_input = cx.new(|cx| {
            InputState::new(&mut *window, cx)
                .placeholder("1521")
                .default_value("1521")
        });
        let service_input = cx.new(|cx| InputState::new(&mut *window, cx).placeholder("orclpdb"));
        let tns_input = cx
            .new(|cx| InputState::new(&mut *window, cx).placeholder("ORCL atau (DESCRIPTION=...)"));
        let password_input = cx.new(|cx| InputState::new(&mut *window, cx).masked(true));
        Self {
            name_input,
            folder_input,
            user_input,
            host_input,
            port_input,
            service_input,
            tns_input,
            use_tns: false,
            show_advanced: false,
            form_notice: None,
            password_input,
            save_password: false,
            access: ConnectionAccess::default(),
            editing_profile: None,
            show_connection_form: false,
        }
    }
}

impl Workbench {
    /// The typed password, untrimmed, wrapped so it is redacted in
    /// `Debug` and zeroed on drop.
    fn typed_password(&self, cx: &App) -> SecretString {
        SecretString::from(
            self.connection_form
                .password_input
                .read(cx)
                .value()
                .to_string(),
        )
    }

    /// Password for a connect/test from the dialog: what was typed, or the
    /// keychain entry when the field is blank and the profile being edited
    /// has a saved password.
    pub(super) fn dialog_password(&self, cx: &App) -> Password {
        let typed = self.typed_password(cx);
        if ora_core::credentials::ExposeSecret::expose_secret(&typed).is_empty() {
            if let Some(profile) = self
                .connection_form
                .editing_profile
                .and_then(|ix| self.profiles.get(ix))
                .filter(|p| p.save_password && self.connection_form.save_password)
            {
                return Password::Saved {
                    profile_id: profile.id.clone(),
                };
            }
        }
        Password::Typed(typed)
    }

    /// Run a keychain write/delete off the UI thread; report failures in
    /// Messages (and the dialog, if open).
    fn apply_keychain(
        &mut self,
        profile_id: String,
        action: PasswordAction,
        cx: &mut Context<Self>,
    ) {
        let storing = matches!(action, PasswordAction::Store(_));
        let id = profile_id.clone();
        let task = cx
            .background_executor()
            .spawn(async move { action.apply(crate::keychain::store(), &id) });
        cx.spawn(async move |view, cx| {
            if let Err(err) = task.await {
                let _ = view.update(cx, |this, cx| {
                    // Nothing was saved: untick the profile so Connect asks
                    // for the password instead of failing on a missing entry.
                    if storing {
                        if let Some(ix) = this.profiles.iter().position(|p| p.id == profile_id) {
                            this.profiles[ix].save_password = false;
                            this.persist_profiles();
                        }
                        this.connection_form.save_password = false;
                    }
                    let text = format!("Gagal menyimpan password ke keychain: {err}");
                    this.push_message(text.clone());
                    if this.connection_form.show_connection_form {
                        this.connection_form.form_notice = Some((true, text));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn set_input(
        entity: &Entity<InputState>,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        entity.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    fn persist_profiles(&mut self) {
        if let Err(err) = store::save_profiles(&self.profiles) {
            self.push_message(format!("Gagal menyimpan koneksi: {err}"));
        }
    }

    pub(super) fn move_profile_to_folder(
        &mut self,
        id: &str,
        folder: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .profiles
            .iter()
            .position(|profile| profile.id == id && profile.folder != folder)
        else {
            return;
        };
        let mut updated = self.profiles.clone();
        updated[ix].folder = folder.to_owned();
        match store::save_profiles(&updated) {
            Ok(()) => {
                self.profiles = updated;
                self.collapsed_connection_folders.remove(folder);
                self.side_conn_folded = false;
            }
            Err(err) => self.push_message(format!("Could not move connection: {err}")),
        }
        cx.notify();
    }

    pub(super) fn set_connection_folder_color(
        &mut self,
        folder: &str,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        let mut colors = self.connection_folder_colors.clone();
        colors.insert(folder.to_owned(), color.to_owned());
        match store::save_connection_folder_colors(&colors) {
            Ok(()) => self.connection_folder_colors = colors,
            Err(err) => self.push_message(format!("Could not save folder color: {err}")),
        }
        cx.notify();
    }

    /// The connect string the dialog describes: the TNS field as typed,
    /// or `host:port/service` built from the Basic fields. Empty when the
    /// required field (TNS text or host) is blank.
    pub(super) fn form_connect_string(&self, cx: &App) -> String {
        if self.connection_form.use_tns {
            return Self::input_text(&self.connection_form.tns_input, cx);
        }
        let host = Self::input_text(&self.connection_form.host_input, cx);
        if host.is_empty() {
            return String::new();
        }
        compose_easy_connect(
            &host,
            &Self::input_text(&self.connection_form.port_input, cx),
            &Self::input_text(&self.connection_form.service_input, cx),
        )
    }

    pub(super) fn fill_form(
        &mut self,
        profile: &SavedProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connection_form.show_advanced = false;
        Self::set_input(
            &self.connection_form.name_input.clone(),
            profile.name.clone(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.user_input.clone(),
            profile.username.clone(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.folder_input,
            profile.folder.clone(),
            window,
            cx,
        );
        // Easy Connect strings split into the Basic fields; descriptors and
        // TNS aliases open in TNS mode unchanged.
        match parse_easy_connect(&profile.connect_string) {
            Some((host, port, service)) => {
                self.connection_form.use_tns = false;
                Self::set_input(&self.connection_form.host_input.clone(), host, window, cx);
                Self::set_input(&self.connection_form.port_input.clone(), port, window, cx);
                Self::set_input(
                    &self.connection_form.service_input.clone(),
                    service,
                    window,
                    cx,
                );
                Self::set_input(
                    &self.connection_form.tns_input.clone(),
                    String::new(),
                    window,
                    cx,
                );
            }
            None => {
                self.connection_form.use_tns = true;
                Self::set_input(
                    &self.connection_form.host_input.clone(),
                    String::new(),
                    window,
                    cx,
                );
                Self::set_input(
                    &self.connection_form.port_input.clone(),
                    "1521".into(),
                    window,
                    cx,
                );
                Self::set_input(
                    &self.connection_form.service_input.clone(),
                    String::new(),
                    window,
                    cx,
                );
                Self::set_input(
                    &self.connection_form.tns_input.clone(),
                    profile.connect_string.clone(),
                    window,
                    cx,
                );
            }
        }
        // A saved password is never loaded into the field: blank means
        // "use the one in the keychain" (see `dialog_password`).
        Self::set_input(
            &self.connection_form.password_input.clone(),
            String::new(),
            window,
            cx,
        );
        self.connection_form.access = profile.access;
        self.connection_form.save_password = profile.save_password;
    }

    /// Open the connection dialog for a saved profile.
    pub(super) fn edit_profile(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(profile) = self.profiles.get(ix).cloned() {
            self.connection_form.editing_profile = Some(ix);
            self.fill_form(&profile, window, cx);
            self.connection_form.form_notice = None;
            self.conn.connect_error = None;
            self.connection_form.show_connection_form = true;
            cx.notify();
        }
    }

    fn new_profile_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connection_form.editing_profile = None;
        self.connection_form.use_tns = false;
        self.connection_form.show_advanced = false;
        self.connection_form.form_notice = None;
        self.conn.connect_error = None;
        Self::set_input(
            &self.connection_form.folder_input,
            String::new(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.name_input.clone(),
            String::new(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.user_input.clone(),
            String::new(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.host_input.clone(),
            String::new(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.port_input.clone(),
            "1521".into(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.service_input.clone(),
            String::new(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.tns_input.clone(),
            String::new(),
            window,
            cx,
        );
        Self::set_input(
            &self.connection_form.password_input.clone(),
            String::new(),
            window,
            cx,
        );
        self.connection_form.save_password = false;
        self.connection_form.access = ConnectionAccess::default();
        cx.notify();
    }

    /// "+" / "New connection": open an empty connection dialog.
    pub(super) fn open_new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_profile_form(window, cx);
        self.connection_form.show_connection_form = true;
        cx.notify();
    }

    pub(super) fn close_connection_dialog(&mut self, cx: &mut Context<Self>) {
        self.connection_form.show_connection_form = false;
        cx.notify();
    }

    pub(super) fn save_profile(&mut self, cx: &mut Context<Self>) {
        if self.save_profile_quiet(cx) {
            // A stale connect error would hide the "saved" notice.
            self.conn.connect_error = None;
            let text = if self.connection_form.save_password {
                "Profil koneksi tersimpan, password di keychain."
            } else {
                "Profil koneksi tersimpan."
            };
            self.push_message(text.to_string());
            self.connection_form.form_notice = Some((false, text.to_string()));
            cx.notify();
        }
    }

    /// Save the dialog into its profile (new or edited) and bring the
    /// keychain entry in line with "Save password". Returns false when
    /// the form is incomplete.
    pub(super) fn save_profile_quiet(&mut self, cx: &mut Context<Self>) -> bool {
        let username = Self::input_text(&self.connection_form.user_input, cx);
        let connect_string = self.form_connect_string(cx);
        if username.is_empty() || connect_string.is_empty() {
            self.connection_form.show_advanced = false;
            self.connection_form.form_notice = Some((true, FORM_INCOMPLETE.to_string()));
            cx.notify();
            return false;
        }
        let name = Self::input_text(&self.connection_form.name_input, cx);
        let existing = self
            .connection_form
            .editing_profile
            .and_then(|ix| self.profiles.get(ix))
            .cloned();
        let profile = SavedProfile {
            id: existing
                .as_ref()
                .map(|p| p.id.clone())
                .unwrap_or_else(store::new_profile_id),
            name: if name.is_empty() {
                format!("{username}@{connect_string}")
            } else {
                name
            },
            username,
            connect_string,
            access: self.connection_form.access,
            folder: Self::input_text(&self.connection_form.folder_input, cx),
            save_password: self.connection_form.save_password,
        };
        let action =
            PasswordAction::decide(self.connection_form.save_password, self.typed_password(cx));
        // Skip the keychain entirely for a profile that never had a
        // saved password and still does not want one.
        let touch_keychain = self.connection_form.save_password
            || existing.as_ref().is_some_and(|p| p.save_password);
        let profile_id = profile.id.clone();
        match self.connection_form.editing_profile {
            Some(ix) if ix < self.profiles.len() => self.profiles[ix] = profile,
            _ => {
                self.profiles.push(profile);
                self.connection_form.editing_profile = Some(self.profiles.len() - 1);
            }
        }
        self.persist_profiles();
        if touch_keychain && !matches!(action, PasswordAction::Keep) {
            self.apply_keychain(profile_id, action, cx);
        }
        true
    }

    pub(super) fn remove_profile(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.profiles.len() || self.find_open_conn(Some(ix), false).is_some() {
            return;
        }
        let removed = self.profiles.remove(ix);
        if removed.save_password {
            self.apply_keychain(removed.id.clone(), PasswordAction::Delete, cx);
        }
        self.shift_profile_ix(ix);
        if self.connection_form.editing_profile == Some(ix) {
            self.connection_form.editing_profile = None;
        } else if let Some(editing) = self.connection_form.editing_profile {
            if ix < editing {
                self.connection_form.editing_profile = Some(editing - 1);
            }
        }
        self.persist_profiles();
        self.push_message(format!("Profil '{}' dihapus.", removed.name));
        cx.notify();
    }
}

pub(super) const FORM_INCOMPLETE: &str = "Isi username dan host (atau TNS) dulu.";

impl Workbench {
    pub(super) fn export_connections(&self, cx: &mut Context<Self>) {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&dir, Some("ora-connections.json"));
        let settings = store::export_connection_profiles(&self.profiles);
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(path, settings) })
                .await;
            let _ = view.update(cx, |this, cx| {
                let message = match result {
                    Ok(()) => "Connection profiles exported.".to_string(),
                    Err(err) => format!("Could not export connection profiles: {err}"),
                };
                this.push_message(message.clone());
                this.conn.status = message;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn import_connections(&self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import connections (Ora JSON or Toad logins)".into()),
        });
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else { return; };
            let Some(path) = paths.into_iter().next() else { return; };
            let loaded = cx.background_executor().spawn(async move {
                use std::io::Read as _;
                let mut bytes = Vec::new();
                const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;
                std::fs::File::open(path).and_then(|file| file.take(MAX_FILE_SIZE + 1).read_to_end(&mut bytes))
                    .map_err(|err| err.to_string())?;
                if bytes.len() as u64 > MAX_FILE_SIZE { return Err("Connections file exceeds 4 MiB.".into()); }
                let text = store::decode_text_file(&bytes)?;
                store::import_connection_profiles(&text)
            }).await;
            let _ = view.update(cx, |this, cx| {
                let message = match loaded {
                    Ok(profiles) => {
                        let mut merged = this.profiles.clone();
                        let before = merged.len();
                        let mut duplicates = 0;
                        for profile in profiles {
                            let duplicate = merged.iter().any(|existing| existing.name == profile.name &&
                                existing.username == profile.username && existing.connect_string == profile.connect_string &&
                                existing.folder == profile.folder);
                            if duplicate { duplicates += 1; } else { merged.push(profile); }
                        }
                        match store::save_profiles(&merged) {
                            Ok(()) => {
                                let added = merged.len() - before;
                                this.profiles = merged;
                                format!("{added} connection profiles imported. {duplicates} duplicates skipped.")
                            }
                            Err(err) => format!("Could not save imported profiles: {err}"),
                        }
                    }
                    Err(err) => format!("Could not import connection profiles: {err}"),
                };
                this.push_message(message.clone());
                this.conn.status = message;
                cx.notify();
            });
        }).detach();
    }
}
