//! Update check against GitHub Releases. At startup (at most once a day,
//! silent on failure) and from "Check for Updates" (menu / Settings). A
//! newer release shows a banner with its notes and a button that opens the
//! release page in the browser (the banner is one minimal line; a
//! "What's new" tab next to Home lists the full release notes). The tab
//! also has "Update Now": gpui-updater-core downloads the asset for
//! this OS, swaps the app in, then "Restart" relaunches. "Download" and
//! "Update Now" both check the file with `release_verify` (signed
//! SHA256SUMS + minisign signature, key in release-key.pub) while it is
//! still in a private temp directory; only a verified file is handed on.
//! The request is one anonymous GET of the public API: no identifiers.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use ora_core::store::{self, UpdatePrefs};
use ora_core::update::{self, ReleaseInfo};

use crate::release_fetch::{download_asset, fetch_release, install_release, set_http_proxy};

use super::*;

enum CheckDialog {
    Checking,
    UpToDate,
    Available,
    Failed(String),
}

pub(super) struct UpdateState {
    prefs: UpdatePrefs,
    checking: bool,
    check_generation: u64,
    dialog: Option<CheckDialog>,
    dialog_focus: gpui_kit::FocusHandle,
    previous_focus: Option<gpui_kit::FocusHandle>,
    /// Newest release, when newer than this build.
    available: Option<ReleaseInfo>,
    /// Banner visible (hidden by "Later" until the next newer version).
    banner: bool,
    /// "What's new" tab exists (next to Home) and is the shown page.
    tab_open: bool,
    tab_active: bool,
    installed_notes_selected: bool,
    installed_notes: Option<Result<ReleaseInfo, String>>,
    /// Result of the last manual check, shown in Settings.
    status: Option<String>,
    /// Download of the release file in progress / its outcome.
    downloading: bool,
    download_note: Option<(bool, String)>,
    /// In-app update running (download, verify, install).
    installing: bool,
    /// Update installed; relaunch path (None = relaunch the same path).
    staged: Option<Option<std::path::PathBuf>>,
}

impl UpdateState {
    pub(super) fn load(cx: &mut Context<Workbench>) -> Self {
        let prefs = store::load_update_prefs();
        // The saved proxy (with its password from the keychain) is loaded
        // off the UI thread; the first update request waits for it too.
        cx.background_executor()
            .spawn(async {
                if let Some(warning) = crate::release_fetch::ensure_proxy_resolved() {
                    crate::error_log::append(&warning);
                }
            })
            .detach();
        Self {
            prefs,
            checking: false,
            check_generation: 0,
            dialog: None,
            dialog_focus: cx.focus_handle(),
            previous_focus: None,
            available: None,
            banner: false,
            tab_open: false,
            tab_active: false,
            installed_notes_selected: false,
            installed_notes: None,
            status: None,
            downloading: false,
            download_note: None,
            installing: false,
            staged: None,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Workbench {
    /// Download the file for this OS, then show it in the file manager.
    fn download_update(&mut self, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if u.downloading {
            return;
        }
        let Some((info, asset)) = u.available.as_ref().and_then(|r| {
            update::pick_asset(&r.assets, std::env::consts::OS).map(|a| (r.clone(), a.clone()))
        }) else {
            u.download_note = Some((
                true,
                "No download is available for this operating system. Open the release page for details.".into(),
            ));
            cx.notify();
            return;
        };
        u.downloading = true;
        u.download_note = Some((false, format!("Downloading and verifying {}…", asset.name)));
        let task = cx
            .background_executor()
            .spawn(async move { download_asset(&info, &asset) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.updates.downloading = false;
                this.updates.download_note = Some(match result {
                    Ok(path) => {
                        cx.reveal_path(&path);
                        (false, format!("Verified and saved to {}", path.display()))
                    }
                    Err(err) => (true, format!("Download failed: {err}")),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Download, verify and install the update in place.
    fn install_update(&mut self, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if u.installing || u.downloading {
            return;
        }
        let Some(info) = u.available.clone() else {
            return;
        };
        u.installing = true;
        u.download_note = Some((
            false,
            format!("Downloading and installing {}…", info.version),
        ));
        let task = cx
            .background_executor()
            .spawn(async move { install_release(&info) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.updates.installing = false;
                this.updates.download_note = Some(match result {
                    Ok(path) => {
                        this.updates.staged = Some(path);
                        (
                            false,
                            "Update installed. Restart to use the new version.".into(),
                        )
                    }
                    Err(err) => (true, format!("Update failed: {err}")),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Relaunch into the installed update.
    fn restart_into_update(&mut self, cx: &mut Context<Self>) {
        if let Some(Some(path)) = self.updates.staged.clone() {
            cx.set_restart_path(path);
        }
        cx.restart();
    }

    /// Startup check: only if enabled and not checked within the interval.
    pub fn auto_check_updates(&mut self, cx: &mut Context<Self>) {
        // Windows: delete the `.old.exe` a previous update left behind.
        #[cfg(target_os = "windows")]
        if let Ok(root) = gpui_updater_core::current_install_root() {
            gpui_updater_core::install::remove_leftovers(&root);
        }
        self.show_installed_update(cx);
        let u = &self.updates;
        if update::should_auto_check(u.prefs.auto_check, u.prefs.last_checked, now_secs()) {
            self.check_updates(cx);
        }
    }

    fn show_installed_update(&mut self, cx: &mut Context<Self>) {
        let current_text = env!("CARGO_PKG_VERSION");
        let Some(current) = update::Version::parse(current_text) else {
            return;
        };
        let previous = self.updates.prefs.last_launched_version.as_deref();
        if previous == Some(current_text) {
            return;
        }
        let upgraded = previous.and_then(update::Version::parse).map_or_else(
            // Legacy builds did not record their running version. A prior
            // check distinguishes an existing installation from a clean one.
            || self.updates.prefs.last_checked != 0,
            |version| version < current,
        );
        self.updates.prefs.last_launched_version = Some(current_text.to_string());
        self.save_update_prefs();
        if !upgraded {
            return;
        }
        self.updates.installed_notes_selected = true;
        self.open_whats_new();

        let task = cx.background_executor().spawn(async move {
            let url = format!(
                "https://api.github.com/repos/abu-abbas/gpui-like-toad/releases/tags/v{current}"
            );
            let release = fetch_release(&url)?;
            if release.version == current {
                Ok(release)
            } else {
                Err("The release does not match the installed version.".into())
            }
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.updates.installed_notes = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_update_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if u.dialog.is_none() {
            u.previous_focus = window.focused(cx);
        }
        u.dialog = Some(CheckDialog::Checking);
        u.status = None;
        window.focus(&u.dialog_focus, cx);
        self.check_updates(cx);
        cx.notify();
    }

    fn check_updates(&mut self, cx: &mut Context<Self>) {
        if self.updates.checking {
            return;
        }
        self.updates.checking = true;
        self.updates.check_generation += 1;
        let generation = self.updates.check_generation;
        let task = cx
            .background_executor()
            .spawn(async move { fetch_release(update::LATEST_RELEASE_API) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                if this.updates.check_generation == generation {
                    this.finish_update_check(result);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_update_check(&mut self, result: Result<ReleaseInfo, String>) {
        let u = &mut self.updates;
        u.checking = false;
        let manual = matches!(u.dialog, Some(CheckDialog::Checking));
        match result {
            Err(err) => {
                if manual {
                    u.status = Some(format!("Could not check for updates. {err}"));
                    u.dialog = Some(CheckDialog::Failed(err));
                }
            }
            Ok(release) => {
                u.prefs.last_checked = now_secs();
                match update::newer_than(release, env!("CARGO_PKG_VERSION")) {
                    Some(r) => {
                        let dismissed = u.prefs.dismissed_version.as_deref() == Some(&r.tag);
                        u.status = Some(format!("Version {} is available.", r.version));
                        u.banner = manual || !dismissed;
                        u.tab_open |= u.banner;
                        u.available = Some(r);
                        if manual {
                            u.dialog = Some(CheckDialog::Available);
                        }
                    }
                    None => {
                        u.available = None;
                        u.banner = false;
                        if manual {
                            u.status = Some("You're up to date.".to_string());
                            u.dialog = Some(CheckDialog::UpToDate);
                        }
                    }
                }
                self.save_update_prefs();
            }
        }
    }

    fn close_update_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if matches!(u.dialog.take(), Some(CheckDialog::Checking)) {
            // The blocking HTTP request can finish on its thread. Ignore its
            // response so a later check owns the result and the UI.
            u.checking = false;
            u.check_generation += 1;
        }
        let focus = u
            .previous_focus
            .take()
            .unwrap_or_else(|| self.focus_handle.clone());
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn update_check_open(&self) -> bool {
        self.updates.dialog.is_some()
    }

    pub(super) fn render_update_check(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let u = &self.updates;
        let dialog = u.dialog.as_ref()?;
        let checking = matches!(dialog, CheckDialog::Checking);
        let available = matches!(dialog, CheckDialog::Available);
        let version = env!("CARGO_PKG_VERSION");
        let (title, detail) = match dialog {
            CheckDialog::Checking => ("Checking for updates…", None),
            CheckDialog::UpToDate => (
                "You're up to date",
                Some(format!("{} {version} is the latest version.", app_name!())),
            ),
            CheckDialog::Available => (
                "Update available",
                u.available.as_ref().map(|release| {
                    format!(
                        "Version {} is available. You're running {version}.",
                        release.version
                    )
                }),
            ),
            CheckDialog::Failed(err) => ("Couldn't check for updates", Some(err.clone())),
        };
        Some(
            div()
                .id("update-check-overlay")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .justify_center()
                .items_center()
                .bg(gpui_kit::black().opacity(0.25))
                .child(
                    div()
                        .id("update-check-dialog")
                        .track_focus(&u.dialog_focus)
                        .on_key_down(cx.listener(
                            |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                                if event.keystroke.key == "escape" {
                                    cx.stop_propagation();
                                    this.close_update_check(window, cx);
                                }
                            },
                        ))
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_w(px(380.))
                        .rounded(px(RADIUS_MD))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .shadow_lg()
                        .gap(px(SPACE_3))
                        .p(px(SPACE_4))
                        .child(
                            div()
                                .flex()
                                .gap(px(SPACE_3))
                                .items_start()
                                .child(crate::about::app_icon(40., theme::appearance(cx).app_icon))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(SPACE_2))
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.popover_foreground)
                                                .child(title),
                                        )
                                        .children(detail.map(|text| body_text(text, theme)))
                                        .when(checking, |content| {
                                            content.child(
                                                Progress::new("check-update-progress")
                                                    .loading(true)
                                                    .small()
                                                    .accessibility_label("Checking for updates"),
                                            )
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(SPACE_2))
                                .child(
                                    Button::new("update-check-close")
                                        .small()
                                        .label(if checking || available {
                                            "Cancel"
                                        } else {
                                            "OK"
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close_update_check(window, cx)
                                        })),
                                )
                                .when(available, |actions| {
                                    actions.child(
                                        Button::new("update-check-details")
                                            .small()
                                            .primary()
                                            .label("View Update")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.close_update_check(window, cx);
                                                if this.settings.open {
                                                    this.close_settings(window, cx);
                                                }
                                                this.open_available_update();
                                                cx.notify();
                                            })),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    fn save_update_prefs(&mut self) {
        if let Err(err) = store::save_update_prefs(&self.updates.prefs) {
            self.push_message(format!("Failed to save update settings: {err}"));
        }
    }

    pub(super) fn set_auto_check_updates(&mut self, on: bool) {
        self.updates.prefs.auto_check = on;
        self.save_update_prefs();
    }

    /// Settings > About Kilat > Proxy: apply and remember the proxy URL (blank =
    /// use the system environment variables).
    pub(super) fn set_proxy(&mut self, text: &str, cx: &mut Context<Self>) {
        let url = Some(text.trim().to_string()).filter(|u| !u.is_empty());
        if url == self.updates.prefs.proxy {
            return;
        }
        // The password goes to the keychain, the rest to settings.json; the
        // prefs in memory never hold it.
        let (clean, password) = match &url {
            Some(u) => {
                let (c, p) = ora_core::redact::split_proxy_password(u);
                (Some(c), p)
            }
            None => (None, None),
        };
        let had_user = self
            .updates
            .prefs
            .proxy
            .as_deref()
            .is_some_and(ora_core::redact::proxy_has_user);
        set_http_proxy(url);
        self.updates.prefs.proxy = clean;
        self.save_update_prefs();
        self.save_proxy_password(had_user, password, cx);
    }

    /// Put the proxy password in the keychain (or drop the old one), off
    /// the UI thread because the keychain can block.
    fn save_proxy_password(
        &mut self,
        had_user: bool,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let task = cx.background_executor().spawn(async move {
            store::sync_proxy_password(crate::keychain::store(), had_user, password.as_deref())
        });
        cx.spawn(async move |view, cx| {
            if let Some(warning) = task.await {
                let _ = view.update(cx, |this, cx| {
                    this.push_message(warning);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn dismiss_update_banner(&mut self) {
        self.updates.banner = false;
        if let Some(r) = &self.updates.available {
            self.updates.prefs.dismissed_version = Some(r.tag.clone());
            self.save_update_prefs();
        }
    }

    /// Updates section on the About page.
    pub(super) fn render_update_settings(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let u = &self.updates;
        let auto_label = "Check for updates on startup";
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .border_t_1()
            .border_color(theme.border)
            .pt(px(SPACE_4))
            .child(form_label("Updates", theme))
            .child(
                settings::setting_row(
                    auto_label,
                    "At most once a day.",
                    gpui_kit::component::switch::Switch::new("auto-check-updates")
                        .accessibility_label(auto_label)
                        .small()
                        .checked(u.prefs.auto_check)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_auto_check_updates(*checked);
                            cx.notify();
                        })),
                    theme,
                )
                .px(px(0.))
                .min_h(px(32.)),
            )
            .child(
                settings::setting_row(
                    "Manual update check",
                    "",
                    Button::new("check-updates")
                        .small()
                        .label("Check for updates")
                        .icon(IconName::RefreshCw)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_update_check(window, cx)),
                        ),
                    theme,
                )
                .px(px(0.))
                .min_h(px(32.)),
            );
        if let Some(status) = &u.status {
            section = section.child(caption_text(status.clone(), theme));
        }
        section.child(
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .mt(px(SPACE_2))
                .pt(px(SPACE_4))
                .border_t_1()
                .border_color(theme.border)
                .child(form_label("Network", theme))
                .child(body_text("Update proxy".into(), theme))
                .child(Input::new(&self.settings.proxy_input).small().w_full())
                .child(caption_text(
                    "Leave blank to use the environment proxy for update checks and downloads."
                        .into(),
                    theme,
                ))
                .child(caption_text(
                    "Proxy passwords are stored in the system keychain.".into(),
                    theme,
                )),
        )
    }

    pub(super) fn whats_new_active(&self) -> bool {
        self.updates.tab_open && self.updates.tab_active
    }

    pub(super) fn show_whats_new_tab(&self) -> bool {
        self.updates.tab_open
    }

    pub(super) fn leave_whats_new(&mut self) {
        self.updates.tab_active = false;
    }

    fn open_whats_new(&mut self) {
        self.updates.tab_open = true;
        self.updates.tab_active = true;
        // Suspend query shortcuts while release notes are active.
        self.show_empty_workspace = true;
    }

    fn open_available_update(&mut self) {
        self.updates.installed_notes_selected = false;
        self.open_whats_new();
    }

    /// Minimal one-line banner above the whole window when a newer release
    /// exists; clicking the text or "View Details" opens the popup.
    pub(super) fn render_update_banner(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let u = &self.updates;
        let release = u.available.as_ref().filter(|_| u.banner)?;
        let text = format!(
            "Version {} is available. You're running {}.",
            release.version,
            env!("CARGO_PKG_VERSION")
        );
        Some(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(SPACE_2))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(SPACE_3))
                .bg(theme.accent)
                .border_b_1()
                .border_color(theme.border)
                .child(icon(IconName::Download, 14., theme.primary))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
                        .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
                        .text_color(theme.accent_foreground)
                        .child(text),
                )
                .child(
                    Button::new("update-details")
                        .label("View Details")
                        .primary()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_available_update();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("update-dismiss")
                        .icon(IconName::X)
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dismiss_update_banner();
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// The "What's new" tab button, placed right after Home.
    pub(super) fn render_whats_new_tab(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let active = self.whats_new_active();
        let mut container = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(28.))
            .rounded(px(RADIUS_MD));
        container = if active {
            container
                .bg(theme.tab_active)
                .border_1()
                .border_color(theme.border)
        } else {
            container.hover(|this| this.bg(theme.secondary))
        };
        let color = if active {
            theme.tab_active_foreground
        } else {
            theme.tab_foreground
        };
        container
            .child(
                div()
                    .id("tab-whats-new")
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(relative(1.0))
                    .gap(px(SPACE_1))
                    .pl(px(SPACE_2))
                    .pr(px(SPACE_1))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_whats_new();
                        cx.notify();
                    }))
                    .child(icon(
                        IconName::Sparkles,
                        14.,
                        if active {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        },
                    ))
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
                            .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color)
                            .child("What's new"),
                    ),
            )
            .child(
                div()
                    .id("tab-whats-new-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(20.))
                    .w(px(20.))
                    .mr(px(SPACE_1))
                    .rounded(px(RADIUS_SM))
                    .hover(|this| this.bg(theme.danger).text_color(theme.danger_foreground))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.updates.tab_open = false;
                        this.updates.tab_active = false;
                        cx.notify();
                    }))
                    .child(icon(IconName::X, 12., theme.muted_foreground)),
            )
            .into_any_element()
    }

    /// The What's new page: every note of the release, plus the button
    /// that opens the release page in the browser.
    pub(super) fn render_whats_new_page(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let mut page = div()
            .id("whats-new-page")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(SPACE_2))
            .p(px(SPACE_6));
        if self.updates.installed_notes_selected {
            let current = env!("CARGO_PKG_VERSION");
            let url =
                format!("https://github.com/abu-abbas/gpui-like-toad/releases/tag/v{current}");
            return page
                .child(
                    div()
                        .text_size(theme::ui_size(theme, FONT_HEADING_XL))
                        .line_height(theme::ui_size(theme, LEADING_HEADING_XL))
                        .child(format!("What's new in {current}")),
                )
                .child(
                    div().flex().child(
                        Button::new("installed-release-page")
                            .small()
                            .label("Open release page")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                )
                .children(match &self.updates.installed_notes {
                    Some(Ok(release)) if !release.notes.trim().is_empty() => release
                        .notes
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| body_text(line.to_string(), theme))
                        .collect::<Vec<_>>(),
                    Some(Ok(_)) => vec![caption_text(
                        "No release notes were published.".into(),
                        theme,
                    )],
                    Some(Err(_)) => {
                        vec![caption_text("Couldn't load release notes.".into(), theme)]
                    }
                    None => vec![caption_text("Loading release notes…".into(), theme)],
                })
                .into_any_element();
        }
        let Some(release) = self.updates.available.as_ref() else {
            return page
                .child(
                    div()
                        .text_size(crate::theme::ui_size(&theme, FONT_HEADING_XL))
                        .line_height(crate::theme::ui_size(&theme, LEADING_HEADING_XL))
                        .child("What's new"),
                )
                .child(caption_text(
                    format!(
                        "You are running the latest version ({}).",
                        env!("CARGO_PKG_VERSION")
                    ),
                    theme,
                ))
                .into_any_element();
        };
        let url = release.url.clone();
        let can_download = update::pick_asset(&release.assets, std::env::consts::OS).is_some();
        let can_install = update::pick_installable(&release.assets, std::env::consts::OS).is_some();
        let staged = self.updates.staged.is_some();
        let installing = self.updates.installing;
        let downloading = self.updates.downloading || installing;
        let note = self.updates.download_note.clone();
        page = page
            .child(
                div()
                    .text_size(crate::theme::ui_size(&theme, FONT_HEADING_XL))
                    .line_height(crate::theme::ui_size(&theme, LEADING_HEADING_XL))
                    .child(format!("What's new di {}", release.version)),
            )
            .child(caption_text(
                format!("You are running version {}.", env!("CARGO_PKG_VERSION")),
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .py(px(SPACE_2))
                    .when(staged, |row| {
                        row.child(
                            Button::new("update-restart")
                                .label("Restart")
                                .icon(IconName::Play)
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.restart_into_update(cx)),
                                ),
                        )
                    })
                    .when(can_install && !staged, |row| {
                        row.child(
                            Button::new("update-install")
                                .label("Update Now")
                                .icon(IconName::Download)
                                .primary()
                                .loading(installing)
                                .disabled(downloading)
                                .on_click(cx.listener(|this, _, _, cx| this.install_update(cx))),
                        )
                    })
                    .when(can_download && !staged, |row| {
                        row.child({
                            let b = Button::new("update-download")
                                .label("Download")
                                .icon(IconName::Download)
                                .disabled(downloading)
                                .on_click(cx.listener(|this, _, _, cx| this.download_update(cx)));
                            if can_install {
                                b
                            } else {
                                b.primary()
                            }
                        })
                    })
                    .child(
                        Button::new("update-open")
                            .label("Open release page")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            );
        if let Some((is_error, text)) = note {
            let line = if is_error {
                div()
                    .text_size(crate::theme::ui_size(&theme, FONT_CAPTION))
                    .line_height(crate::theme::ui_size(&theme, LEADING_CAPTION))
                    .text_color(theme.danger)
                    .child(text)
            } else {
                caption_text(text, theme)
            };
            // A spinner next to the note while the update downloads/installs.
            page = page.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .when(downloading, |row| {
                        row.child(Spinner::new().small().color(theme.primary))
                    })
                    .child(line),
            );
        }
        let notes = release.notes.trim();
        if notes.is_empty() {
            page = page.child(empty_state_text(
                "No release notes available.".to_string(),
                theme,
            ));
        }
        for line in notes.lines().filter(|l| !l.trim().is_empty()) {
            page = page.child(body_text(line.to_string(), theme));
        }
        page.into_any_element()
    }
}
