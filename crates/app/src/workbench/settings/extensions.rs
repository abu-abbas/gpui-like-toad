//! Settings > Extensions: switch the optional databases and developer
//! tools on and off. Stage 1: every extension is bundled in Kilat, so
//! Install / Uninstall only toggles it; saved connections, tools and
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

/// Extensions that are downloaded from GitHub Releases (not bundled).
#[derive(Default)]
pub(super) struct OnlineState {
    index: Option<ora_core::extension_index::ExtensionIndex>,
    /// Downloaded extensions on disk, by id.
    installed: std::collections::HashMap<String, store::RemoteInstall>,
    /// The index is loading, or the id being downloaded.
    checking: bool,
    busy: Option<String>,
    /// (is_error, text) of the last action.
    note: Option<(bool, String)>,
}

impl OnlineState {
    fn refresh_installed(&mut self) {
        self.installed = self
            .index
            .iter()
            .flat_map(|i| i.entries.iter())
            .filter_map(|e| store::remote_install(&e.id).map(|r| (e.id.clone(), r)))
            .collect();
    }
}

impl Workbench {
    /// Fetch the signed index in the background.
    fn check_online_extensions(&mut self, cx: &mut Context<Self>) {
        let online = &mut self.settings.ext_online;
        if online.checking || online.busy.is_some() {
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
                        online.note = Some((
                            false,
                            format!("{} extension(s) available online.", index.entries.len()),
                        ));
                        online.index = Some(index);
                        online.refresh_installed();
                    }
                    Err(err) => online.note = Some((true, format!("Could not check: {err}"))),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Download, verify and install one extension in the background.
    fn download_extension(&mut self, id: String, cx: &mut Context<Self>) {
        let online = &mut self.settings.ext_online;
        if online.checking || online.busy.is_some() {
            return;
        }
        let Some(entry) = online.index.as_ref().and_then(|i| i.get(&id)).cloned() else {
            return;
        };
        online.busy = Some(id);
        online.note = Some((
            false,
            format!(
                "Downloading and verifying {} {}…",
                entry.name, entry.version
            ),
        ));
        let name = entry.name.clone();
        let entry_id = entry.id.clone();
        let task = cx
            .background_executor()
            .spawn(async move { crate::release_fetch::install_extension(&entry) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let downloaded = result.is_ok();
            let _ = view.update(cx, |this, cx| {
                let online = &mut this.settings.ext_online;
                online.busy = None;
                online.note = Some(match result {
                    Ok(r) => (
                        false,
                        format!("{name} {} downloaded and verified.", r.version),
                    ),
                    Err(err) => (true, format!("Install failed: {err}")),
                });
                online.refresh_installed();
                // The download replaces a driver that is also bundled:
                // switch the extension on, or its connection type would
                // stay hidden.
                if downloaded {
                    this.set_extension_installed_by_id(&entry_id, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn remove_downloaded_extension(&mut self, id: &str, cx: &mut Context<Self>) {
        let online = &mut self.settings.ext_online;
        online.note = Some(match store::remove_remote_install(id) {
            Ok(()) => (
                false,
                "Removed. Your saved connections are untouched.".into(),
            ),
            Err(err) => (true, format!("Could not remove: {err}")),
        });
        online.refresh_installed();
        cx.notify();
    }

    fn render_online_extensions(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let online = &self.settings.ext_online;
        let busy = online.checking || online.busy.is_some();
        // Only what this app can run: database extensions for an engine it
        // knows (the bundled driver is then replaced while the download is
        // installed). Tool extensions need a UI protocol that does not exist.
        let runnable = |e: &ora_core::extension_index::IndexEntry| {
            e.kind == ora_core::store::ExtensionKind::Database
                && ora_core::DbKind::ALL
                    .iter()
                    .any(|k| *k != ora_core::DbKind::Oracle && k.key() == e.id)
        };
        let rows: Vec<_> = online
            .index
            .iter()
            .flat_map(|i| i.entries.iter())
            .filter(|e| runnable(e))
            .enumerate()
            .map(|(ix, entry)| {
                let id = entry.id.clone();
                let have = online.installed.get(&entry.id);
                let platform = ora_core::extension_index::current_platform();
                let app = ora_core::update::Version::parse(env!("CARGO_PKG_VERSION"));
                let unavailable = if entry.file_for(&platform).is_none() {
                    Some(format!("Not available for {platform}"))
                } else if !app.is_some_and(|a| entry.compatible_with(a)) {
                    Some("Needs a different Kilat version".to_string())
                } else {
                    None
                };
                let label = match have {
                    Some(r) if r.version != entry.version.to_string() => "Update",
                    Some(_) => "Uninstall",
                    None => "Install",
                };
                let remove = have.is_some_and(|r| r.version == entry.version.to_string());
                let control = match unavailable {
                    Some(why) if !remove => caption_text(why, theme).into_any_element(),
                    _ => Button::new(("ext-online", ix))
                        .small()
                        .label(label)
                        .disabled(busy)
                        .when(!remove, |b| b.primary())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if remove {
                                this.remove_downloaded_extension(&id, cx);
                            } else {
                                this.download_extension(id.clone(), cx);
                            }
                        }))
                        .into_any_element(),
                };
                let built_in = store::extension_info(&entry.id).is_some();
                let state = match have {
                    Some(r) => format!("Downloaded v{}", r.version),
                    None => format!("v{}", entry.version),
                };
                let state = if built_in {
                    format!("{state} · runs as a separate program instead of the built-in driver")
                } else {
                    state
                };
                setting_row_owned(
                    entry.name.clone(),
                    format!("{} · {state}", entry.description),
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
            .mt(px(SPACE_2))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(div().flex_1().child(form_label("Available online", theme)))
                    .child(
                        Button::new("ext-check-online")
                            .small()
                            .label(if online.checking {
                                "Checking…"
                            } else {
                                "Check online"
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.check_online_extensions(cx)
                            })),
                    ),
            )
            .children(status.map(|(error, text)| {
                caption_text(text, theme).when(error, |t| t.text_color(theme.danger))
            }))
            .children((!rows.is_empty()).then(|| setting_group(theme, rows)))
            .child(caption_text(
                "Downloads come from this project's GitHub Releases and are checked against its signing key before they are saved.".into(),
                theme,
            ))
    }

    fn set_extension_installed_by_id(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(info) = store::extension_info(id) {
            self.set_extension_installed(info.id, true, cx);
        }
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
        let rows: Vec<_> = store::REGISTRY
            .iter()
            .enumerate()
            .filter(|(_, info)| filter.matches(info, self.extensions.is_installed(info.id)))
            .map(|(ix, info)| {
                let installed = self.extensions.is_installed(info.id);
                let id = info.id;
                let control = if info.built_in {
                    caption_text("Built in".into(), theme).into_any_element()
                } else {
                    Button::new(("ext", ix))
                        .small()
                        .label(if installed { "Uninstall" } else { "Install" })
                        .when(!installed, |b| b.primary())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_extension_installed(id, !installed, cx)
                        }))
                        .into_any_element()
                };
                setting_row(info.name, info.description, control, theme)
            })
            .collect();
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
            .child(caption_text(
                "Uninstalling keeps your saved connections and passwords; they open again once the extension is back.".into(),
                theme,
            ))
            .child(self.render_online_extensions(theme, cx))
    }
}
