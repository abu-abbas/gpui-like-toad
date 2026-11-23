//! Settings > Extensions: one row per extension. Developer tools are
//! bundled in Kilat, so Install / Uninstall only switches them on and off.
//! A database that also exists as a downloadable extension (SQLite) is
//! downloaded when installed: Install asks GitHub for the signed index,
//! downloads and verifies the program, then switches the extension on, so
//! there is no separate "check" step. If the download fails nothing is
//! switched on and the page says so. Such an extension counts as installed
//! only while its program is on disk. Saved connections, tools and
//! passwords are never deleted.

use super::appearance::setting_group;
use super::setting_row_owned;
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtFilter {
    All,
    Installed,
    Databases,
    Tools,
}

impl ExtFilter {
    const ALL: [Self; 4] = [Self::All, Self::Installed, Self::Databases, Self::Tools];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Installed => "Installed",
            Self::Databases => "Databases",
            Self::Tools => "Developer tools",
        }
    }

    fn matches(self, info: &store::ExtensionInfo, installed: bool) -> bool {
        match self {
            Self::All => true,
            Self::Installed => installed,
            Self::Databases => info.kind == store::ExtensionKind::Database,
            Self::Tools => info.kind == store::ExtensionKind::Tool,
        }
    }
}

/// Does a downloadable build of this extension exist for the app to run?
/// Only database engines the app knows; developer tools would need a UI
/// protocol that does not exist yet.
fn has_online_version(id: &str) -> bool {
    ora_core::DbKind::ALL
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
    /// The id being installed.
    busy: Option<String>,
    /// (is_error, text) of the last action.
    note: Option<(bool, String)>,
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
        self.checking || self.busy.is_some()
    }

    /// The online entry for `id` when it is newer than what is downloaded
    /// and this app can run it.
    fn update_for(&self, id: &str) -> Option<&ora_core::extension_index::IndexEntry> {
        let entry = self.index.as_ref()?.get(id)?;
        let have = ora_core::update::Version::parse(&self.installed.get(id)?.version)?;
        let app = ora_core::update::Version::parse(env!("CARGO_PKG_VERSION"))?;
        (have < entry.version && entry.compatible_with(app)).then_some(entry)
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
                        let updates: Vec<String> = store::REGISTRY
                            .iter()
                            .filter_map(|e| {
                                online
                                    .update_for(e.id)
                                    .map(|u| format!("{} {}", e.name, u.version))
                            })
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
                    Err(err) => online.note = Some((true, format!("Could not check: {err}"))),
                }
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
        online.busy = Some(id.to_string());
        online.note = Some((false, format!("Downloading and verifying {name}…")));
        let task = cx.background_executor().spawn(async move {
            let index = crate::release_fetch::fetch_extension_index()?;
            let entry = index
                .get(id)
                .cloned()
                .ok_or_else(|| format!("{name} is not in the online index."))?;
            let installed = crate::release_fetch::install_extension(&entry)?;
            Ok::<_, String>((index, installed))
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let online = &mut this.settings.ext_online;
                online.busy = None;
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
        if self.settings.ext_online.installed.contains_key(id) {
            if let Err(err) = store::remove_remote_install(id) {
                let name = store::extension_info(id).map_or(id, |i| i.name);
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
                let update = online.update_for(id).map(|u| u.version.to_string());
                let installing = online.busy.as_deref() == Some(id);
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
                            .label(if online.checking {
                                "Checking…"
                            } else {
                                "Check for updates"
                            })
                            .disabled(working)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.check_online_extensions(cx)
                            }))),
                    ),
            )
    }
}
