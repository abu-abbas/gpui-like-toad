//! Settings > Sync: keep connections, favorites and appearance the same on
//! every computer through a folder (a plain folder, or one that Google
//! Drive / OneDrive / iCloud / Dropbox already syncs), plus export and
//! import of a `.kilat` file. Passwords are never synced. PROD
//! connections are uploaded only when the user ticks them here.
//!
//! The work is done by `ora_core::store::sync`; this file is the page, the
//! background runs and the status shown at the end of the status bar.

use super::appearance::setting_group;
use super::setting_row_owned;
use super::*;
use ora_core::store::{Choice, Conflict, Json, SyncCategories, SyncConfig, SyncOutcome};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone, PartialEq)]
pub(in crate::workbench) enum SyncStatus {
    Off,
    Idle,
    Syncing,
    Synced,
    Conflicts(usize),
    Failed(String),
}

pub(super) struct SyncUi {
    pub(super) config: SyncConfig,
    running: bool,
    status: SyncStatus,
    note: Option<(bool, String)>,
    conflicts: Vec<Conflict>,
    choices: BTreeMap<String, Choice>,
    /// Set while the open conflicts come from an import, not from the folder.
    import_file: Option<PathBuf>,
    detected: Vec<(String, PathBuf)>,
}

impl SyncUi {
    pub(super) fn load() -> Self {
        let config = store::load_sync_config();
        Self {
            status: if config.enabled {
                SyncStatus::Idle
            } else {
                SyncStatus::Off
            },
            config,
            running: false,
            note: None,
            conflicts: Vec::new(),
            choices: BTreeMap::new(),
            import_file: None,
            detected: Vec::new(),
        }
    }
}

fn ago(unix: u64) -> String {
    if unix == 0 {
        return "never".into();
    }
    let secs = store::now_unix_secs().saturating_sub(unix);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

fn describe(key: &str, value: &Option<Json>) -> String {
    let Some(value) = value else {
        return "(deleted)".into();
    };
    let text = |k: &str| {
        value
            .get(k)
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string()
    };
    if key.starts_with("conn:") {
        format!(
            "{} · {}@{}",
            text("name"),
            text("username"),
            text("connect_string")
        )
    } else if key.starts_with("fav:") {
        text("sql").split_whitespace().collect::<Vec<_>>().join(" ")
    } else if key.starts_with("folder-color:") {
        format!("color {}", value.as_str().unwrap_or(""))
    } else {
        format!("theme {}, accent {}", text("theme"), text("accent"))
    }
}

fn conflict_title(c: &Conflict) -> String {
    let name = |v: &Option<Json>| {
        v.as_ref()
            .and_then(|v| v.get("name"))
            .and_then(Json::as_str)
            .map(str::to_string)
    };
    if c.key.starts_with("conn:") {
        format!(
            "Connection: {}",
            name(&c.local).or(name(&c.remote)).unwrap_or_default()
        )
    } else if let Some(folder) = c.key.strip_prefix("folder-color:") {
        format!("Folder color: {folder}")
    } else if c.key.starts_with("fav:") {
        "Favorite query".into()
    } else {
        "Appearance".into()
    }
}

impl Workbench {
    fn sync_categories(&self) -> SyncCategories {
        self.settings.sync.config.categories
    }

    fn save_sync_config(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = store::save_sync_config(&self.settings.sync.config) {
            self.settings.sync.note = Some((true, format!("Could not save sync settings: {err}")));
        }
        cx.notify();
    }

    /// Use `folder` for sync: create it, turn sync on and run the first
    /// sync, which merges what is in the folder with this computer.
    fn sync_use_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if let Err(err) = std::fs::create_dir_all(&folder) {
            self.settings.sync.note =
                Some((true, format!("Could not use {}: {err}", folder.display())));
            cx.notify();
            return;
        }
        let sync = &mut self.settings.sync;
        sync.config.enabled = true;
        sync.config.folder = folder.to_string_lossy().into_owned();
        sync.status = SyncStatus::Idle;
        sync.note = None;
        sync.conflicts.clear();
        sync.choices.clear();
        sync.import_file = None;
        self.save_sync_config(cx);
        self.run_sync(false, cx);
    }

    fn sync_choose_folder(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose the sync folder".into()),
        });
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            let _ = view.update(cx, |this, cx| this.sync_use_folder(folder, cx));
        })
        .detach();
    }

    fn sync_turn_off(&mut self, cx: &mut Context<Self>) {
        let sync = &mut self.settings.sync;
        if let Err(err) = store::disable_sync_in(&store::config_dir()) {
            sync.note = Some((true, format!("Could not turn sync off: {err}")));
        }
        sync.config = store::load_sync_config();
        sync.status = SyncStatus::Off;
        sync.conflicts.clear();
        sync.choices.clear();
        sync.import_file = None;
        cx.notify();
    }

    /// Run a sync (or finish an import) in the background. `quiet` runs,
    /// the once-a-minute check, show nothing unless something changed.
    pub(in crate::workbench) fn run_sync(&mut self, quiet: bool, cx: &mut Context<Self>) {
        let sync = &mut self.settings.sync;
        if sync.running || !sync.config.enabled && sync.import_file.is_none() {
            return;
        }
        sync.running = true;
        if !quiet {
            sync.status = SyncStatus::Syncing;
            sync.note = None;
        }
        let choices = sync.choices.clone();
        let import = sync.import_file.clone();
        let cats = sync.config.categories;
        let task = cx.background_executor().spawn(async move {
            let dir = store::config_dir();
            match import {
                Some(file) => store::import_kilat_file(&dir, &file, cats, &choices),
                None => store::sync_with(&dir, &choices),
            }
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| this.finish_sync(quiet, result, cx));
        })
        .detach();
        cx.notify();
    }

    fn finish_sync(
        &mut self,
        quiet: bool,
        result: Result<SyncOutcome, String>,
        cx: &mut Context<Self>,
    ) {
        let importing = self.settings.sync.import_file.is_some();
        let sync = &mut self.settings.sync;
        sync.running = false;
        sync.config = {
            let mut fresh = store::load_sync_config();
            // Keep what the page shows if the file could not be read.
            fresh.enabled |= sync.config.enabled;
            fresh
        };
        match result {
            Ok(SyncOutcome::Synced { pulled, pushed }) => {
                sync.conflicts.clear();
                sync.choices.clear();
                sync.import_file = None;
                sync.status = if sync.config.enabled {
                    SyncStatus::Synced
                } else {
                    SyncStatus::Off
                };
                if !quiet || pulled + pushed > 0 {
                    sync.note = Some((
                        false,
                        if importing {
                            format!("Imported. {pulled} items added or changed.")
                        } else if pulled + pushed == 0 {
                            "Everything is up to date.".to_string()
                        } else {
                            format!(
                                "Synced: {pulled} items updated here, {pushed} sent to the folder."
                            )
                        },
                    ));
                }
                if pulled > 0 {
                    self.reload_after_sync(cx);
                }
            }
            Ok(SyncOutcome::Conflicts(conflicts)) => {
                sync.status = SyncStatus::Conflicts(conflicts.len());
                sync.note = None;
                sync.conflicts = conflicts;
                sync.choices.clear();
            }
            Err(err) => {
                crate::error_log::append(&format!("Sync failed: {err}"));
                sync.status = SyncStatus::Failed(err.clone());
                sync.note = Some((true, err));
            }
        }
        cx.notify();
    }

    /// Re-read what a sync may have changed on disk.
    fn reload_after_sync(&mut self, cx: &mut Context<Self>) {
        self.profiles = store::load_profiles();
        self.connection_folder_colors = store::load_connection_folder_colors();
        self.history = store::load_history();
        self.tools.instances = store::load_tools();
        self.auto_count = store::load_auto_count();
        let handle = self.window_handle;
        let _ = cx.update_window(handle, |_, window, cx| {
            theme::set_appearance(store::load_appearance(), cx);
            theme::set_theme_names(store::load_theme_names(), cx);
            theme::apply_preference(store::load_theme_preference(), window, cx);
        });
    }

    fn sync_export(&mut self, cx: &mut Context<Self>) {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&dir, Some("backup.kilat"));
        let cats = self.sync_categories();
        let approved: std::collections::BTreeSet<String> = self
            .settings
            .sync
            .config
            .prod_approved
            .iter()
            .cloned()
            .collect();
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    store::export_kilat_file(&store::config_dir(), &path, cats, &approved)
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                this.settings.sync.note = Some(match result {
                    Ok(n) => (
                        false,
                        format!("Exported {n} items. Passwords are not included."),
                    ),
                    Err(err) => (true, err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn sync_import(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import a .kilat file".into()),
        });
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(file) = paths.into_iter().next() else {
                return;
            };
            let _ = view.update(cx, |this, cx| {
                let sync = &mut this.settings.sync;
                sync.import_file = Some(file);
                sync.conflicts.clear();
                sync.choices.clear();
                this.run_sync(false, cx);
            });
        })
        .detach();
    }

    /// Once a minute: sync if this computer or the folder changed. Starts
    /// a few seconds after launch.
    pub(in crate::workbench) fn start_sync_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            loop {
                let alive = view.update(cx, |this, cx| {
                    let sync = &this.settings.sync;
                    if sync.config.enabled && sync.config.auto && sync.conflicts.is_empty() {
                        this.run_sync(true, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(60))
                    .await;
            }
        })
        .detach();
    }

    /// The sync chip at the end of the status bar; nothing when sync is off.
    pub(in crate::workbench) fn render_sync_chip(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let sync = &self.settings.sync;
        let (symbol, color, label, tip) = match &sync.status {
            SyncStatus::Off => return None,
            SyncStatus::Idle | SyncStatus::Synced => (
                IconName::Check,
                theme.muted_foreground,
                format!("Synced {}", ago(sync.config.last_synced)),
                format!("Synced with {}", sync.config.folder),
            ),
            SyncStatus::Syncing => (
                IconName::RefreshCw,
                theme.muted_foreground,
                "Syncing…".to_string(),
                "Sync is running".to_string(),
            ),
            SyncStatus::Conflicts(n) => (
                IconName::TriangleAlert,
                theme.warning,
                format!("{n} conflict{}", if *n == 1 { "" } else { "s" }),
                "Changed on two computers. Click to choose which version to keep.".to_string(),
            ),
            SyncStatus::Failed(err) => (
                IconName::CircleAlert,
                theme.danger,
                "Sync failed".to_string(),
                err.clone(),
            ),
        };
        Some(
            div()
                .id("status-sync")
                .flex()
                .items_center()
                .gap(px(SPACE_1))
                .px(px(SPACE_2))
                .rounded(px(RADIUS_MD))
                .cursor_pointer()
                .hover(|this| this.bg(theme.secondary))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.settings.page = SettingsPage::Sync;
                    if this.settings.open {
                        cx.notify();
                    } else {
                        this.open_settings(window, cx);
                    }
                }))
                .child(icon(symbol, 12., color))
                .child(caption_text(label, theme).text_color(color)),
        )
    }

    pub(super) fn render_sync(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sync = &self.settings.sync;
        let enabled = sync.config.enabled;
        let running = sync.running;
        let mut page = div().flex().flex_col().gap(px(SPACE_4)).child(caption_text(
            "Keep your connections, favorites and appearance the same on every computer. Passwords stay in this computer's Keychain and are never synced.".into(),
            theme,
        ));

        if let Some((is_error, text)) = &sync.note {
            page = page.child(body_text(text.clone(), theme).text_color(if *is_error {
                theme.danger
            } else {
                theme.success
            }));
        }
        if !sync.conflicts.is_empty() {
            page = page.child(self.render_sync_conflicts(theme, cx));
        }

        if !enabled {
            let mut rows = vec![setting_row_owned(
                "Folder on this computer".into(),
                "Any folder: a USB disk, a network drive, or a folder that Google Drive, OneDrive, iCloud or Dropbox already syncs. No sign-in.".into(),
                Button::new("sync-choose-folder")
                    .small()
                    .primary()
                    .label("Choose folder…")
                    .on_click(cx.listener(|this, _, _, cx| this.sync_choose_folder(cx))),
                theme,
            )];
            for (ix, (label, path)) in sync.detected.iter().enumerate() {
                let folder = path.clone();
                rows.push(setting_row_owned(
                    format!("Use {label}"),
                    path.display().to_string(),
                    Button::new(("sync-detected", ix))
                        .small()
                        .label("Use this folder")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sync_use_folder(folder.clone(), cx)
                        })),
                    theme,
                ));
            }
            rows.push(setting_row_owned(
                "Google Drive account".into(),
                "Sign in with Google, no folder needed. Coming later.".into(),
                Button::new("sync-google")
                    .small()
                    .label("Coming soon")
                    .disabled(true),
                theme,
            ));
            page = page.child(setting_group(theme, rows));
        } else {
            let folder = sync.config.folder.clone();
            let status = if running {
                "Syncing…".to_string()
            } else {
                format!("Last synced {}", ago(sync.config.last_synced))
            };
            page = page.child(setting_group(
                theme,
                [
                    setting_row_owned(
                        "Folder".into(),
                        format!("{folder}\n{status}"),
                        div()
                            .flex()
                            .gap(px(SPACE_2))
                            .child(
                                Button::new("sync-now")
                                    .small()
                                    .primary()
                                    .label(if running { "Syncing…" } else { "Sync now" })
                                    .disabled(running)
                                    .on_click(cx.listener(|this, _, _, cx| this.run_sync(false, cx))),
                            )
                            .child(
                                Button::new("sync-change")
                                    .small()
                                    .label("Change…")
                                    .disabled(running)
                                    .on_click(cx.listener(|this, _, _, cx| this.sync_choose_folder(cx))),
                            ),
                        theme,
                    ),
                    setting_row_owned(
                        "Sync automatically".into(),
                        "Checks every minute and syncs as soon as something changed here or in the folder.".into(),
                        Checkbox::new("sync-auto")
                            .small()
                            .checked(sync.config.auto)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.settings.sync.config.auto = *checked;
                                this.save_sync_config(cx);
                            })),
                        theme,
                    ),
                ],
            ));

            let cats = sync.config.categories;
            let toggle = |id: &'static str,
                          label: &'static str,
                          desc: &'static str,
                          on: bool,
                          set: fn(&mut SyncCategories, bool),
                          cx: &mut Context<Self>| {
                setting_row_owned(
                    label.into(),
                    desc.into(),
                    Checkbox::new(id).small().checked(on).on_click(cx.listener(
                        move |this, checked: &bool, _, cx| {
                            set(&mut this.settings.sync.config.categories, *checked);
                            this.save_sync_config(cx);
                        },
                    )),
                    theme,
                )
            };
            page = page
                .child(div().font_weight(FontWeight::SEMIBOLD).child("What to sync"))
                .child(setting_group(
                    theme,
                    [
                        toggle(
                            "sync-conn",
                            "Connections and folders",
                            "Host, port, service name, username, folder colors, DEV / STAGING flags. Not passwords.",
                            cats.connections,
                            |c, v| c.connections = v,
                            cx,
                        ),
                        toggle(
                            "sync-fav",
                            "Favorite queries",
                            "Only the queries you starred. The rest of the history stays on this computer.",
                            cats.favorites,
                            |c, v| c.favorites = v,
                            cx,
                        ),
                        toggle(
                            "sync-tools",
                            "Developer Tools",
                            "Redis and MailHog entries. The command that starts a server is never synced.",
                            cats.tools,
                            |c, v| c.tools = v,
                            cx,
                        ),
                        toggle(
                            "sync-grid",
                            "Data grid preferences",
                            "Automatic row count.",
                            cats.grid,
                            |c, v| c.grid = v,
                            cx,
                        ),
                        toggle(
                            "sync-appearance",
                            "Appearance",
                            "Light and dark theme, accent, fonts, app icon.",
                            cats.appearance,
                            |c, v| c.appearance = v,
                            cx,
                        ),
                    ],
                ));

            let mut prod: Vec<(String, String, String)> = Vec::new();
            if cats.connections {
                prod.extend(
                    self.profiles
                        .iter()
                        .filter(|p| {
                            p.access.environment == ora_core::ConnectionEnvironment::Production
                        })
                        .map(|p| {
                            (
                                p.id.clone(),
                                p.name.clone(),
                                format!("PROD · {}@{}", p.username, p.connect_string),
                            )
                        }),
                );
            }
            if cats.tools {
                prod.extend(
                    self.tools
                        .instances
                        .iter()
                        .filter(|t| t.environment == ora_core::ConnectionEnvironment::Production)
                        .map(|t| {
                            (
                                t.id.clone(),
                                t.name.clone(),
                                format!("PROD · {} {}", t.kind.label(), t.address()),
                            )
                        }),
                );
            }
            if !prod.is_empty() {
                let rows = prod.into_iter().map(|(pid, pname, desc)| {
                    let id = pid.clone();
                    let checked = sync.config.prod_approved.contains(&pid);
                    setting_row_owned(
                        pname,
                        desc,
                        Checkbox::new(format!("sync-prod-{pid}"))
                            .small()
                            .label("Sync")
                            .checked(checked)
                            .on_click(cx.listener(move |this, on: &bool, _, cx| {
                                let list = &mut this.settings.sync.config.prod_approved;
                                list.retain(|existing| existing != &id);
                                if *on {
                                    list.push(id.clone());
                                }
                                this.save_sync_config(cx);
                            })),
                        theme,
                    )
                });
                page = page
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("PROD connections and tools"))
                    .child(caption_text(
                        "PROD connections and tools stay on this computer unless you tick them here. Ticked ones are uploaded to the folder (without password).".into(),
                        theme,
                    ))
                    .child(setting_group(theme, rows.collect::<Vec<_>>()));
            }
        }

        page = page
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Backup file"))
            .child(setting_group(
                theme,
                [
                    setting_row_owned(
                        "Export to a .kilat file".into(),
                        "A backup, or a file for a teammate. Passwords are never included.".into(),
                        Button::new("sync-export")
                            .small()
                            .label("Export…")
                            .on_click(cx.listener(|this, _, _, cx| this.sync_export(cx))),
                        theme,
                    ),
                    setting_row_owned(
                        "Import a .kilat file".into(),
                        "Adds what is missing here. Anything that differs, you choose. Nothing is deleted.".into(),
                        Button::new("sync-import")
                            .small()
                            .label("Import…")
                            .on_click(cx.listener(|this, _, _, cx| this.sync_import(cx))),
                        theme,
                    ),
                ],
            ));

        if enabled {
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_3))
                    .child(
                        Button::new("sync-off")
                            .small()
                            .label("Turn off sync")
                            .on_click(cx.listener(|this, _, _, cx| this.sync_turn_off(cx))),
                    )
                    .child(caption_text(
                        "Your data here and in the folder stays as it is.".into(),
                        theme,
                    )),
            );
        }
        page
    }

    fn render_sync_conflicts(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sync = &self.settings.sync;
        let all_chosen = sync
            .conflicts
            .iter()
            .all(|c| sync.choices.contains_key(&c.key));
        let mut card = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_3))
            .p(px(SPACE_4))
            .rounded(px(10.))
            .border_1()
            .border_color(theme.warning)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(icon(IconName::TriangleAlert, 16., theme.warning))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!(
                                "{} item{} changed on two computers",
                                sync.conflicts.len(),
                                if sync.conflicts.len() == 1 { "" } else { "s" }
                            )),
                    ),
            )
            .child(caption_text(
                "Everything else is already synced. Choose which version to keep. Nothing changes until you click Apply.".into(),
                theme,
            ));
        for (ix, c) in sync.conflicts.iter().enumerate() {
            let chosen = sync.choices.get(&c.key).copied();
            let side = |id: &'static str,
                        title: &'static str,
                        text: String,
                        choice: Choice,
                        key: String,
                        cx: &mut Context<Self>| {
                let selected = chosen == Some(choice);
                div()
                    .id((id, ix))
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .p(px(SPACE_2))
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(if selected {
                        theme.primary
                    } else {
                        theme.border
                    })
                    .when(selected, |d| d.bg(theme.accent))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.sync.choices.insert(key.clone(), choice);
                        cx.notify();
                    }))
                    .child(caption_text(title.into(), theme))
                    .child(body_text(text, theme).truncate())
            };
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(conflict_title(c)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(SPACE_2))
                            .child(side(
                                "conflict-local",
                                "This computer",
                                describe(&c.key, &c.local),
                                Choice::Local,
                                c.key.clone(),
                                cx,
                            ))
                            .child(side(
                                "conflict-remote",
                                if sync.import_file.is_some() {
                                    "In the file"
                                } else {
                                    "Other computer"
                                },
                                describe(&c.key, &c.remote),
                                Choice::Remote,
                                c.key.clone(),
                                cx,
                            )),
                    ),
            );
        }
        card.child(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_2))
                .child(
                    Button::new("conflict-all-local")
                        .small()
                        .label("Keep all from this computer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let sync = &mut this.settings.sync;
                            for c in &sync.conflicts {
                                sync.choices.insert(c.key.clone(), Choice::Local);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("conflict-all-remote")
                        .small()
                        .label("Keep all from the other")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let sync = &mut this.settings.sync;
                            for c in &sync.conflicts {
                                sync.choices.insert(c.key.clone(), Choice::Remote);
                            }
                            cx.notify();
                        })),
                )
                .child(div().flex_1())
                .child(
                    Button::new("conflict-later")
                        .small()
                        .label("Decide later")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let sync = &mut this.settings.sync;
                            sync.import_file = None;
                            sync.conflicts.clear();
                            sync.choices.clear();
                            sync.status = SyncStatus::Idle;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("conflict-apply")
                        .small()
                        .primary()
                        .label("Apply")
                        .disabled(!all_chosen)
                        .on_click(cx.listener(|this, _, _, cx| this.run_sync(false, cx))),
                ),
        )
    }

    /// Look for cloud folders when the page opens.
    pub(super) fn detect_sync_folders(&mut self) {
        self.settings.sync.detected = store::detect_cloud_folders();
    }
}
