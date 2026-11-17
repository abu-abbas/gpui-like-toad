//! Update check against GitHub Releases. At startup (at most once a day,
//! silent on failure) and from "Check for Updates" (menu / Settings). A
//! newer release shows a banner with its notes and a button that opens the
//! release page in the browser; nothing is downloaded or installed here.
//! The request is one anonymous GET of the public API: no identifiers.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ora_core::store::{self, UpdatePrefs};
use ora_core::update::{self, ReleaseInfo};

use super::*;

/// Lines of release notes shown in the banner.
const NOTES_LINES: usize = 8;

pub(super) struct UpdateState {
    prefs: UpdatePrefs,
    checking: bool,
    /// Newest release, when newer than this build.
    available: Option<ReleaseInfo>,
    /// Banner visible (hidden by "Nanti" until the next newer version).
    banner: bool,
    notes_open: bool,
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
            notes_open: false,
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
                        u.notes_open = false;
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

    /// Banner above the whole window when a newer release exists.
    pub(super) fn render_update_banner(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let u = &self.updates;
        let release = u.available.as_ref().filter(|_| u.banner)?;
        let url = release.url.clone();
        let notes: String = release
            .notes
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(NOTES_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        let mut bar = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_1))
            .px(px(SPACE_3))
            .py(px(SPACE_2))
            .bg(theme.accent)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(icon(IconName::Download, 16., theme.primary))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(FONT_LABEL))
                            .line_height(px(LEADING_LABEL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.accent_foreground)
                            .child(format!(
                                "Versi {} tersedia (kamu pakai {}).",
                                release.version,
                                env!("CARGO_PKG_VERSION")
                            )),
                    )
                    .when(!notes.is_empty(), |row| {
                        row.child(
                            Button::new("update-notes")
                                .label(if u.notes_open {
                                    "Sembunyikan catatan"
                                } else {
                                    "Catatan rilis"
                                })
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.updates.notes_open = !this.updates.notes_open;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("update-open")
                            .label("Buka halaman rilis")
                            .primary()
                            .small()
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Button::new("update-dismiss")
                            .label("Nanti")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dismiss_update_banner();
                                cx.notify();
                            })),
                    ),
            );
        if u.notes_open && !notes.is_empty() {
            bar = bar.child(caption_text(notes, theme));
        }
        Some(bar.into_any_element())
    }
}
