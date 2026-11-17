//! Update check against GitHub Releases. At startup (at most once a day,
//! silent on failure) and from "Check for Updates" (menu / Settings). A
//! newer release shows a banner with its notes and a button that opens the
//! release page in the browser (the banner is one minimal line; its
//! "What's new" popup lists the full release notes); nothing is downloaded or installed here.
//! The request is one anonymous GET of the public API: no identifiers.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ora_core::store::{self, UpdatePrefs};
use ora_core::update::{self, ReleaseInfo};

use super::*;

pub(super) struct UpdateState {
    prefs: UpdatePrefs,
    checking: bool,
    /// Newest release, when newer than this build.
    available: Option<ReleaseInfo>,
    /// Banner visible (hidden by "Nanti" until the next newer version).
    banner: bool,
    /// "What's new" popup open.
    details_open: bool,
    /// Result of the last manual check, shown in Settings.
    status: Option<String>,
}

impl UpdateState {
    pub(super) fn load() -> Self {
        Self {
            prefs: store::load_update_prefs(),
            checking: false,
            available: None,
            banner: false,
            details_open: false,
            status: None,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Blocking fetch of the latest release; run on a background thread.
fn fetch_latest() -> Result<Option<ReleaseInfo>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        // OS trust store (macOS Keychain, Windows, Linux CA bundle), so
        // corporate TLS-inspecting proxies work like in the browser.
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .user_agent(concat!("gpui-like-toad/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent
        .get(update::LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok(update::parse_release(&body))
}

impl Workbench {
    /// Startup check: only if enabled and not checked within the interval.
    pub fn auto_check_updates(&mut self, cx: &mut Context<Self>) {
        let u = &self.updates;
        if update::should_auto_check(u.prefs.auto_check, u.prefs.last_checked, now_secs()) {
            self.check_updates(false, cx);
        }
    }

    /// `manual`: report "up to date" / errors and ignore a dismissed version.
    pub(super) fn check_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.updates.checking {
            return;
        }
        self.updates.checking = true;
        if manual {
            self.updates.status = Some("Mengecek update...".to_string());
        }
        let task = cx
            .background_executor()
            .spawn(async move { fetch_latest() });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.finish_update_check(result, manual);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_update_check(&mut self, result: Result<Option<ReleaseInfo>, String>, manual: bool) {
        let u = &mut self.updates;
        u.checking = false;
        match result {
            Err(err) => {
                // Silent at startup (offline is normal); retry next launch.
                if manual {
                    u.status = Some(format!("Gagal mengecek update: {err}"));
                }
            }
            Ok(release) => {
                u.prefs.last_checked = now_secs();
                let newer = release.and_then(|r| update::newer_than(r, env!("CARGO_PKG_VERSION")));
                match newer {
                    Some(r) => {
                        let dismissed = u.prefs.dismissed_version.as_deref() == Some(&r.tag);
                        u.status = Some(format!("Versi {} tersedia.", r.version));
                        u.banner = manual || !dismissed;
                        u.details_open = false;
                        u.available = Some(r);
                    }
                    None => {
                        u.available = None;
                        u.banner = false;
                        if manual {
                            u.status = Some("Sudah versi terbaru.".to_string());
                        }
                    }
                }
                self.save_update_prefs();
            }
        }
    }

    fn save_update_prefs(&mut self) {
        if let Err(err) = store::save_update_prefs(&self.updates.prefs) {
            self.push_message(format!("Gagal menyimpan pengaturan update: {err}"));
        }
    }

    pub(super) fn set_auto_check_updates(&mut self, on: bool) {
        self.updates.prefs.auto_check = on;
        self.save_update_prefs();
    }

    fn dismiss_update_banner(&mut self) {
        self.updates.banner = false;
        if let Some(r) = &self.updates.available {
            self.updates.prefs.dismissed_version = Some(r.tag.clone());
            self.save_update_prefs();
        }
    }

    /// Settings > Updates section.
    pub(super) fn render_update_settings(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let u = &self.updates;
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(form_label("Updates", theme))
            .child(
                Checkbox::new("auto-check-updates")
                    .label("Cek update otomatis saat start")
                    .checked(u.prefs.auto_check)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_auto_check_updates(*checked);
                        cx.notify();
                    })),
            )
            .child(
                div().flex().flex_row().child(
                    Button::new("check-updates")
                        .label("Check for Updates")
                        .icon(IconName::RefreshCw)
                        .disabled(u.checking)
                        .on_click(cx.listener(|this, _, _, cx| this.check_updates(true, cx))),
                ),
            );
        if let Some(status) = &u.status {
            section = section.child(caption_text(status.clone(), theme));
        }
        section.child(caption_text(
            "Cek ke GitHub Releases (maksimal sehari sekali); tidak ada data kamu yang dikirim."
                .to_string(),
            theme,
        ))
    }

    /// Minimal one-line banner above the whole window when a newer release
    /// exists; clicking the text or "Lihat detail" opens the popup.
    pub(super) fn render_update_banner(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let u = &self.updates;
        let release = u.available.as_ref().filter(|_| u.banner)?;
        let text = format!(
            "Versi {} tersedia (kamu pakai {}).",
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
                        .text_size(px(FONT_LABEL))
                        .line_height(px(LEADING_LABEL))
                        .text_color(theme.accent_foreground)
                        .child(text),
                )
                .child(
                    Button::new("update-details")
                        .label("Lihat detail")
                        .primary()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.updates.details_open = true;
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

    /// "What's new" popup: every note of the release, plus the button that
    /// opens the release page.
    pub(super) fn render_update_details(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let release = self
            .updates
            .available
            .as_ref()
            .filter(|_| self.updates.details_open)?;
        let url = release.url.clone();
        let notes = release.notes.trim();
        let mut list = div()
            .id("update-notes")
            .flex()
            .flex_col()
            .gap(px(SPACE_1))
            .max_h(px(320.))
            .overflow_y_scroll()
            .px(px(SPACE_4))
            .py(px(SPACE_2));
        if notes.is_empty() {
            list = list.child(caption_text("Tidak ada catatan rilis.".to_string(), theme));
        }
        for line in notes.lines().filter(|l| !l.trim().is_empty()) {
            list = list.child(body_text(line.to_string(), theme));
        }
        let card = div()
            .id("update-card")
            .flex()
            .flex_col()
            .w(px(560.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(SPACE_4))
                    .pt(px(SPACE_3))
                    .pb(px(SPACE_2))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(form_label("What's new", theme))
                    .child(caption_text(
                        format!(
                            "Versi {} (kamu pakai {})",
                            release.version,
                            env!("CARGO_PKG_VERSION")
                        ),
                        theme,
                    )),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_4))
                    .py(px(SPACE_3))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("update-close")
                            .label("Tutup")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.updates.details_open = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("update-open")
                            .label("Buka halaman rilis")
                            .primary()
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            );
        Some(
            div()
                .id("update-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .pt(px(96.))
                .bg(gpui_kit::hsla(0., 0., 0., 0.35))
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.updates.details_open = false;
                    cx.notify();
                }))
                .child(card)
                .into_any_element(),
        )
    }
}
