//! Settings > Extensions: one row per extension. Developer tools are
//! bundled in Kilat, so Install / Uninstall only switches them on and off.
//! A database that also exists as a downloadable extension (SQLite) is
//! downloaded when installed: Install asks GitHub for the signed index,
//! downloads and verifies the program, then switches the extension on, so
//! there is no separate "check" step. If the download fails nothing is
//! switched on and the page says so. Such an extension counts as installed
//! only while its program is on disk. Saved connections, tools and
//! passwords are never deleted.

use super::super::updates::DownloadProgress;
use super::appearance::setting_group;
use super::setting_row_owned;
use super::*;
use gpui_kit::component::progress::Progress;
use ora_core::extension_index::{self, IndexEntry};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtFilter {
    All,
    Installed,
    Databases,
    Tools,
    Actions,
}

impl ExtFilter {
    const ALL: [Self; 5] = [
        Self::All,
        Self::Installed,
        Self::Databases,
        Self::Tools,
        Self::Actions,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Installed => "Installed",
            Self::Databases => "Databases",
            Self::Tools => "Developer tools",
            Self::Actions => "SQL tools",
        }
    }

    fn matches(self, info: &store::ExtensionInfo, installed: bool) -> bool {
        match self {
            Self::All => true,
            Self::Installed => installed,
            Self::Databases => info.kind == store::ExtensionKind::Database,
            Self::Tools => info.kind == store::ExtensionKind::Tool,
            Self::Actions => info.kind == store::ExtensionKind::Action,
        }
    }
}

/// Does this build know how to run the downloaded extension?
fn has_online_version(id: &str) -> bool {
    store::ToolKind::from_key(id).is_some()
        || id == super::super::dryrun::ID
        || ora_core::DbKind::ALL
            .iter()
            .any(|k| *k != ora_core::DbKind::Oracle && k.key() == id)
}

fn extension_matches(info: &store::ExtensionInfo, search: &str) -> bool {
    search.is_empty()
        || info.name.to_lowercase().contains(search)
        || info.description.to_lowercase().contains(search)
        || info.id.contains(search)
}

/// Downloaded extensions and the last check of the online index.
#[derive(Default)]
pub(super) struct OnlineState {
    index: Option<ora_core::extension_index::ExtensionIndex>,
    /// Downloaded extensions on disk, by id.
    installed: std::collections::HashMap<String, store::RemoteInstall>,
    /// The index is being fetched (Check for updates).
    checking: bool,
    download: Option<ExtensionDownload>,
    progress: Option<DownloadProgress>,
    /// (is_error, text) of the last action.
    note: Option<(bool, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExtensionDownload {
    Single(&'static str),
    All,
}

impl OnlineState {
    pub(in crate::workbench) fn refresh_installed(&mut self) {
        self.installed = store::REGISTRY
            .iter()
            .filter(|e| has_online_version(e.id))
            .filter_map(|e| store::remote_install(e.id).map(|r| (e.id.to_string(), r)))
            .collect();
    }

    fn working(&self) -> bool {
        self.checking || self.download.is_some()
    }

    /// The online entry for `id` when it is newer than what is downloaded
    /// and this app can run it.
    fn update_for(&self, id: &str) -> Option<&IndexEntry> {
        let entry = self.index.as_ref()?.get(id)?;
        let have = ora_core::update::Version::parse(&self.installed.get(id)?.version)?;
        let app = ora_core::update::Version::parse(env!("CARGO_PKG_VERSION"))?;
        (have < entry.version
            && entry.compatible_with(app)
            && entry
                .file_for(&extension_index::current_platform())
                .is_some())
        .then_some(entry)
    }

    fn available_updates<'a>(
        &'a self,
        extensions: &'a store::Extensions,
    ) -> impl Iterator<Item = (&'static store::ExtensionInfo, &'a IndexEntry)> {
        store::REGISTRY
            .iter()
            .filter(|extension| !extension.built_in && extensions.is_installed(extension.id))
            .filter_map(|extension| {
                self.update_for(extension.id)
                    .map(|entry| (extension, entry))
            })
    }
}

impl Workbench {
    /// Fetch the signed index in the background (Check for updates).
    fn check_online_extensions(&mut self, cx: &mut Context<Self>) {
        let online = &mut self.settings.ext_online;
        if online.working() {
            return;
        }
        online.checking = true;
        online.note = None;
        let task = cx
            .background_executor()
            .spawn(async { crate::release_fetch::fetch_extension_index() });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let online = &mut this.settings.ext_online;
                online.checking = false;
                match result {
                    Ok(index) => {
                        online.index = Some(index);
                        online.refresh_installed();
                        let updates: Vec<String> = online
                            .available_updates(&this.extensions)
                            .map(|(info, entry)| format!("{} {}", info.name, entry.version))
                            .collect();
                        online.note = Some((
                            false,
                            if updates.is_empty() {
                                "Everything is up to date.".to_string()
                            } else {
                                format!("Update available: {}.", updates.join(", "))
                            },
                        ));
                    }
                    Err(err) => {
                        crate::error_log::append(&format!("Extension index check failed: {err}"));
                        online.note = Some((true, format!("Could not check: {err}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Install the versions from the checked, signed index. Each package
    /// is verified and installed separately so a failure leaves its old
    /// version available and does not stop the rest of the batch.
    fn update_all_extensions(&mut self, cx: &mut Context<Self>) {
        let online = &mut self.settings.ext_online;
        if online.working() {
            return;
        }
        online.refresh_installed();
        let updates: Vec<_> = online
            .available_updates(&self.extensions)
            .map(|(info, entry)| (info.name, entry.clone()))
            .collect();
        if updates.is_empty() {
            online.note = Some((false, "Everything is up to date.".into()));
            cx.notify();
            return;
        }

        online.download = Some(ExtensionDownload::All);
        online.note = Some((
            false,
            format!("Downloading and verifying {} updates…", updates.len()),
        ));
        let progress = DownloadProgress::default();
        online.progress = Some(progress.clone());
        progress.watch(cx);
        let task = cx.background_executor().spawn(async move {
            let mut updated = Vec::new();
            let mut failed = Vec::new();
            let count = updates.len();
            for (index, (name, entry)) in updates.into_iter().enumerate() {
                progress.package(format!("{} of {count} · {name}", index + 1));
                match crate::release_fetch::install_extension(&entry, |received, total| {
                    progress.record(received, total)
                }) {
                    Ok(installed) => updated.push(format!("{name} {}", installed.version)),
                    Err(error) => failed.push(format!("{name} ({error})")),
                }
            }
            (updated, failed)
        });
        cx.spawn(async move |view, cx| {
            let (updated, failed) = task.await;
            let _ = view.update(cx, |this, cx| {
                let online = &mut this.settings.ext_online;
                online.download = None;
                online.progress = None;
                online.refresh_installed();
                let mut summary = Vec::new();
                if !updated.is_empty() {
                    summary.push(format!("Updated: {}.", updated.join(", ")));
                }
                if !failed.is_empty() {
                    summary.push(format!("Could not update: {}.", failed.join(", ")));
                }
                online.note = Some((!failed.is_empty(), summary.join(" ")));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Install or update one extension. Downloadable ones are fetched from
    /// GitHub (index, program, signatures) in the background; if that
    /// fails nothing is switched on and the page says so.
    fn install_extension_flow(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if self.settings.ext_online.working() {
            return;
        }
        let Some(info) = store::extension_info(id) else {
            return;
        };
        if !has_online_version(id) {
            self.set_extension_installed(id, true, cx);
            return;
        }
        let name = info.name;
        let online = &mut self.settings.ext_online;
        online.download = Some(ExtensionDownload::Single(id));
        online.note = Some((false, format!("Downloading and verifying {name}…")));
        let progress = DownloadProgress::default();
        online.progress = Some(progress.clone());
        progress.watch(cx);
        let task = cx.background_executor().spawn(async move {
            let index = crate::release_fetch::fetch_extension_index().inspect_err(|e| {
                crate::error_log::append(&format!(
                    "Extension install failed: {id}: extension index: {e}"
                ))
            })?;
            let entry = index
                .get(id)
                .cloned()
                .ok_or_else(|| format!("{name} is not in the online index."))?;
            let installed = crate::release_fetch::install_extension(&entry, |received, total| {
                progress.record(received, total)
            })?;
            Ok::<_, String>((index, installed))
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let online = &mut this.settings.ext_online;
                online.download = None;
                online.progress = None;
                let ok = result.is_ok();
                online.note = Some(match result {
                    Ok((index, installed)) => {
                        online.index = Some(index);
                        (
                            false,
                            format!("{name} {} downloaded and verified.", installed.version),
                        )
                    }
                    Err(err) => (true, format!("Could not download {name}: {err}")),
                });
                online.refresh_installed();
                if ok {
                    this.set_extension_installed(id, true, cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Switch an extension off and remove its downloaded program, if any.
    /// Nothing is switched off when the program cannot be removed.
    fn uninstall_extension_flow(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if self.settings.ext_online.working() {
            return;
        }
        if store::ToolKind::from_key(id).is_some() && !self.stop_pilot_before_uninstall(id) {
            let name = store::extension_info(id).map_or(id, |i| i.name);
            self.settings.ext_online.note = Some((
                true,
                format!("Wait for the {name} request to finish, then retry."),
            ));
            cx.notify();
            return;
        }
        if self.settings.ext_online.installed.contains_key(id) {
            if let Err(err) = store::remove_remote_install(id) {
                let name = store::extension_info(id).map_or(id, |i| i.name);
                crate::error_log::append(&format!("Extension uninstall failed: {id}: {err}"));
                self.settings.ext_online.note = Some((
                    true,
                    format!("Could not remove the downloaded {name}: {err}"),
                ));
                self.settings.ext_online.refresh_installed();
                cx.notify();
                return;
            }
        }
        let online = &mut self.settings.ext_online;
        online.note = None;
        online.refresh_installed();
        self.set_extension_installed(id, false, cx);
    }

    /// Open Settings > Extensions (the "Get more" links).
    pub(in crate::workbench) fn open_extensions_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(window, cx);
        self.settings.page = SettingsPage::Extensions;
        self.settings.ext_filter = ExtFilter::All;
        cx.notify();
    }

    pub(in crate::workbench) fn open_extension_updates(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_extensions_page(window, cx);
        self.check_online_extensions(cx);
    }

    /// A connection needs an extension that is not installed: open Settings >
    /// Extensions and say so there.
    pub(in crate::workbench) fn show_extension_needed(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = store::extension_info(id).map_or(id, |i| i.name);
        self.open_settings(window, cx);
        self.settings.page = SettingsPage::Extensions;
        self.settings.ext_filter = ExtFilter::All;
        self.settings.ext_online.note =
            Some((true, format!("Install {name} to open this connection.")));
        cx.notify();
    }

    /// Installed as far as this page is concerned: switched on and, for an
    /// extension that is downloaded, its program is on disk.
    pub(in crate::workbench) fn extension_ready(&self, id: &str) -> bool {
        self.extensions.is_installed(id)
            && (!has_online_version(id) || self.settings.ext_online.installed.contains_key(id))
    }

    fn set_extension_installed(&mut self, id: &'static str, install: bool, cx: &mut Context<Self>) {
        if install {
            self.extensions.install(id);
        } else {
            self.extensions.uninstall(id);
        }
        if let Err(err) = store::save_extensions(&self.extensions) {
            crate::error_log::append(&format!("Saving extensions.json failed: {err}"));
            self.push_message(format!("Could not save extensions: {err}"));
        }
        cx.notify();
    }

    pub(super) fn render_extensions(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let filter = self.settings.ext_filter;
        let tabs = ExtFilter::ALL.into_iter().fold(
            div().flex().flex_row().gap(px(SPACE_2)),
            |row, tab| {
                row.child(
                    Button::new(tab.label())
                        .small()
                        .ghost()
                        .label(tab.label())
                        .selected(tab == filter)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.ext_filter = tab;
                            cx.notify();
                        })),
                )
            },
        );
        let online = &self.settings.ext_online;
        let working = online.working();
        let update_all = online.available_updates(&self.extensions).count() > 1;
        let updating_all = online.download == Some(ExtensionDownload::All);
        // The search box of the Settings dialog also narrows this list, as
        // long as something in it matches.
        let search = Self::input_text(&self.settings.search, cx).to_lowercase();
        let search = if store::REGISTRY
            .iter()
            .any(|e| extension_matches(e, &search))
        {
            search
        } else {
            String::new()
        };
        let rows: Vec<_> = store::REGISTRY
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                filter.matches(info, self.extension_ready(info.id))
                    && extension_matches(info, &search)
            })
            .map(|(ix, info)| {
                let installed = self.extension_ready(info.id);
                let id = info.id;
                let downloaded = online.installed.get(id);
                let update = online
                    .update_for(id)
                    .filter(|_| installed)
                    .map(|u| u.version.to_string());
                let installing = online.download == Some(ExtensionDownload::Single(id));
                let control = if info.built_in {
                    caption_text("Built in".into(), theme).into_any_element()
                } else {
                    div()
                        .flex()
                        .gap(px(SPACE_2))
                        .children(update.is_some().then(|| {
                            Button::new(("ext-update", ix))
                                .small()
                                .primary()
                                .label("Update")
                                .disabled(working)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.install_extension_flow(id, cx)
                                }))
                        }))
                        .child(
                            Button::new(("ext", ix))
                                .small()
                                .label(if installing {
                                    "Installing…"
                                } else if installed {
                                    "Uninstall"
                                } else {
                                    "Install"
                                })
                                .when(!installed, |b| b.primary())
                                .disabled(working)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if installed {
                                        this.uninstall_extension_flow(id, cx)
                                    } else {
                                        this.install_extension_flow(id, cx)
                                    }
                                })),
                        )
                        .into_any_element()
                };
                let state = match (installed, downloaded, &update) {
                    (true, Some(r), Some(u)) => {
                        format!(" · Downloaded v{} · v{u} available", r.version)
                    }
                    (true, Some(r), None) => format!(" · Downloaded v{}", r.version),
                    _ => String::new(),
                };
                setting_row_owned(
                    info.name.to_string(),
                    format!("{}{state}", info.description),
                    control,
                    theme,
                )
            })
            .collect();
        let status = online.note.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_3))
            .child(tabs)
            .child(if rows.is_empty() {
                div()
                    .child(caption_text("Nothing here yet.".into(), theme))
                    .into_any_element()
            } else {
                setting_group(theme, rows).into_any_element()
            })
            .children(status.map(|(error, text)| {
                caption_text(text, theme).when(error, |t| t.text_color(theme.danger))
            }))
            .when(online.checking, |panel| {
                panel.child(
                    Progress::new("extension-check-progress")
                        .small()
                        .loading(true)
                        .accessibility_label("Checking for extension updates"),
                )
            })
            .children(
                online.progress.as_ref().map(|progress| {
                    progress.render("extension-download-progress", theme)
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(div().flex_1().min_w(px(0.)).child(caption_text(
                        "Uninstalling keeps your saved connections and passwords; they open again once the extension is back. Downloads come from this project's GitHub Releases and are checked against its signing key before they are saved.".into(),
                        theme,
                    )))
                    .child(
                        div().flex_none().child(Button::new("ext-check-updates")
                            .small()
                            .when(update_all || updating_all, |b| b.primary())
                            .label(if online.checking {
                                "Checking…"
                            } else if updating_all {
                                "Updating…"
                            } else if update_all {
                                "Update all"
                            } else {
                                "Check for updates"
                            })
                            .disabled(working)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if update_all {
                                    this.update_all_extensions(cx);
                                } else {
                                    this.check_online_extensions(cx);
                                }
                            }))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ora_core::update::Version;

    #[test]
    fn bulk_update_includes_only_eligible_installed_extensions() {
        let platform = extension_index::current_platform();
        let mut online = OnlineState::default();
        let mut extensions = store::Extensions::default();
        let mut entries = Vec::new();
        let current = Version(1, 0, 0);
        let newer = Version(1, 0, 1);
        let any_app = Version(0, 0, 0);
        let future_app = Version(u64::MAX, 0, 0);
        for (id, installed, downloaded, version, min_app, ships_platform) in [
            ("sqlite", true, true, newer, any_app, true),
            ("redis", true, true, newer, any_app, true),
            ("mysql", true, true, current, any_app, true),
            ("postgres", true, true, newer, future_app, true),
            ("mssql", true, true, newer, any_app, false),
            ("mailhog", false, true, newer, any_app, true),
            ("dryrun", true, false, newer, any_app, true),
        ] {
            let info = store::extension_info(id).unwrap();
            if installed {
                extensions.install(id);
            }
            if downloaded {
                online.installed.insert(
                    id.into(),
                    store::RemoteInstall {
                        id: id.into(),
                        version: "1.0.0".into(),
                        file: id.into(),
                        path: id.into(),
                    },
                );
            }
            entries.push(IndexEntry {
                id: id.into(),
                name: info.name.into(),
                description: String::new(),
                kind: info.kind,
                version,
                min_app,
                max_app: None,
                tag: format!("ext-{id}-{version}"),
                files: ships_platform
                    .then(|| (platform.clone(), id.to_string()))
                    .into_iter()
                    .collect(),
            });
        }
        assert_eq!(online.available_updates(&extensions).count(), 0);
        online.index = Some(extension_index::ExtensionIndex {
            revision: 1,
            entries,
        });
        let pending: Vec<_> = online
            .available_updates(&extensions)
            .map(|(info, _)| info.id)
            .collect();
        assert_eq!(pending, ["sqlite", "redis"]);

        online.installed.get_mut("redis").unwrap().version = "1.0.1".into();
        assert_eq!(online.available_updates(&extensions).count(), 1);
    }
}
